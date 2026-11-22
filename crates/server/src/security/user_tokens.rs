//! Les jetons à part de chaque personne, chacun pour un seul usage, pour l'iPhone :
//!
//! - un **jeton de mode** (type `mode`) ne sert qu'à changer le mode de la maison
//!   (`PUT /api/mode`) et à dire où l'on est (`PUT /api/presence`) : c'est celui des raccourcis ;
//! - un **jeton de lecture** (type `read`) ne sert qu'à lire l'état (`GET /api/state`) et les
//!   événements de détection (`GET /api/events`) : c'est celui du widget Scriptable.
//!
//! Aucun n'ouvre d'autre porte : ni appareils, ni WebSocket, ni caméras, ni export.
//!
//! Chaque jeton appartient à une personne et porte un libellé (« iPhone de Hamdi ») : une personne
//! peut en avoir plusieurs (un par téléphone) et les révoquer un par un, sans toucher à ceux des
//! autres. Ils vivent dans `serveur/jetons.json` (0600, jamais exporté).
//!
//! Comme une session, un jeton est tiré au hasard (256 bits) et seul son SHA-256 est enregistré :
//! une fuite du fichier ne donne aucun jeton utilisable. Le jeton en clair n'existe qu'à sa
//! création et n'est jamais écrit dans un journal.
//!
//! Migration depuis la 2.0.0 (un seul jeton de mode, un seul de lecture pour toute la maison) :
//! `jeton-mode.json` et `jeton-lecture.json` deviennent deux jetons du premier utilisateur, avec
//! la même empreinte (donc les raccourcis et le widget déjà installés continuent de marcher).
//! Les anciens fichiers ne sont ni modifiés ni supprimés, et ne sont plus lus une fois
//! `jetons.json` écrit : la migration est idempotente.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{account, now, write_secret};

/// Ancien jeton de mode (2.0.0) : lu pour la migration, jamais réécrit.
pub const MODE_FILE: &str = "jeton-mode.json";
/// Ancien jeton de lecture (2.0.0) : lu pour la migration, jamais réécrit.
pub const READ_FILE: &str = "jeton-lecture.json";
/// Les jetons de chaque personne.
pub const FILE: &str = "jetons.json";
/// Jetons au plus par personne (un par téléphone, avec de la marge).
pub const MAX_PER_USER: usize = 16;
const MAX_LABEL_LEN: usize = 48;
/// Libellés des jetons repris de la 2.0.0.
pub const LEGACY_MODE_LABEL: &str = "Raccourcis (jeton d'origine)";
pub const LEGACY_READ_LABEL: &str = "Widget (jeton d'origine)";

/// À quoi sert un jeton.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Mode,
    Read,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Mode => "mode",
            Kind::Read => "lecture",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    user_id: String,
    kind: Kind,
    label: String,
    /// SHA-256 du jeton, hexadécimal.
    hash: String,
    /// Secondes Unix.
    created: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    tokens: Vec<Entry>,
}

/// Format de `jeton-mode.json` et `jeton-lecture.json` (2.0.0).
#[derive(Deserialize)]
struct Legacy {
    hash: String,
    created: f64,
}

/// Un jeton, sans son empreinte : ce que l'on peut montrer.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenInfo {
    pub id: String,
    pub user_id: String,
    pub kind: Kind,
    pub label: String,
    /// Secondes Unix.
    pub created: f64,
}

impl From<&Entry> for TokenInfo {
    fn from(e: &Entry) -> Self {
        TokenInfo {
            id: e.id.clone(),
            user_id: e.user_id.clone(),
            kind: e.kind,
            label: e.label.clone(),
            created: e.created,
        }
    }
}

/// Pourquoi un jeton n'a pas pu être créé.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// Déjà [`MAX_PER_USER`] jetons pour cette personne.
    TooMany,
    /// Écriture impossible (le détail est au journal).
    Storage,
}

struct State {
    entries: Vec<Entry>,
    /// `jetons.json` fait foi (ou il n'y avait rien à migrer).
    migrated: bool,
}

pub struct UserTokens {
    /// Le dossier `serveur/` ; `None` : pas de dossier utilisable, les jetons ne survivent pas au
    /// redémarrage.
    dir: Option<PathBuf>,
    state: Mutex<State>,
    /// Une création ou révocation à la fois (une signature de raccourcis dure quelques secondes) :
    /// à part de `state`, pour qu'une vérification n'attende jamais derrière une signature.
    changing: Mutex<()>,
}

