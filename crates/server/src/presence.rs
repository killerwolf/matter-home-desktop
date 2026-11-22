//! La présence **déclarée** de chaque personne : « à la maison » ou « partie », telle que son
//! raccourci « Je rentre » / « Je pars » (ou, plus tard, son téléphone vu sur le Wi-Fi) l'a dite.
//!
//! Rangée dans `serveur/presence.json` (0600, jamais exportée) : la dernière déclaration de chacun,
//! avec la date du dernier changement. [`Presence`] ne déduit rien : elle garde ce que chacun a
//! dit. La règle qui en tire le mode de la maison est [`auto_mode`] : la même pour le téléphone vu
//! sur le Wi-Fi (`hub/wifi.rs`) et pour les raccourcis « Je pars » / « Je rentre » (jeton de mode),
//! et seulement quand l'interrupteur « Changer le mode de la maison selon la présence » est activé.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use protocol::mode::HomeMode;
use serde::{Deserialize, Serialize};

use crate::security::write_secret;

pub const FILE: &str = "presence.json";

/// Où est la personne.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Whereabouts {
    Home,
    Away,
}

/// Comment on l'a su.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Un raccourci Apple (jeton de mode de la personne).
    Shortcut,
    /// La personne, depuis une session de l'app.
    Session,
    /// Son téléphone relié, vu (ou plus vu) sur le Wi-Fi de la box.
    Wifi,
}

/// Ce que le serveur sait d'un téléphone relié, d'après ses propres relevés de la box : depuis quand
/// on le voit de façon continue, ou depuis quand on ne le voit plus. Jamais les deux à la fois.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sighting {
    /// Premier relevé du séjour actuel sur le Wi-Fi.
    pub active_since: Option<f64>,
    /// Premier relevé sans le téléphone (il n'est plus actif, ou plus dans la liste).
    pub inactive_since: Option<f64>,
}

/// Ce que le Wi-Fi dit de la personne, au vu de sa dernière déclaration : `Some(état)` à déclarer,
/// ou `None` : on ne change rien. Le signal **le plus récent** l'emporte :
///
/// - une arrivée vue sur le Wi-Fi compte tout de suite, sauf si un « Je pars » plus récent que
///   l'arrivée du téléphone a été dit ;
/// - un départ vu seulement sur le Wi-Fi ne compte qu'après `delay_s` secondes sans voir le
///   téléphone (un iPhone en veille coupe son Wi-Fi), et un « Je rentre » plus récent que le début
///   de l'absence l'annule.
pub fn wifi_vote(
    declared: Option<&Declared>,
    sighting: &Sighting,
    now: f64,
    delay_s: f64,
) -> Option<Whereabouts> {
    if let Some(since) = sighting.active_since {
        let newer_away =
            declared.is_some_and(|d| d.state == Whereabouts::Away && d.last_word() >= since);
        return (!newer_away).then_some(Whereabouts::Home);
    }
    let gone_since = sighting.inactive_since?;
    if now - gone_since < delay_s {
        return None;
    }
    let newer_home =
        declared.is_some_and(|d| d.state == Whereabouts::Home && d.last_word() >= gone_since);
    (!newer_home).then_some(Whereabouts::Away)
}

/// Ce qui vient d'arriver à la maison d'après le Wi-Fi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    /// Quelqu'un vient d'arriver.
    Arrival,
    /// Quelqu'un vient de partir, et plus personne n'est à la maison.
    LastDeparture,
}

/// Le mode que prend la maison toute seule, ou `None` : le mode ne change pas.
///
/// - **Absent** seulement quand la dernière personne est partie, et seulement depuis **Présent** :
///   Nuit et Vacances sont choisis à la main, une absence ne les quitte jamais ;
/// - **Présent** à une arrivée, depuis Absent ou Vacances (le premier qui rentre remet la maison en
///   Présent) ; une arrivée pendant Nuit ne change rien.
pub fn auto_mode(movement: Movement, current: HomeMode) -> Option<HomeMode> {
    match (movement, current) {
        (Movement::LastDeparture, HomeMode::Present) => Some(HomeMode::Absent),
        (Movement::Arrival, HomeMode::Absent | HomeMode::Vacances) => Some(HomeMode::Present),
        _ => None,
    }
}

/// Les autres personnes actives déclarées « à la maison » (identifiants internes).
pub fn others_at_home(
    user_id: &str,
    declared: &[Declared],
    is_active: impl Fn(&str) -> bool,
) -> Vec<String> {
    declared
        .iter()
        .filter(|d| d.user_id != user_id && d.state == Whereabouts::Home && is_active(&d.user_id))
        .map(|d| d.user_id.clone())
        .collect()
}

