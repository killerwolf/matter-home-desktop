//! Le modèle du menu de la barre de menus, construit sans interface : `ui.rs` n'a plus qu'à le
//! transformer en menu natif. Rien d'autre que ces entrées : pas d'option cachée.
//!
//! Le menu répond à une question, « est-ce que tout va bien ? » : le titre du résumé, une ligne
//! par sujet, et une seule action quand quelque chose ne va pas. Les tâches rares (connecter un
//! appareil, accès à distance, réglages, journal) sont dans la fenêtre, où il y a la place de les
//! expliquer : le menu ne fait que l'ouvrir au bon onglet.

use protocol::mode::HomeMode;

use crate::overview::{Overview, Tab};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// L'action proposée par le résumé (`Overview::fix`) : ouvre la fenêtre là où ça se règle.
    Fix,
    /// Ouvrir la fenêtre sur un onglet.
    Open(Tab),
    Quit,
    /// Choisir le mode de la maison (sous-menu « Mode »).
    Mode(HomeMode),
}

impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Action::Fix => "fix",
            Action::Open(Tab::Overview) => "open-vue",
            Action::Open(Tab::Devices) => "open-appareils",
            Action::Open(Tab::Cameras) => "open-cameras",
            Action::Open(Tab::Connect) => "open-connexion",
            Action::Open(Tab::Settings) => "open-reglages",
            Action::Quit => "quit",
            Action::Mode(HomeMode::Present) => "mode-present",
            Action::Mode(HomeMode::Absent) => "mode-absent",
            Action::Mode(HomeMode::Nuit) => "mode-nuit",
            Action::Mode(HomeMode::Vacances) => "mode-vacances",
        }
    }

    pub fn all() -> impl Iterator<Item = Action> {
        [Action::Fix, Action::Quit]
            .into_iter()
            .chain(Tab::ALL.map(Action::Open))
            .chain(HomeMode::ALL.map(Action::Mode))
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::all().find(|a| a.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Ligne d'information, grisée (non cliquable).
    Info(String),
    Item {
        action: Action,
        label: String,
        enabled: bool,
        /// `Some` : élément à cocher.
        checked: Option<bool>,
    },
    /// Sous-menu : son titre et ses entrées (sans autre sous-menu).
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
    Separator,
}

/// Ce que le menu montre.
#[derive(Debug, Clone)]
pub struct MenuInput<'a> {
    /// Le résumé (« Tout va bien », une ligne par sujet, l'action à proposer).
    pub overview: &'a Overview,
    /// Le mode de la maison : `Some` tant que le serveur tourne (c'est lui qui le tient).
    pub mode: Option<HomeMode>,
}

fn item(action: Action, label: &str) -> Entry {
    Entry::Item {
        action,
        label: label.into(),
        enabled: true,
        checked: None,
    }
}

