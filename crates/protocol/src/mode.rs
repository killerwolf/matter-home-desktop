//! Mode global de la maison : un seul réglage, côté serveur, que tout le monde voit.
//!
//! Il pilote le comportement des caméras (voir `wire::CameraModes`) et sert de déclencheur
//! et de condition aux automatisations (`Trigger::Mode`, `Condition::Mode`). Sur le fil et dans
//! les fichiers, un mode s'écrit `present`, `absent`, `nuit` ou `vacances`.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HomeMode {
    /// Quelqu'un est à la maison (mode par défaut).
    #[default]
    Present,
    /// Personne à la maison, pour la journée.
    Absent,
    /// La nuit : tout le monde dort.
    Nuit,
    /// Absence prolongée.
    Vacances,
}

impl HomeMode {
    pub const ALL: [HomeMode; 4] = [
        HomeMode::Present,
        HomeMode::Absent,
        HomeMode::Nuit,
        HomeMode::Vacances,
    ];

    /// Libellé affiché dans les interfaces.
    pub fn label(self) -> &'static str {
        match self {
            HomeMode::Present => "Présent",
            HomeMode::Absent => "Absent",
            HomeMode::Nuit => "Nuit",
            HomeMode::Vacances => "Vacances",
        }
    }

    /// Nom dans le JSON (`present`, `absent`, `nuit`, `vacances`).
    pub fn key(self) -> &'static str {
        match self {
            HomeMode::Present => "present",
            HomeMode::Absent => "absent",
            HomeMode::Nuit => "nuit",
            HomeMode::Vacances => "vacances",
        }
    }

    pub fn from_key(key: &str) -> Option<HomeMode> {
        HomeMode::ALL.into_iter().find(|m| m.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_json_and_default_is_present() {
        assert_eq!(HomeMode::default(), HomeMode::Present);
        for mode in HomeMode::ALL {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.key()));
            assert_eq!(serde_json::from_str::<HomeMode>(&json).unwrap(), mode);
            assert_eq!(HomeMode::from_key(mode.key()), Some(mode));
        }
        assert_eq!(HomeMode::from_key("Absent"), None);
    }
}
