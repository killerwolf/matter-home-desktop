//! Mode global de la maison : un seul réglage, côté serveur, que tout le monde voit.
//!
//! Il pilote le comportement des caméras (voir `wire::CameraModes`) et sert de déclencheur
//! et de condition aux automatisations (`Trigger::Mode`, `Condition::Mode`). Sur le fil et dans
//! les fichiers, un mode s'écrit `present`, `absent`, `nuit` ou `vacances`.

use serde::{Deserialize, Serialize};

use crate::wire::{CameraBehavior, CameraModes, CameraPlacement};

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

    /// Ce que le mode fait des caméras, en une ligne (« Caméras intérieures éteintes, extérieures
    /// en diffusion. »). Calculée à partir de `CameraModes::for_placement` : elle ne peut pas
    /// contredire le comportement réel. C'est la ligne par défaut : une caméra dont les réglages
    /// ont été changés à la main s'écarte de cette phrase.
    pub fn effect(self) -> String {
        let indoor = CameraModes::for_placement(CameraPlacement::Interieure).get(self);
        let outdoor = CameraModes::for_placement(CameraPlacement::Exterieure).get(self);
        let state = |b: CameraBehavior| match b {
            CameraBehavior::Eteinte => "éteintes",
            CameraBehavior::Diffusion => "en diffusion",
            CameraBehavior::Surveillance => "en surveillance",
        };
        if indoor == outdoor {
            format!("Toutes les caméras {}.", state(indoor))
        } else {
            format!(
                "Caméras intérieures {}, extérieures {}.",
                state(indoor),
                state(outdoor)
            )
        }
    }

    pub fn from_key(key: &str) -> Option<HomeMode> {
        HomeMode::ALL.into_iter().find(|m| m.key() == key)
    }
}

/// D'où vient un changement de mode : écrit au journal du serveur et des clients, diffusé avec
/// `mode_changed` et disponible dans les notifications des règles (`{source}`).
///
/// Sur le fil : `{"kind": "shortcut"}`, `{"kind": "rule", "name": "Nuit à 23 h"}`… Une valeur
/// inconnue (serveur plus récent) se lit comme `Other` : un ancien client ne casse pas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModeSource {
    /// Un raccourci de l'iPhone (requête portant le jeton de mode).
    Shortcut,
    /// L'app web.
    Web,
    /// L'app de bureau.
    Desktop,
    /// La fenêtre du serveur (app de la barre de menus).
    ServerApp,
    /// Une automatisation (« Alors : changer le mode »), avec le nom de la règle.
    Rule {
        #[serde(default)]
        name: String,
    },
    /// Un widget de l'iPhone qui le dit lui-même (avec le jeton de mode). Le widget Scriptable ne
    /// fait que lire ; les boutons de mode de l'écran d'accueil lancent les raccourcis, qui
    /// arrivent comme `Shortcut`.
    Widget,
    /// La maison toute seule : le Wi-Fi de la box a vu quelqu'un arriver ou la dernière personne
    /// partir (règle de présence).
    Presence,
    /// Un agent d'IA (serveur MCP intégré, jeton d'agent). Un ancien client le lit `Other`.
    Agent,
    /// Un client qui ne dit rien (ancienne version) ou une source inconnue.
    #[serde(other)]
    Other,
}

impl ModeSource {
    /// En quelques mots, pour le journal et les notifications : « raccourci iPhone »,
    /// « automatisation « Nuit à 23 h » »…
    pub fn label(&self) -> String {
        match self {
            ModeSource::Shortcut => "raccourci iPhone".into(),
            ModeSource::Web => "app web".into(),
            ModeSource::Desktop => "app de bureau".into(),
            ModeSource::ServerApp => "fenêtre du serveur".into(),
            ModeSource::Rule { name } if name.is_empty() => "automatisation".into(),
            ModeSource::Rule { name } => format!("automatisation « {name} »"),
            ModeSource::Widget => "widget iPhone".into(),
            ModeSource::Presence => "présence (Wi-Fi)".into(),
            ModeSource::Agent => "agent d'IA".into(),
            ModeSource::Other => "autre".into(),
        }
    }

