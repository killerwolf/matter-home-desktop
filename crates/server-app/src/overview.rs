//! « La maison va bien ? » : le résumé que montrent le menu de la barre de menus, l'icône et
//! l'onglet « Vue d'ensemble » de la fenêtre. Un seul calcul pour les trois, sans interface, donc
//! testable : une phrase de titre, une ligne par sujet (appareils, caméras, accès à distance) et,
//! quand quelque chose ne va pas, **une** action à proposer.
//!
//! Les mots sont ceux de l'utilisateur : pas de « Serve », de « tailnet » ni de « flux » ici.

use ostal_server::cameras::CameraAdmin;
use protocol::status::{self, Point};
use protocol::wire::{CameraStatus, DetectionStatusView};

use crate::status::{clock, Health, Phase};
use crate::tailscale::WebAccess;
use ostal_server::wifi::{Link, Status as WifiStatus};

/// Les onglets de la fenêtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Devices,
    Cameras,
    Connect,
    Journal,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Overview,
        Tab::Devices,
        Tab::Cameras,
        Tab::Connect,
        Tab::Journal,
        Tab::Settings,
    ];

    /// Le nom de l'onglet, tel qu'il s'affiche.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Vue d'ensemble",
            Tab::Devices => "Appareils",
            Tab::Cameras => "Caméras",
            Tab::Connect => "Connexion",
            Tab::Journal => "Activité",
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
            Tab::Journal => "journal",
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
    /// La version courte du menu (« 10 sur 10 joignables ») ; la même que `text` quand elle
    /// n'a pas de forme plus courte.
    pub menu_text: String,
    pub fix: Option<Fix>,
}

/// Une phrase : majuscule au début, un point à la fin si rien ne la termine.
pub fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out: String = first.to_uppercase().chain(chars).collect();
    if !out.ends_with(['.', '…', '!', '?']) {
        out.push('.');
    }
    out
}

/// Une phrase du résumé sans son point final : « Lampe d'Adam est injoignable ».
fn clause(phrase: String) -> String {
    phrase.trim_end_matches('.').to_string()
}

/// Ce que le résumé sait des caméras et de la détection (lu dans le hub du serveur).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CameraSummary {
    pub total: usize,
    pub online: usize,
    /// Éteintes dans le mode actuel de la maison (voulu, pas une panne).
    pub off: usize,
    /// Noms des caméras injoignables (triés).
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