/// Ce que devient la maison après la déclaration d'une personne par un raccourci.
#[derive(Clone, Debug, PartialEq)]
pub struct Followed {
    /// Le mode de la maison une fois la règle appliquée.
    pub mode: HomeMode,
    /// Le mode vient-il de changer ?
    pub changed: bool,
    /// Les autres personnes déclarées à la maison (identifiants internes) au moment de la règle.
    pub others_home: Vec<String>,
    /// Le mode automatique est-il actif (interrupteur allumé, au moins un téléphone relié) ?
    /// Sinon la maison ne change jamais : on le dit à la personne.
    pub auto: bool,
}

/// La phrase, en français, qui dit à la personne ce qui s'est passé. `names` : prénoms (noms
/// affichés) de `Followed::others_home`, jamais d'identifiant interne.
pub fn message(wanted: Whereabouts, followed: &Followed, names: &[String]) -> String {
    let mode = followed.mode;
    let label = mode.label();
    if !followed.auto {
        return format!(
            "C'est noté. Le mode automatique est désactivé : la maison reste en {label}."
        );
    }
    if followed.changed {
        return format!("Maison en mode {label}.");
    }
    match (wanted, mode) {
        (_, HomeMode::Nuit) => "Mode Nuit : la maison ne change pas.".into(),
        (Whereabouts::Away, HomeMode::Vacances) => {
            "Mode Vacances : la maison ne change pas.".into()
        }
        (Whereabouts::Away, HomeMode::Absent) => "Maison en mode Absent.".into(),
        (Whereabouts::Away, _) if !names.is_empty() => {
            let (verb, who) = match names {
                [one] => ("est", one.clone()),
                [init @ .., last] => ("sont", format!("{} et {last}", init.join(", "))),
                [] => unreachable!(),
            };
            format!("{who} {verb} encore à la maison : la maison reste en {label}.")
        }
        _ => format!("Maison en mode {label}."),
    }
}

/// La dernière déclaration d'une personne.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Declared {
    pub user_id: String,
    pub state: Whereabouts,
    /// Secondes Unix du dernier **changement** (redire la même chose ne le change pas).
    pub since: f64,
    pub source: Source,
    /// Secondes Unix de la dernière fois que la personne (ou son téléphone) a dit la même chose :
    /// « Je rentre » redit à l'identique ne change pas son état, mais compte comme plus récent que
    /// le Wi-Fi. 0 : fichier d'avant (on prend `since`).
    #[serde(default)]
    pub said: f64,
}

