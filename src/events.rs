//! Messages échangés entre l'interface (thread egui) et la tâche de fond (tokio).

use std::sync::mpsc;

use crate::home::Symbol;

/// Commandes envoyées par l'interface.
#[derive(Debug, Clone)]
pub enum Command {
    /// Appairer un appareil avec ce code (manuel ou `MT:...`).
    Pair(String),
    /// Retenter immédiatement la connexion à un nœud.
    Retry(u64),
    /// Oublier un nœud localement (l'appareil garde notre fabric : voir README).
    Forget(u64),
    /// Effacer l'historique enregistré d'un nœud (sur disque).
    ClearHistory(u64),
    /// Nouvelles règles d'automatisation (déjà sauvegardées par l'interface).
    SetRules(Vec<crate::automation::Rule>),
    /// Exécuter tout de suite les actions d'une règle (bouton « Tester »).
    TestRule(u64),
    /// Nom et icône choisis par l'utilisateur ; `None` = revenir à l'automatique.
    Customize {
        node_id: u64,
        name: Option<String>,
        icon: Option<Symbol>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeStatus {
    Connecting,
    Online,
    /// Injoignable ; nouvel essai dans `retry_in_s` secondes.
    Unreachable { error: String, retry_in_s: u64 },
}

/// Grandeurs numériques suivies dans le temps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
}

/// États binaires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    /// `true` = ouvert (pas de contact).
    Open,
    /// `true` = allumé.
    OnOff,
    /// `true` = pile faible ou critique (BatChargeLevel ≠ OK).
    BatteryLow,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReportingMode {
    /// Souscription active ; l'appareil promet un rapport au moins toutes les `max_interval_s`.
    Subscription { max_interval_s: u64 },
    /// Souscription impossible : interrogation périodique.
    Polling { every_s: u64 },
}

/// Ce que l'appareil sait mesurer, d'après sa description (clusters présents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub temperature: bool,
    pub humidity: bool,
    pub contact: bool,
    pub battery: bool,
    pub power: bool,
    pub energy: bool,
    pub on_off: bool,
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
    }
}

#[derive(Debug, Clone, Default)]
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
}

/// Événements envoyés par la tâche de fond.
#[derive(Debug, Clone)]
pub enum Event {
    /// Nœud connu (au démarrage, après appairage, ou après lecture de sa description).
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
    /// Une règle s'est exécutée (ou a échoué).
    RuleRun {
        rule_id: u64,
        name: String,
        at: f64,
        ok: bool,
        detail: String,
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

/// Côté émetteur des événements : réveille l'interface à chaque envoi.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<Event>,
    ctx: eframe::egui::Context,
    /// Copie des événements pour le moteur d'automatisations.
    tap: Option<tokio::sync::mpsc::UnboundedSender<Event>>,
}

impl EventSink {
    pub fn new(tx: mpsc::Sender<Event>, ctx: eframe::egui::Context) -> Self {
        Self { tx, ctx, tap: None }
    }

    /// Même destination, avec une copie de chaque événement vers `tap`.
    pub fn with_tap(&self, tap: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
        Self { tx: self.tx.clone(), ctx: self.ctx.clone(), tap: Some(tap) }
    }

    pub fn send(&self, event: Event) {
        if let Some(tap) = &self.tap {
            let _ = tap.send(event.clone());
        }
        // L'interface peut avoir été fermée : on ignore l'erreur.
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
