//! Messages échangés entre le client (interface) et le serveur (tâche de fond Matter).
//!
//! Les appareils sont désignés par un identifiant `u64` : l'identifiant du nœud Matter,
//! ou, pour un appareil exposé par un pont (volet Somfy derrière une TaHoma…), un
//! identifiant propre tiré dans une plage que Matter réserve et n'attribue jamais à un
//! nœud opérationnel. Les champs s'appellent `node_id` par continuité.

use serde::{Deserialize, Serialize};

use crate::automation::{CoverCommand, OnOffCommand, Rule};
use crate::home::Symbol;
use crate::mode::{HomeMode, ModeSource};

/// Plage des identifiants des appareils qu'une **extension** apporte (volets Somfy…) : en haut de
/// l'espace des nœuds opérationnels de Matter, juste sous les appareils pontés. L'appairage Matter
/// n'y tire jamais d'identifiant. Tous dépassent 2^53 : en JSON ils sont du texte.
pub const EXTENSION_ID_MIN: u64 = 0xFFFF_FFEF_0000_0000;
pub const EXTENSION_ID_MAX: u64 = 0xFFFF_FFEF_FFFF_FFFF;

/// `true` si `id` est dans la plage des appareils d'extension (un simple contrôle de plage).
pub fn is_extension_id(id: u64) -> bool {
    (EXTENSION_ID_MIN..=EXTENSION_ID_MAX).contains(&id)
}

/// Commandes envoyées par l'interface.
#[derive(Debug, Clone)]
pub enum Command {
    /// Appairer un appareil avec ce code (manuel ou `MT:...`).
    Pair(String),
    /// Retenter immédiatement la connexion à un nœud (ou au pont d'un appareil).
    Retry(u64),
    /// Oublier un nœud localement (l'appareil garde notre fabric : voir README).
    Forget(u64),
    /// Effacer l'historique enregistré d'un appareil (sur disque).
    ClearHistory(u64),
    /// Nouvelles règles d'automatisation (déjà sauvegardées par le serveur).
    SetRules(Vec<Rule>),
    /// Exécuter tout de suite les actions d'une règle (bouton « Tester »).
    TestRule(u64),
    /// Le mode de la maison vient de changer (déjà enregistré par le serveur) : le moteur
    /// d'automatisations le retient et déclenche les règles « Quand le mode devient… ».
    /// `source` : d'où vient le changement (`{source}` dans les notifications des règles).
    SetHomeMode { mode: HomeMode, source: ModeSource },
    /// Un passage détecté sur une caméra vient de se terminer (ticket n° 18) : le moteur
    /// d'automatisations déclenche les règles « Quand … est détecté ».
    /// `opening` : le passage commence (toutes les règles qui le couvrent) ; sinon, une classe
    /// **nouvelle** dans un passage en cours (`event.classes` ne porte qu'elle) : seules les règles
    /// qui suivent cette classe partent.
    ///
    /// `awaiting` : une description (ticket n° 19) va arriver pour ce passage : les règles dont le
    /// texte emploie `{description}` ou `{niveau}` attendent [`Command::DetectionDescribed`].
    Detection {
        event: Box<crate::wire::DetectionEventView>,
        opening: bool,
        awaiting: bool,
    },
    /// La description d'un passage est arrivée (ou ne viendra pas : refus, plafond, erreur) : les
    /// règles qui l'attendaient partent maintenant, avec `event.description` si elle existe.
    DetectionDescribed {
        event: Box<crate::wire::DetectionEventView>,
    },
    /// Nom et icône choisis par l'utilisateur ; `None` = revenir à l'automatique.
    Customize {
        node_id: u64,
        name: Option<String>,
        icon: Option<Symbol>,
    },
    /// Ouvrir, fermer, arrêter un volet ou l'amener à une position (boutons de la fiche).
    SetCover { node_id: u64, command: CoverCommand },
    /// Allumer, éteindre ou inverser une prise ou une lampe.
    SetOnOff { node_id: u64, command: OnOffCommand },
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeStatus {
    Connecting,
    Online,
    /// Injoignable ; nouvel essai dans `retry_in_s` secondes (0 : signalé par un pont,
    /// sans nouvel essai de notre part).
    Unreachable {
        error: String,
        retry_in_s: u64,
    },
}

/// Grandeurs numériques suivies dans le temps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeasureKind {
    /// °C
    Temperature,
    /// %
    Humidity,
    /// Puissance active, W
    Power,
    /// Énergie consommée cumulée, kWh
    Energy,
    /// Charge de la pile, %
    Battery,
    /// Ouverture d'un volet ou store, % (0 = fermé, 100 = tout ouvert).
    Position,
}