impl Declared {
    /// La date du dernier mot : le plus récent des signaux l'emporte.
    pub fn last_word(&self) -> f64 {
        self.said.max(self.since)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    people: Vec<Declared>,
}

pub struct Presence {
    path: Option<PathBuf>,
    people: Mutex<Vec<Declared>>,
}

impl Presence {
    /// Ouvre `dir/presence.json` (absent : personne n'a rien déclaré). Un fichier illisible est mis
    /// de côté : on repart de zéro.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let people = match crate::persistence::load_json::<File>(&path) {
            Ok(file) => file.map(|f| f.people).unwrap_or_default(),
            Err(e) => {
                let aside = path.with_extension("json.illisible");
                log::error!(
                    "présence illisible, personne n'a rien déclaré ({e:#}) ; fichier conservé sous {}",
                    aside.display()
                );
                let _ = std::fs::rename(&path, aside);
                Vec::new()
            }
        };
        Self {
            path: Some(path),
            people: Mutex::new(people),
        }
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            people: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Declared>> {
        self.people.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Enregistre ce que la personne vient de dire. `true` : cela change son état (rien n'est
    /// écrit si elle redit la même chose).
    pub fn declare(&self, user_id: &str, state: Whereabouts, source: Source) -> Result<bool> {
        self.declare_at(user_id, state, source, crate::security::now())
    }

    /// Comme [`Presence::declare`], à la date `at` (secondes Unix).
    pub fn declare_at(
        &self,
        user_id: &str,
        state: Whereabouts,
        source: Source,
        at: f64,
    ) -> Result<bool> {
        let mut people = self.lock();
        if let Some(old) = people
            .iter()
            .find(|d| d.user_id == user_id && d.state == state)
        {
            // La même chose redite ne change rien à l'état. Mais ce que la personne dit elle-même
            // compte comme plus récent que ce que le Wi-Fi avait vu (« Je rentre » après un
            // iPhone endormi) ; le Wi-Fi, lui, ne reprend jamais la parole à la personne.
            if source != Source::Wifi && at > old.last_word() {
                let mut next = people.clone();
                for d in next.iter_mut().filter(|d| d.user_id == user_id) {
                    d.said = at;
                    d.source = source;
                }
                self.store(&mut people, next)?;
            }
            return Ok(false);
        }
        let mut next = people.clone();
        next.retain(|d| d.user_id != user_id);
        next.push(Declared {
            user_id: user_id.to_string(),
            state,
            since: at,
            source,
            said: at,
        });
        self.store(&mut people, next)?;
        Ok(true)
    }

    fn store(&self, people: &mut Vec<Declared>, next: Vec<Declared>) -> Result<()> {
        if let Some(path) = &self.path {
            let file = File {
                version: 1,
                people: next.clone(),
            };
            write_secret(path, serde_json::to_string_pretty(&file)?.as_bytes())?;
        }
        *people = next;
        Ok(())
    }

    /// Oublie ce que le Wi-Fi avait dit de cette personne (son téléphone est retiré ou changé) :
    /// une déclaration ancienne ne doit pas garder la maison « occupée ». Ce qu'elle a dit elle-même
    /// (raccourci, session) reste. `true` : quelque chose a été oublié.
    pub fn forget_wifi(&self, user_id: &str) -> Result<bool> {
        let mut people = self.lock();
        if !people
            .iter()
            .any(|d| d.user_id == user_id && d.source == Source::Wifi)
        {
            return Ok(false);
        }
        let next: Vec<Declared> = people
            .iter()
            .filter(|d| !(d.user_id == user_id && d.source == Source::Wifi))
            .cloned()
            .collect();
        self.store(&mut people, next)?;
        Ok(true)
    }

    /// Ce que cette personne a déclaré en dernier, s'il y a quelque chose.
    pub fn get(&self, user_id: &str) -> Option<Declared> {
        self.lock().iter().find(|d| d.user_id == user_id).cloned()
    }

    /// Les dernières déclarations de tout le monde.
    pub fn all(&self) -> Vec<Declared> {
        self.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_says_what_became_of_the_house() {
        let followed = |mode, changed| Followed {
            mode,
            changed,
            others_home: vec![],
            auto: true,
        };
        let one = vec!["Sam".to_string()];
        let two = vec!["Sam".to_string(), "Léa".to_string()];
        assert_eq!(
            message(Whereabouts::Away, &followed(HomeMode::Absent, true), &[]),
            "Maison en mode Absent."
        );
        assert_eq!(
            message(Whereabouts::Away, &followed(HomeMode::Present, false), &one),
            "Sam est encore à la maison : la maison reste en Présent."
        );
        assert_eq!(
            message(Whereabouts::Away, &followed(HomeMode::Nuit, false), &two),
            "Mode Nuit : la maison ne change pas."
        );
        assert_eq!(
            message(Whereabouts::Away, &followed(HomeMode::Present, false), &two),
            "Sam et Léa sont encore à la maison : la maison reste en Présent."
        );
        assert_eq!(
            message(Whereabouts::Away, &followed(HomeMode::Vacances, false), &[]),
            "Mode Vacances : la maison ne change pas."
        );
        assert_eq!(
            message(Whereabouts::Home, &followed(HomeMode::Present, true), &[]),
            "Maison en mode Présent."
        );
        // Interrupteur éteint : la maison ne change jamais, et le raccourci le dit.
        let off = Followed {
            mode: HomeMode::Present,
            changed: false,
            others_home: vec![],
            auto: false,
        };
        assert_eq!(
            message(Whereabouts::Away, &off, &[]),
            "C'est noté. Le mode automatique est désactivé : la maison reste en Présent."
        );
    }

    #[test]
    fn the_last_word_of_each_person_is_kept_and_a_repeat_changes_nothing() {
        let presence = Presence::in_memory();
        assert!(presence.get("lea").is_none());
        assert!(presence
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        let first = presence.get("lea").unwrap();
        assert!(!presence
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        assert_eq!(presence.get("lea").unwrap().since, first.since);
        assert!(presence
            .declare("lea", Whereabouts::Home, Source::Shortcut)
            .unwrap());
        assert!(presence
            .declare("adam", Whereabouts::Away, Source::Session)
            .unwrap());
        assert_eq!(presence.get("lea").unwrap().state, Whereabouts::Home);
        assert_eq!(presence.get("adam").unwrap().state, Whereabouts::Away);
        assert_eq!(presence.all().len(), 2);
    }

    fn declared(state: Whereabouts, since: f64) -> Declared {
        Declared {
            user_id: "sam".into(),
            state,
            since,
            source: Source::Shortcut,
            said: 0.0,
        }
    }

    fn seen_since(at: f64) -> Sighting {
        Sighting {
            active_since: Some(at),
            inactive_since: None,
        }
    }

    fn gone_since(at: f64) -> Sighting {
        Sighting {
            active_since: None,
            inactive_since: Some(at),
        }
    }

    #[test]
    fn an_arrival_seen_on_wifi_counts_at_once_unless_a_later_leave_was_said() {
        let delay = 600.0;
        // Rien de déclaré, ou parti : le téléphone qui apparaît dit « à la maison » tout de suite.
        assert_eq!(
            wifi_vote(None, &seen_since(1000.0), 1000.0, delay),
            Some(Whereabouts::Home)
        );
        assert_eq!(
            wifi_vote(
                Some(&declared(Whereabouts::Away, 900.0)),
                &seen_since(1000.0),
                1000.0,
                delay
            ),
            Some(Whereabouts::Home)
        );
        // « Je pars » dit après l'arrivée du téléphone : le plus récent l'emporte (le téléphone est
        // encore sur le Wi-Fi quelques minutes).
        assert_eq!(
            wifi_vote(
                Some(&declared(Whereabouts::Away, 1100.0)),
                &seen_since(1000.0),
                1200.0,
                delay
            ),
            None
        );
    }

    #[test]
    fn a_departure_seen_only_on_wifi_counts_after_the_delay_and_a_later_return_cancels_it() {
        let delay = 600.0;
        let home = declared(Whereabouts::Home, 500.0);
        // Pas encore dix minutes : un téléphone en veille coupe son Wi-Fi.
        assert_eq!(
            wifi_vote(Some(&home), &gone_since(1000.0), 1599.0, delay),
            None
        );
        assert_eq!(
            wifi_vote(Some(&home), &gone_since(1000.0), 1600.0, delay),
            Some(Whereabouts::Away)
        );
        // « Je rentre » plus récent que le début de l'absence : annulé.
        assert_eq!(
            wifi_vote(
                Some(&declared(Whereabouts::Home, 1300.0)),
                &gone_since(1000.0),
                2000.0,
                delay
            ),
            None
        );
        // Jamais vu : rien à dire.
        assert_eq!(wifi_vote(None, &Sighting::default(), 2000.0, delay), None);
    }

    #[test]
    fn the_automatic_mode_never_leaves_night_or_holidays_by_an_absence() {
        use HomeMode::*;
        use Movement::*;
        // Départ : seulement Présent → Absent.
        assert_eq!(auto_mode(LastDeparture, Present), Some(Absent));
        assert_eq!(auto_mode(LastDeparture, Nuit), None);
        assert_eq!(auto_mode(LastDeparture, Vacances), None);
        assert_eq!(auto_mode(LastDeparture, Absent), None);
        // Arrivée : Absent ou Vacances → Présent ; Nuit ne change pas.
        assert_eq!(auto_mode(Arrival, Absent), Some(Present));
        assert_eq!(auto_mode(Arrival, Vacances), Some(Present));
        assert_eq!(auto_mode(Arrival, Nuit), None);
        assert_eq!(auto_mode(Arrival, Present), None);
    }

    #[test]
    fn what_the_wifi_said_can_be_forgotten_without_touching_what_a_person_said() {
        let presence = Presence::in_memory();
        presence
            .declare_at("lea", Whereabouts::Home, Source::Wifi, 10.0)
            .unwrap();
        presence
            .declare_at("sam", Whereabouts::Home, Source::Shortcut, 10.0)
            .unwrap();
        assert!(presence.forget_wifi("lea").unwrap());
        assert!(!presence.forget_wifi("lea").unwrap());
        assert!(!presence.forget_wifi("sam").unwrap());
        assert!(presence.get("lea").is_none());
        assert_eq!(presence.get("sam").unwrap().state, Whereabouts::Home);
    }

    #[test]
    fn declarations_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("ostal-presence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Presence::open(&dir)
            .declare("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap();
        let again = Presence::open(&dir);
        assert_eq!(again.get("lea").unwrap().state, Whereabouts::Away);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