fn hash_of(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn new_id() -> String {
    let mut raw = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut raw);
    hex::encode(raw)
}

/// Un libellé propre : sans caractères de contrôle, espaces compactés, 48 caractères au plus.
pub fn clean_label(label: &str, fallback: &str) -> String {
    let text: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LABEL_LEN)
        .collect();
    if text.is_empty() {
        fallback.to_string()
    } else {
        text
    }
}

fn same_label(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl UserTokens {
    /// Ouvre `dir/jetons.json`. Absent : migre les jetons de la 2.0.0 s'il y en a.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(FILE);
        let (entries, migrated) = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<File>(&text) {
                Ok(file) => (file.tokens, true),
                Err(e) => {
                    // Mis de côté, sans rien rejouer des anciens fichiers : un jeton révoqué ne
                    // doit pas revenir.
                    let aside = path.with_extension("json.illisible");
                    log::error!(
                        "jetons illisibles, aucun jeton ({e}) ; fichier conservé sous {}",
                        aside.display()
                    );
                    std::fs::rename(&path, &aside)
                        .with_context(|| format!("mise de côté de {}", path.display()))?;
                    (Vec::new(), true)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), false),
            Err(e) => return Err(e).with_context(|| format!("lecture de {}", path.display())),
        };
        let tokens = Self {
            dir: Some(dir.to_path_buf()),
            state: Mutex::new(State { entries, migrated }),
            changing: Mutex::new(()),
        };
        tokens.ensure_migrated(&mut tokens.lock());
        Ok(tokens)
    }

    /// Un carnet sans fichier (dossier inutilisable, démonstration) : rien ne survit.
    pub fn in_memory() -> Self {
        Self {
            dir: None,
            state: Mutex::new(State {
                entries: Vec::new(),
                migrated: true,
            }),
            changing: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read_legacy(&self, name: &str) -> Option<Legacy> {
        let path = self.dir.as_ref()?.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::error!("{} illisible, non repris : {e}", path.display());
                return None;
            }
        };
        match serde_json::from_str::<Legacy>(&text) {
            Ok(old) if old.hash.len() == 64 => Some(old),
            _ => {
                log::error!("{} illisible, non repris", path.display());
                None
            }
        }
    }

    /// Reprend les jetons de la 2.0.0 au nom du premier utilisateur, une fois. Sans utilisateur
    /// encore, on recommence au prochain appel (un compte créé pendant que le serveur tourne).
    fn ensure_migrated(&self, state: &mut State) {
        if state.migrated {
            return;
        }
        let Some(dir) = &self.dir else {
            state.migrated = true;
            return;
        };
        let legacy = [
            (Kind::Mode, LEGACY_MODE_LABEL, self.read_legacy(MODE_FILE)),
            (Kind::Read, LEGACY_READ_LABEL, self.read_legacy(READ_FILE)),
        ];
        if legacy.iter().all(|(_, _, old)| old.is_none()) {
            state.migrated = true;
            return;
        }
        let Some(user_id) = account::first_user_id(dir) else {
            return;
        };
        let entries: Vec<Entry> = legacy
            .into_iter()
            .filter_map(|(kind, label, old)| {
                old.map(|old| Entry {
                    id: new_id(),
                    user_id: user_id.clone(),
                    kind,
                    label: label.to_string(),
                    hash: old.hash,
                    created: old.created,
                })
            })
            .collect();
        match write_if_absent(dir, &entries) {
            Ok(()) => {
                // Si un autre processus a écrit `jetons.json` entre-temps, c'est lui qui fait foi.
                if let Ok(Some(file)) = crate::persistence::load_json::<File>(&dir.join(FILE)) {
                    state.entries = file.tokens;
                } else {
                    state.entries = entries;
                }
                state.migrated = true;
                log::info!(
                    "jetons de mode et de lecture d'avant les comptes nominatifs repris pour le premier utilisateur"
                );
            }
            Err(e) => log::error!("migration des jetons : {e:#}"),
        }
    }

    /// Le droit de créer ou révoquer des jetons, en attendant son tour.
    pub fn change(&self) -> Change<'_> {
        Change {
            store: self,
            _one_at_a_time: self.changing.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// Le droit de changer les jetons, tout de suite ou jamais : `None` si un changement est déjà
    /// en cours (par exemple une signature de raccourcis).
    pub fn try_change(&self) -> Option<Change<'_>> {
        let guard = match self.changing.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(Change {
            store: self,
            _one_at_a_time: guard,
        })
    }

    /// Les jetons d'une personne, le plus récent d'abord. Jamais ceux des autres.
    pub fn list(&self, user_id: &str) -> Vec<TokenInfo> {
        let mut state = self.lock();
        self.ensure_migrated(&mut state);
        let mut out: Vec<TokenInfo> = state
            .entries
            .iter()
            .filter(|e| e.user_id == user_id)
            .map(TokenInfo::from)
            .collect();
        out.sort_by(|a, b| b.created.total_cmp(&a.created));
        out
    }

    /// Existe-t-il un jeton de ce type (pour n'importe qui) ?
    pub fn any(&self, kind: Kind) -> bool {
        let mut state = self.lock();
        self.ensure_migrated(&mut state);
        state.entries.iter().any(|e| e.kind == kind)
    }

    /// À qui est ce jeton ? `None` s'il est inconnu, révoqué, ou d'un autre type. Comparaison en
    /// temps constant.
    pub fn verify(&self, kind: Kind, token: &str) -> Option<TokenInfo> {
        let mut state = self.lock();
        self.ensure_migrated(&mut state);
        let wanted = hash_of(token);
        state
            .entries
            .iter()
            .filter(|e| e.kind == kind)
            .find(|e| match hex::decode(&e.hash) {
                Ok(bytes) if bytes.len() == 32 => bytes.ct_eq(wanted.as_slice()).into(),
                _ => false,
            })
            .map(TokenInfo::from)
    }

    fn persist(&self, entries: &[Entry]) -> Result<()> {
        if let Some(dir) = &self.dir {
            let file = File {
                version: 1,
                tokens: entries.to_vec(),
            };
            write_secret(
                &dir.join(FILE),
                serde_json::to_string_pretty(&file)?.as_bytes(),
            )?;
        }
        Ok(())
    }

    /// Applique un changement : fichier d'abord, mémoire ensuite. En cas d'échec d'écriture, rien
    /// ne change.
    fn commit<R>(
        &self,
        change: impl FnOnce(&mut Vec<Entry>) -> Result<R, InstallError>,
    ) -> Result<R, InstallError> {
        let mut state = self.lock();
        self.ensure_migrated(&mut state);
        let mut next = state.entries.clone();
        let result = change(&mut next)?;
        self.persist(&next).map_err(|e| {
            log::error!("jetons : {e:#}");
            InstallError::Storage
        })?;
        state.entries = next;
        Ok(result)
    }
}

