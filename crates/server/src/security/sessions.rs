//! Sessions : jetons opaques aléatoires (256 bits). Seul leur SHA-256 est stocké, dans
//! `sessions.json` : une fuite du fichier ne donne aucun jeton utilisable. Les sessions
//! survivent au redémarrage, expirent (inactivité, durée maximale) et se révoquent.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::now;
use crate::persistence;

pub const FILE_NAME: &str = "sessions.json";

/// Une session inutilisée expire après 30 jours ; chaque usage repousse l'échéance.
pub const IDLE_TTL_S: f64 = 30.0 * 86_400.0;
/// Durée de vie maximale d'une session, quel que soit son usage.
pub const MAX_TTL_S: f64 = 180.0 * 86_400.0;
/// Le fichier n'est réécrit pour un simple usage qu'au bout de ce délai.
const TOUCH_EVERY_S: f64 = 3600.0;
/// Sessions simultanées au plus (les plus anciennes sont retirées).
const MAX_SESSIONS: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    /// SHA-256 du jeton, hexadécimal.
    id: String,
    account_id: String,
    created: f64,
    last_used: f64,
    expires: f64,
    max_expires: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct FileData {
    #[serde(default)]
    sessions: Vec<Stored>,
}

/// Session reconnue.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    /// Identifiant de la session (empreinte du jeton, pas le jeton).
    pub id: String,
}

/// Jeton créé : à remettre une seule fois au client.
pub struct NewSession {
    pub token: String,
    pub expires_at: f64,
    pub max_expires_at: f64,
}

pub struct SessionStore {
    path: PathBuf,
    sessions: Mutex<Vec<Stored>>,
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn same_hash(id: &str, hash: &[u8; 32]) -> bool {
    match hex::decode(id) {
        Ok(bytes) if bytes.len() == 32 => bytes.ct_eq(hash.as_slice()).into(),
        _ => false,
    }
}

impl SessionStore {
    /// Ouvre `dir/sessions.json` (absent = aucune session). Les sessions expirées sont retirées.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(FILE_NAME);
        let data: FileData = persistence::load_json(&path)?.unwrap_or_default();
        let store = Self {
            path,
            sessions: Mutex::new(data.sessions),
        };
        {
            let mut sessions = store.lock();
            let before = sessions.len();
            let t = now();
            sessions.retain(|s| s.expires > t && s.max_expires > t);
            if sessions.len() != before {
                store.save(&sessions);
            }
        }
        Ok(store)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Stored>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, sessions: &[Stored]) {
        let data = FileData {
            sessions: sessions.to_vec(),
        };
        if let Err(e) = persistence::save_json(&self.path, &data) {
            // Le jeton reste valable en mémoire ; il serait perdu au redémarrage.
            log::error!("sessions non enregistrées : {e:#}");
        }
    }

    /// Ouvre une session pour `account_id`.
    pub fn create(&self, account_id: &str) -> NewSession {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let t = now();
        let stored = Stored {
            id: hex::encode(token_hash(&token)),
            account_id: account_id.to_string(),
            created: t,
            last_used: t,
            expires: t + IDLE_TTL_S,
            max_expires: t + MAX_TTL_S,
        };
        let out = NewSession {
            token,
            expires_at: stored.expires,
            max_expires_at: stored.max_expires,
        };
        let mut sessions = self.lock();
        sessions.retain(|s| s.expires > t && s.max_expires > t);
        sessions.push(stored);
        if sessions.len() > MAX_SESSIONS {
            sessions.sort_by(|a, b| a.created.total_cmp(&b.created));
            let excess = sessions.len() - MAX_SESSIONS;
            sessions.drain(..excess);
        }
        self.save(&sessions);
        out
    }

