//! Comptes nominatifs du serveur, rangés dans `comptes.json` (créés en ligne de commande ou
//! dans l'app Ostal Server). Les mots de passe sont hachés avec argon2id ; ils ne sont jamais
//! écrits ni journalisés.
//!
//! Migration : l'ancien compte unique (`compte.json`, version 2.0.0) devient le premier
//! utilisateur de `comptes.json`, avec le même identifiant interne, donc sans casser ses
//! sessions. `compte.json` n'est jamais modifié ni supprimé, et la migration ne se refait pas
//! une fois `comptes.json` écrit (elle est idempotente).
//!
//! Couper les sessions d'un utilisateur (nouveau mot de passe, désactivation) = augmenter son
//! « époque de session » : une session n'est valable que si elle porte l'époque actuelle de son
//! utilisateur. Le serveur relit `comptes.json` dès qu'il change, donc une commande lancée
//! pendant qu'il tourne (CLI, app) prend effet tout de suite.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::now;

/// Ancien fichier du compte unique (2.0.0) : lu pour la migration, jamais réécrit.
pub const FILE_NAME: &str = "compte.json";
/// Fichier des comptes nominatifs.
pub const USERS_FILE: &str = "comptes.json";
/// Longueur minimale d'un mot de passe.
pub const MIN_PASSWORD_LEN: usize = 10;
const MAX_PASSWORD_LEN: usize = 512;
const MAX_USERNAME_LEN: usize = 64;
const MAX_DISPLAY_NAME_LEN: usize = 64;

/// Rôle d'un utilisateur. En V1, tout le monde est administrateur (accès complet) ; le champ
/// existe pour que les rôles puissent venir plus tard sans migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Admin,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredUser {
    /// Identifiant interne aléatoire : les sessions s'y rattachent.
    id: String,
    /// Identifiant de connexion.
    username: String,
    #[serde(default)]
    display_name: String,
    /// Hachage argon2id au format PHC (sel et paramètres inclus).
    password_hash: String,
    #[serde(default)]
    role: Role,
    #[serde(default = "default_true")]
    active: bool,
    created: f64,
    /// Augmenté pour couper toutes les sessions de cet utilisateur.
    #[serde(default)]
    session_epoch: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct UsersFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    users: Vec<StoredUser>,
}

/// Ancien format : `compte.json`.
#[derive(Deserialize)]
struct LegacyAccountFile {
    id: String,
    username: String,
    password_hash: String,
    created: f64,
}

/// Un utilisateur, sans son mot de passe.
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub active: bool,
    pub created: f64,
    pub session_epoch: u64,
}

impl From<&StoredUser> for User {
    fn from(u: &StoredUser) -> Self {
        User {
            id: u.id.clone(),
            username: u.username.clone(),
            display_name: if u.display_name.is_empty() {
                u.username.clone()
            } else {
                u.display_name.clone()
            },
            role: u.role,
            active: u.active,
            created: u.created,
            session_epoch: u.session_epoch,
        }
    }
}

/// Ce qu'une connexion réussie donne à la session qui s'ouvre.
#[derive(Clone, Debug, PartialEq)]
pub struct Granted {
    pub user_id: String,
    pub epoch: u64,
}

/// Résultat d'une vérification d'identifiants.
pub enum Verdict {
    /// Identifiants valides pour un compte actif.
    Valid(Granted),
    /// Mauvais identifiant, mauvais mot de passe, compte désactivé (ou aucun compte) : on ne
    /// distingue pas.
    Invalid,
}

fn hasher() -> Argon2<'static> {
    // argon2id, 19 Mio, 2 passes : le minimum conseillé par l'OWASP.
    Argon2::default()
}

fn new_salt() -> Result<SaltString> {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    SaltString::encode_b64(&bytes).map_err(|e| anyhow::anyhow!("sel invalide : {e}"))
}

fn hash_password(password: &str) -> Result<String> {
    Ok(hasher()
        .hash_password(password.as_bytes(), &new_salt()?)
        .map_err(|e| anyhow::anyhow!("hachage du mot de passe : {e}"))?
        .to_string())
}

