//! Le modèle du menu de la barre de menus, construit sans interface : `main.rs` n'a plus qu'à le
//! transformer en menu natif. Rien d'autre que ces entrées : pas d'option cachée.

use crate::status::{status_line, Health, Phase};
use crate::tailscale::WebAccess;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    CopyUrl,
    CopyFingerprint,
    OpenLog,
    ToggleLogin,
    Cameras,
    Settings,
    Quit,
    WebAccess,
    WebOpen,
    WebCopy,
    WebAuth,
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
            Action::WebAccess => "web-access",
            Action::WebOpen => "web-open",
            Action::WebCopy => "web-copy",
            Action::WebAuth => "web-auth",
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
            Action::WebAccess,
            Action::WebOpen,
            Action::WebCopy,
            Action::WebAuth,
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
    /// Le nom complet de la version (`version::display_version`).
    pub version: &'a str,
    /// L'accès web par Tailscale (lu hors du fil principal).
    pub web: &'a WebAccess,
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

/// Les lignes de l'accès web. Un état qui n'est pas actif ne propose rien d'autre que la case
/// (grisée quand elle ne peut rien faire) ; actif, il propose d'ouvrir et de copier l'adresse.
fn web_entries(web: &WebAccess) -> Vec<Entry> {
    let check = |label: &str, enabled: bool, on: bool| Entry::Item {
        action: Action::WebAccess,
        label: label.into(),
        enabled,
        checked: Some(on),
    };
    match web {
        WebAccess::Checking => vec![check("Accès web (Tailscale)", false, false)],
        WebAccess::Off { .. } => vec![check("Accès web (Tailscale)", true, false)],
        WebAccess::On { dns } => vec![
            check("Accès web (Tailscale)", true, true),
            item(Action::WebOpen, &format!("Ouvrir https://{dns}/app/"), true),
            item(Action::WebCopy, "Copier l'adresse", true),
        ],
        WebAccess::NotInstalled => vec![check("Accès web : Tailscale non installé", false, false)],
        WebAccess::Disconnected => vec![check("Accès web : Tailscale déconnecté", false, false)],
        WebAccess::PortBusy => vec![check(
            "Accès web : port 443 déjà utilisé par Tailscale",
            false,
            false,
        )],
        WebAccess::Funnel => vec![check(
            "Accès web : Funnel actif (public), à couper dans Tailscale",
            false,
            false,
        )],
        WebAccess::NeedsAuth { .. } => vec![
            check("Accès web : à autoriser dans Tailscale", true, false),
            item(
                Action::WebAuth,
                "Ouvrir la page d'autorisation de Tailscale…",
                true,
            ),
        ],
        WebAccess::Unavailable => vec![check(
            "Accès web : état de Tailscale illisible",
            false,
            false,
        )],
    }
}

