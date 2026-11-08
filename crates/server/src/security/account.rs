//! Compte unique du serveur, créé en ligne de commande (`create-account`).
//! Le mot de passe est haché avec argon2id ; il n'est jamais écrit ni journalisé.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::now;

pub const FILE_NAME: &str = "compte.json";
/// Longueur minimale d'un mot de passe.
pub const MIN_PASSWORD_LEN: usize = 10;
const MAX_PASSWORD_LEN: usize = 512;
const MAX_USERNAME_LEN: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
struct AccountFile {
    version: u32,
    /// Identifiant interne aléatoire : une session ne vaut que pour le compte qui l'a créée
    /// (remplacer le compte invalide toutes les sessions).
    id: String,
    username: String,
    /// Hachage argon2id au format PHC (sel et paramètres inclus).
    password_hash: String,
    created: f64,
}

/// Compte chargé en mémoire.
#[derive(Clone)]
pub struct Account {
    pub id: String,
    username: String,
    password_hash: String,
}

/// Résultat d'une vérification d'identifiants.
pub enum Verdict {
    /// Identifiants valides ; identifiant interne du compte.
    Valid(String),
    /// Mauvais identifiant ou mauvais mot de passe (ou aucun compte) : on ne distingue pas.
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

/// Crée (ou, avec `replace`, remplace) le compte dans `dir`. Remplacer le compte annule
/// toutes les sessions existantes. Retourne `true` si un compte existait déjà.
pub fn create(dir: &Path, username: &str, password: &str, replace: bool) -> Result<bool> {
    check_username(username)?;
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        bail!("mot de passe trop court ({MIN_PASSWORD_LEN} caractères au minimum)");
    }
    if password.len() > MAX_PASSWORD_LEN {
        bail!("mot de passe trop long");
    }
    let path = dir.join(FILE_NAME);
    let existed = path.exists();
    if existed && !replace {
        bail!("un compte existe déjà ; utilisez --replace pour le remplacer (les sessions ouvertes seront fermées)");
    }
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    let file = AccountFile {
        version: 1,
        id: hex::encode(id),
        username: username.to_string(),
        password_hash: hash_password(password)?,
        created: now(),
    };
    super::write_secret(&path, serde_json::to_string_pretty(&file)?.as_bytes())?;
    // Les anciennes sessions ne correspondent plus à ce compte : on les retire aussi du disque.
    if existed {
        let _ = std::fs::remove_file(dir.join(super::sessions::FILE_NAME));
    }
    Ok(existed)
}

/// Accès au compte du serveur. Le fichier est relu tant qu'aucun compte n'existe, pour que
/// `create-account` fonctionne pendant que le serveur tourne.
pub struct AccountStore {
    path: PathBuf,
    current: Mutex<Option<Account>>,
}

impl AccountStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let store = Self {
            path: dir.join(FILE_NAME),
            current: Mutex::new(None),
        };
        store.reload()?;
        Ok(store)
    }

    fn reload(&self) -> Result<Option<Account>> {
        let account = match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                let file: AccountFile = serde_json::from_str(&text)
                    .with_context(|| format!("compte illisible dans {}", self.path.display()))?;
                Some(Account {
                    id: file.id,
                    username: file.username,
                    password_hash: file.password_hash,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("lecture de {}", self.path.display())),
        };
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = account.clone();
        Ok(account)
    }

    /// Compte chargé (relit le fichier s'il n'y en avait pas encore).
    fn account(&self) -> Option<Account> {
        let cached = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        cached.or_else(|| self.reload().ok().flatten())
    }

    /// Identifiant interne du compte actuel.
    pub fn account_id(&self) -> Option<String> {
        self.account().map(|a| a.id)
    }

    pub fn exists(&self) -> bool {
        self.account().is_some()
    }

    /// Vérifie des identifiants. Opération coûteuse (argon2) : à lancer hors du runtime.
    /// Dans tous les cas, un hachage est calculé et les comparaisons sont à temps constant.
    pub fn verify(&self, username: &str, password: &str) -> Verdict {
        let account = self.account();
        let (hash, known_user, id) = match &account {
            Some(a) => (
                a.password_hash.as_str(),
                same_text(username, &a.username),
                Some(a.id.clone()),
            ),
            None => (dummy_hash(), false, None),
        };
        let password_ok = verify_hash(password, hash);
        match (known_user && password_ok, id) {
            (true, Some(id)) => Verdict::Valid(id),
            _ => Verdict::Invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-account-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
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

        assert!(!create(&dir, "hamdi", "un-mot-de-passe-solide", false).unwrap());
        // Le serveur déjà ouvert voit le compte créé après coup.
        assert!(store.exists());
        assert!(matches!(
            store.verify("hamdi", "un-mot-de-passe-solide"),
            Verdict::Valid(_)
        ));
        assert!(matches!(
            store.verify("hamdi", "un-mot-de-passe-faux"),
            Verdict::Invalid
        ));
        assert!(matches!(
            store.verify("autre", "un-mot-de-passe-solide"),
            Verdict::Invalid
        ));

        // Un seul compte : la création suivante est refusée sans --replace.
        assert!(create(&dir, "autre", "un-autre-mot-de-passe", false).is_err());
        let first_id = store.account_id().unwrap();
        assert!(create(&dir, "autre", "un-autre-mot-de-passe", true).unwrap());
        let fresh = AccountStore::open(&dir).unwrap();
        assert_ne!(fresh.account_id().unwrap(), first_id);
        assert!(matches!(
            fresh.verify("autre", "un-autre-mot-de-passe"),
            Verdict::Valid(_)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn password_is_hashed_and_file_private() {
        let dir = temp("hash");
        create(&dir, "hamdi", "un-mot-de-passe-solide", false).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(!text.contains("un-mot-de-passe-solide"));
        assert!(text.contains("$argon2id$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE_NAME))
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
        assert!(create(&dir, "", "un-mot-de-passe-solide", false).is_err());
        assert!(create(&dir, " hamdi", "un-mot-de-passe-solide", false).is_err());
        assert!(!dir.join(FILE_NAME).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
