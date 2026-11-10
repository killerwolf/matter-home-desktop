//! Ce que toutes les vues de caméras (plan, page, lecteur) disent et colorent de la même façon :
//! la phrase de chaque motif d'injoignabilité et les couleurs des états.

use eframe::egui::Color32;
use protocol::wire::CameraProblem;

pub const ONLINE: Color32 = Color32::from_rgb(40, 160, 70);
pub const OFFLINE: Color32 = Color32::from_rgb(210, 60, 50);
pub const NEUTRAL: Color32 = Color32::from_rgb(140, 140, 140);

/// Pourquoi une caméra est injoignable, en un morceau de phrase (sans adresse ni détail technique).
pub fn problem_text(problem: CameraProblem) -> &'static str {
    match problem {
        CameraProblem::Unreachable => "ne répond pas (éteinte, hors réseau ou RTSP désactivé)",
        CameraProblem::Unauthorized => "identifiant ou mot de passe refusé",
        CameraProblem::NotFound => "adresse du flux introuvable",
        CameraProblem::Protocol => "réponse inattendue (pas une caméra RTSP ?)",
        CameraProblem::Failed => "échec de la connexion",
    }
}

/// « Hors ligne depuis 14:02 » (aujourd'hui), « … depuis hier 22:15 », ou la date si c'est plus
/// ancien (« depuis le 03/11 à 14:02 », avec l'année si ce n'est pas l'année en cours).
/// `since` : secondes Unix ; `now` donne aussi le fuseau. Une heure dans le futur (horloges
/// décalées) se lit comme « maintenant ».
pub fn offline_since<Tz: chrono::TimeZone>(since: u64, now: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use chrono::{Datelike, Duration};
    let at = i64::try_from(since)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|t| t.with_timezone(&now.timezone()))
        .filter(|t| t <= now)
        .unwrap_or_else(|| now.clone());
    let (day, today) = (at.date_naive(), now.date_naive());
    let clock = at.format("%H:%M");
    if day == today {
        format!("Hors ligne depuis {clock}")
    } else if day == today - Duration::days(1) {
        format!("Hors ligne depuis hier {clock}")
    } else if day.year() == today.year() {
        format!("Hors ligne depuis le {} à {clock}", at.format("%d/%m"))
    } else {
        format!("Hors ligne depuis le {} à {clock}", at.format("%d/%m/%Y"))
    }
}

/// [`offline_since`] à l'heure d'aujourd'hui, dans le fuseau de l'ordinateur.
pub fn offline_since_now(since: u64) -> String {
    offline_since(since, &chrono::Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn the_offline_time_reads_naturally() {
        let now = at(2026, 11, 10, 16, 30);
        let since = |t: chrono::DateTime<Utc>| offline_since(t.timestamp() as u64, &now);
        assert_eq!(since(at(2026, 11, 10, 14, 2)), "Hors ligne depuis 14:02");
        assert_eq!(
            since(at(2026, 11, 9, 22, 15)),
            "Hors ligne depuis hier 22:15"
        );
        assert_eq!(
            since(at(2026, 11, 3, 8, 5)),
            "Hors ligne depuis le 03/11 à 08:05"
        );
        assert_eq!(
            since(at(2025, 12, 31, 23, 59)),
            "Hors ligne depuis le 31/12/2025 à 23:59"
        );
        // Une heure dans le futur : « maintenant », jamais une date absurde.
        assert_eq!(since(at(2026, 11, 10, 18, 0)), "Hors ligne depuis 16:30");
    }
}
