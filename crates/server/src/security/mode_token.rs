//! Le « jeton de mode » : un secret à part, qui ne sert qu'à changer le mode de la maison
//! (`PUT /api/mode`), pour les automatisations Raccourcis de l'iPhone. Il ne donne accès à rien
//! d'autre : ni état, ni appareils, ni WebSocket.
//!
//! Comme une session, il est tiré au hasard (256 bits) et seul son SHA-256 est enregistré
//! (`serveur/jeton-mode.json`, 0600) : une fuite du fichier ne donne aucun jeton utilisable. Le
//! jeton en clair n'existe qu'à sa création, que la fenêtre du serveur affiche une seule fois.
//! Il n'est jamais écrit dans un journal. En générer un nouveau révoque l'ancien.
//! `serveur/` n'est ni exporté ni importé : le jeton reste propre à ce Mac.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{now, write_secret};
use crate::persistence;

pub const FILE_NAME: &str = "jeton-mode.json";

#[derive(Serialize, Deserialize)]
struct Stored {
    /// SHA-256 du jeton, hexadécimal.
    hash: String,
    /// Secondes Unix.
    created: f64,
}

pub struct ModeTokenStore {
    /// `None` : pas de dossier utilisable, le jeton ne survit pas au redémarrage.
    path: Option<PathBuf>,
    stored: Mutex<Option<Stored>>,
}

fn hash_of(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl ModeTokenStore {
    /// Ouvre `dir/jeton-mode.json` (absent : aucun jeton, donc aucun accès par jeton).
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(FILE_NAME);
        let stored = persistence::load_json::<Stored>(&path)?;
        Ok(Self {
            path: Some(path),
            stored: Mutex::new(stored),
        })
    }

    /// Un carnet sans fichier (dossier inutilisable) : le jeton ne vit qu'en mémoire.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            stored: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Stored>> {
        self.stored.lock().unwrap_or_else(|e| e.into_inner())
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
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let stored = Stored {
            hash: hex::encode(hash_of(&token)),
            created: now(),
        };
        if let Some(path) = &self.path {
            let text = serde_json::to_string_pretty(&stored)?;
            write_secret(path, text.as_bytes())?;
        }
        *self.lock() = Some(stored);
        Ok(token)
    }

    /// Supprime le jeton : plus aucun accès par jeton.
    pub fn revoke(&self) -> anyhow::Result<()> {
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

impl std::fmt::Debug for ModeTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeTokenStore")
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

    #[test]
    fn a_token_works_until_a_new_one_replaces_it() {
        let dir = temp("replace");
        let store = ModeTokenStore::open(&dir).unwrap();
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
        assert!(!dir.join(FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_hash_is_stored_with_private_rights_and_survives_a_restart() {
        let dir = temp("hash");
        let token = ModeTokenStore::open(&dir).unwrap().generate().unwrap();
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(
            !text.contains(&token),
            "le jeton en clair n'est jamais écrit"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let reopened = ModeTokenStore::open(&dir).unwrap();
        assert!(reopened.verify(&token));
        assert!(!format!("{reopened:?}").contains(&token));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_folder_the_token_lives_in_memory() {
        let store = ModeTokenStore::in_memory();
        let token = store.generate().unwrap();
        assert!(store.verify(&token));
    }
}