pub fn build(i: &MenuInput<'_>) -> Vec<Entry> {
    let mut m = vec![
        Entry::Info(format!("Matter Home Server {}", i.version)),
        Entry::Separator,
        Entry::Info(status_line(i.phase, i.health)),
    ];
    match i.phase {
        Phase::Running if !i.health.unreachable.is_empty() => {
            m.push(Entry::Info("Appareils injoignables :".into()));
            for name in &i.health.unreachable {
                m.push(Entry::Info(format!("    {name}")));
            }
        }
        Phase::Blocked(_) => m.push(Entry::Info(
            "Ouvrez « Réglages… » pour la marche à suivre.".into(),
        )),
        Phase::Problem(_) => m.push(Entry::Info("Ouvrez « Journal » pour le détail.".into())),
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
        // L'adresse et les deux « Copier » : ce qu'il faut donner au client pour se connecter.
        m.push(Entry::Submenu {
            label: "Connecter un client".into(),
            entries: vec![
                Entry::Info(format!("Adresse : {url}")),
                item(Action::CopyUrl, "Copier l'adresse", true),
                item(
                    Action::CopyFingerprint,
                    "Copier l'empreinte du certificat",
                    i.fingerprint.is_some(),
                ),
            ],
        });
        m.extend(web_entries(i.web));
        m.push(Entry::Separator);
    }
    // Les caméras se gèrent depuis le hub du serveur : seulement quand il tourne.
    m.push(item(
        Action::Cameras,
        "Caméras…",
        matches!(i.phase, Phase::Running),
    ));
    m.push(item(Action::Settings, "Réglages…", true));
    m.push(item(Action::OpenLog, "Journal", true));
    m.push(Entry::Separator);
    m.push(Entry::Item {
        action: Action::ToggleLogin,
        label: "Ouvrir à la connexion".into(),
        enabled: i.login_available,
        checked: Some(i.login_enabled),
    });
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
            version: "1.0.0-rc.7",
            web: &WebAccess::Checking,
            phase: &Phase::Running,
            health: &health,
            url: Some("https://mac.local:8443"),
            fingerprint: Some("AA:BB"),
            login_enabled: true,
            desktop_doubt: false,
            login_available: true,
        });
        assert_eq!(
            menu[2],
            Entry::Info("12 appareils en ligne, 1 injoignable".into())
        );
        assert!(menu.contains(&Entry::Info("    Salon".into())));
        // L'adresse et les deux « Copier » sont dans le sous-menu « Connecter un client ».
        let Some(Entry::Submenu { label, entries }) =
            menu.iter().find(|e| matches!(e, Entry::Submenu { .. }))
        else {
            panic!("sous-menu attendu");
        };
        assert_eq!(label, "Connecter un client");
        assert_eq!(
            entries[0],
            Entry::Info("Adresse : https://mac.local:8443".into())
        );
        assert_eq!(actions(entries), [Action::CopyUrl, Action::CopyFingerprint]);
        assert_eq!(
            actions(&menu),
            [
                Action::WebAccess,
                Action::Cameras,
                Action::Settings,
                Action::OpenLog,
                Action::ToggleLogin,
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
            version: "1.0.0-rc.7",
            web: &WebAccess::Checking,
            phase: &Phase::Blocked(Blocker::Service),
            health: &health,
            url: None,
            fingerprint: None,
            login_enabled: false,
            desktop_doubt: false,
            login_available: true,
        });
        assert!(!menu.iter().any(|e| matches!(e, Entry::Submenu { .. })));
        assert!(actions(&menu).contains(&Action::Settings));
        assert!(actions(&menu).contains(&Action::Quit));
    }

    #[test]
    fn the_cameras_entry_is_only_enabled_while_the_server_runs() {
        let health = Health::default();
        let entry = |phase: &Phase| {
            build(&MenuInput {
                version: "1.0.0",
                web: &WebAccess::Checking,
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
                version: "1.0.0",
                web: &WebAccess::Checking,
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

    fn menu_with(web: &WebAccess) -> Vec<Entry> {
        build(&MenuInput {
            version: "1.0.0-rc.7",
            web,
            phase: &Phase::Running,
            health: &Health::default(),
            url: Some("https://mac.local:8443"),
            fingerprint: None,
            login_enabled: false,
            desktop_doubt: false,
            login_available: true,
        })
    }

    fn web_items(menu: &[Entry]) -> Vec<(Action, String, bool, Option<bool>)> {
        menu.iter()
            .filter_map(|e| match e {
                Entry::Item {
                    action:
                        action @ (Action::WebAccess
                        | Action::WebOpen
                        | Action::WebCopy
                        | Action::WebAuth),
                    label,
                    enabled,
                    checked,
                } => Some((*action, label.clone(), *enabled, *checked)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_version_is_the_first_line_and_not_clickable() {
        let menu = menu_with(&WebAccess::Checking);
        assert_eq!(menu[0], Entry::Info("Matter Home Server 1.0.0-rc.7".into()));
    }

    #[test]
    fn web_access_shows_the_reason_when_it_cannot_be_used() {
        for (web, label) in [
            (
                WebAccess::NotInstalled,
                "Accès web : Tailscale non installé",
            ),
            (WebAccess::Disconnected, "Accès web : Tailscale déconnecté"),
            (
                WebAccess::PortBusy,
                "Accès web : port 443 déjà utilisé par Tailscale",
            ),
            (
                WebAccess::Funnel,
                "Accès web : Funnel actif (public), à couper dans Tailscale",
            ),
        ] {
            assert_eq!(
                web_items(&menu_with(&web)),
                [(Action::WebAccess, label.to_string(), false, Some(false))]
            );
        }
    }

    #[test]
    fn web_access_when_off_is_an_enabled_unchecked_box() {
        let web = WebAccess::Off { dns: None };
        assert_eq!(
            web_items(&menu_with(&web)),
            [(
                Action::WebAccess,
                "Accès web (Tailscale)".to_string(),
                true,
                Some(false)
            )]
        );
    }

    #[test]
    fn active_web_access_offers_to_open_and_copy_the_address() {
        let web = WebAccess::On {
            dns: "mac.tailnet.ts.net".into(),
        };
        assert_eq!(
            web_items(&menu_with(&web)),
            [
                (
                    Action::WebAccess,
                    "Accès web (Tailscale)".to_string(),
                    true,
                    Some(true)
                ),
                (
                    Action::WebOpen,
                    "Ouvrir https://mac.tailnet.ts.net/app/".to_string(),
                    true,
                    None
                ),
                (Action::WebCopy, "Copier l'adresse".to_string(), true, None),
            ]
        );
    }

    #[test]
    fn serve_to_authorise_offers_the_link() {
        let web = WebAccess::NeedsAuth {
            url: "https://login.tailscale.com/f/serve?node=x".into(),
        };
        let items = web_items(&menu_with(&web));
        assert_eq!(items[0].1, "Accès web : à autoriser dans Tailscale");
        assert_eq!(items[1].0, Action::WebAuth);
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
            Action::WebAccess,
            Action::WebOpen,
            Action::WebCopy,
            Action::WebAuth,
        ] {
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id("autre"), None);
    }
}
