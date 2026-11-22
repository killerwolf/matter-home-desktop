//! Le côté hub de la présence par le Wi-Fi : le relevé périodique de la box, la règle qui en tire la
//! présence de chaque personne, et le mode automatique.
//!
//! # Règles de sûreté
//!
//! - L'interrupteur « Changer le mode de la maison selon la présence » est **désactivé** tant que
//!   l'administrateur ne l'a pas activé (aussi après une mise à jour) : le Wi-Fi renseigne alors la
//!   présence affichée, sans jamais toucher au mode.
//! - Avec **zéro téléphone relié** (à un compte actif), la règle ne se déclenche jamais.
//! - Box injoignable ou mot de passe refusé : **rien n'est conclu**, le mode est figé (jamais lu comme
//!   « personne à la maison »).
//! - Nuit et Vacances se choisissent à la main : une absence ne les quitte jamais ; un départ
//!   pendant Vacances ne fait rien ; une arrivée pendant Vacances remet Présent ; une arrivée
//!   pendant Nuit ne change rien (voir `presence::auto_mode`).
//! - Un changement automatique est journalisé avec la personne concernée.

use protocol::mode::{HomeMode, ModeSource};

use crate::audit::{Action, ConnRef, Entry, ModeCause, Origin, Who};
use crate::presence::{self, Movement, Source, Whereabouts};
use crate::security::account::AccountStore;
use crate::wifi::{AssignError, Link, LinkError, Observed};

use super::Hub;

/// Qui sont les gens de la maison : le hub ne connaît pas les comptes, l'appelant les lui prête.
pub trait People {
    /// Le compte existe-t-il et est-il actif ? (Une personne désactivée ne compte pas.)
    fn is_active(&self, user_id: &str) -> bool;
    /// Son nom affiché.
    fn name(&self, user_id: &str) -> Option<String>;
}

impl People for AccountStore {
    fn is_active(&self, user_id: &str) -> bool {
        AccountStore::is_active(self, user_id)
    }

    fn name(&self, user_id: &str) -> Option<String> {
        self.display_name(user_id)
    }
}

/// Ce qu'un relevé a changé.
#[derive(Debug, Clone, PartialEq)]
pub struct WifiReport {
    /// L'état de la liaison après le relevé.
    pub link: Link,
    /// Ce qui a changé dans la présence de chacun (identifiant du compte, nouvel état).
    pub declared: Vec<(String, Whereabouts)>,
    /// Le mode que la maison vient de prendre toute seule.
    pub mode: Option<HomeMode>,
}

/// La connexion « Wi-Fi de la box » du journal.
fn wifi_who(people: &dyn People, user_id: &str) -> Who {
    let mut who = Who::user(user_id, people.name(user_id));
    who.connection = Some(ConnRef::new(None, "Wi-Fi de la box", "wifi"));
    who
}

impl Hub {
    /// La box de la maison et les téléphones des comptes.
    pub fn wifi(&self) -> &crate::wifi::Wifi {
        &self.wifi
    }

    /// Relève la box maintenant (`force` : même si le dernier relevé est tout récent).
    pub fn wifi_poll(&self, people: &dyn People, force: bool) -> WifiReport {
        self.wifi_poll_at(crate::security::now(), people, force)
    }

    /// Comme [`Hub::wifi_poll`], à la date `now` (secondes Unix).
    pub fn wifi_poll_at(&self, now: f64, people: &dyn People, force: bool) -> WifiReport {
        let mut report = WifiReport {
            link: self.wifi.status().link,
            declared: Vec::new(),
            mode: None,
        };
        match self.wifi.observe(now, force) {
            Observed::Fresh => {}
            Observed::Skipped | Observed::Frozen(_) => {
                report.link = self.wifi.status().link;
                return report;
            }
        }
        report.link = Link::Ok;
        let delay = self.wifi.away_delay_s();
        for (user_id, sighting) in self.wifi.sightings() {
            if !people.is_active(&user_id) {
                continue;
            }
            let declared = self.presence.get(&user_id);
            let Some(state) = presence::wifi_vote(declared.as_ref(), &sighting, now, delay) else {
                continue;
            };
            match self.presence.declare_at(&user_id, state, Source::Wifi, now) {
                Ok(true) => {
                    self.audit.record(
                        Entry::new(Action::Presence, &wifi_who(people, &user_id))
                            .target(match state {
                                Whereabouts::Home => "home",
                                Whereabouts::Away => "away",
                            })
                            .source(Origin::Automatic)
                            .detail("wifi")
                            .at(now),
                    );
                    report.declared.push((user_id, state));
                }
                Ok(false) => {}
                Err(e) => log::error!("présence par le Wi-Fi non enregistrée : {e:#}"),
            }
        }
        report.mode = self.apply_wifi_rule(&report.declared, people);
        report
    }