/// Hachage factice pour vérifier un mot de passe même quand il n'y a pas de compte (ou que
/// l'identifiant est faux) : la réponse prend le même temps dans tous les cas.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let mut secret = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut secret);
        hash_password(&hex::encode(secret)).unwrap_or_default()
    })
}

fn verify_hash(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => hasher()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Égalité de deux textes en temps constant (on compare leurs empreintes, de même longueur).
fn same_text(a: &str, b: &str) -> bool {
    let (ha, hb) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    ha.ct_eq(&hb).into()
}

/// Clé de comparaison d'un identifiant : sans distinction de casse (un téléphone met souvent
/// une majuscule au premier caractère).
pub fn username_key(username: &str) -> String {
    username.to_lowercase()
}

fn new_id() -> String {
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    hex::encode(id)
}

/// Vérifie un nom d'utilisateur à la création.
pub fn check_username(username: &str) -> Result<()> {
    let ok = !username.is_empty()
        && username.chars().count() <= MAX_USERNAME_LEN
        && !username.chars().any(char::is_control)
        && username.trim() == username;
    if !ok {
        bail!("nom d'utilisateur invalide (1 à {MAX_USERNAME_LEN} caractères, sans espace en bordure ni caractère de contrôle)");
    }
    Ok(())
}

/// Vérifie un nom affiché (le prénom que voient les autres dans la maison).
pub fn check_display_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_DISPLAY_NAME_LEN
        && !name.chars().any(char::is_control)
        && name.trim() == name;
    if !ok {
        bail!("nom affiché invalide (1 à {MAX_DISPLAY_NAME_LEN} caractères, sans espace en bordure ni caractère de contrôle)");
    }
    Ok(())
}

/// Vérifie un mot de passe à la création ou au changement.
pub fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        bail!("mot de passe trop court ({MIN_PASSWORD_LEN} caractères au minimum)");
    }
    if password.len() > MAX_PASSWORD_LEN {
        bail!("mot de passe trop long");
    }
    Ok(())
}

fn users_path(dir: &Path) -> PathBuf {
    dir.join(USERS_FILE)
}

fn legacy_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Écrit `comptes.json` seulement s'il n'existe pas encore (lien dur : jamais d'écrasement,
/// même si un autre processus vient de l'écrire).
fn write_users_if_absent(dir: &Path, file: &UsersFile) -> Result<()> {
    let path = users_path(dir);
    let staged = dir.join("comptes.json.nouveau");
    let _ = std::fs::remove_file(&staged);
    super::write_secret(&staged, serde_json::to_string_pretty(file)?.as_bytes())?;
    let linked = std::fs::hard_link(&staged, &path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("écriture de {}", path.display())),
    }
}

/// Lit les comptes. Sans `comptes.json`, migre l'ancien `compte.json` s'il existe (il reste en
/// place) ; sans l'un ni l'autre, la liste est vide.
fn read_users(dir: &Path) -> Result<UsersFile> {
    let path = users_path(dir);
    for _ in 0..2 {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                return serde_json::from_str(&text)
                    .with_context(|| format!("comptes illisibles dans {}", path.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("lecture de {}", path.display())),
        }
        let legacy = legacy_path(dir);
        let text = match std::fs::read_to_string(&legacy) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UsersFile::default()),
            Err(e) => return Err(e).with_context(|| format!("lecture de {}", legacy.display())),
        };
        let old: LegacyAccountFile = serde_json::from_str(&text)
            .with_context(|| format!("compte illisible dans {}", legacy.display()))?;
        let migrated = UsersFile {
            version: 1,
            users: vec![StoredUser {
                id: old.id,
                display_name: old.username.clone(),
                username: old.username,
                password_hash: old.password_hash,
                role: Role::Admin,
                active: true,
                created: old.created,
                session_epoch: 0,
            }],
        };
        write_users_if_absent(dir, &migrated)?;
        // On relit : si un autre processus a écrit `comptes.json` entre-temps, c'est lui qui fait foi.
    }
    bail!("comptes illisibles dans {}", path.display())
}