/// La phrase de l'onglet Appareils : combien sont joignables, et ce que font les autres. Elle dit
/// la même chose que la ligne « Appareils » du résumé (au démarrage, les appareils se
/// connectent).
pub fn devices_sentence(health: &Health) -> String {
    let n = health.online;
    if n == 0 && health.total > 0 {
        let mut text = format!("Aucun appareil sur {} n'est joignable.", health.total);
        if status::dropped_together(health.unreachable.len(), health.total) {
            text.push_str(
                " Le Mac n'est sans doute plus sur le réseau de la maison : vérifiez son Wi-Fi.",
            );
        }
        return text;
    }
    let mut text = format!(
        "{n} appareil{} sur {} {}.",
        plural(n),
        health.total,
        if n > 1 {
            "sont joignables"
        } else {
            "est joignable"
        },
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
    /// « La maison va bien. », « Une chose à regarder », « Le serveur est à l'arrêt »…
    pub headline: String,
    pub level: Level,
    pub checks: Vec<Check>,
    /// La phrase qui dit ce qui ne va pas (celle de `protocol::status::detail`), ou la raison de
    /// l'arrêt. `None` quand rien n'est à regarder.
    pub detail: Option<String>,
    /// Le titre de la ligne dont vient `detail` : le menu ne la répète pas.
    pub detail_from: Option<&'static str>,
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

fn devices_check(health: &Health) -> Check {
    let missing = health.unreachable.len();
    let mut menu_text = None;
    let (level, text, fix) = if health.total == 0 {
        (
            Level::Info,
            "aucun pour l'instant".to_string(),
            fix("Ajouter un appareil…", Tab::Devices),
        )
    } else if missing == 0 {
        menu_text = Some(format!(
            "{} sur {} joignable{}",
            health.online,
            health.total,
            plural(health.online)
        ));
        let text = if health.online == health.total {
            clause(status::all_reachable(health.total))
        } else {
            // Certains sont encore en cours de connexion (au démarrage) : ce n'est pas une panne.
            format!(
                "{} sur {} joignables, les autres se connectent",
                health.online, health.total
            )
        };
        (Level::Ok, text, None)
    } else {
        let together = status::dropped_together(missing, health.total);
        let mut phrase = if together {
            // La phrase complète (cause et conseil) est dans le détail du résumé et sous la ligne.
            if missing == health.total {
                "tous ont décroché en même temps".to_string()
            } else {
                "la plupart ont décroché en même temps".to_string()
            }
        } else {
            clause(Point::Devices(health.unreachable.clone()).phrase())
        };
        // « depuis 18:02 » : seulement pour un appareil, et si le hub sait quand il a répondu.
        if let [only] = health.unreachable_details.as_slice() {
            if let Some(at) = only.last_seen.and_then(clock) {
                phrase.push_str(&format!(" depuis {at}"));
            }
        }
        (
            Level::Warn,
            format!(
                "{} sur {} joignables · {phrase}",
                health.online, health.total
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
        menu_text: menu_text.unwrap_or_else(|| text.clone()),
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
            menu_text: "aucune pour l'instant".into(),
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
                "Voir les caméras injoignables…"
            } else {
                "Voir la caméra injoignable…"
            },
            Tab::Cameras,
        );
        // En milieu de ligne : « la caméra « Garage » est injoignable ».
        let phrase = clause(Point::Cameras(cameras.unreachable.clone()).phrase());
        let mut letters = phrase.chars();
        let lower: String = letters
            .next()
            .map(|c| c.to_lowercase().chain(letters).collect())
            .unwrap_or_default();
        text.push_str(&format!(" · {lower}"));
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
        menu_text: text.clone(),
        text,
        fix: action,
    }
}

fn web_check(web: &WebAccess) -> Check {
    let (level, text, action) = match web {
        WebAccess::Checking => (Level::Info, "vérification…", None),
        WebAccess::On { .. } => (Level::Ok, "activé", None),
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
        menu_text: text.into(),
        fix: action,
    }
}

/// La ligne « Présence » : la box de la maison (Bbox) qui dit qui est là. Rien d'alarmant tant
/// qu'elle n'est pas reliée (la présence marche aussi avec les seuls raccourcis) ; un souci de
/// liaison fige la présence, et le dit.
pub fn presence_check(wifi: &WifiStatus) -> Check {
    let (level, text, action) = match (wifi.linked, wifi.link) {
        (false, _) | (_, Link::None) => (
            Level::Info,
            "aucune box reliée (facultatif)".to_string(),
            fix("Relier la box…", Tab::Settings),
        ),
        (true, Link::Ok) => (
            Level::Ok,
            match wifi.active_hosts {
                0 => "box reliée · aucun appareil vu sur le Wi-Fi".to_string(),
                1 => "box reliée · 1 appareil vu sur le Wi-Fi".to_string(),
                n => format!("box reliée · {n} appareils vus sur le Wi-Fi"),
            },
            None,
        ),
        (true, Link::Unreachable) => (
            Level::Warn,
            "la box ne répond pas : la présence est figée, le mode de la maison ne change pas"
                .to_string(),
            fix("Voir la présence…", Tab::Settings),
        ),
        (true, Link::Refused) => (
            Level::Warn,
            "la box a refusé le mot de passe : la présence est figée, le mode de la maison ne change pas"
                .to_string(),
            fix("Donner le nouveau mot de passe…", Tab::Settings),
        ),
    };
    Check {
        title: "Présence",
        level,
        menu_text: text.clone(),
        text,
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
    build_with_presence(phase, health, cameras, web, desktop_doubt, None)
}

/// Comme [`build`], avec la liaison à la box de la maison (`None` : la ligne « Présence »
/// n'apparaît pas).
pub fn build_with_presence(
    phase: &Phase,
    health: &Health,
    cameras: Option<&CameraSummary>,
    web: &WebAccess,
    desktop_doubt: bool,
    presence: Option<&WifiStatus>,
) -> Overview {
    let stopped = |headline: &str,
                   level: Level,
                   text: String,
                   detail: Option<String>,
                   action: Option<Fix>| Overview {
        headline: headline.into(),
        level,
        checks: if text.is_empty() {
            Vec::new()
        } else {
            vec![Check {
                title: "Serveur",
                level,
                menu_text: text.clone(),
                text,
                fix: None,
            }]
        },
        detail_from: detail.as_ref().map(|_| "Serveur"),
        detail,
        fix: action,
    };
    match phase {
        Phase::Starting => stopped(
            "Démarrage en cours…",
            Level::Info,
            String::new(),
            None,
            None,
        ),
        Phase::Stopping => stopped("Arrêt en cours…", Level::Info, String::new(), None, None),
        Phase::NeedsSetup => stopped(
            "Configuration à terminer",
            Level::Warn,
            "il reste une étape avant de démarrer".into(),
            Some("Il reste une étape avant de démarrer.".into()),
            fix("Terminer la configuration…", Tab::Overview),
        ),
        Phase::Blocked(b) => stopped(
            "Le serveur est à l'arrêt",
            Level::Error,
            b.short().trim_end_matches('.').to_string(),
            Some(b.short().into()),
            fix("Voir pourquoi…", Tab::Overview),
        ),
        Phase::Problem(_) => stopped(
            "Le serveur ne tourne pas",
            Level::Error,
            "il s'est arrêté après une erreur".into(),
            Some("Il s'est arrêté après une erreur.".into()),
            fix("Voir le problème…", Tab::Overview),
        ),
        Phase::Running => {
            let mut checks = vec![devices_check(health)];
            if let Some(cameras) = cameras {
                checks.push(cameras_check(cameras));
            }
            checks.push(web_check(web));
            if let Some(wifi) = presence {
                checks.push(presence_check(wifi));
            }
            if desktop_doubt {
                let text = "un programme « ostal » tourne : si c'est l'ancienne application de bureau, fermez-la";
                checks.push(Check {
                    title: "Ancienne application",
                    level: Level::Warn,
                    text: text.into(),
                    menu_text: text.into(),
                    fix: None,
                });
            }
            // Une chose à regarder par catégorie, jamais une par appareil.
            let points: Vec<(&'static str, Point)> = checks
                .iter()
                .filter(|c| c.level.needs_attention())
                .map(|c| {
                    let point = match c.title {
                        "Appareils" if !health.unreachable.is_empty() => {
                            status::devices_point(health.unreachable.clone(), health.total)
                        }
                        "Caméras" => match cameras {
                            Some(cam) if !cam.unreachable.is_empty() => {
                                Point::Cameras(cam.unreachable.clone())
                            }
                            _ => Point::Other(sentence(&c.text)),
                        },
                        _ => Point::Other(sentence(&c.text)),
                    };
                    (c.title, point)
                })
                .collect();
            let only: Vec<Point> = points.iter().map(|(_, p)| p.clone()).collect();
            let waiting = status::count(&only);
            // La ligne que `status::detail` met en avant : on retrouve sa catégorie.
            let detail = status::detail(&only);
            let detail_from = detail.as_ref().and_then(|d| {
                points
                    .iter()
                    .find(|(_, p)| &p.phrase() == d)
                    .map(|(title, _)| *title)
            });
            let level = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
            Overview {
                headline: status::headline(waiting),
                // « Info » (désactivé, pas configuré) n'entame pas le « La maison va bien. ».
                level: if waiting == 0 { Level::Ok } else { level },
                fix: checks
                    .iter()
                    .filter(|c| c.level.needs_attention())
                    .find_map(|c| c.fix.clone()),
                detail,
                detail_from,
                checks,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::Blocker;

    fn health(total: usize, unreachable: &[&str]) -> Health {
        Health {
            total,
            online: total - unreachable.len(),
            unreachable: unreachable.iter().map(|s| s.to_string()).collect(),
            ..Health::default()
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
        assert_eq!(o.headline, "La maison va bien.");
        assert_eq!(o.level, Level::Ok);
        assert_eq!(o.fix, None);
        assert_eq!(o.detail, None);
        assert_eq!(
            texts(&o),
            [
                "Appareils : 13 appareils, tous joignables",
                "Caméras : 3 caméras en service · détection active sur 2",
                "Accès à distance : activé",
            ]
        );
    }

    #[test]
    fn what_is_simply_not_set_up_is_not_a_problem() {
        // Ni appareil, ni caméra, ni accès à distance : rien à réparer, donc « La maison va bien. ».
        let o = running(&health(0, &[]), &cameras(0, &[]), &WebAccess::NotInstalled);
        assert_eq!(o.headline, "La maison va bien.");
        assert_eq!(o.level, Level::Ok);
        assert_eq!(o.fix, None);
        assert!(o.checks.iter().all(|c| c.level == Level::Info));
        let o = running(
            &health(2, &[]),
            &cameras(1, &[]),
            &WebAccess::Off { dns: None },
        );
        assert_eq!(o.headline, "La maison va bien.");
    }

    #[test]
    fn an_unreachable_device_is_named_and_gets_the_single_action() {
        let o = running(
            &health(13, &["Salon"]),
            &cameras(2, &[]),
            &WebAccess::Off { dns: None },
        );
        assert_eq!(o.headline, "Une chose à regarder");
        assert_eq!(o.level, Level::Warn);
        assert_eq!(
            o.checks[0].text,
            "12 sur 13 joignables · « Salon » est injoignable"
        );
        assert_eq!(o.detail.as_deref(), Some("« Salon » est injoignable."));
        assert_eq!(o.detail_from, Some("Appareils"));
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
            &health(6, &["A", "B", "C", "D"]),
            &c,
            &WebAccess::NeedsAuth {
                url: "https://login.tailscale.com/f/serve".into(),
            },
        );
        assert_eq!(o.headline, "3 choses à regarder");
        assert_eq!(
            o.checks[0].text,
            "2 sur 6 joignables · « A », « B » et 2 autres sont injoignables"
        );
        assert_eq!(
            o.checks[1].text,
            "1 sur 3 en service · les caméras « Entrée » et « Jardin » sont injoignables · détection suspendue (le Mac est trop chaud)"
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
            ..Health::default()
        };
        let o = running(&h, &cameras(0, &[]), &WebAccess::Checking);
        assert_eq!(o.headline, "La maison va bien.");
        assert_eq!(
            o.checks[0].text,
            "3 sur 8 joignables, les autres se connectent"
        );
        assert_eq!(o.checks[0].menu_text, "3 sur 8 joignables");
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
        // 4 caméras : 1 répond, 1 éteinte dans ce mode, 1 injoignable, 1 pas encore vérifiée.
        let mut c = cameras(4, &["Garage"]);
        c.online = 1;
        c.off = 1;
        let check = cameras_check(&c);
        assert_eq!(check.level, Level::Warn);
        assert_eq!(
            check.text,
            "1 sur 4 en service · la caméra « Garage » est injoignable · 1 éteinte dans ce mode · 1 pas encore vérifiée"
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
            assert_eq!(
                (o.headline.as_str(), o.level),
                ("La maison va bien.", Level::Ok)
            );
            assert_eq!(o.fix, None);
        }
        // La chaleur dure : elle reste une chose à regarder.
        assert!(is_thermal_pause("le Mac est trop chaud"));
        let mut c = cameras(2, &[]);
        c.detection.paused = Some("le Mac est trop chaud".into());
        c.detection.thermal = true;
        let o = running(&health(3, &[]), &c, &WebAccess::Off { dns: None });
        assert_eq!(o.headline, "Une chose à regarder");
        assert_eq!(o.fix.map(|f| f.tab), Some(Tab::Cameras));
        // Pas de caméra injoignable : la phrase vient de la ligne elle-même.
        assert_eq!(o.detail_from, Some("Caméras"));
    }

    #[test]
    fn the_devices_sentence_counts_only_those_that_answer() {
        assert_eq!(
            devices_sentence(&health(8, &[])),
            "8 appareils sur 8 sont joignables."
        );
        assert_eq!(
            devices_sentence(&health(1, &[])),
            "1 appareil sur 1 est joignable."
        );
        let none = Health {
            total: 3,
            online: 0,
            unreachable: vec!["Salon".into()],
            ..Health::default()
        };
        assert_eq!(
            devices_sentence(&none),
            "Aucun appareil sur 3 n'est joignable."
        );
        assert_eq!(
            devices_sentence(&health(5, &["Salon"])),
            "4 appareils sur 5 sont joignables."
        );
        // Au démarrage : rien n'est injoignable, mais tous ne répondent pas encore.
        let starting = Health {
            total: 8,
            online: 3,
            unreachable: vec![],
            ..Health::default()
        };
        assert_eq!(
            devices_sentence(&starting),
            "3 appareils sur 8 sont joignables. Les autres se connectent."
        );
        let almost = Health {
            total: 8,
            online: 6,
            unreachable: vec!["Salon".into()],
            ..Health::default()
        };
        assert_eq!(
            devices_sentence(&almost),
            "6 appareils sur 8 sont joignables. Le dernier se connecte."
        );
    }

    #[test]
    fn a_public_funnel_is_flagged() {
        let o = running(&health(1, &[]), &cameras(0, &[]), &WebAccess::Funnel);
        assert_eq!(o.headline, "Une chose à regarder");
        assert_eq!(o.fix.map(|f| f.tab), Some(Tab::Connect));
        assert_eq!(
            o.detail.as_deref(),
            Some("Ce Mac est ouvert à tout Internet par Tailscale.")
        );
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
        assert_eq!(o.fix.unwrap().label, "Voir pourquoi…");
        assert_eq!(
            o.detail.as_deref(),
            Some("Le service installé utilise encore les données.")
        );
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
    fn an_uncertain_old_desktop_app_is_something_to_look_at() {
        let o = build(
            &Phase::Running,
            &health(1, &[]),
            Some(&cameras(0, &[])),
            &WebAccess::Checking,
            true,
        );
        assert_eq!(o.headline, "Une chose à regarder");
        assert!(texts(&o).iter().any(|t| t.contains("ostal")));
        // Aucune version dans le texte : l'ancienne application, c'est tout.
        assert!(!texts(&o).join(" ").contains("0.2"));
    }

    #[test]
    fn tab_keys_round_trip() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_key(tab.key()), Some(tab));
        }
        assert_eq!(Tab::from_key("autre"), None);
    }

    #[test]
    fn the_journal_tab_is_called_activity() {
        assert_eq!(Tab::Journal.label(), "Activité");
        // Le mot de la ligne de commande ne change pas.
        assert_eq!(Tab::Journal.key(), "journal");
    }

    #[test]
    fn everything_dropped_at_once_points_at_the_wifi_not_the_power() {
        let h = health(5, &["A", "B", "C", "D", "E"]);
        let o = running(&h, &cameras(0, &[]), &WebAccess::Checking);
        assert_eq!(o.headline, "Une chose à regarder");
        assert!(o
            .detail
            .as_deref()
            .unwrap()
            .starts_with("Tous les appareils ont décroché en même temps"));
        assert_eq!(
            o.checks[0].text,
            "0 sur 5 joignables · tous ont décroché en même temps"
        );
        assert_eq!(
            devices_sentence(&h),
            "Aucun appareil sur 5 n'est joignable. Le Mac n'est sans doute plus sur le réseau de la maison : vérifiez son Wi-Fi."
        );
    }

    #[test]
    fn one_or_two_devices_down_keep_their_names() {
        let h = health(5, &["A", "B"]);
        let o = running(&h, &cameras(0, &[]), &WebAccess::Checking);
        assert_eq!(
            o.detail.as_deref(),
            Some("« A » et « B » sont injoignables.")
        );
        // Deux appareils sur deux : trop peu pour accuser le réseau.
        assert_eq!(
            devices_sentence(&health(2, &["A", "B"])),
            "Aucun appareil sur 2 n'est joignable."
        );
    }

    fn wifi(linked: bool, link: Link, active: usize) -> WifiStatus {
        WifiStatus {
            linked,
            connector: linked.then_some("Bbox"),
            link,
            auto_mode: true,
            active_hosts: active,
            last_ok: None,
            away_delay_min: 10,
        }
    }

    fn with_presence(w: &WifiStatus) -> Overview {
        build_with_presence(
            &Phase::Running,
            &health(3, &[]),
            Some(&cameras(0, &[])),
            &WebAccess::Off { dns: None },
            false,
            Some(w),
        )
    }

    #[test]
    fn the_box_shows_in_the_overview() {
        // Sans box : une ligne d'information, la maison « va bien ».
        let o = with_presence(&wifi(false, Link::None, 0));
        let line = o.checks.iter().find(|c| c.title == "Présence").unwrap();
        assert_eq!(line.level, Level::Info);
        assert_eq!(o.headline, "La maison va bien.");
        // Reliée.
        let o = with_presence(&wifi(true, Link::Ok, 4));
        let line = o.checks.iter().find(|c| c.title == "Présence").unwrap();
        assert_eq!(line.text, "box reliée · 4 appareils vus sur le Wi-Fi");
        assert_eq!(o.headline, "La maison va bien.");
        // Injoignable : à regarder, mode figé, renvoi vers Réglages.
        let o = with_presence(&wifi(true, Link::Unreachable, 0));
        assert_eq!(o.headline, "Une chose à regarder");
        assert_eq!(
            o.detail.as_deref(),
            Some(
                "La box ne répond pas : la présence est figée, le mode de la maison ne change pas."
            )
        );
        assert_eq!(o.fix.as_ref().map(|f| f.tab), Some(Tab::Settings));
        // Mot de passe refusé.
        let o = with_presence(&wifi(true, Link::Refused, 0));
        assert!(o.detail.unwrap().contains("refusé le mot de passe"));
    }

    #[test]
    fn without_a_wifi_status_there_is_no_presence_line() {
        let o = running(&health(3, &[]), &cameras(0, &[]), &WebAccess::Checking);
        assert!(o.checks.iter().all(|c| c.title != "Présence"));
    }
}