    /// La règle du mode automatique, pour ce que le relevé vient de changer. Rend le mode pris.
    /// Cette règle-ci ne vaut que pour la source Wi-Fi : sans téléphone relié à un compte actif,
    /// un relevé n'a rien à dire.
    fn apply_wifi_rule(
        &self,
        changes: &[(String, Whereabouts)],
        people: &dyn People,
    ) -> Option<HomeMode> {
        if changes.is_empty() || !self.wifi.auto_mode() {
            return None;
        }
        // Zéro téléphone relié à un compte actif : la règle ne se déclenche jamais.
        if !self
            .wifi
            .phones()
            .iter()
            .any(|p| people.is_active(&p.user_id))
        {
            return None;
        }
        // Une arrivée l'emporte sur un départ au même relevé : on ne risque jamais d'armer la maison
        // pendant que quelqu'un rentre.
        let (user_id, movement, detail) =
            if let Some((id, _)) = changes.iter().find(|(_, s)| *s == Whereabouts::Home) {
                (id, Movement::Arrival, "arrivée")
            } else {
                let (id, _) = changes.iter().find(|(_, s)| *s == Whereabouts::Away)?;
                (id, Movement::LastDeparture, "départ")
            };
        let cause = ModeCause {
            who: wifi_who(people, user_id),
            origin: Origin::Automatic,
            detail: Some(detail.to_string()),
        };
        // Le départ se décide sous le verrou du mode : quelqu'un d'autre déclaré à la maison (par le
        // Wi-Fi, un raccourci ou l'app) au dernier moment, et la maison n'est pas armée.
        match self.set_mode_if(
            |current| {
                if movement == Movement::LastDeparture
                    && self.someone_else_home("", |id| people.is_active(id))
                {
                    return None;
                }
                presence::auto_mode(movement, current)
            },
            ModeSource::Presence,
            cause,
        ) {
            Ok(mode) => mode,
            Err(e) => {
                log::warn!("mode automatique non appliqué : {e:?}");
                None
            }
        }
    }

    /// Attribue (ou retire, `None`) le téléphone d'un compte. `phones_only` : un membre ne peut
    /// choisir que parmi les noms de téléphones ; l'administrateur, parmi tous les appareils libres.
    /// Ce que le Wi-Fi avait dit de la personne est oublié (un ancien téléphone ne garde pas la maison
    /// « occupée »).
    pub fn wifi_assign(
        &self,
        user_id: &str,
        host: Option<&str>,
        phones_only: bool,
        who: &Who,
        target_name: &str,
    ) -> Result<bool, AssignError> {
        let changed = self.wifi.assign(user_id, host, phones_only)?;
        if changed {
            if let Err(e) = self.presence.forget_wifi(user_id) {
                log::error!("présence par le Wi-Fi non effacée : {e:#}");
            }
            self.audit.record(
                Entry::new(Action::Phone, who)
                    .target(target_name)
                    .detail(if host.is_some() { "set" } else { "cleared" }),
            );
        }
        Ok(changed)
    }

    /// Relie la box (voir [`crate::wifi::Wifi::link`]) et le note au journal.
    pub fn wifi_link(&self, url: &str, password: &str, who: &Who) -> Result<(), LinkError> {
        let first = !self.wifi.status().linked;
        self.wifi.link(url, password)?;
        self.audit
            .record(Entry::new(Action::BoxSetting, who).detail(if first {
                "linked"
            } else {
                "password"
            }));
        Ok(())
    }

    /// Délie la box : mot de passe effacé, mode automatique désactivé.
    pub fn wifi_unlink(&self, who: &Who) -> Result<(), LinkError> {
        self.wifi.unlink()?;
        self.audit
            .record(Entry::new(Action::BoxSetting, who).detail("unlinked"));
        Ok(())
    }

