//! Le modèle du menu de la barre de menus, construit sans interface : `main.rs` n'a plus qu'à le
//! transformer en menu natif. Rien d'autre que ces entrées : pas d'option cachée.

use crate::status::{status_line, Health, Phase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    CopyUrl,
    CopyFingerprint,
    OpenLog,
    ToggleLogin,
    Cameras,
    Settings,
    Quit,
}

impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Action::CopyUrl => "copy-url",
            Action::CopyFingerprint => "copy-fingerprint",
            Action::OpenLog => "open-log",
            Action::ToggleLogin => "toggle-login",
            Action::Cameras => "cameras",
            Action::Settings => "settings",
            Action::Quit => "quit",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        [
            Action::CopyUrl,
            Action::CopyFingerprint,
            Action::OpenLog,
            Action::ToggleLogin,
            Action::Cameras,
            Action::Settings,
            Action::Quit,
        ]
        .into_iter()
        .find(|a| a.id() == id)
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
    Separator,
}

/// Ce que le menu montre.
#[derive(Debug, Clone)]
pub struct MenuInput<'a> {
    pub phase: &'a Phase,
    pub health: &'a Health,
    pub url: Option<&'a str>,
    pub fingerprint: Option<&'a str>,
    pub login_enabled: bool,
    /// Un processus `matter-home-desktop` incertain : le serveur tourne, mais on le signale.
    pub desktop_doubt: bool,
    /// L'ouverture à la connexion peut être (dés)activée (application installée, pas en essai).
    pub login_available: bool,
}

fn item(action: Action, label: &str, enabled: bool) -> Entry {
    Entry::Item {
        action,
        label: label.into(),
        enabled,
        checked: None,
    }
}

pub fn build(i: &MenuInput<'_>) -> Vec<Entry> {
    let mut m = vec![Entry::Info(status_line(i.phase, i.health))];
    match i.phase {
        Phase::Running if !i.health.unreachable.is_empty() => {
            m.push(Entry::Info("Appareils injoignables :".into()));
            for name in &i.health.unreachable {
                m.push(Entry::Info(format!("    {name}")));
            }
        }
        Phase::Blocked(_) => m.push(Entry::Info(
            "Ouvrez « Compte et réglages… » pour la marche à suivre.".into(),
        )),
        Phase::Problem(_) => m.push(Entry::Info(
            "Ouvrez « Ouvrir le journal » pour le détail.".into(),
        )),
        _ => {}
    }
    if i.desktop_doubt && matches!(i.phase, Phase::Running) {
        m.push(Entry::Info(
            "Un programme « matter-home-desktop » est incertain : si c'est la v0.2, fermez-la."
                .into(),
        ));
    }
    m.push(Entry::Separator);
    if let Some(url) = i.url {
        m.push(Entry::Info(format!("Adresse : {url}")));
        m.push(item(Action::CopyUrl, "Copier l'adresse", true));
        m.push(item(
            Action::CopyFingerprint,
            "Copier l'empreinte du certificat",
            i.fingerprint.is_some(),
        ));
        m.push(Entry::Separator);
    }
    m.push(item(Action::OpenLog, "Ouvrir le journal", true));
    m.push(Entry::Item {
        action: Action::ToggleLogin,
        label: "Ouvrir à la connexion".into(),
        enabled: i.login_available,
        checked: Some(i.login_enabled),
    });
    // Les caméras se gèrent depuis le hub du serveur : seulement quand il tourne.
    m.push(item(
        Action::Cameras,
        "Caméras…",
        matches!(i.phase, Phase::Running),
    ));
    m.push(item(Action::Settings, "Compte et réglages…", true));
    m.push(Entry::Separator);
    m.push(item(Action::Quit, "Quitter Matter Home Server…", true));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::Blocker;

    fn actions(menu: &[Entry]) -> Vec<Action> {
        menu.iter()
            .filter_map(|e| match e {
                Entry::Item { action, .. } => Some(*action),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn running_menu_has_exactly_the_requested_entries() {
        let health = Health {
            total: 13,
            online: 12,
            unreachable: vec!["Salon".into()],
        };
        let menu = build(&MenuInput {
            phase: &Phase::Running,
            health: &health,
            url: Some("https://mac.local:8443"),
            fingerprint: Some("AA:BB"),
            login_enabled: true,
            desktop_doubt: false,
            login_available: true,
        });
        assert_eq!(
            menu[0],
            Entry::Info("En marche · 12 appareils sur 13 en ligne".into())
        );
        assert!(menu.contains(&Entry::Info("    Salon".into())));
        assert!(menu.contains(&Entry::Info("Adresse : https://mac.local:8443".into())));
        assert_eq!(
            actions(&menu),
            [
                Action::CopyUrl,
                Action::CopyFingerprint,
                Action::OpenLog,
                Action::ToggleLogin,
                Action::Cameras,
                Action::Settings,
                Action::Quit
            ]
        );
        assert!(menu.iter().any(|e| matches!(
            e,
            Entry::Item {
                action: Action::ToggleLogin,
                checked: Some(true),
                ..
            }
        )));
    }

    #[test]
    fn blocked_menu_has_no_address_and_points_to_the_window() {
        let health = Health::default();
        let menu = build(&MenuInput {
            phase: &Phase::Blocked(Blocker::Service),
            health: &health,
            url: None,
            fingerprint: None,
            login_enabled: false,
            desktop_doubt: false,
            login_available: true,
        });
        assert!(!actions(&menu).contains(&Action::CopyUrl));
        assert!(actions(&menu).contains(&Action::Settings));
        assert!(actions(&menu).contains(&Action::Quit));
    }

    #[test]
    fn the_cameras_entry_is_only_enabled_while_the_server_runs() {
        let health = Health::default();
        let entry = |phase: &Phase| {
            build(&MenuInput {
                phase,
                health: &health,
                url: None,
                fingerprint: None,
                login_enabled: false,
                desktop_doubt: false,
                login_available: true,
            })
            .into_iter()
            .find_map(|e| match e {
                Entry::Item {
                    action: Action::Cameras,
                    label,
                    enabled,
                    ..
                } => Some((label, enabled)),
                _ => None,
            })
            .expect("entrée Caméras")
        };
        assert_eq!(entry(&Phase::Running), ("Caméras…".to_string(), true));
        assert!(!entry(&Phase::Starting).1);
        assert!(!entry(&Phase::Blocked(Blocker::Service)).1);
    }

    #[test]
    fn running_menu_mentions_an_uncertain_desktop_process() {
        let health = Health::default();
        let build_with = |doubt| {
            build(&MenuInput {
                phase: &Phase::Running,
                health: &health,
                url: None,
                fingerprint: None,
                login_enabled: false,
                desktop_doubt: doubt,
                login_available: true,
            })
        };
        assert!(build_with(true)
            .iter()
            .any(|e| matches!(e, Entry::Info(t) if t.contains("matter-home-desktop"))));
        assert!(!build_with(false)
            .iter()
            .any(|e| matches!(e, Entry::Info(t) if t.contains("matter-home-desktop"))));
    }

    #[test]
    fn ids_round_trip() {
        for a in [
            Action::CopyUrl,
            Action::CopyFingerprint,
            Action::OpenLog,
            Action::ToggleLogin,
            Action::Cameras,
            Action::Settings,
            Action::Quit,
        ] {
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id("autre"), None);
    }
}