pub fn build(i: &MenuInput<'_>) -> Vec<Entry> {
    let o = i.overview;
    let mut m = vec![Entry::Info(o.headline.clone())];
    m.extend(
        o.checks
            .iter()
            .map(|c| Entry::Info(format!("{} : {}", c.title, c.text))),
    );
    // Une seule action, et seulement quand quelque chose attend l'utilisateur.
    if let Some(fix) = &o.fix {
        m.push(item(Action::Fix, &fix.label));
    }
    m.push(Entry::Separator);
    // Le mode global : un sous-menu à quatre choix, la coche sur le mode actuel.
    if let Some(current) = i.mode {
        m.push(Entry::Submenu {
            label: format!("Mode de la maison : {}", current.label()),
            entries: HomeMode::ALL
                .into_iter()
                .map(|mode| Entry::Item {
                    action: Action::Mode(mode),
                    label: mode.label().into(),
                    enabled: true,
                    checked: Some(mode == current),
                })
                .collect(),
        });
        m.push(Entry::Separator);
    }
    m.push(item(
        Action::Open(Tab::Overview),
        "Ouvrir Matter Home Server…",
    ));
    // Ces deux onglets n'ont de contenu que lorsque le serveur tourne.
    if i.mode.is_some() {
        m.push(item(
            Action::Open(Tab::Connect),
            "Connecter un Mac ou un iPhone…",
        ));
        m.push(item(Action::Open(Tab::Cameras), "Caméras…"));
    }
    m.push(item(Action::Open(Tab::Settings), "Réglages…"));
    m.push(Entry::Separator);
    m.push(item(Action::Quit, "Quitter Matter Home Server…"));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::{self, CameraSummary};
    use crate::startup::Blocker;
    use crate::status::{Health, Phase};
    use crate::tailscale::WebAccess;

    fn actions(menu: &[Entry]) -> Vec<Action> {
        menu.iter()
            .filter_map(|e| match e {
                Entry::Item { action, .. } => Some(*action),
                _ => None,
            })
            .collect()
    }

    fn infos(menu: &[Entry]) -> Vec<&str> {
        menu.iter()
            .filter_map(|e| match e {
                Entry::Info(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn running(unreachable: &[&str], web: &WebAccess) -> Overview {
        let health = Health {
            total: 13,
            online: 13 - unreachable.len(),
            unreachable: unreachable.iter().map(|s| s.to_string()).collect(),
        };
        let cameras = CameraSummary {
            total: 2,
            online: 2,
            ..CameraSummary::default()
        };
        overview::build(&Phase::Running, &health, Some(&cameras), web, false)
    }

    #[test]
    fn a_healthy_server_shows_its_summary_and_no_corrective_action() {
        let o = running(&[], &WebAccess::Off { dns: None });
        let menu = build(&MenuInput {
            overview: &o,
            mode: Some(HomeMode::Nuit),
        });
        assert_eq!(
            infos(&menu),
            [
                "Tout va bien",
                "Appareils : 13 appareils, tous joignables",
                "Caméras : 2 caméras en service · détection indisponible",
                "Accès à distance : désactivé",
            ]
        );
        assert_eq!(
            actions(&menu),
            [
                Action::Open(Tab::Overview),
                Action::Open(Tab::Connect),
                Action::Open(Tab::Cameras),
                Action::Open(Tab::Settings),
                Action::Quit
            ]
        );
    }

    #[test]
    fn something_wrong_adds_exactly_one_action_right_under_the_summary() {
        let o = running(
            &["Salon"],
            &WebAccess::NeedsAuth {
                url: "https://login.tailscale.com/f/serve?node=x".into(),
            },
        );
        let menu = build(&MenuInput {
            overview: &o,
            mode: Some(HomeMode::Present),
        });
        assert_eq!(menu[0], Entry::Info("2 points à regarder".into()));
        assert_eq!(
            menu[4],
            Entry::Item {
                action: Action::Fix,
                label: "Voir l'appareil injoignable…".into(),
                enabled: true,
                checked: None
            }
        );
        assert_eq!(
            actions(&menu).iter().filter(|a| **a == Action::Fix).count(),
            1
        );
        // Aucun lien ni adresse dans le menu.
        assert!(!infos(&menu).join(" ").contains("http"));
    }

    #[test]
    fn the_mode_submenu_has_four_choices_and_one_check() {
        let o = running(&[], &WebAccess::Checking);
        let menu = build(&MenuInput {
            overview: &o,
            mode: Some(HomeMode::Nuit),
        });
        let Some(Entry::Submenu { label, entries }) =
            menu.iter().find(|e| matches!(e, Entry::Submenu { .. }))
        else {
            panic!("sous-menu Mode attendu");
        };
        assert_eq!(label, "Mode de la maison : Nuit");
        assert_eq!(
            entries
                .iter()
                .map(|e| match e {
                    Entry::Item {
                        label,
                        checked: Some(on),
                        ..
                    } => (label.as_str(), *on),
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>(),
            [
                ("Présent", false),
                ("Absent", false),
                ("Nuit", true),
                ("Vacances", false)
            ]
        );
    }

    #[test]
    fn a_stopped_server_points_to_the_window_and_offers_no_mode() {
        let o = overview::build(
            &Phase::Blocked(Blocker::Service),
            &Health::default(),
            None,
            &WebAccess::Checking,
            false,
        );
        let menu = build(&MenuInput {
            overview: &o,
            mode: None,
        });
        assert_eq!(
            infos(&menu),
            [
                "Le serveur est à l'arrêt",
                "Serveur : le service installé tient les données"
            ]
        );
        assert!(!menu.iter().any(|e| matches!(e, Entry::Submenu { .. })));
        assert_eq!(
            actions(&menu),
            [
                Action::Fix,
                Action::Open(Tab::Overview),
                Action::Open(Tab::Settings),
                Action::Quit
            ]
        );
    }

    #[test]
    fn ids_round_trip_and_are_unique() {
        let all: Vec<Action> = Action::all().collect();
        for a in &all {
            assert_eq!(Action::from_id(a.id()), Some(*a));
        }
        let mut ids: Vec<&str> = all.iter().map(|a| a.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        assert_eq!(Action::from_id("autre"), None);
    }
}
