//! Les jetons à part, chacun pour un seul usage, pour l'iPhone :
//!
//! - le **jeton de mode** (`serveur/jeton-mode.json`) ne sert qu'à changer le mode de la maison
//!   (`PUT /api/mode`), pour les raccourcis et les automatisations de Raccourcis ;
//! - le **jeton de lecture** (`serveur/jeton-lecture.json`) ne sert qu'à lire l'état
//!   (`GET /api/state`) et les événements de détection (`GET /api/events`), pour le widget
//!   Scriptable.
//!
//! Aucun n'ouvre d'autre porte : ni appareils, ni WebSocket, ni caméras, ni export.
//!
//! Comme une session, un jeton est tiré au hasard (256 bits) et seul son SHA-256 est enregistré
//! (0600) : une fuite du fichier ne donne aucun jeton utilisable. Le jeton en clair n'existe qu'à
//! sa création. Il n'est jamais écrit dans un journal. En générer un nouveau révoque l'ancien.
//! `serveur/` n'est ni exporté ni importé : les jetons restent propres à ce Mac.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{now, write_secret};
use crate::persistence;

/// Le jeton de mode.
pub const MODE_FILE: &str = "jeton-mode.json";
/// Le jeton de lecture.
pub const READ_FILE: &str = "jeton-lecture.json";

#[derive(Serialize, Deserialize)]
struct Stored {
    /// SHA-256 du jeton, hexadécimal.
    hash: String,
    /// Secondes Unix.
    created: f64,
}

pub struct TokenStore {
    /// `None` : pas de dossier utilisable, le jeton ne survit pas au redémarrage.
    path: Option<PathBuf>,
    /// « jeton de mode », « jeton de lecture » : pour le journal.
    label: &'static str,
    stored: Mutex<Option<Stored>>,
    /// Une création (ou suppression) à la fois : deux créations simultanées ne rendent jamais un
    /// jeton déjà révoqué. À part de `stored`, pour qu'une vérification n'attende jamais derrière
    /// une signature de raccourcis.
    changing: Mutex<()>,
}

