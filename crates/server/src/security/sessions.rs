//! Sessions : jetons opaques aléatoires (256 bits). Seul leur SHA-256 est stocké, dans
//! `sessions.json` : une fuite du fichier ne donne aucun jeton utilisable. Les sessions
//! survivent au redémarrage, expirent (inactivité, durée maximale) et se révoquent. Chaque
//! session est rattachée à un utilisateur (`user_id`) et à son « époque » : couper les sessions
//! d'un utilisateur revient à changer son époque (voir `security::account`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protocol::wire::{ConnectionKind, DeviceClass, Installation, Via};
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
/// Sessions simultanées au plus **par utilisateur** (les plus anciennes sont retirées).
const MAX_SESSIONS_PER_USER: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    /// SHA-256 du jeton, hexadécimal.
    id: String,
    /// Utilisateur propriétaire. Le nom écrit dans le fichier reste `account_id`, celui de la
    /// 2.0.0 : ses sessions sont celles du premier utilisateur (même identifiant), et un retour à
    /// la 2.0.0 relit ce fichier sans erreur (elle ignore l'époque).
    #[serde(rename = "account_id")]
    user_id: String,
    /// Époque de l'utilisateur à l'ouverture de la session (0 pour les anciennes).
    #[serde(default)]
    epoch: u64,
    created: f64,
    last_used: f64,
    expires: f64,
    max_expires: f64,
    /// Le téléphone ou l'ordinateur où cette session est ouverte (protocole 4). Absent d'une
    /// session ouverte par un client plus ancien : sa connexion est « inconnue ». Champs ajoutés
    /// sans toucher aux autres : la 2.0.0 relit le fichier en les ignorant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    installation: Option<Installation>,
    /// Dernier accès (secondes Unix) : à chaque requête en mémoire, écrit avec le prochain
    /// enregistrement. 0 pour les anciennes sessions (on prend alors `last_used`).
    #[serde(default)]
    seen: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    via: Option<Via>,
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
    pub user_id: String,
    pub epoch: u64,
}

/// Jeton créé : à remettre une seule fois au client.
pub struct NewSession {
    pub token: String,
    pub expires_at: f64,
    pub max_expires_at: f64,
}

/// Une connexion ouverte : un téléphone ou un ordinateur où un compte est ouvert.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionRecord {
    /// Pour la couper : l'identifiant de l'installation, ou `inconnue-…` (dérivé de la session).
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub kind: ConnectionKind,
    pub device: DeviceClass,
    pub last_seen: f64,
    pub via: Via,
    /// Identifiant de la session (empreinte du jeton) : pour reconnaître « cette connexion-ci ».
    pub session_id: String,
    /// Le client n'a rien dit de lui-même (app pas encore à jour).
    pub unknown: bool,
}

/// Préfixe des identifiants de connexions inconnues ; refusé pour une installation déclarée.
const UNKNOWN_PREFIX: &str = "inconnue-";

/// Identifiant d'installation acceptable : 8 à 64 caractères, lettres, chiffres et tirets.
fn valid_installation_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !id.starts_with(UNKNOWN_PREFIX)
}

/// Ce que dit le client de lui-même, remis en forme : un nom propre et court, un type connu. Un
/// identifiant illisible rend la connexion « inconnue » (jamais un refus de se connecter).
pub fn clean_installation(raw: Option<Installation>) -> Option<Installation> {
    let raw = raw?;
    if !valid_installation_id(&raw.id) {
        return None;
    }
    let name: String = raw
        .name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(64)
        .collect();
    let name = if name.is_empty() {
        match raw.kind {
            ConnectionKind::Mac => "Mac".to_string(),
            _ => "Navigateur".to_string(),
        }
    } else {
        name
    };
    Some(Installation { name, ..raw })
}

impl Stored {
    fn connection(&self) -> ConnectionRecord {
        let (id, name, kind, device, unknown) = match &self.installation {
            Some(i) => (i.id.clone(), i.name.clone(), i.kind, i.device, false),
            None => (
                format!("{UNKNOWN_PREFIX}{}", &self.id[..self.id.len().min(16)]),
                "Connexion inconnue".to_string(),
                ConnectionKind::Other,
                DeviceClass::Other,
                true,
            ),
        };
        ConnectionRecord {
            id,
            user_id: self.user_id.clone(),
            name,
            kind,
            device,
            last_seen: self.seen.max(self.last_used),
            via: self.via.unwrap_or(Via::Local),
            session_id: self.id.clone(),
            unknown,
        }
    }
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

    /// Ouvre une session pour `user_id`, à l'époque `epoch` de cet utilisateur, sans dire d'où.
    pub fn create(&self, user_id: &str, epoch: u64) -> NewSession {
        self.create_from(user_id, epoch, None, Via::Local)
    }