    /// Valide un jeton pour le compte `account_id` ; repousse son échéance.
    pub fn validate(&self, token: &str, account_id: &str) -> Option<SessionInfo> {
        let hash = token_hash(token);
        let t = now();
        let mut sessions = self.lock();
        // Parcours complet et comparaisons à temps constant : la durée ne dit rien du jeton.
        let mut found = None;
        for (i, s) in sessions.iter().enumerate() {
            if same_hash(&s.id, &hash) {
                found = Some(i);
            }
        }
        let i = found?;
        let s = &mut sessions[i];
        if s.account_id != account_id || s.expires <= t || s.max_expires <= t {
            return None;
        }
        let info = SessionInfo { id: s.id.clone() };
        if t - s.last_used >= TOUCH_EVERY_S {
            s.last_used = t;
            s.expires = (t + IDLE_TTL_S).min(s.max_expires);
            self.save(&sessions);
        }
        Some(info)
    }

    /// La session `id` existe-t-elle encore (non révoquée, non expirée) ?
    pub fn is_valid(&self, id: &str, account_id: &str) -> bool {
        let t = now();
        self.lock()
            .iter()
            .any(|s| s.id == id && s.account_id == account_id && s.expires > t && s.max_expires > t)
    }

    /// Révoque une session (déconnexion).
    pub fn revoke(&self, id: &str) -> bool {
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|s| s.id != id);
        let removed = sessions.len() != before;
        if removed {
            self.save(&sessions);
        }
        removed
    }

    pub fn count(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-sessions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn token_is_stored_hashed_and_survives_restart() {
        let dir = temp("hash");
        let store = SessionStore::open(&dir).unwrap();
        let s = store.create("compte-1");
        assert_eq!(s.token.len(), 64);
        let file = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(
            !file.contains(&s.token),
            "le jeton ne doit pas être écrit en clair"
        );
        assert!(file.contains(&hex::encode(token_hash(&s.token))));

        let info = store.validate(&s.token, "compte-1").unwrap();
        assert!(store
            .validate("0".repeat(64).as_str(), "compte-1")
            .is_none());
        assert!(store.validate(&s.token, "autre-compte").is_none());

        // Redémarrage : la même session est toujours valide.
        let again = SessionStore::open(&dir).unwrap();
        assert_eq!(again.validate(&s.token, "compte-1"), Some(info.clone()));

        // Révocation, persistante elle aussi.
        assert!(again.revoke(&info.id));
        assert!(again.validate(&s.token, "compte-1").is_none());
        assert!(!again.is_valid(&info.id, "compte-1"));
        assert!(SessionStore::open(&dir)
            .unwrap()
            .validate(&s.token, "compte-1")
            .is_none());
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
    fn expired_sessions_are_refused_and_dropped() {
        let dir = temp("exp");
        let store = SessionStore::open(&dir).unwrap();
        let s = store.create("c");
        // On vieillit la session dans le fichier.
        {
            let mut sessions = store.lock();
            sessions[0].expires = now() - 1.0;
            store.save(&sessions);
        }
        assert!(store.validate(&s.token, "c").is_none());
        let reopened = SessionStore::open(&dir).unwrap();
        assert_eq!(reopened.count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn use_slides_expiry_but_not_past_the_maximum() {
        let dir = temp("slide");
        let store = SessionStore::open(&dir).unwrap();
        let s = store.create("c");
        {
            let mut sessions = store.lock();
            sessions[0].last_used = now() - 2.0 * TOUCH_EVERY_S;
            sessions[0].expires = now() + 100.0;
            sessions[0].max_expires = now() + 1000.0;
        }
        assert!(store.validate(&s.token, "c").is_some());
        let sessions = store.lock();
        assert!(sessions[0].expires > now() + 900.0, "échéance repoussée");
        assert!(
            sessions[0].expires <= sessions[0].max_expires,
            "mais plafonnée"
        );
    }

    #[test]
    fn session_count_is_bounded() {
        let dir = temp("max");
        let store = SessionStore::open(&dir).unwrap();
        let first = store.create("c");
        for _ in 0..MAX_SESSIONS + 3 {
            store.create("c");
        }
        assert_eq!(store.count(), MAX_SESSIONS);
        assert!(
            store.validate(&first.token, "c").is_none(),
            "la plus ancienne est retirée"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