fn hash_of(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl TokenStore {
    /// Ouvre `dir/<file_name>` (absent : aucun jeton, donc aucun accès par ce jeton).
    pub fn open(dir: &Path, file_name: &str, label: &'static str) -> anyhow::Result<Self> {
        let path = dir.join(file_name);
        // Un fichier illisible est mis de côté (comme `mode.json`) : pas de jeton, on repart de zéro.
        let stored = match persistence::load_json::<Stored>(&path) {
            Ok(stored) => stored,
            Err(e) => {
                let aside = path.with_extension("json.illisible");
                log::error!(
                    "{label} illisible, aucun jeton ({e:#}) ; fichier conservé sous {}",
                    aside.display()
                );
                std::fs::rename(&path, &aside)?;
                None
            }
        };
        Ok(Self {
            path: Some(path),
            label,
            stored: Mutex::new(stored),
            changing: Mutex::new(()),
        })
    }

    /// Un carnet sans fichier (dossier inutilisable) : le jeton ne vit qu'en mémoire.
    pub fn in_memory(label: &'static str) -> Self {
        Self {
            path: None,
            label,
            stored: Mutex::new(None),
            changing: Mutex::new(()),
        }
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Stored>> {
        self.stored.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.changing.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Un jeton existe-t-il ?
    pub fn exists(&self) -> bool {
        self.lock().is_some()
    }

    /// Date de création du jeton actuel (secondes Unix).
    pub fn created(&self) -> Option<f64> {
        self.lock().as_ref().map(|s| s.created)
    }

    /// Tire un nouveau jeton, remplace l'ancien (révoqué) et rend le jeton en clair : à montrer
    /// une seule fois. En cas d'échec d'écriture, l'ancien reste en place.
    pub fn generate(&self) -> anyhow::Result<String> {
        self.generate_with(|_| Ok(())).map(|(token, ())| token)
    }

    /// Comme `generate`, en deux temps : `prepare` reçoit le nouveau jeton (pour fabriquer ce qui
    /// le porte, par exemple les raccourcis signés) **avant** qu'il remplace l'ancien. Si `prepare`
    /// échoue, rien ne change : l'ancien jeton reste valable.
    pub fn generate_with<T>(
        &self,
        prepare: impl FnOnce(&str) -> anyhow::Result<T>,
    ) -> anyhow::Result<(String, T)> {
        let _one_at_a_time = self.changing();
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let prepared = prepare(&token)?;
        let stored = Stored {
            hash: hex::encode(hash_of(&token)),
            created: now(),
        };
        if let Some(path) = &self.path {
            let text = serde_json::to_string_pretty(&stored)?;
            write_secret(path, text.as_bytes())?;
        }
        *self.lock() = Some(stored);
        Ok((token, prepared))
    }

    /// Supprime le jeton : plus aucun accès par ce jeton.
    pub fn revoke(&self) -> anyhow::Result<()> {
        let _one_at_a_time = self.changing();
        if let Some(path) = &self.path {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        *self.lock() = None;
        Ok(())
    }

    /// Le jeton présenté est-il le bon ? Comparaison en temps constant.
    pub fn verify(&self, token: &str) -> bool {
        let guard = self.lock();
        let Some(stored) = guard.as_ref() else {
            return false;
        };
        match hex::decode(&stored.hash) {
            Ok(bytes) if bytes.len() == 32 => bytes.ct_eq(hash_of(token).as_slice()).into(),
            _ => false,
        }
    }
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("label", &self.label)
            .field("exists", &self.exists())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-modetoken-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn open(dir: &Path) -> TokenStore {
        TokenStore::open(dir, MODE_FILE, "jeton de mode").unwrap()
    }

    #[test]
    fn a_token_works_until_a_new_one_replaces_it() {
        let dir = temp("replace");
        let store = open(&dir);
        assert!(!store.exists());
        assert!(!store.verify("n'importe quoi"));
        let first = store.generate().unwrap();
        assert_eq!(first.len(), 64);
        assert!(store.verify(&first));
        assert!(!store.verify(&first[..63]));
        let second = store.generate().unwrap();
        assert_ne!(first, second);
        assert!(!store.verify(&first), "l'ancien jeton est révoqué");
        assert!(store.verify(&second));
        store.revoke().unwrap();
        assert!(!store.verify(&second));
        assert!(!dir.join(MODE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_preparation_keeps_the_old_token() {
        let dir = temp("prepare");
        let store = open(&dir);
        let old = store.generate().unwrap();
        let mut seen = String::new();
        let failed = store.generate_with(|candidate| -> anyhow::Result<()> {
            seen = candidate.to_string();
            anyhow::bail!("signature impossible")
        });
        assert!(failed.is_err());
        assert!(store.verify(&old), "l'ancien jeton reste valable");
        assert!(
            !seen.is_empty() && !store.verify(&seen),
            "jamais enregistré"
        );
        assert!(open(&dir).verify(&old));
        // Une préparation réussie remplace l'ancien, avec le jeton qu'elle a reçu.
        let (new, prepared) = store.generate_with(|c| Ok(c.to_string())).unwrap();
        assert_eq!(prepared, new);
        assert!(store.verify(&new) && !store.verify(&old));
        // Le jeton de lecture est un fichier à part : aucun ne vaut pour l'autre.
        let read = TokenStore::open(&dir, READ_FILE, "jeton de lecture").unwrap();
        assert!(!read.exists() && !read.verify(&new));
        let lecture = read.generate().unwrap();
        assert!(read.verify(&lecture) && !store.verify(&lecture));
        assert!(dir.join(READ_FILE).exists() && dir.join(MODE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_hash_is_stored_with_private_rights_and_survives_a_restart() {
        let dir = temp("hash");
        let token = open(&dir).generate().unwrap();
        let text = std::fs::read_to_string(dir.join(MODE_FILE)).unwrap();
        assert!(
            !text.contains(&token),
            "le jeton en clair n'est jamais écrit"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(MODE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let reopened = open(&dir);
        assert!(reopened.verify(&token));
        assert!(!format!("{reopened:?}").contains(&token));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_file_is_kept_aside_and_the_server_starts_without_a_token() {
        let dir = temp("broken");
        std::fs::write(dir.join(MODE_FILE), "{pas du json").unwrap();
        let store = open(&dir);
        assert!(!store.exists());
        assert!(dir.join("jeton-mode.json.illisible").exists());
        // On peut en générer un tout de suite, et il survit au redémarrage.
        let token = store.generate().unwrap();
        assert!(open(&dir).verify(&token));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_folder_the_token_lives_in_memory() {
        let store = TokenStore::in_memory("jeton de mode");
        let token = store.generate().unwrap();
        assert!(store.verify(&token));
    }
}