    /// Ouvre une session depuis une connexion (`installation`, déjà passée par
    /// [`clean_installation`]). Se reconnecter depuis la même installation ferme sa session
    /// précédente : une connexion, une session.
    pub fn create_from(
        &self,
        user_id: &str,
        epoch: u64,
        installation: Option<Installation>,
        via: Via,
    ) -> NewSession {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let t = now();
        let stored = Stored {
            id: hex::encode(token_hash(&token)),
            user_id: user_id.to_string(),
            epoch,
            created: t,
            last_used: t,
            expires: t + IDLE_TTL_S,
            max_expires: t + MAX_TTL_S,
            installation,
            seen: t,
            via: Some(via),
        };
        let out = NewSession {
            token,
            expires_at: stored.expires,
            max_expires_at: stored.max_expires,
        };
        let mut sessions = self.lock();
        sessions.retain(|s| s.expires > t && s.max_expires > t);
        if let Some(own) = stored.installation.as_ref().map(|i| i.id.clone()) {
            sessions.retain(|s| s.installation.as_ref().is_none_or(|i| i.id != own));
        }
        sessions.push(stored);
        // Au plus 32 par utilisateur : les plus anciennes de CET utilisateur sont retirées, celles
        // des autres ne sont jamais touchées.
        let mut mine: Vec<(f64, String)> = sessions
            .iter()
            .filter(|s| s.user_id == user_id)
            .map(|s| (s.created, s.id.clone()))
            .collect();
        if mine.len() > MAX_SESSIONS_PER_USER {
            mine.sort_by(|a, b| a.0.total_cmp(&b.0));
            let excess = mine.len() - MAX_SESSIONS_PER_USER;
            let dropped: Vec<String> = mine.into_iter().take(excess).map(|(_, id)| id).collect();
            sessions.retain(|s| !dropped.contains(&s.id));
        }
        self.save(&sessions);
        out
    }

    /// Valide un jeton (existence et échéances) ; repousse son échéance. Que l'utilisateur soit
    /// toujours actif et à la bonne époque se vérifie à part (`Auth::authenticate`).
    pub fn validate(&self, token: &str) -> Option<SessionInfo> {
        self.validate_from(token, None)
    }

    /// Comme [`SessionStore::validate`], en notant le dernier accès (et d'où il vient).
    pub fn validate_from(&self, token: &str, via: Option<Via>) -> Option<SessionInfo> {
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
        if s.expires <= t || s.max_expires <= t {
            return None;
        }
        let info = SessionInfo {
            id: s.id.clone(),
            user_id: s.user_id.clone(),
            epoch: s.epoch,
        };
        // Le dernier accès reste en mémoire : il part au disque avec le prochain enregistrement.
        s.seen = t;
        if via.is_some() {
            s.via = via;
        }
        if t - s.last_used >= TOUCH_EVERY_S {
            s.last_used = t;
            s.expires = (t + IDLE_TTL_S).min(s.max_expires);
            self.save(&sessions);
        }
        Some(info)
    }

    /// La session `id` existe-t-elle encore (non révoquée, non expirée) ?
    pub fn is_valid(&self, id: &str) -> bool {
        let t = now();
        self.lock()
            .iter()
            .any(|s| s.id == id && s.expires > t && s.max_expires > t)
    }

    /// Comme [`SessionStore::is_valid`], pour un flux ouvert : tant qu'il l'est, la connexion
    /// est « là » (dernier accès : maintenant).
    pub fn is_live(&self, id: &str) -> bool {
        let t = now();
        let mut sessions = self.lock();
        match sessions
            .iter_mut()
            .find(|s| s.id == id && s.expires > t && s.max_expires > t)
        {
            Some(s) => {
                s.seen = t;
                true
            }
            None => false,
        }
    }

    /// Les connexions ouvertes (une par session vivante), la plus récente d'abord.
    pub fn connections(&self) -> Vec<ConnectionRecord> {
        let t = now();
        let mut out: Vec<ConnectionRecord> = self
            .lock()
            .iter()
            .filter(|s| s.expires > t && s.max_expires > t)
            .map(Stored::connection)
            .collect();
        out.sort_by(|a, b| b.last_seen.total_cmp(&a.last_seen));
        out
    }