fn write_users(dir: &Path, file: &UsersFile) -> Result<()> {
    super::write_secret(
        &users_path(dir),
        serde_json::to_string_pretty(file)?.as_bytes(),
    )
}

fn find_index(file: &UsersFile, key: &str) -> Option<usize> {
    let wanted = username_key(key);
    file.users
        .iter()
        .position(|u| u.id == key || username_key(&u.username) == wanted)
}

/// Les utilisateurs de `dir` (migre l'ancien compte au passage).
pub fn list(dir: &Path) -> Result<Vec<User>> {
    Ok(read_users(dir)?.users.iter().map(User::from).collect())
}

/// L'identifiant du premier utilisateur (le plus ancien : celui de l'ancien `compte.json` après
/// migration), **en lecture seule** : rien n'est migré ni écrit. Les jetons et abonnements d'avant
/// les comptes nominatifs lui reviennent.
pub fn first_user_id(dir: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(users_path(dir)) {
        let file: UsersFile = serde_json::from_str(&text).ok()?;
        return file.users.first().map(|u| u.id.clone());
    }
    let text = std::fs::read_to_string(legacy_path(dir)).ok()?;
    serde_json::from_str::<LegacyAccountFile>(&text)
        .ok()
        .map(|old| old.id)
}

/// Un compte existe-t-il (comptes nominatifs ou ancien compte) ? Lecture seule.
pub fn any_account_file(dir: &Path) -> bool {
    users_path(dir).is_file() || legacy_path(dir).is_file()
}

/// Ajoute un utilisateur (administrateur en V1). `display_name` vaut par défaut l'identifiant.
pub fn add(dir: &Path, username: &str, display_name: Option<&str>, password: &str) -> Result<User> {
    check_username(username)?;
    let display_name = display_name.unwrap_or(username);
    check_display_name(display_name)?;
    check_password(password)?;
    let mut file = read_users(dir)?;
    if find_index(&file, username).is_some() {
        bail!("l'identifiant « {username} » est déjà pris");
    }
    let user = StoredUser {
        id: new_id(),
        username: username.to_string(),
        display_name: display_name.to_string(),
        password_hash: hash_password(password)?,
        role: Role::Admin,
        active: true,
        created: now(),
        session_epoch: 0,
    };
    let out = User::from(&user);
    file.version = 1;
    file.users.push(user);
    write_users(dir, &file)?;
    Ok(out)
}

/// Définit un nouveau mot de passe (`key` : identifiant de connexion ou identifiant interne) et
/// coupe toutes les sessions de cet utilisateur.
pub fn set_password(dir: &Path, key: &str, password: &str) -> Result<User> {
    check_password(password)?;
    let mut file = read_users(dir)?;
    let i = find_index(&file, key).with_context(|| format!("aucun compte « {key} »"))?;
    file.users[i].password_hash = hash_password(password)?;
    file.users[i].session_epoch += 1;
    let out = User::from(&file.users[i]);
    write_users(dir, &file)?;
    Ok(out)
}

/// Désactive ou réactive un compte. Désactiver coupe toutes ses sessions ; le dernier compte
/// actif ne peut pas être désactivé (plus personne ne pourrait se connecter).
pub fn set_active(dir: &Path, key: &str, active: bool) -> Result<User> {
    let mut file = read_users(dir)?;
    let i = find_index(&file, key).with_context(|| format!("aucun compte « {key} »"))?;
    if file.users[i].active == active {
        return Ok(User::from(&file.users[i]));
    }
    if !active && file.users.iter().filter(|u| u.active).count() <= 1 {
        bail!("c'est le dernier compte actif : le désactiver empêcherait toute connexion");
    }
    file.users[i].active = active;
    if !active {
        file.users[i].session_epoch += 1;
    }
    let out = User::from(&file.users[i]);
    write_users(dir, &file)?;
    Ok(out)
}

