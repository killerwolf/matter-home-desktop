//! L'onglet « Activité » d'Ostal Server (maquette Serveur-Journal) : qui a fait quoi, depuis quel
//! téléphone ou ordinateur. Ici, tout ce qui se décide sans interface : les périodes, le groupement
//! par jour (« Aujourd'hui · samedi 22 novembre »), les lignes. Les phrases viennent de
//! `ostal_server::audit::describe`, les mêmes que celles de `ostal-server audit`. Le dessin est
//! dans `journal_ui.rs`.

use std::path::Path;

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use ostal_server::audit::{self, AuditLog, Entry, Filter, Tone};

/// Lignes montrées au plus : la fenêtre n'est pas un outil d'export, la ligne de commande lit tout.
pub const MAX_ROWS: usize = 500;

/// La période du filtre, dans l'ordre de la maquette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    TodayAndYesterday,
    Today,
    Week,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::TodayAndYesterday, Period::Today, Period::Week];

    pub fn label(self) -> &'static str {
        match self {
            Period::TodayAndYesterday => "Aujourd'hui et hier",
            Period::Today => "Aujourd'hui",
            Period::Week => "7 derniers jours",
        }
    }

    /// Les jours remontés en plus d'aujourd'hui.
    fn days_back(self) -> i64 {
        match self {
            Period::Today => 0,
            Period::TodayAndYesterday => 1,
            Period::Week => 6,
        }
    }

    /// Le début de la période : minuit, heure locale, du premier jour.
    pub fn since(self, today: NaiveDate) -> f64 {
        let first = today - Duration::days(self.days_back());
        first
            .and_hms_opt(0, 0, 0)
            .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
            .map_or(0.0, |d| d.timestamp() as f64)
    }
}

/// Une ligne de la liste : Heure, Qui, Depuis, Quoi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub time: String,
    pub who: String,
    pub from: String,
    pub what: String,
    pub tone: Tone,
}

/// Les lignes d'un même jour, sous son titre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub title: String,
    pub rows: Vec<Row>,
}

const DAYS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// « samedi 22 novembre » ; « Aujourd'hui · samedi 22 novembre » ; « Hier · vendredi 21 novembre ».
pub fn day_title(date: NaiveDate, today: NaiveDate) -> String {
    let day = format!(
        "{} {} {}",
        DAYS[date.weekday().num_days_from_monday() as usize],
        date.day(),
        MONTHS[date.month0() as usize]
    );
    match (today - date).num_days() {
        0 => format!("Aujourd'hui · {day}"),
        1 => format!("Hier · {day}"),
        _ => day,
    }
}

/// Les entrées (la plus récente d'abord) groupées par jour local.
pub fn days_of(entries: &[Entry], today: NaiveDate) -> Vec<Day> {
    let mut days: Vec<(NaiveDate, Day)> = Vec::new();
    for entry in entries {
        let Some(date) = Local
            .timestamp_opt(entry.at.floor() as i64, 0)
            .single()
            .map(|d| d.date_naive())
        else {
            continue;
        };
        let (tone, what) = audit::describe(entry);
        let row = Row {
            time: audit::clock_label(entry.at),
            who: audit::who_label(entry),
            from: audit::from_label(entry),
            what,
            tone,
        };
        match days.last_mut() {
            Some((d, day)) if *d == date => day.rows.push(row),
            _ => days.push((
                date,
                Day {
                    title: day_title(date, today),
                    rows: vec![row],
                },
            )),
        }
    }
    days.into_iter().map(|(_, day)| day).collect()
}

/// Les entrées de la période pour cette personne (`None` : tout le monde), au plus [`MAX_ROWS`].
pub fn load(log: &AuditLog, user: Option<&str>, period: Period, now: f64) -> Vec<Entry> {
    let today = Local
        .timestamp_opt(now as i64, 0)
        .single()
        .map_or_else(|| Local::now().date_naive(), |d| d.date_naive());
    log.read_at(
        &Filter {
            user: user.map(str::to_string),
            since: Some(period.since(today)),
            limit: Some(MAX_ROWS),
            ..Filter::default()
        },
        now,
    )
}

/// Le journal du dossier de données du serveur.
pub fn open(data_dir: &Path) -> AuditLog {
    AuditLog::reader(&data_dir.join(ostal_server::persistence::SERVER_DIR))
}

/// Aujourd'hui, en heure locale.
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// « 42 lignes », « 1 ligne », « Aucune ligne » : le décompte sous la liste.
pub fn count_label(shown: usize) -> String {
    match shown {
        0 => "Aucune entrée".to_string(),
        1 => "1 entrée".to_string(),
        n if n >= MAX_ROWS => format!("Les {MAX_ROWS} plus récentes"),
        n => format!("{n} entrées"),
    }
}

/// Ce que dit la liste quand elle est vide, selon le filtre.
pub fn empty_text(period: Period, person: bool) -> String {
    let when = match period {
        Period::Today => "aujourd'hui",
        Period::TodayAndYesterday => "aujourd'hui et hier",
        Period::Week => "ces 7 derniers jours",
    };
    if person {
        format!("Rien à signaler pour cette personne {when}.")
    } else {
        format!("Rien à signaler {when}.")
    }
}

