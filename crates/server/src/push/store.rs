//! Ce que les notifications gardent sur disque, dans `serveur/` (dossier 0700, fichiers 0600,
//! **jamais exportés** dans une sauvegarde `.matterhome`, comme la clé d'API des descriptions) :
//!
//! - `push-vapid.json` : la clé privée VAPID du serveur (PKCS#8, base64), créée au premier
//!   lancement. Perdue ou illisible, une nouvelle est créée **et les abonnements sont oubliés**
//!   (le service de push refuserait la nouvelle clé pour eux) : chaque appareil se réabonne ;
//! - `push.json` : le réglage général, les abonnements (adresse, clés `p256dh` et `auth`, nom,
//!   date, choix des catégories) et les appareils déjà signalés pour leur pile faible.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use protocol::wire::PushCategories;

use super::crypto::VapidKey;

pub const VAPID_FILE: &str = "push-vapid.json";
pub const FILE: &str = "push.json";

#[derive(Serialize, Deserialize)]
struct VapidFile {
    /// PKCS#8 en base64 standard.
    pkcs8: String,
    created_at: f64,
}

/// Un abonnement d'appareil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub endpoint: String,
    /// Clé publique de l'appareil (base64url).
    pub p256dh: String,
    /// Secret d'authentification (base64url).
    pub auth: String,
    pub label: String,
    #[serde(default)]
    pub user_agent: String,
    pub created_at: f64,
    #[serde(default)]
    pub last_sent_at: Option<f64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub categories: PushCategories,
}

/// Le contenu de `push.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    /// Réglage général : activé (rien ne part tant qu'aucun appareil n'est abonné).
    pub enabled: bool,
    pub subscriptions: Vec<Subscription>,
    /// Appareils déjà signalés pour leur pile faible (réarmés au-dessus de 20 %).
    pub battery_alerted: BTreeSet<u64>,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            enabled: true,
            subscriptions: Vec::new(),
            battery_alerted: BTreeSet::new(),
        }
    }
}

/// Où écrire (`None` : démonstration, tout reste en mémoire).
#[derive(Debug, Clone)]
pub struct Files {
    dir: Option<PathBuf>,
}

impl Files {
    pub fn new(server_dir: Option<&Path>) -> Files {
        Files {
            dir: server_dir.map(Path::to_path_buf),
        }
    }

    /// La clé VAPID, créée si besoin. `true` : elle vient d'être créée (les abonnements
    /// enregistrés ne valent plus rien).
    pub fn load_or_create_vapid(&self) -> Result<(VapidKey, bool)> {
        let Some(dir) = &self.dir else {
            return Ok((VapidKey::generate().map_err(anyhow::Error::msg)?, true));
        };
        let path = dir.join(VAPID_FILE);
        match crate::persistence::load_json::<VapidFile>(&path) {
            Ok(Some(file)) => {
                let key = base64::engine::general_purpose::STANDARD
                    .decode(file.pkcs8.trim())
                    .ok()
                    .and_then(|der| VapidKey::from_pkcs8(&der).ok());
                if let Some(key) = key {
                    return Ok((key, false));
                }
                log::warn!("notifications : clé VAPID illisible, une nouvelle est créée");
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("notifications : clé VAPID illisible ({e:#}), une nouvelle est créée")
            }
        }
        let key = VapidKey::generate().map_err(anyhow::Error::msg)?;
        let file = VapidFile {
            pkcs8: base64::engine::general_purpose::STANDARD.encode(key.pkcs8()),
            created_at: crate::events::now_unix(),
        };
        let text = serde_json::to_vec_pretty(&file)?;
        crate::security::write_secret(&path, &text)
            .with_context(|| format!("écriture de {}", path.display()))?;
        Ok((key, true))
    }

    pub fn load(&self) -> Stored {
        let Some(dir) = &self.dir else {
            return Stored::default();
        };
        match crate::persistence::load_json::<Stored>(&dir.join(FILE)) {
            Ok(Some(stored)) => stored,
            Ok(None) => Stored::default(),
            Err(e) => {
                log::warn!("notifications : {FILE} illisible, abonnements oubliés : {e:#}");
                Stored::default()
            }
        }
    }

    pub fn save(&self, stored: &Stored) -> Result<()> {
        if let Some(dir) = &self.dir {
            let path = dir.join(FILE);
            let text = serde_json::to_vec_pretty(stored)?;
            crate::security::write_secret(&path, &text)
                .with_context(|| format!("écriture de {}", path.display()))?;
        }
        Ok(())
    }
}
