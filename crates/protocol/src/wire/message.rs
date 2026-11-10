//! Messages du serveur vers le client : état complet, événements, vues des appareils.

use serde::{Deserialize, Serialize};

use super::camera::CameraView;
use super::error::ErrorCode;
use super::text_ids::{text_id, TextIds};
use crate::automation::RuleSet;
use crate::events::{Capabilities, CoverMotion};
use crate::home::{HomePlan, Symbol};

/// Grandeur suivie : mesures numériques et états binaires (0 ou 1). Ce sont aussi les
/// clés de l'historique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// °C
    Temperature,
    /// %
    Humidity,
    /// W
    Power,
    /// kWh cumulés
    Energy,
    /// Charge de la pile, %
    Battery,
    /// Ouverture d'un volet, % (100 = tout ouvert)
    Position,
    /// État binaire : 1 = ouvert
    Open,
    /// État binaire : 1 = allumé
    OnOff,
    /// État binaire : 1 = pile faible
    BatteryLow,
}

impl Metric {
    pub const ALL: [Metric; 9] = [
        Metric::Temperature,
        Metric::Humidity,
        Metric::Power,
        Metric::Energy,
        Metric::Battery,
        Metric::Position,
        Metric::Open,
        Metric::OnOff,
        Metric::BatteryLow,
    ];

    /// Nom dans le JSON (et clé des fichiers d'historique).
    pub fn key(self) -> &'static str {
        match self {
            Metric::Temperature => "temperature",
            Metric::Humidity => "humidity",
            Metric::Power => "power",
            Metric::Energy => "energy",
            Metric::Battery => "battery",
            Metric::Position => "position",
            Metric::Open => "open",
            Metric::OnOff => "on_off",
            Metric::BatteryLow => "battery_low",
        }
    }

    pub fn from_key(key: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.key() == key)
    }

    /// `true` pour les états binaires (valeur 0 ou 1).
    pub fn is_state(self) -> bool {
        matches!(self, Metric::Open | Metric::OnOff | Metric::BatteryLow)
    }
}

/// Pont qui expose un appareil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireBridge {
    #[serde(with = "text_id")]
    pub node_id: u64,
    pub endpoint: u16,
}

/// Description d'un appareil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireNode {
    #[serde(with = "text_id")]
    pub node_id: u64,
    /// Nom affiché : celui choisi par l'utilisateur, sinon celui de l'appareil.
    pub name: String,
    pub device_name: String,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub icon: Option<Symbol>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub device_types: Vec<String>,
    pub caps: Capabilities,
    #[serde(default)]
    pub bridge: Option<WireBridge>,
}

/// État de la liaison avec un appareil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WireStatus {
    Connecting,
    Online,
    /// Injoignable ; nouvel essai dans `retry_in_s` secondes (0 : signalé par un pont,
    /// sans nouvel essai de la part du serveur).
    Unreachable {
        code: ErrorCode,
        #[serde(default)]
        detail: Option<String>,
        retry_in_s: u64,
    },
}

/// Mode de remontée des valeurs d'un appareil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WireMode {
    Subscription { max_interval_s: u64 },
    Polling { every_s: u64 },
}

/// Dernière valeur connue d'une grandeur.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatestReading {
    pub metric: Metric,
    pub value: f64,
    /// Secondes Unix.
    pub at: f64,
}

/// Un appareil dans l'état complet : sa description et ses dernières valeurs. Les séries
/// temporelles ne sont pas incluses : elles se lisent à la demande (historique).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceView {
    pub device: WireNode,
    pub status: WireStatus,
    #[serde(default)]
    pub last_seen: Option<f64>,
    #[serde(default)]
    pub mode: Option<WireMode>,
    /// Dernière valeur de chaque grandeur, triées par grandeur.
    #[serde(default)]
    pub latest: Vec<LatestReading>,
    /// Instant du dernier changement d'ouverture observé (capteur d'ouverture).
    #[serde(default)]
    pub open_since: Option<f64>,
    #[serde(default)]
    pub cover_motion: Option<CoverMotion>,
}

/// Une exécution de règle (ou son échec).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleRunView {
    #[serde(with = "text_id")]
    pub rule_id: u64,
    pub name: String,
    pub at: f64,
    pub ok: bool,
    /// Compte rendu des actions (texte humain, sans chemin local).
    pub detail: String,
}

/// Événement diffusé en temps réel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireEvent {
    /// Appareil connu ou modifié (nom, icône, description relue…).
    DeviceUpdated {
        device: WireNode,
    },
    DeviceRemoved {
        #[serde(with = "text_id")]
        node_id: u64,
    },
    Status {
        #[serde(with = "text_id")]
        node_id: u64,
        status: WireStatus,
    },
    /// Dernière communication réussie.
    Seen {
        #[serde(with = "text_id")]
        node_id: u64,
        at: f64,
    },
    Mode {
        #[serde(with = "text_id")]
        node_id: u64,
        mode: WireMode,
    },
    /// Nouvelle valeur d'une grandeur (mesure ou état).
    Reading {
        #[serde(with = "text_id")]
        node_id: u64,
        metric: Metric,
        value: f64,
        at: f64,
    },
    CoverMotion {
        #[serde(with = "text_id")]
        node_id: u64,
        motion: CoverMotion,
    },
    RuleRun(RuleRunView),
    PairingProgress {
        message: String,
    },
    PairingDone {
        #[serde(with = "text_id")]
        node_id: u64,
        name: String,
    },
    PairingFailed {
        code: ErrorCode,
        #[serde(default)]
        detail: Option<String>,
    },
    /// Information diverse du serveur (journal de l'interface).
    Notice {
        text: String,
    },
    /// Notification d'une règle, à afficher par le client. Un client fermé ne la reçoit pas.
    Notification {
        title: String,
        message: String,
        at: f64,
    },
    /// Le plan a été modifié (par n'importe quel client, ou par l'oubli d'un appareil).
    PlanChanged {
        plan: TextIds<HomePlan>,
    },
    /// Les règles ont été modifiées.
    RulesChanged {
        rules: TextIds<RuleSet>,
    },
    /// La liste des caméras ou l'état de l'une d'elles a changé : la liste **complète**
    /// (une trentaine d'entrées au plus), à appliquer en remplaçant la précédente.
    CamerasChanged {
        cameras: Vec<CameraView>,
    },
}

/// État complet, envoyé à la connexion puis chaque fois qu'un client a pris du retard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Numéro du dernier événement déjà pris en compte ; les suivants ont un `seq` plus grand.
    pub seq: u64,
    pub server_time: f64,
    pub devices: Vec<DeviceView>,
    pub plan: TextIds<HomePlan>,
    pub rules: TextIds<RuleSet>,
    /// Les plus récentes en dernier.
    pub rule_runs: Vec<RuleRunView>,
    /// Un appairage est en cours.
    pub pairing_busy: bool,
    /// Les caméras, triées par nom, sans adresse ni mot de passe. Absent d'un serveur de
    /// version 1 (lu comme vide).
    #[serde(default)]
    pub cameras: Vec<CameraView>,
}

/// Message envoyé sur le WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Premier message : version du protocole et du serveur.
    Hello {
        protocol_version: u32,
        server_version: String,
    },
    Snapshot(Box<Snapshot>),
    Event {
        seq: u64,
        event: WireEvent,
    },
}
