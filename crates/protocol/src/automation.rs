//! Modèle des automatisations « Quand… Si… Alors… » : règles, déclencheurs, conditions,
//! actions. Partagé entre le serveur (qui les exécute) et le client (qui les édite).
//! Le moteur d'exécution est dans le crate serveur.

use serde::{Deserialize, Serialize};

use crate::events::MeasureKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnOffCommand {
    On,
    Off,
    Toggle,
}

impl OnOffCommand {
    pub const ALL: [OnOffCommand; 3] = [OnOffCommand::On, OnOffCommand::Off, OnOffCommand::Toggle];

    pub fn label(self) -> &'static str {
        match self {
            OnOffCommand::On => "Allumer",
            OnOffCommand::Off => "Éteindre",
            OnOffCommand::Toggle => "Inverser",
        }
    }

    /// Identifiant de commande du cluster On/Off (0x0006).
    pub fn command_id(self) -> u32 {
        match self {
            OnOffCommand::Off => 0x00,
            OnOffCommand::On => 0x01,
            OnOffCommand::Toggle => 0x02,
        }
    }
}

/// Commande d'un volet ou store ; `Position` : ouverture visée en % (100 = tout ouvert).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoverCommand {
    Open,
    Close,
    Stop,
    Position(u8),
}

impl CoverCommand {
    /// Une valeur par genre de commande, pour les sélecteurs de l'interface.
    pub const KINDS: [CoverCommand; 4] = [
        CoverCommand::Open,
        CoverCommand::Close,
        CoverCommand::Stop,
        CoverCommand::Position(50),
    ];

    pub fn label(self) -> String {
        match self {
            CoverCommand::Position(p) => format!("Ouvrir à {p} %"),
            other => other.kind_label().to_string(),
        }
    }

    /// Libellé du genre de commande, sans la valeur.
    pub fn kind_label(self) -> &'static str {
        match self {
            CoverCommand::Open => "Ouvrir",
            CoverCommand::Close => "Fermer",
            CoverCommand::Stop => "Arrêter",
            CoverCommand::Position(_) => "Ouvrir à…",
        }
    }

    pub fn same_kind(self, other: CoverCommand) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// Identifiant de commande du cluster Window Covering (0x0102).
    pub fn command_id(self) -> u32 {
        match self {
            CoverCommand::Open => 0x00,
            CoverCommand::Close => 0x01,
            CoverCommand::Stop => 0x02,
            CoverCommand::Position(_) => 0x05,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Trigger {
    /// Une porte ou fenêtre s'ouvre (`open = true`) ou se ferme.
    Contact { node_id: u64, open: bool },
    /// Une mesure dépasse (`above`) ou descend sous `value`, pendant au moins `for_s` secondes.
    Threshold {
        node_id: u64,
        measure: MeasureKind,
        above: bool,
        value: f64,
        #[serde(default)]
        for_s: u64,
    },
    /// Une prise ou lampe s'allume (`on = true`) ou s'éteint.
    Switch { node_id: u64, on: bool },
    /// Un appareil reste injoignable `for_s` secondes.
    Unreachable { node_id: u64, for_s: u64 },
    /// Tous les jours cochés (lundi = 0) à l'heure dite.
    Time {
        hour: u8,
        minute: u8,
        #[serde(default = "all_days")]
        days: [bool; 7],
    },
}

fn all_days() -> [bool; 7] {
    [true; 7]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    /// Entre deux heures (la plage peut passer minuit : 22:00 → 07:00).
    TimeBetween {
        from: (u8, u8),
        to: (u8, u8),
    },
    Contact {
        node_id: u64,
        open: bool,
    },
    Switch {
        node_id: u64,
        on: bool,
    },
    Threshold {
        node_id: u64,
        measure: MeasureKind,
        above: bool,
        value: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    /// Notification envoyée aux clients connectés (pas au Mac du serveur, où personne ne
    /// la verrait). Variables : {appareil}, {valeur}, {heure}, {regle}.
    Notify {
        title: String,
        message: String,
    },
    SetOnOff {
        node_id: u64,
        command: OnOffCommand,
    },
    /// Ouvrir, fermer, arrêter un volet ou l'amener à une position.
    SetCover {
        node_id: u64,
        command: CoverCommand,
    },
    /// Appel HTTP : POST si `body` n'est pas vide, sinon GET. Mêmes variables.
    Webhook {
        url: String,
        #[serde(default)]
        body: String,
    },
    Delay {
        seconds: u64,
    },
}

fn default_cooldown() -> u64 {
    60
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub trigger: Trigger,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Délai minimal entre deux exécutions (anti-rafale).
    #[serde(default = "default_cooldown")]
    pub cooldown_s: u64,
}

impl Rule {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            name: "Nouvelle règle".into(),
            enabled: true,
            trigger: Trigger::Time {
                hour: 7,
                minute: 0,
                days: all_days(),
            },
            conditions: Vec::new(),
            actions: vec![Action::Notify {
                title: "{regle}".into(),
                message: "Déclenchée à {heure}".into(),
            }],
            cooldown_s: default_cooldown(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleSet {
    pub version: u32,
    pub rules: Vec<Rule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self {
            version: 1,
            rules: Vec::new(),
        }
    }
}

impl RuleSet {
    pub fn next_id(&self) -> u64 {
        self.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }
}