    /// Coupe une connexion : ses sessions sont révoquées, son jeton cesse de marcher tout de
    /// suite. `user_id` : limite à un utilisateur (l'API d'une personne ne touche jamais les
    /// connexions des autres). Rend le nombre de sessions retirées.
    pub fn cut_connection(&self, user_id: Option<&str>, connection_id: &str) -> usize {
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|s| {
            let c = s.connection();
            !(c.id == connection_id && user_id.is_none_or(|u| u == c.user_id))
        });
        let removed = before - sessions.len();
        if removed > 0 {
            self.save(&sessions);
        }
        removed
    }

    /// Retire les sessions dont `keep(user_id, époque)` dit non (utilisateur supprimé ou
    /// désactivé, sessions coupées). Retourne le nombre de sessions retirées.
    pub fn retain_valid(&self, keep: impl Fn(&str, u64) -> bool) -> usize {
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|s| keep(&s.user_id, s.epoch));
        let removed = before - sessions.len();
        if removed > 0 {
            self.save(&sessions);
        }
        removed
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
        let s = store.create("compte-1", 0);
        assert_eq!(s.token.len(), 64);
        let file = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(
            !file.contains(&s.token),
            "le jeton ne doit pas être écrit en clair"
        );
        assert!(file.contains(&hex::encode(token_hash(&s.token))));

        let info = store.validate(&s.token).unwrap();
        assert_eq!(info.user_id, "compte-1");
        assert!(store.validate("0".repeat(64).as_str()).is_none());

        // Redémarrage : la même session est toujours valide.
        let again = SessionStore::open(&dir).unwrap();
        assert_eq!(again.validate(&s.token), Some(info.clone()));

        // Révocation, persistante elle aussi.
        assert!(again.revoke(&info.id));
        assert!(again.validate(&s.token).is_none());
        assert!(!again.is_valid(&info.id));
        assert!(SessionStore::open(&dir)
            .unwrap()
            .validate(&s.token)
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
        let s = store.create("c", 0);
        // On vieillit la session dans le fichier.
        {
            let mut sessions = store.lock();
            sessions[0].expires = now() - 1.0;
            store.save(&sessions);
        }
        assert!(store.validate(&s.token).is_none());
        let reopened = SessionStore::open(&dir).unwrap();
        assert_eq!(reopened.count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn use_slides_expiry_but_not_past_the_maximum() {
        let dir = temp("slide");
        let store = SessionStore::open(&dir).unwrap();
        let s = store.create("c", 0);
        {
            let mut sessions = store.lock();
            sessions[0].last_used = now() - 2.0 * TOUCH_EVERY_S;
            sessions[0].expires = now() + 100.0;
            sessions[0].max_expires = now() + 1000.0;
        }
        assert!(store.validate(&s.token).is_some());
        let sessions = store.lock();
        assert!(sessions[0].expires > now() + 900.0, "échéance repoussée");
        assert!(
            sessions[0].expires <= sessions[0].max_expires,
            "mais plafonnée"
        );
    }

    #[test]
    fn session_count_is_bounded_per_user() {
        let dir = temp("max");
        let store = SessionStore::open(&dir).unwrap();
        let other = store.create("autre", 0);
        let first = store.create("c", 0);
        for _ in 0..MAX_SESSIONS_PER_USER + 3 {
            store.create("c", 0);
        }
        assert_eq!(store.count(), MAX_SESSIONS_PER_USER + 1);
        assert!(
            store.validate(&first.token).is_none(),
            "la plus ancienne de cet utilisateur est retirée"
        );
        assert!(
            store.validate(&other.token).is_some(),
            "celles des autres utilisateurs ne sont pas touchées"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_file_keeps_the_2_0_0_key_so_a_rollback_can_still_read_it() {
        let dir = temp("rollback");
        let store = SessionStore::open(&dir).unwrap();
        store.create("compte-1", 3);
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(
            text.contains("\"account_id\"") && !text.contains("\"user_id\""),
            "{text}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_sessions_without_epoch_or_user_id_still_load() {
        let dir = temp("old");
        // Forme de la 2.0.0 : `account_id`, pas d'époque.
        let token = "ab".repeat(32);
        let t = now();
        let old = serde_json::json!({ "sessions": [{
            "id": hex::encode(token_hash(&token)),
            "account_id": "ancien-compte",
            "created": t, "last_used": t,
            "expires": t + 1000.0, "max_expires": t + 2000.0,
        }]});
        std::fs::write(dir.join(FILE_NAME), old.to_string()).unwrap();
        let store = SessionStore::open(&dir).unwrap();
        let info = store.validate(&token).unwrap();
        assert_eq!(info.user_id, "ancien-compte");
        assert_eq!(info.epoch, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retain_valid_drops_cut_sessions() {
        let dir = temp("retain");
        let store = SessionStore::open(&dir).unwrap();
        let a = store.create("a", 0);
        let b = store.create("b", 1);
        assert_eq!(
            store.retain_valid(|user, epoch| !(user == "b" && epoch == 1)),
            1
        );
        assert!(store.validate(&a.token).is_some());
        assert!(store.validate(&b.token).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn installation(id: &str, name: &str) -> Installation {
        Installation {
            id: id.into(),
            kind: ConnectionKind::Web,
            device: DeviceClass::Iphone,
            name: name.into(),
        }
    }

    #[test]
    fn connections_are_one_per_session_unknown_ones_included() {
        let dir = temp("conns");
        let store = SessionStore::open(&dir).unwrap();
        let a = store.create_from(
            "a",
            0,
            Some(installation("iphone-a-01", "iPhone")),
            Via::Local,
        );
        store.create("a", 0);
        store.create_from(
            "b",
            0,
            Some(installation("mac-b-0001", "Mac")),
            Via::Tailscale,
        );
        let all = store.connections();
        assert_eq!(all.len(), 3);
        assert_eq!(all.iter().filter(|c| c.unknown).count(), 1);
        let unknown = all.iter().find(|c| c.unknown).unwrap();
        assert!(unknown.id.starts_with(UNKNOWN_PREFIX));
        assert_eq!(unknown.name, "Connexion inconnue");
        let mac = all.iter().find(|c| c.name == "Mac").unwrap();
        assert_eq!(mac.via, Via::Tailscale);

        // Se reconnecter depuis la même installation remplace sa session.
        let again = store.create_from(
            "a",
            0,
            Some(installation("iphone-a-01", "iPhone")),
            Via::Local,
        );
        assert!(store.validate(&a.token).is_none());
        assert!(store.validate(&again.token).is_some());
        assert_eq!(store.connections().len(), 3);

        // Couper : limité à l'utilisateur demandé, jamais une connexion d'un autre.
        assert_eq!(store.cut_connection(Some("b"), "iphone-a-01"), 0);
        assert_eq!(store.cut_connection(Some("a"), "iphone-a-01"), 1);
        assert!(store.validate(&again.token).is_none());
        assert_eq!(store.cut_connection(Some("a"), &unknown.id), 1);
        assert_eq!(store.cut_connection(None, "mac-b-0001"), 1);
        assert_eq!(store.connections().len(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_last_access_follows_requests_and_open_streams_and_survives_a_restart() {
        let dir = temp("seen");
        let store = SessionStore::open(&dir).unwrap();
        let s = store.create_from(
            "a",
            0,
            Some(installation("iphone-a-01", "iPhone")),
            Via::Local,
        );
        store.lock()[0].seen = now() - 1000.0;
        assert!(store
            .validate_from(&s.token, Some(Via::Tailscale))
            .is_some());
        let c = &store.connections()[0];
        assert_eq!(c.via, Via::Tailscale);
        assert!(now() - c.last_seen < 5.0);
        store.lock()[0].seen = now() - 1000.0;
        let id = c.session_id.clone();
        assert!(store.is_live(&id));
        assert!(now() - store.connections()[0].last_seen < 5.0);
        assert!(!store.is_live("0000"));
        // Écrit avec le prochain enregistrement, relu tel quel.
        store.create("z", 0);
        let reopened = SessionStore::open(&dir).unwrap();
        let c = reopened
            .connections()
            .into_iter()
            .find(|c| !c.unknown)
            .unwrap();
        assert_eq!((c.name.as_str(), c.via), ("iPhone", Via::Tailscale));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_a_client_says_about_itself_is_cleaned_never_refused() {
        let ok = clean_installation(Some(installation("abcd-1234", "  Mac\u{7} du \n salon  ")))
            .unwrap();
        assert_eq!(ok.name, "Mac du  salon");
        let long = "é".repeat(200);
        assert_eq!(
            clean_installation(Some(installation("abcd-1234", &long)))
                .unwrap()
                .name
                .chars()
                .count(),
            64
        );
        let blank = clean_installation(Some(installation("abcd-1234", "  "))).unwrap();
        assert_eq!(blank.name, "Navigateur");
        for bad in [
            "court",
            "avec espace 123",
            "inconnue-0123456789",
            "é-accentué-12",
            &"a".repeat(65),
        ] {
            assert!(
                clean_installation(Some(installation(bad, "x"))).is_none(),
                "{bad}"
            );
        }
        assert!(clean_installation(None).is_none());
    }

    #[test]
    fn a_file_with_installations_is_still_readable_by_the_2_0_0_shape() {
        // La 2.0.0 lit `account_id` et ignore le reste : les nouveaux champs sont facultatifs.
        let dir = temp("compat");
        let store = SessionStore::open(&dir).unwrap();
        store.create_from(
            "a",
            2,
            Some(installation("iphone-a-01", "iPhone")),
            Via::Local,
        );
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let s = &value["sessions"][0];
        for key in [
            "id",
            "account_id",
            "created",
            "last_used",
            "expires",
            "max_expires",
        ] {
            assert!(s.get(key).is_some(), "{key} manque : {text}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