pub const FOOTER: &str = "Gardé 6 mois. Cette activité ne se lit que dans Ostal Server.";

#[cfg(test)]
mod tests {
    use super::*;
    use ostal_server::audit::{Action, ConnRef, Who};
    use protocol::wire::Via;

    fn ts(date: NaiveDate, hour: u32, minute: u32) -> f64 {
        Local
            .from_local_datetime(&date.and_hms_opt(hour, minute, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp() as f64
    }

    fn who(id: &str, name: &str, phone: &str) -> Who {
        let mut w = Who::user(id, Some(name.into()));
        w.connection = Some(ConnRef::new(Some("iphone-0001"), phone, "web"));
        w.via = Some(Via::Local);
        w
    }

    #[test]
    fn day_titles_are_in_french_like_the_mockup() {
        let today = NaiveDate::from_ymd_opt(2025, 11, 22).unwrap();
        assert_eq!(day_title(today, today), "Aujourd'hui · samedi 22 novembre");
        assert_eq!(
            day_title(today - Duration::days(1), today),
            "Hier · vendredi 21 novembre"
        );
        assert_eq!(
            day_title(NaiveDate::from_ymd_opt(2025, 11, 19).unwrap(), today),
            "mercredi 19 novembre"
        );
        assert_eq!(
            day_title(NaiveDate::from_ymd_opt(2027, 2, 1).unwrap(), today),
            "lundi 1 février"
        );
    }

    #[test]
    fn periods_start_at_local_midnight() {
        let today = NaiveDate::from_ymd_opt(2025, 11, 22).unwrap();
        let midnight = ts(today, 0, 0);
        assert_eq!(Period::Today.since(today), midnight);
        let yesterday = Period::TodayAndYesterday.since(today);
        assert!((midnight - yesterday - 86_400.0).abs() <= 3_600.0);
        let week = Period::Week.since(today);
        assert!((midnight - week - 6.0 * 86_400.0).abs() <= 3_600.0);
        assert_eq!(Period::default(), Period::TodayAndYesterday);
        assert_eq!(
            Period::ALL.map(Period::label),
            ["Aujourd'hui et hier", "Aujourd'hui", "7 derniers jours"]
        );
    }

    #[test]
    fn rows_are_grouped_by_day_newest_first_with_who_from_and_what() {
        let today = NaiveDate::from_ymd_opt(2025, 11, 22).unwrap();
        let yesterday = today - Duration::days(1);
        let entries = vec![
            Entry::new(Action::DeviceCommand, &who("a", "Hamdi", "iPhone de Hamdi"))
                .at(ts(today, 18, 41))
                .target("Lampe canapé")
                .detail("on"),
            Entry::new(Action::Login, &who("b", "Sam", "iPhone de Sam")).at(ts(today, 18, 40)),
            Entry::new(Action::Login, &Who::default())
                .at(ts(yesterday, 23, 2))
                .result(audit::Outcome::Refused),
        ];
        let days = days_of(&entries, today);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].title, "Aujourd'hui · samedi 22 novembre");
        assert_eq!(days[1].title, "Hier · vendredi 21 novembre");
        assert_eq!(
            days[0].rows[0],
            Row {
                time: "18:41".into(),
                who: "Hamdi".into(),
                from: "iPhone de Hamdi".into(),
                what: "Lampe canapé : allumé".into(),
                tone: Tone::Device,
            }
        );
        assert_eq!(days[0].rows[1].what, "Connexion à l'app web, sur place");
        assert_eq!(days[0].rows[1].who, "Sam");
        let refused = &days[1].rows[0];
        assert_eq!(
            (refused.who.as_str(), refused.from.as_str(), refused.tone),
            ("Inconnu", "—", Tone::Warn)
        );
    }

    #[test]
    fn the_window_reads_the_same_files_as_the_command_line_with_both_filters() {
        let dir = std::env::temp_dir().join(format!("ostal-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = AuditLog::open(&dir);
        let today = today();
        let now = ts(today, 20, 0);
        log.record(Entry::new(Action::Login, &who("a", "Hamdi", "Mac")).at(ts(today, 18, 0)));
        log.record(Entry::new(Action::Login, &who("b", "Sam", "Phone")).at(ts(today, 19, 0)));
        log.record(Entry::new(Action::Login, &who("a", "Hamdi", "Mac")).at(ts(
            today - Duration::days(3),
            12,
            0,
        )));
        assert_eq!(load(&log, None, Period::TodayAndYesterday, now).len(), 2);
        assert_eq!(load(&log, None, Period::Week, now).len(), 3);
        assert_eq!(load(&log, Some("a"), Period::Today, now).len(), 1);
        assert_eq!(load(&log, Some("a"), Period::Week, now).len(), 2);
        assert!(load(&log, Some("zz"), Period::Week, now).is_empty());
        assert_eq!(count_label(0), "Aucune entrée");
        assert_eq!(count_label(1), "1 entrée");
        assert_eq!(count_label(12), "12 entrées");
        assert_eq!(
            empty_text(Period::Today, true),
            "Rien à signaler pour cette personne aujourd'hui."
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
