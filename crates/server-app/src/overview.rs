//! « Est-ce que tout va bien ? » : le résumé que montrent le menu de la barre de menus, l'icône et
//! l'onglet « Vue d'ensemble » de la fenêtre. Un seul calcul pour les trois, sans interface, donc
//! testable : une phrase de titre, une ligne par sujet (appareils, caméras, accès à distance) et,
//! quand quelque chose ne va pas, **une** action à proposer.
//!
//! Les mots sont ceux de l'utilisateur : pas de « Serve », de « tailnet » ni de « flux » ici.

use matter_home_server::cameras::CameraAdmin;
use protocol::wire::{CameraStatus, DetectionStatusView};

use crate::startup::Blocker;
use crate::status::{Health, Phase};
use crate::tailscale::WebAccess;

/// Les onglets de la fenêtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Devices,
    Cameras,
    Connect,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Devices,
        Tab::Cameras,
        Tab::Connect,
        Tab::Settings,
    ];

    /// Le nom de l'onglet, tel qu'il s'affiche.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Vue d'ensemble",
            Tab::Devices => "Appareils",
            Tab::Cameras => "Caméras",
            Tab::Connect => "Connexion",
            Tab::Settings => "Réglages",
        }
    }

    /// Le mot de la ligne de commande (`--open cameras`) et de l'identifiant du menu.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Overview => "vue",
            Tab::Devices => "appareils",
            Tab::Cameras => "cameras",
            Tab::Connect => "connexion",
            Tab::Settings => "reglages",
        }
    }

    pub fn from_key(key: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// La gravité d'une ligne. `Info` : rien à faire (désactivé, pas encore configuré, en cours).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

impl Level {
    /// Quelque chose attend l'utilisateur.
    pub fn needs_attention(self) -> bool {
        matches!(self, Level::Warn | Level::Error)
    }
}

/// L'action proposée pour une ligne : le texte du bouton et l'onglet qu'il ouvre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub label: String,
    pub tab: Tab,
}

fn fix(label: &str, tab: Tab) -> Option<Fix> {
    Some(Fix {
        label: label.into(),
        tab,
    })
}

/// Une ligne du résumé : « Caméras : 3 caméras en service ».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub title: &'static str,
    pub level: Level,
    pub text: String,
    pub fix: Option<Fix>,
}

/// Ce que le résumé sait des caméras et de la détection (lu dans le hub du serveur).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CameraSummary {
    pub total: usize,
    pub online: usize,
    /// Éteintes dans le mode actuel de la maison (voulu, pas une panne).
    pub off: usize,
    /// Noms des caméras hors ligne (triés).
    pub unreachable: Vec<String>,
    pub detection: DetectionSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectionSummary {
    /// Le moteur de détection est chargé.
    pub available: bool,
    /// Caméras analysées en ce moment.
    pub watched: usize,
    /// Suspendue (Mac surchargé ou trop chaud), avec la raison en clair.
    pub paused: Option<String>,
    /// La suspension vient de la température du Mac : elle dure, et mérite un coup d'œil. Les
    /// autres (analyse qui ne suit plus, trop lente) durent trente secondes et reviennent d'elles-
    /// mêmes : les signaler ferait clignoter l'icône et le menu.
    pub thermal: bool,
}

/// Une suspension pour cause de chaleur, d'après la raison donnée par le serveur (« le Mac est
/// trop chaud »). Le serveur ne transmet que ce texte : c'est le seul repère.
pub fn is_thermal_pause(reason: &str) -> bool {
    reason.contains("chaud")
}

/// La phrase de l'onglet Appareils : combien répondent, et ce que font les autres. Elle dit la
/// même chose que la ligne « Appareils » du résumé (au démarrage, les appareils se connectent).
pub fn devices_sentence(health: &Health) -> String {
    let n = health.online;
    let mut text = format!(
        "{n} appareil{} sur {} répond{} au serveur.",
        plural(n),
        health.total,
        if n > 1 { "ent" } else { "" },
    );
    let connecting = health
        .total
        .saturating_sub(n)
        .saturating_sub(health.unreachable.len());
    if connecting > 0 {
        text.push_str(if connecting > 1 {
            " Les autres se connectent."
        } else {
            " Le dernier se connecte."
        });
    }
    text
}