    /// L'interrupteur « Changer le mode de la maison selon la présence ».
    pub fn wifi_set_auto_mode(&self, on: bool, who: &Who) -> Result<(), &'static str> {
        let before = self.wifi.auto_mode();
        self.wifi.set_auto_mode(on)?;
        if before != on {
            self.audit
                .record(Entry::new(Action::BoxSetting, who).detail(if on {
                    "auto_on"
                } else {
                    "auto_off"
                }));
        }
        Ok(())
    }

    /// Le délai avant de compter un départ vu seulement sur le Wi-Fi (minutes), noté au journal quand
    /// il change. Appelé par la page Présence d'Ostal Server.
    pub fn wifi_set_away_delay(&self, minutes: u32, who: &Who) -> Result<(), &'static str> {
        let before = self.wifi.status().away_delay_min;
        self.wifi.set_away_delay_min(minutes)?;
        let after = self.wifi.status().away_delay_min;
        if before != after {
            self.audit.record(
                Entry::new(Action::BoxSetting, who)
                    .target(after.to_string())
                    .detail("delay"),
            );
        }
        Ok(())
    }

    /// Où est chacun d'après ce qu'il a déclaré (raccourci, session, téléphone sur le Wi-Fi), pour
    /// les personnes actives seulement.
    pub fn presence_of(&self, people: &dyn People) -> Vec<presence::Declared> {
        self.presence
            .all()
            .into_iter()
            .filter(|d| people.is_active(&d.user_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::super::tests::{hub_with, temp};
    use super::*;
    use crate::audit::Filter;
    use crate::wifi::bbox::{BoxApi, BoxError, Host};
    use crate::wifi::Wifi;

    type Answer = Option<Result<Vec<Host>, BoxError>>;

    /// Une fausse box : la liste (ou l'erreur) qu'on lui donne, et le nombre de relevés.
    #[derive(Clone, Default)]
    struct FakeBox {
        answer: Arc<Mutex<Answer>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeBox {
        fn set(&self, hosts: &[(&str, bool)]) {
            *self.answer.lock().unwrap() = Some(Ok(hosts
                .iter()
                .map(|(n, a)| Host {
                    name: n.to_string(),
                    active: *a,
                })
                .collect()));
        }

        fn fail(&self, error: BoxError) {
            *self.answer.lock().unwrap() = Some(Err(error));
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl BoxApi for FakeBox {
        fn hosts(&self, _url: &str, _password: &str) -> Result<Vec<Host>, BoxError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(Err(BoxError::Unreachable))
        }
    }

    struct Crew(Mutex<HashMap<String, (String, bool)>>);

    impl Crew {
        fn new(people: &[(&str, &str)]) -> Crew {
            Crew(Mutex::new(
                people
                    .iter()
                    .map(|(id, name)| (id.to_string(), (name.to_string(), true)))
                    .collect(),
            ))
        }

        fn deactivate(&self, id: &str) {
            self.0.lock().unwrap().get_mut(id).unwrap().1 = false;
        }
    }

    impl People for Crew {
        fn is_active(&self, id: &str) -> bool {
            self.0.lock().unwrap().get(id).is_some_and(|p| p.1)
        }

        fn name(&self, id: &str) -> Option<String> {
            self.0.lock().unwrap().get(id).map(|p| p.0.clone())
        }
    }

    const SAM: &str = "iPhone-de-Sam";
    const LEA: &str = "Galaxy-de-Lea";
    const NAS: &str = "NAS-salon";

    struct Rig {
        hub: Hub,
        fake: FakeBox,
        crew: Crew,
        dir: std::path::PathBuf,
        /// Le temps du test (secondes Unix, à partir de maintenant).
        t0: f64,
        _rx: CommandRx,
    }

    type CommandRx = tokio::sync::mpsc::UnboundedReceiver<crate::events::Command>;

    /// Une maison avec Sam et Léa, la box reliée, les deux téléphones attribués. Délai d'absence : 10 min.
    fn rig(tag: &str) -> Rig {
        let dir = temp(tag);
        let (mut hub, rx) = hub_with(&dir, 16);
        let fake = FakeBox::default();
        fake.set(&[(SAM, true), (LEA, true), (NAS, true)]);
        hub.wifi = Wifi::open(
            &crate::security::server_dir(&dir).unwrap(),
            Box::new(fake.clone()),
        );
        hub.wifi
            .link("http://box.test", "mot-de-passe-de-la-box")
            .unwrap();
        let crew = Crew::new(&[("sam", "Sam"), ("lea", "Léa")]);
        let who = Who::server_app();
        assert!(hub
            .wifi_assign("sam", Some(SAM), true, &who, "Sam")
            .unwrap());
        assert!(hub
            .wifi_assign("lea", Some(LEA), true, &who, "Léa")
            .unwrap());
        Rig {
            hub,
            fake,
            crew,
            dir,
            t0: crate::security::now(),
            _rx: rx,
        }
    }

    impl Rig {
        fn poll(&self, dt: f64) -> WifiReport {
            self.hub.wifi_poll_at(self.t0 + dt, &self.crew, true)
        }

        fn mode(&self) -> HomeMode {
            self.hub.mode()
        }

        fn set_mode(&self, mode: HomeMode) {
            self.hub.set_mode(mode, ModeSource::Web).unwrap();
        }

        fn auto(&self, on: bool) {
            self.hub.wifi_set_auto_mode(on, &Who::server_app()).unwrap();
        }

        fn state_of(&self, id: &str) -> Option<Whereabouts> {
            self.hub.presence.get(id).map(|d| d.state)
        }

        /// Sam et Léa sont chez eux, vus par la box.
        fn both_home(&self) {
            self.fake.set(&[(SAM, true), (LEA, true), (NAS, true)]);
            self.poll(0.0);
        }

        /// Léa et/ou Sam partent : sont absents de la liste active.
        fn only(&self, present: &[&str]) {
            self.fake.set(&[
                (SAM, present.contains(&"sam")),
                (LEA, present.contains(&"lea")),
                (NAS, true),
            ]);
        }

        /// Laisse passer le délai d'absence en relevant la box avant et après.
        fn wait_for_departure(&self, from: f64) -> WifiReport {
            self.poll(from + 1.0);
            self.poll(from + 301.0);
            self.poll(from + 599.0);
            self.poll(from + 1.0 + 601.0)
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_switch_is_off_by_default_and_presence_never_touches_the_mode_while_it_is_off() {
        let r = rig("wifi-off");
        assert!(!r.hub.wifi().auto_mode());
        assert!(!r.hub.wifi().status().auto_mode);
        r.both_home();
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Home));
        r.only(&[]);
        r.wait_for_departure(100.0);
        // Tout le monde est parti, mais l'interrupteur est éteint : la maison ne bouge pas.
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Away));
        assert_eq!(r.state_of("lea"), Some(Whereabouts::Away));
        assert_eq!(r.mode(), HomeMode::Present);
        // L'interrupteur reste éteint dans le fichier, y compris après un redémarrage.
        let file = std::fs::read_to_string(r.dir.join("serveur/box.json")).unwrap();
        assert!(file.contains("\"auto_mode\": false"), "{file}");
        let reopened = Wifi::open(&r.dir.join("serveur"), Box::new(r.fake.clone()));
        assert!(!reopened.auto_mode());
    }

    #[test]
    fn a_folder_without_box_settings_starts_unlinked_with_the_switch_off() {
        let dir = temp("wifi-legacy");
        // Un dossier à la forme de la 2.0.0 : ni box.json ni telephones.json.
        std::fs::create_dir_all(dir.join("serveur")).unwrap();
        let (hub, _rx) = hub_with(&dir, 16);
        let status = hub.wifi().status();
        assert!(!status.linked);
        assert!(!status.auto_mode);
        assert_eq!(status.link, Link::None);
        assert!(hub.wifi().phones().is_empty());
        // L'interrupteur peut être allumé sans box : la règle ne suit alors que les raccourcis.
        hub.wifi_set_auto_mode(true, &Who::server_app()).unwrap();
        assert!(hub.wifi().auto_mode() && hub.wifi().status().auto_mode);
        // Aucun relevé sans box : rien n'est tenté, rien ne change.
        let crew = Crew::new(&[("sam", "Sam")]);
        let report = hub.wifi_poll_at(1.0, &crew, true);
        assert_eq!(report.link, Link::None);
        assert!(report.declared.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn with_no_linked_phone_the_rule_never_fires() {
        let r = rig("wifi-zero");
        r.auto(true);
        r.set_mode(HomeMode::Absent);
        // On retire les deux téléphones : plus aucun téléphone relié.
        let who = Who::server_app();
        r.hub.wifi_assign("sam", None, true, &who, "Sam").unwrap();
        r.hub.wifi_assign("lea", None, true, &who, "Léa").unwrap();
        assert!(r.hub.wifi().phones().is_empty());
        r.fake.set(&[(SAM, true), (LEA, true), (NAS, true)]);
        let report = r.poll(0.0);
        assert!(report.declared.is_empty());
        assert_eq!(r.mode(), HomeMode::Absent);
        // Même appelée de force avec une arrivée toute prête, la règle ne fait rien.
        assert_eq!(
            r.hub
                .apply_wifi_rule(&[("sam".to_string(), Whereabouts::Home)], &r.crew),
            None
        );
        assert_eq!(r.mode(), HomeMode::Absent);
    }

    #[test]
    fn phones_of_deactivated_accounts_do_not_count() {
        let r = rig("wifi-inactive");
        r.auto(true);
        r.set_mode(HomeMode::Absent);
        r.crew.deactivate("sam");
        r.crew.deactivate("lea");
        r.fake.set(&[(SAM, true), (LEA, true)]);
        let report = r.poll(0.0);
        assert!(report.declared.is_empty());
        assert_eq!(r.mode(), HomeMode::Absent);
    }

    #[test]
    fn an_unreachable_box_freezes_the_mode_and_never_means_nobody_is_home() {
        let r = rig("wifi-frozen");
        r.auto(true);
        r.both_home();
        assert_eq!(r.mode(), HomeMode::Present);
        // La box ne répond plus pendant des heures.
        r.fake.fail(BoxError::Unreachable);
        for dt in [100.0, 1_000.0, 10_000.0, 100_000.0] {
            let report = r.poll(dt);
            assert_eq!(report.link, Link::Unreachable);
            assert!(report.declared.is_empty());
            assert_eq!(report.mode, None);
        }
        assert_eq!(r.mode(), HomeMode::Present);
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Home));
        assert_eq!(r.state_of("lea"), Some(Whereabouts::Home));
        // Une liste où plus personne n'est actif ressemble à une box qui vient de redémarrer :
        // elle est ignorée aussi.
        r.fake.set(&[(SAM, false), (LEA, false)]);
        for dt in [200_000.0, 300_000.0] {
            assert_eq!(r.poll(dt).link, Link::Unreachable);
        }
        assert_eq!(r.mode(), HomeMode::Present);
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Home));
    }

    #[test]
    fn a_box_that_comes_back_does_not_count_an_absence_it_did_not_watch() {
        let r = rig("wifi-comeback");
        r.auto(true);
        r.both_home();
        r.only(&[]);
        r.poll(100.0); // l'absence commence à être comptée
        r.fake.fail(BoxError::Unreachable);
        r.poll(300.0); // panne : le compte repart de zéro
        r.only(&[]);
        let back = r.poll(1_000.0); // un seul relevé depuis la panne : trop tôt
        assert!(back.declared.is_empty());
        assert_eq!(r.mode(), HomeMode::Present);
        // Dix minutes après ce relevé, l'absence compte.
        let later = r.poll(1_000.0 + 600.0);
        assert_eq!(later.declared.len(), 2);
        assert_eq!(r.mode(), HomeMode::Absent);
    }

    #[test]
    fn a_refused_password_freezes_everything_and_is_not_replayed() {
        let r = rig("wifi-refused");
        r.auto(true);
        r.both_home();
        let before = r.fake.calls();
        r.fake.fail(BoxError::Refused);
        let report = r.poll(500.0);
        assert_eq!(report.link, Link::Refused);
        assert_eq!(r.mode(), HomeMode::Present);
        assert_eq!(r.fake.calls(), before + 1);
        // La box pourrait bloquer l'accès après des essais ratés : plus aucun relevé.
        for dt in [1_000.0, 2_000.0, 3_000.0] {
            let report = r.poll(dt);
            assert_eq!(report.link, Link::Refused);
        }
        assert_eq!(r.fake.calls(), before + 1);
        // Un nouveau mot de passe relance les relevés.
        r.fake.set(&[(SAM, true), (LEA, true)]);
        r.hub
            .wifi()
            .change_password("un-autre-mot-de-passe")
            .unwrap();
        assert_eq!(r.poll(4_000.0).link, Link::Ok);
    }

    #[test]
    fn night_and_holidays_are_chosen_by_hand_and_an_absence_never_leaves_them() {
        let r = rig("wifi-manual");
        r.auto(true);
        r.both_home();
        // Nuit : tout le monde part, la maison reste en Nuit.
        r.set_mode(HomeMode::Nuit);
        r.only(&[]);
        r.wait_for_departure(100.0);
        assert_eq!(r.state_of("lea"), Some(Whereabouts::Away));
        assert_eq!(r.mode(), HomeMode::Nuit);
        // Une arrivée pendant la nuit ne change rien non plus.
        r.only(&["sam"]);
        let report = r.poll(2_000.0);
        assert_eq!(
            report.declared,
            vec![("sam".to_string(), Whereabouts::Home)]
        );
        assert_eq!(report.mode, None);
        assert_eq!(r.mode(), HomeMode::Nuit);
        // Vacances : un départ ne fait rien (la maison est déjà armée), une arrivée remet Présent.
        r.set_mode(HomeMode::Vacances);
        r.only(&[]);
        r.wait_for_departure(3_000.0);
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Away));
        assert_eq!(r.mode(), HomeMode::Vacances);
        r.only(&["lea"]);
        let report = r.poll(5_000.0);
        assert_eq!(report.mode, Some(HomeMode::Present));
        assert_eq!(r.mode(), HomeMode::Present);
    }

    #[test]
    fn the_house_is_armed_when_the_last_person_leaves_and_comes_back_with_the_first_arrival() {
        let r = rig("wifi-rule");
        r.auto(true);
        r.both_home();
        // Sam part : Léa est encore là, la maison ne change pas.
        r.only(&["lea"]);
        r.wait_for_departure(100.0);
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Away));
        assert_eq!(r.mode(), HomeMode::Present);
        // Léa part à son tour : plus personne, Absent.
        r.only(&[]);
        let report = r.wait_for_departure(2_000.0);
        assert_eq!(
            report.declared,
            vec![("lea".to_string(), Whereabouts::Away)]
        );
        assert_eq!(r.mode(), HomeMode::Absent);
        // Sam rentre : Présent dès l'arrivée vue, sans attendre.
        r.only(&["sam"]);
        let report = r.poll(4_000.0);
        assert_eq!(report.mode, Some(HomeMode::Present));
        assert_eq!(r.mode(), HomeMode::Present);
        // Une absence plus courte que le délai ne compte pas (iPhone en veille).
        r.only(&[]);
        r.poll(4_100.0);
        r.poll(4_400.0);
        r.only(&["sam"]);
        let report = r.poll(4_500.0);
        assert!(report.declared.is_empty());
        assert_eq!(r.mode(), HomeMode::Present);
    }

    #[test]
    fn a_shortcut_said_after_the_phone_left_beats_the_wifi() {
        let r = rig("wifi-shortcut");
        r.auto(true);
        r.both_home();
        r.only(&["lea"]);
        r.poll(100.0); // le téléphone de Sam n'est plus vu
                       // « Je rentre » dit juste après : plus récent que le début de l'absence.
        r.hub
            .presence
            .declare_at("sam", Whereabouts::Home, Source::Shortcut, r.t0 + 200.0)
            .unwrap();
        let report = r.poll(900.0);
        assert!(report.declared.is_empty());
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Home));
    }

    #[test]
    fn someone_declared_home_by_a_shortcut_keeps_the_house_awake() {
        let r = rig("wifi-others");
        r.auto(true);
        // Adam n'a pas de téléphone relié mais dit « Je rentre » avec son raccourci.
        r.hub
            .presence
            .declare_at("adam", Whereabouts::Home, Source::Shortcut, r.t0)
            .unwrap();
        let crew = Crew::new(&[("sam", "Sam"), ("lea", "Léa"), ("adam", "Adam")]);
        let poll = |dt: f64| r.hub.wifi_poll_at(r.t0 + dt, &crew, true);
        poll(0.0);
        r.only(&[]);
        poll(100.0);
        poll(800.0);
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Away));
        assert_eq!(r.state_of("lea"), Some(Whereabouts::Away));
        assert_eq!(r.mode(), HomeMode::Present);
    }

    /// « Je pars » / « Je rentre » suivent la même règle que le Wi-Fi, et seulement si l'interrupteur
    /// est allumé : interrupteur × mode, pour une arrivée comme pour un départ.
    #[test]
    fn the_shortcuts_follow_the_same_switch_and_the_same_rule_as_the_wifi() {
        use HomeMode::*;
        use Whereabouts::*;
        // (interrupteur, mode au départ, ce qu'on dit, mode attendu)
        let cases = [
            // Éteint : le comportement d'avant, direct (Nuit et Vacances respectés).
            (false, Present, Away, Absent),
            (false, Absent, Home, Present),
            (false, Vacances, Home, Present),
            (false, Nuit, Away, Nuit),
            (false, Nuit, Home, Nuit),
            (false, Vacances, Away, Vacances),
            (true, Present, Away, Absent),
            (true, Nuit, Away, Nuit),
            (true, Vacances, Away, Vacances),
            (true, Absent, Away, Absent),
            (true, Absent, Home, Present),
            (true, Vacances, Home, Present),
            (true, Nuit, Home, Nuit),
            (true, Present, Home, Present),
        ];
        for (i, (on, start, said, expected)) in cases.into_iter().enumerate() {
            let r = rig(&format!("wifi-shortcut-{i}"));
            r.auto(on);
            r.set_mode(start);
            let followed = r
                .hub
                .follow_presence("sam", said, |id| r.crew.is_active(id), &Who::default())
                .unwrap();
            assert_eq!(r.mode(), expected, "interrupteur {on}, {start:?}, {said:?}");
            assert_eq!(followed.mode, expected);
            assert_eq!(followed.changed, expected != start);
            assert_eq!(followed.auto, on);
        }
        // Box injoignable : un « Je pars » n'arme pas la maison (les relevés peuvent être périmés),
        // mais une arrivée désarme.
        let r = rig("wifi-shortcut-down");
        r.auto(true);
        r.both_home();
        r.fake.fail(BoxError::Unreachable);
        assert_eq!(r.poll(10_000.0).link, Link::Unreachable);
        r.hub
            .presence
            .declare_at("lea", Away, Source::Wifi, r.t0 - 100.0)
            .unwrap();
        let followed = r
            .hub
            .follow_presence("sam", Away, |id| r.crew.is_active(id), &Who::default())
            .unwrap();
        assert!(followed.box_down && !followed.changed);
        assert_eq!(r.mode(), HomeMode::Present);
        assert_eq!(
            presence::message(Away, &followed, &[]),
            "La box ne répond pas : la maison reste en Présent."
        );
        r.set_mode(HomeMode::Absent);
        let followed = r
            .hub
            .follow_presence("sam", Home, |id| r.crew.is_active(id), &Who::default())
            .unwrap();
        assert!(followed.changed && !followed.box_down);
        assert_eq!(r.mode(), HomeMode::Present);
        // Sans téléphone relié à un compte actif, la règle marche avec les seuls raccourcis.
        let r = rig("wifi-shortcut-nophone");
        r.auto(true);
        r.hub
            .wifi_assign("sam", None, true, &Who::server_app(), "Sam")
            .unwrap();
        r.hub
            .wifi_assign("lea", None, true, &Who::server_app(), "Léa")
            .unwrap();
        r.set_mode(HomeMode::Present);
        let followed = r
            .hub
            .follow_presence("sam", Away, |id| r.crew.is_active(id), &Who::default())
            .unwrap();
        assert!(followed.auto && followed.changed && !followed.box_down);
        assert_eq!(r.mode(), HomeMode::Absent);
        // Quelqu'un d'autre est à la maison : la maison n'est pas armée.
        let r = rig("wifi-shortcut-other");
        r.auto(true);
        r.hub
            .presence
            .declare_at("lea", Home, Source::Shortcut, r.t0)
            .unwrap();
        let followed = r
            .hub
            .follow_presence("sam", Away, |id| r.crew.is_active(id), &Who::default())
            .unwrap();
        assert!(!followed.changed);
        assert_eq!(followed.others_home, vec!["lea".to_string()]);
        assert_eq!(r.mode(), HomeMode::Present);
    }

    #[test]
    fn shortcuts_work_without_a_box_and_say_what_the_house_became() {
        use Whereabouts::{Away, Home};
        let dir = temp("wifi-nobox");
        let (hub, _rx) = hub_with(&dir, 16);
        let crew = Crew::new(&[("sam", "Sam"), ("lea", "Léa")]);
        let active = |id: &str| crew.is_active(id);
        let who = Who::default();
        // Interrupteur éteint, sans box : comme avant, « Je pars » met la maison en Absent.
        let f = hub.follow_presence("sam", Away, active, &who).unwrap();
        assert!(!f.auto && f.changed && !f.box_down);
        assert_eq!(hub.mode(), HomeMode::Absent);
        assert_eq!(presence::message(Away, &f, &[]), "Maison en mode Absent.");
        let f = hub.follow_presence("sam", Home, active, &who).unwrap();
        assert!(f.changed);
        assert_eq!(hub.mode(), HomeMode::Present);
        // Éteint, la personne n'a pas à attendre les autres : l'ancien comportement.
        hub.presence
            .declare_at("lea", Home, Source::Shortcut, 10.0)
            .unwrap();
        hub.follow_presence("sam", Away, active, &who).unwrap();
        assert_eq!(hub.mode(), HomeMode::Absent);
        hub.follow_presence("sam", Home, active, &who).unwrap();
        // Allumé sans box : la règle suit les seuls raccourcis. Léa est là : pas d'armement.
        hub.wifi_set_auto_mode(true, &Who::server_app()).unwrap();
        let f = hub.follow_presence("sam", Away, active, &who).unwrap();
        assert!(f.auto && !f.changed && !f.box_down);
        assert_eq!(f.others_home, vec!["lea".to_string()]);
        assert_eq!(hub.mode(), HomeMode::Present);
        // Léa part à son tour : la dernière personne a dit partir, la maison s'arme (sans box, la
        // règle « aucun téléphone relié » ne s'applique pas).
        hub.presence
            .declare_at("lea", Away, Source::Shortcut, 20.0)
            .unwrap();
        let f = hub.follow_presence("lea", Away, active, &who).unwrap();
        assert!(f.changed, "{f:?}");
        assert_eq!(hub.mode(), HomeMode::Absent);
        assert_eq!(presence::message(Away, &f, &[]), "Maison en mode Absent.");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Une déclaration « à la maison » arrive après la décision de partir mais avant son application
    /// (le verrou du mode est tenu ailleurs) : la maison n'est pas armée.
    #[test]
    fn a_declaration_arriving_before_a_departure_is_applied_blocks_it() {
        use std::sync::Arc;
        use Whereabouts::{Away, Home};
        // Le chemin des raccourcis.
        let r = Arc::new(rig("wifi-race-shortcut"));
        r.auto(true);
        r.set_mode(HomeMode::Present);
        let guard = r.hub.write_lock.lock().unwrap();
        let worker = {
            let r = r.clone();
            std::thread::spawn(move || {
                r.hub
                    .follow_presence("sam", Away, |id| r.crew.is_active(id), &Who::default())
                    .unwrap()
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        r.hub
            .presence
            .declare_at("lea", Home, Source::Shortcut, r.t0)
            .unwrap();
        drop(guard);
        let followed = worker.join().unwrap();
        assert!(!followed.changed);
        assert_eq!(followed.others_home, vec!["lea".to_string()]);
        assert_eq!(r.mode(), HomeMode::Present);
        // Le chemin du relevé Wi-Fi.
        let r = Arc::new(rig("wifi-race-poll"));
        r.auto(true);
        r.set_mode(HomeMode::Present);
        let guard = r.hub.write_lock.lock().unwrap();
        let worker = {
            let r = r.clone();
            std::thread::spawn(move || r.hub.apply_wifi_rule(&[("sam".to_string(), Away)], &r.crew))
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        r.hub
            .presence
            .declare_at("lea", Home, Source::Shortcut, r.t0)
            .unwrap();
        drop(guard);
        assert_eq!(worker.join().unwrap(), None);
        assert_eq!(r.mode(), HomeMode::Present);
    }

    #[test]
    fn the_away_delay_change_is_journaled() {
        let r = rig("wifi-delay-journal");
        let who = Who::server_app();
        r.hub.wifi_set_away_delay(10, &who).unwrap();
        r.hub.wifi_set_away_delay(25, &who).unwrap();
        assert_eq!(r.hub.wifi().status().away_delay_min, 25);
        let lines = r.hub.audit().read(&Filter {
            until: Some(r.t0 + 1_000_000.0),
            ..Filter::default()
        });
        let delays: Vec<_> = lines
            .iter()
            .filter(|e| e.action == Action::BoxSetting && e.detail.as_deref() == Some("delay"))
            .collect();
        // Redire la même valeur ne change rien, donc ne s'écrit pas.
        assert_eq!(delays.len(), 1, "{delays:?}");
        assert_eq!(delays[0].target.as_deref(), Some("25"));
        let (_, text) = crate::audit::describe(delays[0]);
        assert_eq!(text, "Délai avant de compter un départ réglé sur 25 min");
    }

    #[test]
    fn a_manual_choice_made_meanwhile_is_never_overwritten() {
        let r = rig("wifi-race");
        r.auto(true);
        r.both_home();
        // La décision se prend sur le mode du moment, sous le verrou : Nuit choisi entre-temps.
        r.set_mode(HomeMode::Nuit);
        let applied = r
            .hub
            .set_mode_if(
                |current| presence::auto_mode(Movement::LastDeparture, current),
                ModeSource::Presence,
                ModeCause {
                    who: Who::default(),
                    origin: Origin::Automatic,
                    detail: None,
                },
            )
            .unwrap();
        assert_eq!(applied, None);
        assert_eq!(r.mode(), HomeMode::Nuit);
    }

    #[test]
    fn an_automatic_change_is_journaled_with_the_person_concerned() {
        let r = rig("wifi-journal");
        r.auto(true);
        r.both_home();
        r.only(&[]);
        r.wait_for_departure(100.0);
        assert_eq!(r.mode(), HomeMode::Absent);
        let lines = r.hub.audit().read(&Filter {
            until: Some(r.t0 + 1_000_000.0),
            ..Filter::default()
        });
        let mode = lines
            .iter()
            .find(|e| e.action == Action::ModeChanged)
            .expect("le changement de mode est noté");
        assert_eq!(mode.target.as_deref(), Some("absent"));
        assert_eq!(mode.source, Some(Origin::Automatic));
        assert!(matches!(mode.user.as_deref(), Some("lea" | "sam")));
        assert!(matches!(mode.user_name.as_deref(), Some("Léa" | "Sam")));
        let (_, text) = crate::audit::describe(mode);
        assert_eq!(text, "La maison passe en Absent (automatique, départ)");
        // Et chaque arrivée ou départ vu sur le Wi-Fi a sa ligne, au nom de la personne.
        let seen: Vec<_> = lines
            .iter()
            .filter(|e| e.action == Action::Presence && e.detail.as_deref() == Some("wifi"))
            .collect();
        assert_eq!(seen.len(), 4, "{seen:?}");
        assert!(seen.iter().all(|e| e.user_name.is_some()));
        // Ni le mot de passe de la box, ni un nom de téléphone, ni une adresse dans le journal.
        let raw = lines
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<String>();
        for secret in ["mot-de-passe-de-la-box", SAM, LEA, "box.test"] {
            assert!(!raw.contains(secret), "{secret} dans le journal : {raw}");
        }
    }

    #[test]
    fn a_member_only_sees_the_free_phone_names_and_the_phone_is_exclusive() {
        let r = rig("wifi-phones");
        r.fake.set(&[
            (SAM, true),
            (LEA, true),
            ("Redmi-de-Adam", true),
            ("iPhone-7", false),
            ("android-3f2a", true),
            (NAS, true),
        ]);
        r.poll(0.0);
        // Ni les appareils déjà attribués, ni les téléphones absents, ni ce qui n'est pas un
        // téléphone.
        assert_eq!(
            r.hub.wifi().free_hosts(true),
            vec!["android-3f2a", "Redmi-de-Adam"]
        );
        // L'administrateur voit aussi les autres appareils.
        assert_eq!(
            r.hub.wifi().free_hosts(false),
            vec!["android-3f2a", NAS, "Redmi-de-Adam"]
        );
        let who = Who::default();
        // Un membre ne choisit que parmi les noms de téléphones libres.
        assert_eq!(
            r.hub.wifi_assign("adam", Some(NAS), true, &who, "Adam"),
            Err(AssignError::NotFree)
        );
        assert_eq!(
            r.hub
                .wifi_assign("adam", Some("iPhone-7"), true, &who, "Adam"),
            Err(AssignError::NotFree)
        );
        assert_eq!(
            r.hub.wifi_assign("adam", Some(SAM), true, &who, "Adam"),
            Err(AssignError::Taken)
        );
        assert!(r
            .hub
            .wifi_assign("adam", Some("redmi-de-adam"), true, &who, "Adam")
            .unwrap());
        assert_eq!(
            r.hub.wifi().phone_of("adam").as_deref(),
            Some("Redmi-de-Adam")
        );
        // Il n'est plus libre pour personne.
        assert_eq!(r.hub.wifi().free_hosts(true), vec!["android-3f2a"]);
        // L'administrateur corrige : il peut attribuer un appareil qui n'a pas un nom de téléphone.
        assert!(r
            .hub
            .wifi_assign("adam", Some(NAS), false, &Who::server_app(), "Adam")
            .unwrap());
        assert_eq!(
            r.hub.wifi().free_hosts(true),
            vec!["android-3f2a", "Redmi-de-Adam"]
        );
        // Les téléphones se retrouvent après un redémarrage.
        let reopened = Wifi::open(&r.dir.join("serveur"), Box::new(r.fake.clone()));
        assert_eq!(reopened.phone_of("sam").as_deref(), Some(SAM));
        assert_eq!(reopened.phone_of("adam").as_deref(), Some(NAS));
    }

    #[test]
    fn changing_or_removing_a_phone_forgets_what_the_wifi_said_of_its_owner() {
        let r = rig("wifi-forget");
        r.both_home();
        assert_eq!(r.state_of("sam"), Some(Whereabouts::Home));
        r.hub
            .wifi_assign("sam", None, true, &Who::server_app(), "Sam")
            .unwrap();
        // Sans téléphone, plus rien ne garde Sam « à la maison » : sa déclaration ancienne est effacée.
        assert_eq!(r.state_of("sam"), None);
        // Ce que Léa a dit elle-même n'est pas touché.
        r.hub
            .presence
            .declare_at("lea", Whereabouts::Away, Source::Shortcut, r.t0 + 10.0)
            .unwrap();
        r.hub
            .wifi_assign("lea", None, true, &Who::server_app(), "Léa")
            .unwrap();
        assert_eq!(r.state_of("lea"), Some(Whereabouts::Away));
    }

    #[test]
    fn the_box_password_is_a_secret_file_and_never_shown() {
        let r = rig("wifi-secret");
        let path = r.dir.join("serveur/box.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mot-de-passe-de-la-box"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Les téléphones : seulement des noms, jamais une adresse.
        let phones = std::fs::read_to_string(r.dir.join("serveur/telephones.json")).unwrap();
        assert!(phones.contains(SAM));
        assert!(!phones.contains("mot-de-passe"));
        // Les réglages affichables ne portent pas le mot de passe.
        let shown = format!("{:?} {:?}", r.hub.wifi().status(), r.hub.wifi().url());
        assert!(!shown.contains("mot-de-passe-de-la-box"), "{shown}");
        // Délier efface le mot de passe du fichier ; l'interrupteur, lui, ne dépend pas de la box.
        r.auto(true);
        r.hub.wifi_unlink(&Who::server_app()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("mot-de-passe-de-la-box"), "{text}");
        assert!(text.contains("\"auto_mode\": true"));
        assert!(!r.hub.wifi().status().linked);
        // Le journal des réglages ne dit pas le mot de passe non plus.
        let lines = r.hub.audit().read(&Filter {
            until: Some(r.t0 + 1_000_000.0),
            ..Filter::default()
        });
        let raw: String = lines
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        assert!(!raw.contains("mot-de-passe"), "{raw}");
        assert!(lines.iter().any(|e| e.action == Action::BoxSetting));
    }

    #[test]
    fn linking_checks_the_password_before_keeping_it() {
        let dir = temp("wifi-link");
        let (mut hub, _rx) = hub_with(&dir, 16);
        let fake = FakeBox::default();
        hub.wifi = Wifi::open(&dir.join("serveur"), Box::new(fake.clone()));
        let who = Who::server_app();
        fake.fail(BoxError::Refused);
        assert_eq!(
            hub.wifi_link("http://box.test", "mauvais", &who),
            Err(LinkError::Refused)
        );
        fake.fail(BoxError::Unreachable);
        assert_eq!(
            hub.wifi_link("http://box.test", "bon", &who),
            Err(LinkError::Unreachable)
        );
        assert_eq!(
            hub.wifi_link("ftp://box", "bon", &who),
            Err(LinkError::BadUrl)
        );
        // Rien n'a été gardé.
        assert!(!hub.wifi().status().linked);
        assert!(!dir.join("serveur/box.json").exists());
        fake.set(&[(SAM, true)]);
        hub.wifi_link("http://box.test", "bon", &who).unwrap();
        assert!(hub.wifi().status().linked);
        assert_eq!(hub.wifi().status().link, Link::Ok);
        assert!(!hub.wifi().auto_mode());
        let _ = std::fs::remove_dir_all(dir);
    }
}