    /// Ce qu'un client peut annoncer lui-même dans `PUT /api/mode`, selon son jeton : avec le
    /// jeton de mode, raccourci ou widget (raccourci par défaut) ; avec une session, app web ou
    /// app de bureau (autre par défaut). Jamais une automatisation ni la fenêtre du serveur.
    pub fn claimed(declared: Option<ModeSource>, mode_token: bool) -> ModeSource {
        match (mode_token, declared) {
            (true, Some(ModeSource::Widget)) => ModeSource::Widget,
            (true, _) => ModeSource::Shortcut,
            (false, Some(s @ (ModeSource::Web | ModeSource::Desktop))) => s,
            (false, _) => ModeSource::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_sources_on_the_wire() {
        let rule = ModeSource::Rule {
            name: "Nuit à 23 h".into(),
        };
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            serde_json::json!({"kind": "rule", "name": "Nuit à 23 h"})
        );
        assert_eq!(
            serde_json::to_value(ModeSource::Shortcut).unwrap(),
            serde_json::json!({"kind": "shortcut"})
        );
        for source in [
            ModeSource::Shortcut,
            ModeSource::Web,
            ModeSource::Desktop,
            ModeSource::ServerApp,
            rule.clone(),
            ModeSource::Widget,
            ModeSource::Presence,
            ModeSource::Agent,
            ModeSource::Other,
        ] {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(serde_json::from_str::<ModeSource>(&json).unwrap(), source);
        }
        // Un serveur plus récent : une source inconnue se lit « autre ».
        let unknown: ModeSource = serde_json::from_str(r#"{"kind":"montre","extra":1}"#).unwrap();
        assert_eq!(unknown, ModeSource::Other);
        assert_eq!(rule.label(), "automatisation « Nuit à 23 h »");
        assert_eq!(ModeSource::Shortcut.label(), "raccourci iPhone");
    }

    #[test]
    fn clients_cannot_claim_any_source() {
        use ModeSource::*;
        let rule = || Rule { name: "x".into() };
        // Jeton de mode : raccourci, ou widget s'il le dit ; jamais autre chose.
        assert_eq!(ModeSource::claimed(None, true), Shortcut);
        assert_eq!(ModeSource::claimed(Some(Widget), true), Widget);
        assert_eq!(ModeSource::claimed(Some(Web), true), Shortcut);
        assert_eq!(ModeSource::claimed(Some(rule()), true), Shortcut);
        // Session : app web ou de bureau ; un ancien client (rien) ou une prétention → autre.
        assert_eq!(ModeSource::claimed(Some(Web), false), Web);
        assert_eq!(ModeSource::claimed(Some(Desktop), false), Desktop);
        assert_eq!(ModeSource::claimed(None, false), Other);
        assert_eq!(ModeSource::claimed(Some(Shortcut), false), Other);
        assert_eq!(ModeSource::claimed(Some(ServerApp), false), Other);
        assert_eq!(ModeSource::claimed(Some(rule()), false), Other);
        // Personne ne peut se dire « agent » : seul le serveur MCP passe cette source au Hub.
        assert_eq!(ModeSource::claimed(Some(Agent), true), Shortcut);
        assert_eq!(ModeSource::claimed(Some(Agent), false), Other);
    }

    #[test]
    fn effect_says_what_the_cameras_do() {
        assert_eq!(
            HomeMode::Present.effect(),
            "Caméras intérieures éteintes, extérieures en diffusion."
        );
        assert_eq!(
            HomeMode::Absent.effect(),
            "Toutes les caméras en surveillance."
        );
        assert_eq!(
            HomeMode::Nuit.effect(),
            "Caméras intérieures éteintes, extérieures en surveillance."
        );
        assert_eq!(
            HomeMode::Vacances.effect(),
            "Toutes les caméras en surveillance."
        );
    }

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