impl CameraSummary {
    pub fn new(cameras: &[CameraAdmin], detection: &DetectionStatusView) -> Self {
        let paused = detection.paused.then(|| {
            detection
                .pause_reason
                .clone()
                .unwrap_or_else(|| "Mac surchargé".into())
        });
        let mut s = CameraSummary {
            total: cameras.len(),
            detection: DetectionSummary {
                available: detection.available,
                watched: detection.watched.len(),
                thermal: paused.as_deref().is_some_and(is_thermal_pause),
                paused,
            },
            ..CameraSummary::default()
        };
        for camera in cameras {
            match camera.status {
                CameraStatus::Online => s.online += 1,
                CameraStatus::Off => s.off += 1,
                CameraStatus::Unreachable { .. } => s.unreachable.push(camera.name.clone()),
                CameraStatus::Unknown => {}
            }
        }
        s.unreachable.sort();
        s
    }
}

/// Le résumé complet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    /// « Tout va bien », « 1 point à regarder », « Le serveur est à l'arrêt »…
    pub headline: String,
    pub level: Level,
    pub checks: Vec<Check>,
    /// L'action unique du menu quand quelque chose ne va pas.
    pub fix: Option<Fix>,
}

impl Default for Overview {
    fn default() -> Self {
        build(
            &Phase::Starting,
            &Health::default(),
            None,
            &WebAccess::Checking,
            false,
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n > 1 {
        "s"
    } else {
        ""
    }
}

/// « Salon », « Salon et Cuisine », « Salon, Cuisine et 3 autres ».
fn names(list: &[String]) -> String {
    match list {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} et {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} et {} autre{}", rest.len(), plural(rest.len())),
    }
}

fn devices_check(health: &Health) -> Check {
    let missing = health.unreachable.len();
    let (level, text, fix) = if health.total == 0 {
        (
            Level::Info,
            "aucun pour l'instant".to_string(),
            fix("Ajouter un appareil…", Tab::Devices),
        )
    } else if missing == 0 {
        let text = if health.online == health.total {
            if health.total == 1 {
                "1 appareil, joignable".to_string()
            } else {
                format!("{} appareils, tous joignables", health.total)
            }
        } else {
            // Certains sont encore en cours de connexion (au démarrage) : ce n'est pas une panne.
            format!(
                "{} sur {} joignables, les autres se connectent",
                health.online, health.total
            )
        };
        (Level::Ok, text, None)
    } else {
        (
            Level::Warn,
            format!(
                "{} sur {} joignables · injoignable{} : {}",
                health.online,
                health.total,
                plural(missing),
                names(&health.unreachable)
            ),
            fix(
                if missing > 1 {
                    "Voir les appareils injoignables…"
                } else {
                    "Voir l'appareil injoignable…"
                },
                Tab::Devices,
            ),
        )
    };
    Check {
        title: "Appareils",
        level,
        text,
        fix,
    }
}

fn cameras_check(cameras: &CameraSummary) -> Check {
    let title = "Caméras";
    if cameras.total == 0 {
        return Check {
            title,
            level: Level::Info,
            text: "aucune pour l'instant".into(),
            fix: fix("Ajouter une caméra…", Tab::Cameras),
        };
    }
    let down = cameras.unreachable.len();
    let mut level = Level::Ok;
    let mut action = None;
    // « En service » : celles qui répondent vraiment. Une caméra éteinte dans ce mode ou pas
    // encore vérifiée n'en fait pas partie, et le détail le dit.
    let mut text = if cameras.online == cameras.total {
        format!(
            "{} caméra{} en service",
            cameras.total,
            plural(cameras.total)
        )
    } else {
        format!("{} sur {} en service", cameras.online, cameras.total)
    };
    if down > 0 {
        level = Level::Warn;
        action = fix(
            if down > 1 {
                "Voir les caméras hors ligne…"
            } else {
                "Voir la caméra hors ligne…"
            },
            Tab::Cameras,
        );
        text.push_str(&format!(" · hors ligne : {}", names(&cameras.unreachable)));
    }
    if cameras.off > 0 {
        text.push_str(&format!(
            " · {} éteinte{} dans ce mode",
            cameras.off,
            plural(cameras.off)
        ));
    }
    let unknown = cameras
        .total
        .saturating_sub(cameras.online + cameras.off + down);
    if unknown > 0 {
        text.push_str(&format!(
            " · {unknown} pas encore vérifiée{}",
            plural(unknown)
        ));
    }
    let d = &cameras.detection;
    if let Some(reason) = &d.paused {
        if d.thermal {
            level = level.max(Level::Warn);
            action = action.or(fix("Voir la détection…", Tab::Cameras));
            text.push_str(&format!(" · détection suspendue ({reason})"));
        } else {
            // Pause courte (trente secondes), qui se lève seule : dite, sans alerte.
            level = level.max(Level::Info);
            text.push_str(" · détection en pause un instant");
        }
    } else if !d.available {
        // Sans moteur de détection, les caméras se regardent quand même : on le dit, sans alarme.
        text.push_str(" · détection indisponible");
    } else if d.watched > 0 {
        text.push_str(&format!(" · détection active sur {}", d.watched));
    }
    Check {
        title,
        level,
        text,
        fix: action,
    }
}

fn web_check(web: &WebAccess) -> Check {
    let (level, text, action) = match web {
        WebAccess::Checking => (Level::Info, "vérification…", None),
        WebAccess::On { .. } => (Level::Ok, "activé (iPhone, hors de la maison)", None),
        WebAccess::Off { .. } => (Level::Info, "désactivé", None),
        WebAccess::NotInstalled => (Level::Info, "pas configuré", None),
        WebAccess::Disconnected => (
            Level::Warn,
            "Tailscale est déconnecté sur ce Mac",
            fix("Voir l'accès à distance…", Tab::Connect),
        ),
        WebAccess::NeedsAuth { .. } => (
            Level::Warn,
            "une autorisation reste à donner",
            fix("Terminer l'accès à distance…", Tab::Connect),
        ),
        WebAccess::PortBusy => (
            Level::Info,
            "indisponible (Tailscale publie déjà autre chose)",
            None,
        ),
        WebAccess::Funnel => (
            Level::Warn,
            "ce Mac est ouvert à tout Internet par Tailscale",
            fix("Voir l'accès à distance…", Tab::Connect),
        ),
        WebAccess::Unavailable => (Level::Info, "état inconnu (Tailscale ne répond pas)", None),
    };
    Check {
        title: "Accès à distance",
        level,
        text: text.into(),
        fix: action,
    }
}

/// Le résumé pour un état donné. `cameras` : `None` tant que le serveur ne tourne pas.
pub fn build(
    phase: &Phase,
    health: &Health,
    cameras: Option<&CameraSummary>,
    web: &WebAccess,
    desktop_doubt: bool,
) -> Overview {
    let stopped = |headline: &str, level: Level, text: String, action: Option<Fix>| Overview {
        headline: headline.into(),
        level,
        checks: if text.is_empty() {
            Vec::new()
        } else {
            vec![Check {
                title: "Serveur",
                level,
                text,
                fix: None,
            }]
        },
        fix: action,
    };
    match phase {
        Phase::Starting => stopped("Démarrage en cours…", Level::Info, String::new(), None),
        Phase::Stopping => stopped("Arrêt en cours…", Level::Info, String::new(), None),
        Phase::NeedsSetup => stopped(
            "Configuration à terminer",
            Level::Warn,
            "il reste une étape avant de démarrer".into(),
            fix("Terminer la configuration…", Tab::Overview),
        ),
        Phase::Blocked(b) => stopped(
            "Le serveur est à l'arrêt",
            Level::Error,
            b.short().into(),
            fix(
                if *b == Blocker::OtherApp {
                    "Voir pourquoi…"
                } else {
                    "Voir comment le relancer…"
                },
                Tab::Overview,
            ),
        ),
        Phase::Problem(_) => stopped(
            "Le serveur ne tourne pas",
            Level::Error,
            "il s'est arrêté sur une erreur".into(),
            fix("Voir le problème…", Tab::Overview),
        ),
        Phase::Running => {
            let mut checks = vec![devices_check(health)];
            if let Some(cameras) = cameras {
                checks.push(cameras_check(cameras));
            }
            checks.push(web_check(web));
            if desktop_doubt {
                checks.push(Check {
                    title: "Ancienne application",
                    level: Level::Warn,
                    text: "un programme « matter-home-desktop » tourne : si c'est l'ancienne version 0.2, fermez-la".into(),
                    fix: None,
                });
            }
            let waiting = checks.iter().filter(|c| c.level.needs_attention()).count();
            let level = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
            Overview {
                headline: match waiting {
                    0 => "Tout va bien".into(),
                    1 => "1 point à regarder".into(),
                    n => format!("{n} points à regarder"),
                },
                // « Info » (désactivé, pas configuré) n'entame pas le « Tout va bien ».
                level: if waiting == 0 { Level::Ok } else { level },
                fix: checks
                    .iter()
                    .filter(|c| c.level.needs_attention())
                    .find_map(|c| c.fix.clone()),
                checks,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(total: usize, unreachable: &[&str]) -> Health {
        Health {
            total,
            online: total - unreachable.len(),
            unreachable: unreachable.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn cameras(total: usize, down: &[&str]) -> CameraSummary {
        CameraSummary {
            total,
            online: total - down.len(),
            off: 0,
            unreachable: down.iter().map(|s| s.to_string()).collect(),
            detection: DetectionSummary {
                available: true,
                watched: 0,
                paused: None,
                thermal: false,
            },
        }
    }

    fn running(h: &Health, c: &CameraSummary, web: &WebAccess) -> Overview {
        build(&Phase::Running, h, Some(c), web, false)
    }

    fn texts(o: &Overview) -> Vec<String> {
        o.checks
            .iter()
            .map(|c| format!("{} : {}", c.title, c.text))
            .collect()
    }

    #[test]
    fn everything_fine_says_so_and_proposes_nothing() {
        let mut c = cameras(3, &[]);
        c.detection.watched = 2;
        let o = running(
            &health(13, &[]),
            &c,
            &WebAccess::On {
                dns: "mac.exemple.ts.net".into(),
            },
        );
        assert_eq!(o.headline, "Tout va bien");
        assert_eq!(o.level, Level::Ok);
        assert_eq!(o.fix, None);
        assert_eq!(
            texts(&o),
            [
                "Appareils : 13 appareils, tous joignables",
                "Caméras : 3 caméras en service · détection active sur 2",
                "Accès à distance : activé (iPhone, hors de la maison)",
            ]
        );
    }

    #[test]
    fn what_is_simply_not_set_up_is_not_a_problem() {
        // Ni appareil, ni caméra, ni accès à distance : rien à réparer, donc « Tout va bien ».
        let o = running(&health(0, &[]), &cameras(0, &[]), &WebAccess::NotInstalled);
        assert_eq!(o.headline, "Tout va bien");
        assert_eq!(o.level, Level::Ok);
        assert_eq!(o.fix, None);
        assert!(o.checks.iter().all(|c| c.level == Level::Info));
        let o = running(
            &health(2, &[]),
            &cameras(1, &[]),
            &WebAccess::Off { dns: None },
        );
        assert_eq!(o.headline, "Tout va bien");
    }

    #[test]
    fn an_unreachable_device_is_named_and_gets_the_single_action() {
        let o = running(
            &health(13, &["Salon"]),
            &cameras(2, &[]),
            &WebAccess::Off { dns: None },
        );
        assert_eq!(o.headline, "1 point à regarder");
        assert_eq!(o.level, Level::Warn);
        assert_eq!(
            o.checks[0].text,
            "12 sur 13 joignables · injoignable : Salon"
        );
        assert_eq!(
            o.fix,
            Some(Fix {
                label: "Voir l'appareil injoignable…".into(),
                tab: Tab::Devices
            })
        );
    }

    #[test]
    fn several_problems_are_counted_and_the_first_one_leads() {
        let mut c = cameras(3, &["Entrée", "Jardin"]);
        c.detection.paused = Some("le Mac est trop chaud".into());
        c.detection.thermal = true;
        let o = running(
            &health(5, &["A", "B", "C", "D"]),
            &c,
            &WebAccess::NeedsAuth {
                url: "https://login.tailscale.com/f/serve".into(),
            },
        );
        assert_eq!(o.headline, "3 points à regarder");
        assert_eq!(
            o.checks[0].text,
            "1 sur 5 joignables · injoignables : A, B et 2 autres"
        );
        assert_eq!(
            o.checks[1].text,
            "1 sur 3 en service · hors ligne : Entrée et Jardin · détection suspendue (le Mac est trop chaud)"
        );
        assert_eq!(o.fix.as_ref().map(|f| f.tab), Some(Tab::Devices));
        // Le lien d'autorisation n'est jamais écrit dans le résumé.
        assert!(!texts(&o).join(" ").contains("http"));
    }

    #[test]
    fn devices_still_connecting_are_not_reported_as_broken() {
        let h = Health {
            total: 8,
            online: 3,
            unreachable: vec![],
        };
        let o = running(&h, &cameras(0, &[]), &WebAccess::Checking);
        assert_eq!(o.headline, "Tout va bien");
        assert_eq!(
            o.checks[0].text,
            "3 sur 8 joignables, les autres se connectent"
        );
    }

    #[test]
    fn cameras_off_in_this_mode_are_said_without_alarm() {
        let mut c = cameras(3, &[]);
        c.online = 1;
        c.off = 2;
        c.detection.available = false;
        let check = cameras_check(&c);
        assert_eq!(check.level, Level::Ok);
        assert_eq!(
            check.text,
            "1 sur 3 en service · 2 éteintes dans ce mode · détection indisponible"
        );
    }

    #[test]
    fn only_cameras_that_answer_count_as_in_service() {
        // 4 caméras : 1 répond, 1 éteinte dans ce mode, 1 hors ligne, 1 pas encore vérifiée.
        let mut c = cameras(4, &["Garage"]);
        c.online = 1;
        c.off = 1;
        let check = cameras_check(&c);
        assert_eq!(check.level, Level::Warn);
        assert_eq!(
            check.text,
            "1 sur 4 en service · hors ligne : Garage · 1 éteinte dans ce mode · 1 pas encore vérifiée"
        );
        // Toutes répondent : la forme courte.
        assert_eq!(cameras_check(&cameras(2, &[])).text, "2 caméras en service");
    }

    #[test]
    fn a_short_detection_pause_is_said_without_raising_attention() {
        // Les pauses de trente secondes du serveur (« file », « lent ») : jamais d'alerte, donc
        // ni icône « à regarder » ni action dans le menu qui apparaît puis disparaît.
        for reason in [
            "l'analyse ne suit plus le flux",
            "analyse trop lente pour cette machine",
        ] {
            assert!(!is_thermal_pause(reason));
            let mut c = cameras(2, &[]);
            c.detection.paused = Some(reason.into());
            let check = cameras_check(&c);
            assert_eq!(check.level, Level::Info, "{reason}");
            assert_eq!(check.fix, None);
            assert_eq!(
                check.text,
                "2 caméras en service · détection en pause un instant"
            );
            let o = running(&health(3, &[]), &c, &WebAccess::Off { dns: None });
            assert_eq!((o.headline.as_str(), o.level), ("Tout va bien", Level::Ok));
            assert_eq!(o.fix, None);
        }
        // La chaleur dure : elle reste un point à regarder.
        assert!(is_thermal_pause("le Mac est trop chaud"));
        let mut c = cameras(2, &[]);
        c.detection.paused = Some("le Mac est trop chaud".into());
        c.detection.thermal = true;
        let o = running(&health(3, &[]), &c, &WebAccess::Off { dns: None });
        assert_eq!(o.headline, "1 point à regarder");
        assert_eq!(o.fix.map(|f| f.tab), Some(Tab::Cameras));
    }

    #[test]
    fn the_devices_sentence_counts_only_those_that_answer() {
        assert_eq!(
            devices_sentence(&health(8, &[])),
            "8 appareils sur 8 répondent au serveur."
        );
        assert_eq!(
            devices_sentence(&health(1, &[])),
            "1 appareil sur 1 répond au serveur."
        );
        assert_eq!(
            devices_sentence(&health(5, &["Salon"])),
            "4 appareils sur 5 répondent au serveur."
        );
        // Au démarrage : rien n'est injoignable, mais tous ne répondent pas encore.
        let starting = Health {
            total: 8,
            online: 3,
            unreachable: vec![],
        };
        assert_eq!(
            devices_sentence(&starting),
            "3 appareils sur 8 répondent au serveur. Les autres se connectent."
        );
        let almost = Health {
            total: 8,
            online: 6,
            unreachable: vec!["Salon".into()],
        };
        assert_eq!(
            devices_sentence(&almost),
            "6 appareils sur 8 répondent au serveur. Le dernier se connecte."
        );
    }

    #[test]
    fn a_public_funnel_is_flagged() {
        let o = running(&health(1, &[]), &cameras(0, &[]), &WebAccess::Funnel);
        assert_eq!(o.headline, "1 point à regarder");
        assert_eq!(o.fix.map(|f| f.tab), Some(Tab::Connect));
    }

    #[test]
    fn a_stopped_server_has_one_line_and_one_action() {
        let h = Health::default();
        let o = build(
            &Phase::Blocked(Blocker::Service),
            &h,
            None,
            &WebAccess::Checking,
            false,
        );
        assert_eq!(o.headline, "Le serveur est à l'arrêt");
        assert_eq!(o.level, Level::Error);
        assert_eq!(o.checks.len(), 1);
        assert_eq!(o.fix.unwrap().label, "Voir comment le relancer…");
        let o = build(
            &Phase::Problem("détail technique".into()),
            &h,
            None,
            &WebAccess::Checking,
            false,
        );
        assert_eq!(o.headline, "Le serveur ne tourne pas");
        assert!(!texts(&o).join(" ").contains("détail technique"));
        let o = build(&Phase::NeedsSetup, &h, None, &WebAccess::Checking, false);
        assert_eq!(o.fix.unwrap().label, "Terminer la configuration…");
        let o = build(&Phase::Starting, &h, None, &WebAccess::Checking, false);
        assert_eq!(
            (o.headline.as_str(), o.fix, o.checks.len()),
            ("Démarrage en cours…", None, 0)
        );
    }

    #[test]
    fn an_uncertain_old_desktop_app_is_a_point_to_look_at() {
        let o = build(
            &Phase::Running,
            &health(1, &[]),
            Some(&cameras(0, &[])),
            &WebAccess::Checking,
            true,
        );
        assert_eq!(o.headline, "1 point à regarder");
        assert!(texts(&o).iter().any(|t| t.contains("matter-home-desktop")));
    }

    #[test]
    fn tab_keys_round_trip() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_key(tab.key()), Some(tab));
        }
        assert_eq!(Tab::from_key("autre"), None);
    }
}