/// Écrit `jetons.json` seulement s'il n'existe pas encore (lien dur : jamais d'écrasement, même si
/// un autre processus vient de l'écrire).
fn write_if_absent(dir: &Path, entries: &[Entry]) -> Result<()> {
    let path = dir.join(FILE);
    let staged = dir.join("jetons.json.nouveau");
    let _ = std::fs::remove_file(&staged);
    let file = File {
        version: 1,
        tokens: entries.to_vec(),
    };
    write_secret(&staged, serde_json::to_string_pretty(&file)?.as_bytes())?;
    let linked = std::fs::hard_link(&staged, &path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("écriture de {}", path.display())),
    }
}

/// Un changement en cours : un seul à la fois. Tant qu'il est tenu, personne d'autre ne crée ni ne
/// révoque de jeton ; la vérification, elle, n'attend jamais.
pub struct Change<'a> {
    store: &'a UserTokens,
    _one_at_a_time: MutexGuard<'a, ()>,
}

impl Change<'_> {
    /// Un nouveau jeton (256 bits, hexadécimal), pas encore valable : `install` le rend valable.
    pub fn draw() -> String {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        hex::encode(raw)
    }

    /// Le jeton tiré devient celui de cette personne pour ce libellé. Un jeton de même type et de
    /// même libellé (le même téléphone qui réinstalle) est remplacé ; les autres restent.
    pub fn install(
        &self,
        user_id: &str,
        kind: Kind,
        label: &str,
        token: &str,
    ) -> Result<TokenInfo, InstallError> {
        let label = clean_label(label, "Mon téléphone");
        let entry = Entry {
            id: new_id(),
            user_id: user_id.to_string(),
            kind,
            label,
            hash: hex::encode(hash_of(token)),
            created: now(),
        };
        let info = TokenInfo::from(&entry);
        self.store.commit(|entries| {
            entries.retain(|e| {
                !(e.user_id == entry.user_id
                    && e.kind == kind
                    && same_label(&e.label, &entry.label))
            });
            if entries
                .iter()
                .filter(|e| e.user_id == entry.user_id)
                .count()
                >= MAX_PER_USER
            {
                return Err(InstallError::TooMany);
            }
            entries.push(entry);
            Ok(())
        })?;
        Ok(info)
    }

    /// Révoque un jeton de cette personne. `false` : elle n'en a pas sous cet identifiant.
    pub fn revoke(&self, user_id: &str, id: &str) -> Result<bool, InstallError> {
        self.store.commit(|entries| {
            let before = entries.len();
            entries.retain(|e| !(e.user_id == user_id && e.id == id));
            Ok(entries.len() != before)
        })
    }

    /// Révoque tous les jetons d'un type de cette personne ; rend leur nombre.
    pub fn revoke_kind(&self, user_id: &str, kind: Kind) -> Result<usize, InstallError> {
        self.store.commit(|entries| {
            let before = entries.len();
            entries.retain(|e| !(e.user_id == user_id && e.kind == kind));
            Ok(before - entries.len())
        })
    }
}