/// Crée le premier compte (première installation, `create-account`) ou, avec `replace`,
/// remplace **tous** les comptes par celui-là et annule toutes les sessions. Retourne `true`
/// si des comptes existaient déjà. Pour en ajouter d'autres : `add`.
pub fn create(dir: &Path, username: &str, password: &str, replace: bool) -> Result<bool> {
    check_username(username)?;
    check_password(password)?;
    let existed = match read_users(dir) {
        Ok(file) => !file.users.is_empty(),
        // Un fichier abîmé ne doit pas empêcher de repartir de zéro avec --replace.
        Err(_) if replace => true,
        Err(e) => return Err(e),
    };
    if existed && !replace {
        bail!("un compte existe déjà ; ajoutez les suivants avec « ostal-server user add », ou utilisez --replace pour tout remplacer (les sessions ouvertes seront fermées)");
    }
    let file = UsersFile {
        version: 1,
        users: vec![StoredUser {
            id: new_id(),
            username: username.to_string(),
            display_name: username.to_string(),
            password_hash: hash_password(password)?,
            role: Role::Admin,
            active: true,
            created: now(),
            session_epoch: 0,
        }],
    };
    write_users(dir, &file)?;
    // Les anciennes sessions ne correspondent plus à aucun compte : on les retire aussi du disque.
    if existed {
        let _ = std::fs::remove_file(dir.join(super::sessions::FILE_NAME));
    }
    Ok(existed)
}

/// (date de modification, taille, inode) : change à chaque écriture de `comptes.json`. Chaque
/// écriture renomme un fichier neuf, donc l'inode change même si la date et la taille restent.
type Stamp = (SystemTime, u64, u64);

fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len(), meta.ino()))
}

#[derive(Default)]
struct Cache {
    users: Vec<StoredUser>,
    stamp: Option<Stamp>,
}

/// Accès aux comptes pour le serveur. `comptes.json` est relu dès qu'il change : un compte créé,
/// désactivé ou au mot de passe changé pendant que le serveur tourne est pris en compte tout de
/// suite.
pub struct AccountStore {
    dir: PathBuf,
    cache: Mutex<Cache>,
}