/// États binaires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateKind {
    /// `true` = ouvert (pas de contact).
    Open,
    /// `true` = allumé.
    OnOff,
    /// `true` = pile faible ou critique (BatChargeLevel ≠ OK).
    BatteryLow,
}

/// Mouvement d'un volet (Window Covering / OperationalStatus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverMotion {
    Stopped,
    Opening,
    Closing,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReportingMode {
    /// Souscription active ; l'appareil promet un rapport au moins toutes les `max_interval_s`.
    Subscription { max_interval_s: u64 },
    /// Souscription impossible : interrogation périodique.
    Polling { every_s: u64 },
}

/// Ce que l'appareil sait mesurer ou faire, d'après sa description (clusters présents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub temperature: bool,
    pub humidity: bool,
    pub contact: bool,
    pub battery: bool,
    pub power: bool,
    pub energy: bool,
    pub on_off: bool,
    /// Volet ou store commandable (cluster Window Covering).
    pub cover: bool,
    /// Le volet connaît sa position (sinon : moteur à sens unique, type RTS).
    pub cover_position: bool,
}

impl Capabilities {
    pub fn any(&self) -> bool {
        self.temperature
            || self.humidity
            || self.contact
            || self.battery
            || self.power
            || self.energy
            || self.on_off
            || self.cover
    }
}

/// Pont qui expose cet appareil, et endpoint de l'appareil sur ce pont.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeLink {
    pub node_id: u64,
    pub endpoint: u16,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeInfo {
    pub node_id: u64,
    /// Nom affiché : celui choisi par l'utilisateur, sinon celui de l'appareil.
    pub name: String,
    /// Nom annoncé par l'appareil (NodeLabel ou ProductName).
    pub device_name: String,
    pub custom_name: Option<String>,
    pub icon: Option<Symbol>,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub device_types: Vec<String>,
    pub caps: Capabilities,
    /// `Some` pour un appareil derrière un pont.
    pub bridge: Option<BridgeLink>,
}

/// Événements envoyés par la tâche de fond.
#[derive(Debug, Clone)]
pub enum Event {
    /// Appareil connu (au démarrage, après appairage, ou après lecture de sa description).
    Node(NodeInfo),
    NodeRemoved(u64),
    Status {
        node_id: u64,
        status: NodeStatus,
    },
    /// Dernière communication réussie (secondes Unix).
    Seen {
        node_id: u64,
        at: f64,
    },
    Mode {
        node_id: u64,
        mode: ReportingMode,
    },
    Measurement {
        node_id: u64,
        kind: MeasureKind,
        value: f64,
        at: f64,
    },
    State {
        node_id: u64,
        kind: StateKind,
        value: bool,
        at: f64,
    },
    /// Un volet s'ouvre, se ferme ou s'arrête (non enregistré dans l'historique).
    CoverMotion {
        node_id: u64,
        motion: CoverMotion,
    },
    /// Une règle demande une commande de volet à un appareil d'**extension** (volet Somfy) : le
    /// serveur la passe à l'extension, comme une demande d'API (`app::pump_events`).
    CoverRequested {
        node_id: u64,
        command: CoverCommand,
    },
    /// Une règle s'est exécutée (ou a échoué).
    RuleRun {
        rule_id: u64,
        name: String,
        at: f64,
        ok: bool,
        detail: String,
    },
    /// Une action « Notifier » d'une règle : à transmettre aux clients connectés
    /// (le serveur n'a pas d'écran, une notification locale ne servirait à rien).
    Notification {
        title: String,
        message: String,
        at: f64,
    },
    /// Le mode global de la maison a changé (le client le met à son journal, avec sa source si
    /// le serveur la donne).
    HomeModeChanged {
        mode: HomeMode,
        source: Option<ModeSource>,
    },
    /// Une règle demande ce mode (`Action::SetMode`) : le serveur le prend en compte comme une
    /// demande d'API (rien ne part vers les clients sous cette forme). `rule` : son nom.
    HomeModeRequested {
        mode: HomeMode,
        rule: String,
    },
    PairingProgress(String),
    PairingDone {
        node_id: u64,
        name: String,
    },
    PairingFailed(String),
    /// Erreur globale (ex. impossible d'ouvrir le socket UDP).
    Fatal(String),
    /// Information diverse affichée dans le journal de l'interface.
    Info(String),
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