impl std::fmt::Debug for UserTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Jamais le contenu, même haché.
        f.debug_struct("UserTokens").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ostal-jetons-{tag}-{}-{}",
            std::process::id(),
            new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn several_tokens_per_person_revoked_one_by_one() {
        let store = UserTokens::in_memory();
        let change = store.change();
        let (t1, t2, t3) = (Change::draw(), Change::draw(), Change::draw());
        let a = change
            .install("lea", Kind::Mode, "iPhone de Léa", &t1)
            .unwrap();
        change.install("lea", Kind::Mode, "iPad", &t2).unwrap();
        change
            .install("adam", Kind::Mode, "iPhone de Léa", &t3)
            .unwrap();
        drop(change);
        assert_eq!(store.verify(Kind::Mode, &t1).unwrap().user_id, "lea");
        assert_eq!(store.verify(Kind::Mode, &t3).unwrap().user_id, "adam");
        // Un jeton de mode n'est pas un jeton de lecture.
        assert!(store.verify(Kind::Read, &t1).is_none());
        assert_eq!(store.list("lea").len(), 2);
        // Révoquer celui d'une personne ne touche ni son autre jeton ni ceux des autres, et on ne
        // révoque pas le jeton de quelqu'un d'autre.
        let change = store.change();
        assert!(!change.revoke("adam", &a.id).unwrap());
        assert!(change.revoke("lea", &a.id).unwrap());
        drop(change);
        assert!(store.verify(Kind::Mode, &t1).is_none());
        assert!(store.verify(Kind::Mode, &t2).is_some() && store.verify(Kind::Mode, &t3).is_some());
    }

    #[test]
    fn reinstalling_on_the_same_phone_replaces_only_that_token() {
        let store = UserTokens::in_memory();
        let change = store.change();
        let (old, other, new) = (Change::draw(), Change::draw(), Change::draw());
        change.install("lea", Kind::Mode, "iPhone", &old).unwrap();
        change.install("lea", Kind::Mode, "iPad", &other).unwrap();
        change
            .install("lea", Kind::Mode, "  iphone ", &new)
            .unwrap();
        drop(change);
        assert!(store.verify(Kind::Mode, &old).is_none());
        assert!(
            store.verify(Kind::Mode, &new).is_some() && store.verify(Kind::Mode, &other).is_some()
        );
        assert_eq!(store.list("lea").len(), 2);
    }

    #[test]
    fn a_person_cannot_hold_more_than_the_limit() {
        let store = UserTokens::in_memory();
        let change = store.change();
        for i in 0..MAX_PER_USER {
            change
                .install("lea", Kind::Read, &format!("tel {i}"), &Change::draw())
                .unwrap();
        }
        assert!(matches!(
            change.install("lea", Kind::Mode, "un de trop", &Change::draw()),
            Err(InstallError::TooMany)
        ));
        // Les autres ne sont pas gênés.
        change
            .install("adam", Kind::Mode, "tel", &Change::draw())
            .unwrap();
    }

    #[test]
    fn labels_are_cleaned() {
        assert_eq!(clean_label("  iPhone \n de   Léa ", "x"), "iPhone de Léa");
        assert_eq!(clean_label("   ", "Mon téléphone"), "Mon téléphone");
        assert_eq!(clean_label(&"a".repeat(200), "x").chars().count(), 48);
    }

    #[test]
    fn tokens_survive_a_restart_and_only_hashes_are_written() {
        let dir = temp("persist");
        let token = Change::draw();
        {
            let store = UserTokens::open(&dir).unwrap();
            store
                .change()
                .install("lea", Kind::Read, "iPhone", &token)
                .unwrap();
        }
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!text.contains(&token), "jamais le jeton en clair");
        let again = UserTokens::open(&dir).unwrap();
        assert_eq!(again.verify(Kind::Read, &token).unwrap().label, "iPhone");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn write_legacy(dir: &Path, name: &str, token: &str, created: f64) {
        let text = format!(
            "{{\"hash\":\"{}\",\"created\":{created}}}",
            hex::encode(hash_of(token))
        );
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn write_legacy_account(dir: &Path, id: &str) {
        std::fs::write(
            dir.join("compte.json"),
            format!(
                "{{\"id\":\"{id}\",\"username\":\"hamdi\",\"password_hash\":\"x\",\"created\":1.0}}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn the_2_0_0_tokens_become_the_first_users_and_the_old_files_stay() {
        let dir = temp("migrate");
        write_legacy_account(&dir, "premier");
        write_legacy(&dir, MODE_FILE, "ancien-mode", 10.0);
        write_legacy(&dir, READ_FILE, "ancien-lecture", 20.0);
        let before = (
            std::fs::read(dir.join(MODE_FILE)).unwrap(),
            std::fs::read(dir.join(READ_FILE)).unwrap(),
            std::fs::read(dir.join("compte.json")).unwrap(),
        );
        let store = UserTokens::open(&dir).unwrap();
        assert_eq!(
            store.verify(Kind::Mode, "ancien-mode").unwrap().user_id,
            "premier"
        );
        assert_eq!(
            store.verify(Kind::Read, "ancien-lecture").unwrap().user_id,
            "premier"
        );
        assert_eq!(store.list("premier").len(), 2);
        assert!(store.list("autre").is_empty());
        // Rien d'ancien n'est touché ; les nouveaux fichiers sont à côté.
        assert_eq!(std::fs::read(dir.join(MODE_FILE)).unwrap(), before.0);
        assert_eq!(std::fs::read(dir.join(READ_FILE)).unwrap(), before.1);
        assert_eq!(std::fs::read(dir.join("compte.json")).unwrap(), before.2);
        assert!(dir.join(FILE).is_file());
        // Idempotent : un second démarrage retrouve les mêmes jetons, sans en ajouter.
        let ids: Vec<String> = store.list("premier").into_iter().map(|t| t.id).collect();
        drop(store);
        let again = UserTokens::open(&dir).unwrap();
        let ids_again: Vec<String> = again.list("premier").into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ids_again);
        // Un jeton révoqué ne revient pas du vieux fichier.
        let id = again.verify(Kind::Mode, "ancien-mode").unwrap().id;
        assert!(again.change().revoke("premier", &id).unwrap());
        drop(again);
        let third = UserTokens::open(&dir).unwrap();
        assert!(third.verify(Kind::Mode, "ancien-mode").is_none());
        assert!(third.verify(Kind::Read, "ancien-lecture").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migration_waits_for_a_first_user() {
        let dir = temp("attente");
        write_legacy(&dir, MODE_FILE, "ancien-mode", 10.0);
        let store = UserTokens::open(&dir).unwrap();
        assert!(store.verify(Kind::Mode, "ancien-mode").is_none());
        assert!(!dir.join(FILE).exists());
        // Le compte arrive pendant que le serveur tourne.
        write_legacy_account(&dir, "premier");
        assert_eq!(
            store.verify(Kind::Mode, "ancien-mode").unwrap().user_id,
            "premier"
        );
        assert!(dir.join(FILE).is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unreadable_file_is_set_aside_without_replaying_the_old_tokens() {
        let dir = temp("illisible");
        write_legacy_account(&dir, "premier");
        write_legacy(&dir, MODE_FILE, "ancien-mode", 10.0);
        std::fs::write(dir.join(FILE), "pas du json").unwrap();
        let store = UserTokens::open(&dir).unwrap();
        assert!(store.verify(Kind::Mode, "ancien-mode").is_none());
        assert!(dir.join("jetons.json.illisible").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