impl AccountStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let file = read_users(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            cache: Mutex::new(Cache {
                users: file.users,
                stamp: stamp_of(&users_path(dir)),
            }),
        })
    }

    fn with_users<T>(&self, f: impl FnOnce(&[StoredUser]) -> T) -> T {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = stamp_of(&users_path(&self.dir));
        if stamp.is_none() || stamp != cache.stamp {
            match read_users(&self.dir) {
                Ok(file) => {
                    cache.users = file.users;
                    cache.stamp = stamp_of(&users_path(&self.dir));
                }
                // Fichier illisible : on garde les comptes connus plutôt que d'ouvrir ou de
                // fermer l'accès au hasard.
                Err(e) => log::error!("comptes non relus : {e:#}"),
            }
        }
        f(&cache.users)
    }

    pub fn exists(&self) -> bool {
        self.with_users(|users| !users.is_empty())
    }

    /// Les utilisateurs, mots de passe exclus.
    pub fn users(&self) -> Vec<User> {
        self.with_users(|users| users.iter().map(User::from).collect())
    }

    /// Ce compte existe-t-il et est-il actif ? (Les jetons de raccourci d'un compte désactivé
    /// cessent de marcher.)
    pub fn is_active(&self, user_id: &str) -> bool {
        self.with_users(|users| users.iter().any(|u| u.id == user_id && u.active))
    }

    /// Le nom affiché d'un compte (pour le journal technique).
    pub fn display_name(&self, user_id: &str) -> Option<String> {
        self.with_users(|users| {
            users
                .iter()
                .find(|u| u.id == user_id)
                .map(|u| User::from(u).display_name)
        })
    }

    /// Cette session (utilisateur et époque) vaut-elle encore : compte existant, actif, et
    /// sessions non coupées depuis ?
    pub fn is_current(&self, user_id: &str, epoch: u64) -> bool {
        self.with_users(|users| {
            users
                .iter()
                .any(|u| u.id == user_id && u.active && u.session_epoch == epoch)
        })
    }

    /// Vérifie des identifiants. Opération coûteuse (argon2) : à lancer hors du runtime.
    /// Dans tous les cas, un hachage est calculé et les comparaisons sont à temps constant.
    pub fn verify(&self, username: &str, password: &str) -> Verdict {
        let wanted = username_key(username);
        let found = self.with_users(|users| {
            let mut found: Option<StoredUser> = None;
            for u in users {
                if same_text(&wanted, &username_key(&u.username)) {
                    found = Some(u.clone());
                }
            }
            found
        });
        let password_ok = match &found {
            Some(u) => verify_hash(password, &u.password_hash),
            None => verify_hash(password, dummy_hash()),
        };
        match found {
            Some(u) if password_ok && u.active => Verdict::Valid(Granted {
                user_id: u.id,
                epoch: u.session_epoch,
            }),
            _ => Verdict::Invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "un-mot-de-passe-solide";

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-account-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn granted(v: Verdict) -> Granted {
        match v {
            Verdict::Valid(g) => g,
            Verdict::Invalid => panic!("identifiants refusés"),
        }
    }

    #[test]
    fn create_verify_and_replace() {
        let dir = temp("cr");
        let store = AccountStore::open(&dir).unwrap();
        assert!(!store.exists());
        assert!(matches!(
            store.verify("hamdi", "n'importe quoi"),
            Verdict::Invalid
        ));

        assert!(!create(&dir, "hamdi", PW, false).unwrap());
        // Le serveur déjà ouvert voit le compte créé après coup.
        assert!(store.exists());
        let first = granted(store.verify("hamdi", PW));
        assert!(matches!(
            store.verify("hamdi", "un-mot-de-passe-faux"),
            Verdict::Invalid
        ));
        assert!(matches!(store.verify("autre", PW), Verdict::Invalid));
        // Sans distinction de casse.
        assert!(matches!(store.verify("Hamdi", PW), Verdict::Valid(_)));

        // La création suivante est refusée sans --replace.
        assert!(create(&dir, "autre", "un-autre-mot-de-passe", false).is_err());
        assert!(create(&dir, "autre", "un-autre-mot-de-passe", true).unwrap());
        let fresh = AccountStore::open(&dir).unwrap();
        let second = granted(fresh.verify("autre", "un-autre-mot-de-passe"));
        assert_ne!(second.user_id, first.user_id);
        assert!(matches!(fresh.verify("hamdi", PW), Verdict::Invalid));
        assert!(!fresh.is_current(&first.user_id, first.epoch));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn password_is_hashed_and_file_private() {
        let dir = temp("hash");
        create(&dir, "hamdi", PW, false).unwrap();
        let text = std::fs::read_to_string(dir.join(USERS_FILE)).unwrap();
        assert!(!text.contains(PW));
        assert!(text.contains("$argon2id$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(USERS_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn weak_input_is_refused() {
        let dir = temp("weak");
        assert!(create(&dir, "hamdi", "court", false).is_err());
        assert!(create(&dir, "", PW, false).is_err());
        assert!(create(&dir, " hamdi", PW, false).is_err());
        assert!(add(&dir, "sam", Some(""), PW).is_err());
        assert!(!dir.join(USERS_FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn several_users_each_with_their_own_password() {
        let dir = temp("multi");
        create(&dir, "hamdi", PW, false).unwrap();
        let sam = add(&dir, "sam", Some("Sam"), "le-mot-de-passe-de-sam").unwrap();
        assert_eq!(sam.display_name, "Sam");
        assert_eq!(sam.role, Role::Admin);
        // Identifiant déjà pris, quelle que soit la casse.
        assert!(add(&dir, "SAM", None, PW).is_err());
        let store = AccountStore::open(&dir).unwrap();
        assert_eq!(store.users().len(), 2);
        assert_eq!(
            granted(store.verify("sam", "le-mot-de-passe-de-sam")).user_id,
            sam.id
        );
        assert!(matches!(store.verify("sam", PW), Verdict::Invalid));
        assert!(matches!(
            store.verify("hamdi", "le-mot-de-passe-de-sam"),
            Verdict::Invalid
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn password_change_and_disable_cut_sessions_of_that_user_only() {
        let dir = temp("cut");
        create(&dir, "hamdi", PW, false).unwrap();
        add(&dir, "sam", None, "le-mot-de-passe-de-sam").unwrap();
        let store = AccountStore::open(&dir).unwrap();
        let h = granted(store.verify("hamdi", PW));
        let s = granted(store.verify("sam", "le-mot-de-passe-de-sam"));
        assert!(store.is_current(&h.user_id, h.epoch));
        assert!(store.is_current(&s.user_id, s.epoch));

        // Le serveur ouvert voit le changement fait par une autre commande.
        set_password(&dir, "sam", "un-nouveau-mot-de-passe").unwrap();
        assert!(
            !store.is_current(&s.user_id, s.epoch),
            "sessions de Sam coupées"
        );
        assert!(
            store.is_current(&h.user_id, h.epoch),
            "celles de Hamdi intactes"
        );
        assert!(matches!(
            store.verify("sam", "le-mot-de-passe-de-sam"),
            Verdict::Invalid
        ));
        let s2 = granted(store.verify("sam", "un-nouveau-mot-de-passe"));
        assert!(store.is_current(&s2.user_id, s2.epoch));

        set_active(&dir, "sam", false).unwrap();
        assert!(!store.is_current(&s2.user_id, s2.epoch));
        assert!(
            matches!(
                store.verify("sam", "un-nouveau-mot-de-passe"),
                Verdict::Invalid
            ),
            "un compte désactivé ne se connecte plus"
        );
        set_active(&dir, "sam", true).unwrap();
        let s3 = granted(store.verify("sam", "un-nouveau-mot-de-passe"));
        assert_ne!(
            s3.epoch, s2.epoch,
            "la réactivation ne ressuscite pas les anciennes sessions"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_last_active_account_cannot_be_disabled() {
        let dir = temp("last");
        create(&dir, "hamdi", PW, false).unwrap();
        assert!(set_active(&dir, "hamdi", false).is_err());
        add(&dir, "sam", None, PW).unwrap();
        set_active(&dir, "hamdi", false).unwrap();
        assert!(set_active(&dir, "sam", false).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Données à la forme de la 2.0.0 (factices).
    fn write_legacy(dir: &Path, id: &str) {
        let file = serde_json::json!({
            "version": 1,
            "id": id,
            "username": "Hamdi",
            "password_hash": hash_password(PW).unwrap(),
            "created": 1_700_000_000.0,
        });
        std::fs::write(
            dir.join(FILE_NAME),
            serde_json::to_string_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn legacy_account_becomes_the_first_user_without_touching_the_old_file() {
        let dir = temp("legacy");
        write_legacy(&dir, "0123456789abcdef0123456789abcdef");
        let before = std::fs::read(dir.join(FILE_NAME)).unwrap();
        assert!(!dir.join(USERS_FILE).exists());

        let store = AccountStore::open(&dir).unwrap();
        let g = granted(store.verify("hamdi", PW));
        // Même identifiant interne : les sessions existantes restent valables.
        assert_eq!(g.user_id, "0123456789abcdef0123456789abcdef");
        assert!(store.is_current(&g.user_id, 0));
        let users = store.users();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].display_name, "Hamdi");
        assert!(users[0].active && users[0].role == Role::Admin);

        // L'ancien fichier est intact ; le nouveau est écrit à côté, en 0600.
        assert_eq!(std::fs::read(dir.join(FILE_NAME)).unwrap(), before);
        assert!(dir.join(USERS_FILE).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(USERS_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Idempotente : rouvrir ne change rien, et une modification faite ensuite n'est pas
        // défaite par un retour au vieux fichier.
        add(&dir, "sam", None, PW).unwrap();
        let again = AccountStore::open(&dir).unwrap();
        assert_eq!(again.users().len(), 2);
        assert_eq!(std::fs::read(dir.join(FILE_NAME)).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migration_works_when_a_cli_runs_first() {
        let dir = temp("legacy-cli");
        write_legacy(&dir, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        // `user add` lancé avant que le serveur ne démarre : migre puis ajoute.
        add(&dir, "sam", None, PW).unwrap();
        let users = list(&dir).unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
