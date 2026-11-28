//! La présence par le Wi-Fi de la box vue de la fenêtre d'Ostal Server (Réglages › Présence et
//! fiche d'un compte) : les phrases et la mise en tableau, sans interface, pour que ce soit
//! testable. Le dessin est dans `presence_ui.rs`. Ni le mot de passe de la box, ni une adresse MAC
//! ou IP n'arrivent jusqu'ici : seulement des noms.

use ostal_server::hub::Hub;
use ostal_server::presence::{Declared, Whereabouts};
use ostal_server::security::account::User;
use ostal_server::wifi::{Link, Status};

/// L'état de la liaison, en une phrase, et s'il est bon.
pub fn link_line(status: &Status) -> (bool, String) {
    match (status.linked, status.link) {
        (false, _) | (_, Link::None) => (
            false,
            "Aucune box reliée : reliez-la ci-dessous".into(),
        ),
        (true, Link::Ok) => (
            true,
            format!(
                "Reliée · {} sur le Wi-Fi",
                match status.active_hosts {
                    0 => "aucun appareil vu".to_string(),
                    1 => "1 appareil vu".to_string(),
                    n => format!("{n} appareils vus"),
                }
            ),
        ),
        (true, Link::Refused) => (
            false,
            "Mot de passe refusé : la box n'est plus interrogée. Donnez-lui le nouveau mot de passe.".into(),
        ),
        (true, Link::Unreachable) => (
            false,
            "La box ne répond pas : la présence est figée, le mode ne change pas.".into(),
        ),
    }
}

/// « HH:MM » en heure locale.
pub fn clock_text(at: f64) -> String {
    use chrono::{Local, TimeZone, Timelike};
    Local.timestamp_opt(at as i64, 0).single().map_or_else(
        || "--:--".into(),
        |d| format!("{:02}:{:02}", d.hour(), d.minute()),
    )
}

/// Où est la personne, d'après ce qu'on sait. Aucun genre n'est supposé.
pub fn presence_text(declared: Option<&Declared>) -> String {
    match declared {
        Some(d) if d.state == Whereabouts::Home => "à la maison".into(),
        Some(d) => format!("pas là depuis {}", clock_text(d.since)),
        None => "pas encore vu".into(),
    }
}

/// Une ligne du tableau « Téléphones reliés ».
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub user_id: String,
    pub name: String,
    /// Le nom du téléphone attribué.
    pub phone: Option<String>,
    pub state: String,
    /// Le téléphone est vu sur le Wi-Fi à l'instant.
    pub seen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub status: Status,
    pub rows: Vec<Row>,
    /// Les appareils vus sur le Wi-Fi, sans propriétaire (noms seulement).
    pub free: Vec<String>,
    /// « Hamdi et Sam sont à la maison. »
    pub who_is_home: String,
}

/// Lit l'état du serveur qui tourne. Les comptes désactivés ne sont pas listés.
pub fn snapshot(hub: &Hub, users: &[User]) -> Snapshot {
    let declared = hub.declared_presence();
    let wifi = hub.wifi();
    let rows: Vec<Row> = users
        .iter()
        .filter(|u| u.active)
        .map(|u| {
            let phone = wifi.phone_of(&u.id);
            Row {
                user_id: u.id.clone(),
                name: u.display_name.clone(),
                state: presence_text(declared.iter().find(|d| d.user_id == u.id)),
                seen: phone.as_deref().is_some_and(|p| wifi.is_seen(p)),
                phone,
            }
        })
        .collect();
    let home: Vec<&str> = users
        .iter()
        .filter(|u| u.active)
        .filter(|u| {
            declared
                .iter()
                .any(|d| d.user_id == u.id && d.state == Whereabouts::Home)
        })
        .map(|u| u.display_name.as_str())
        .collect();
    let who_is_home = match home.as_slice() {
        [] => "Personne n'est à la maison.".to_string(),
        [one] => format!("{one} est à la maison."),
        [init @ .., last] => format!("{} et {last} sont à la maison.", init.join(", ")),
    };
    Snapshot {
        status: wifi.status(),
        rows,
        free: wifi.free_hosts(false),
        who_is_home,
    }
}

/// « Seul le nom du téléphone est gardé, jamais son adresse. »
pub const PRIVACY_NOTE: &str =
    "Seuls les noms s'affichent. Chacun choisit dans l'app web ; ici, vous pouvez corriger.";

#[cfg(test)]
mod tests {
    use super::*;
    use ostal_server::presence::Source;

    fn status(linked: bool, link: Link, active: usize) -> Status {
        Status {
            linked,
            connector: linked.then_some("Bbox"),
            link,
            auto_mode: false,
            active_hosts: active,
            last_ok: None,
            away_delay_min: 10,
        }
    }

    #[test]
    fn the_link_is_told_in_one_sentence() {
        assert_eq!(
            link_line(&status(false, Link::None, 0)),
            (false, "Aucune box reliée : reliez-la ci-dessous".into())
        );
        assert_eq!(
            link_line(&status(true, Link::Ok, 6)),
            (true, "Reliée · 6 appareils vus sur le Wi-Fi".into())
        );
        assert_eq!(
            link_line(&status(true, Link::Ok, 1)).1,
            "Reliée · 1 appareil vu sur le Wi-Fi"
        );
        let refused = link_line(&status(true, Link::Refused, 0));
        assert!(!refused.0 && refused.1.contains("refusé"));
        let down = link_line(&status(true, Link::Unreachable, 0));
        assert!(!down.0 && down.1.contains("figée"));
    }

    #[test]
    fn presence_is_told_without_assuming_a_gender() {
        let at = |state, since| Declared {
            user_id: "u".into(),
            state,
            since,
            source: Source::Wifi,
            said: since,
        };
        assert_eq!(
            presence_text(Some(&at(Whereabouts::Home, 1.0))),
            "à la maison"
        );
        let away = presence_text(Some(&at(Whereabouts::Away, 1_795_000_000.0)));
        assert!(away.starts_with("pas là depuis "), "{away}");
        assert!(!away.contains("partie"));
        assert_eq!(presence_text(None), "pas encore vu");
        assert_eq!(clock_text(f64::MAX).len(), 5);
    }
}
