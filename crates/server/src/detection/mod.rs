//! Détection de personnes, d'animaux et de véhicules sur les caméras en **Surveillance** (ticket
//! n° 18), comme Frigate mais sans enregistrement continu. Voir `docs/detection.md`.
//!
//! # La chaîne, par caméra surveillée
//!
//! ```text
//! session RTSP partagée (cameras/live.rs) ──▶ tâche de veille (watcher.rs) ──▶ pré-enregistrement
//!                                                   │ (file bornée)                (prebuffer.rs)
//!                                                   ▼
//!                       fil d'analyse : VideoToolbox (toutes les images, 2 converties par seconde)
//!                       → mouvement (motion.rs) → détecteur (detector.rs, onnx.rs, un seul pour
//!                       toutes les caméras) → passage (tracker.rs) → clip MP4 (mp4.rs) + capture
//!                       → événement (store.rs) → clients et automatisations
//! ```
//!
//! - **Une seule connexion par caméra** : la veille est un spectateur du registre partagé
//!   (`Hub::live`), comme le direct et les vignettes ; une Aqara G100 échoue au-delà de 2 ou 3
//!   canaux RTSP.
//! - **Pas de détecteur, pas de connexion** : sans modèle (ou sans ONNX Runtime), la détection est
//!   coupée, le dit au journal et dans la fenêtre du serveur, et une caméra en Surveillance se
//!   comporte comme en Diffusion.
//! - **Charge** : images analysées plafonnées par caméra ([`DetectionConfig::fps`]) ; si le fil
//!   d'analyse prend du retard (file pleine), les images sont sautées jusqu'à la prochaine image
//!   clé, la détection se suspend un moment ([`DetectionConfig::pause`]) et le journal le dit.

pub mod detector;
pub mod motion;
pub mod mp4;
pub mod onnx;
pub mod prebuffer;
pub mod store;
pub mod tracker;
pub mod watcher;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use protocol::wire::DetectionStatusView;

pub use store::{EventStore, Part};

/// Le dossier des modèles, dans le dossier de données.
pub const MODELS_DIR: &str = "models";
/// Réglages généraux de la détection, dans le dossier de données (facultatif).
pub const CONFIG_FILE: &str = "detection.json";

/// Réglages généraux (`detection.json`, tous facultatifs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectionConfig {
    /// Plafond de la taille totale des événements (octets).
    pub max_total_bytes: u64,
    /// Images analysées par seconde et par caméra.
    pub fps: f32,
    /// Secondes sans détection avant de clore un passage.
    pub close_after_s: f64,
    /// Durée de la pause quand la machine ne suit plus.
    pub pause_s: u64,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            max_total_bytes: store::DEFAULT_MAX_TOTAL_BYTES,
            fps: 2.0,
            close_after_s: 8.0,
            pause_s: 30,
        }
    }
}

impl DetectionConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.fps.clamp(0.2, 10.0))
    }

    pub fn pause(&self) -> Duration {
        Duration::from_secs(self.pause_s.clamp(5, 600))
    }

    pub fn load(data_dir: &Path) -> DetectionConfig {
        match crate::persistence::load_json::<DetectionConfig>(&data_dir.join(CONFIG_FILE)) {
            Ok(Some(config)) => config,
            Ok(None) => DetectionConfig::default(),
            Err(e) => {
                log::warn!("{CONFIG_FILE} illisible, réglages par défaut : {e:#}");
                DetectionConfig::default()
            }
        }
    }
}

/// La détection côté hub : les événements, l'état affiché, le réveil du superviseur.
pub struct Detection {
    data_dir: PathBuf,
    models_dir: PathBuf,
    demo: bool,
    store: EventStore,
    config: Mutex<DetectionConfig>,
    status: Mutex<DetectionStatusView>,
    /// Caméras où la détection est suspendue, et pourquoi.
    paused: Mutex<std::collections::BTreeMap<(u64, &'static str), String>>,
    wake: Notify,
}

/// La température du Mac telle que macOS la juge (`ProcessInfo.thermalState`) : `true` quand elle
/// est « sérieuse » ou « critique ». Le serveur est un portable : la détection se suspend alors.
#[cfg(target_os = "macos")]
pub fn too_hot() -> bool {
    use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};
    NSProcessInfo::processInfo().thermalState() >= NSProcessInfoThermalState::Serious
}

#[cfg(not(target_os = "macos"))]
pub fn too_hot() -> bool {
    false
}

impl Detection {
    pub fn new(data_dir: &Path, demo: bool) -> Detection {
        let config = if demo {
            DetectionConfig::default()
        } else {
            DetectionConfig::load(data_dir)
        };
        let events = data_dir.join(if demo { "events-demo" } else { store::DIR });
        Detection {
            data_dir: data_dir.to_path_buf(),
            models_dir: data_dir.join(MODELS_DIR),
            demo,
            store: EventStore::new(events, config.max_total_bytes),
            config: Mutex::new(config),
            status: Mutex::new(DetectionStatusView {
                available: false,
                engine: if demo {
                    "absente en démo".into()
                } else {
                    "pas encore démarrée".into()
                },
                ..DetectionStatusView::default()
            }),
            paused: Mutex::new(std::collections::BTreeMap::new()),
            wake: Notify::new(),
        }
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn is_demo(&self) -> bool {
        self.demo
    }

    pub fn config(&self) -> DetectionConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Enregistre de nouveaux réglages généraux (fenêtre du serveur).
    pub fn set_config(&self, config: DetectionConfig) -> anyhow::Result<()> {
        if !self.demo {
            crate::persistence::save_json(&self.data_dir.join(CONFIG_FILE), &config)?;
        }
        self.store.set_max_total_bytes(config.max_total_bytes);
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
        self.wake();
        Ok(())
    }

    pub fn status(&self) -> DetectionStatusView {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        status.paused_cameras = paused
            .keys()
            .map(|(id, _)| *id)
            .collect::<BTreeSet<u64>>()
            .into_iter()
            .collect();
        status.paused = !paused.is_empty();
        status.pause_reason = paused.values().next().cloned();
        status
    }

    /// Suspend (`Some(raison)`) ou reprend (`None`) la détection d'une caméra, pour une cause
    /// (`source`) : file pleine, analyse trop lente, Mac trop chaud.
    pub(crate) fn set_paused(&self, id: u64, source: &'static str, reason: Option<String>) {
        let mut paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        match reason {
            Some(why) => {
                paused.insert((id, source), why);
            }
            None => {
                paused.remove(&(id, source));
            }
        }
    }

    pub(crate) fn update_status(&self, change: impl FnOnce(&mut DetectionStatusView)) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        change(&mut status);
    }

    pub(crate) fn set_watched(&self, ids: &BTreeSet<u64>) {
        self.update_status(|s| s.watched = ids.iter().copied().collect());
    }

    /// Le mode ou une caméra a changé : le superviseur revoit la liste des caméras surveillées.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }
}
