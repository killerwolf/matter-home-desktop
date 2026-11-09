//! Un document partagé (le plan, les règles) modifié ici et par d'autres clients.
//!
//! La règle est « dernière modification gagnante » (docs/protocole.md) : chaque client envoie
//! le document entier, le serveur le garde et le diffuse à tous. Ce suivi décide quoi faire de
//! la version qui arrive, et quand envoyer la nôtre :
//! - rien n'est en cours d'édition ici : on adopte la version du serveur telle quelle ;
//! - c'est l'écho de notre propre envoi : rien à faire ;
//! - un autre client a modifié pendant que l'on éditait : on **garde** notre édition, on propose
//!   de voir l'autre version, et on n'envoie rien tant que l'utilisateur n'a pas choisi (jamais
//!   d'écrasement par surprise) ;
//! - un document **incomplet** (une action sans adresse, par exemple) reste un brouillon visible :
//!   il n'est pas envoyé, le serveur le refuserait ;
//! - un **refus** du serveur (400) garde le brouillon, avec le motif ; il repart à la prochaine
//!   modification. Un autre refus définitif (4xx) revient à la version du serveur. Une panne
//!   passagère (délai dépassé, 503…) garde le document à envoyer et réessaie plus tard ;
//! - une **coupure** n'efface pas le brouillon ([`SyncState::suspend`]) : à la reprise, il repart
//!   si le serveur n'a pas changé entre-temps, sinon l'utilisateur est prévenu.
//!
//! Un seul envoi est en vol à la fois par document, et les modifications rapprochées sont
//! regroupées (délai de calme) : deux requêtes simultanées pourraient arriver dans le désordre
//! chez le serveur, et chaque frappe ne doit pas faire une requête.

use std::time::{Duration, Instant};

use matter_home_client::net::NetError;
use protocol::wire::ErrorCode;

/// Délai de calme avant d'envoyer une modification.
pub const DEBOUNCE: Duration = Duration::from_millis(400);
/// Attente avant de renvoyer après une panne passagère (doublée à chaque échec).
const RETRY_BASE: Duration = Duration::from_secs(3);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Adoptée : le document affiché a été remplacé.
    Adopted,
    /// Écho de notre envoi, ou version identique : rien à faire.
    Echo,
    /// Un autre client a modifié pendant notre édition : bandeau « modifié ailleurs ».
    Conflict,
}

/// Comment un envoi a échoué.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Le serveur refuse ce contenu (400) : le brouillon reste, avec ce motif.
    Refused(String),
    /// Autre refus définitif (404, 413…) : retour à la version du serveur.
    Rejected,
    /// Panne passagère (délai dépassé, serveur occupé, coupure) : on réessaiera.
    Retry(String),
}

impl Failure {
    pub fn of(error: &NetError) -> Failure {
        let message = error.message();
        match error {
            NetError::Api { status, code, .. }
                if *status == 400
                    || matches!(
                        code,
                        Some(ErrorCode::BadRequest | ErrorCode::PlanWithoutFloor)
                    ) =>
            {
                Failure::Refused(message)
            }
            // 408 (corps trop lent) et 429 passent : ce n'est pas le contenu qui est refusé.
            NetError::Api { status, .. }
                if (400..500).contains(status) && *status != 408 && *status != 429 =>
            {
                Failure::Rejected
            }
            _ => Failure::Retry(message),
        }
    }
}

/// Ce qui empêche d'envoyer un document tel qu'il est (`None` : il peut partir).
pub type Check<T> = fn(&T) -> Option<String>;

fn complete<T>(_: &T) -> Option<String> {
    None
}

#[derive(Debug)]
pub struct SyncState<T> {
    server: Option<T>,
    dirty: bool,
    changed_at: Option<Instant>,
    in_flight: Option<(u64, T)>,
    /// Version d'un autre client reçue pendant une édition locale.
    remote: Option<T>,
    /// Le serveur a refusé le brouillon (400) : motif. Le brouillon reste, sans être renvoyé.
    refused: Option<String>,
    /// Pas de nouvel envoi avant cet instant (panne passagère).
    not_before: Option<Instant>,
    retries: u32,
    /// Dernière panne passagère, pour le dire à l'utilisateur.
    waiting: Option<String>,
    /// Après une coupure : version du serveur sur laquelle le brouillon était fondé…
    base: Option<T>,
    /// … et envoi dont on ignore s'il est arrivé.
    maybe_sent: Option<T>,
    check: Check<T>,
}

impl<T> Default for SyncState<T> {
    fn default() -> Self {
        Self::with_check(complete::<T>)
    }
}

impl<T> SyncState<T> {
    /// `check` dit ce qui rend un document impossible à envoyer (brouillon).
    pub fn with_check(check: Check<T>) -> Self {
        Self {
            server: None,
            dirty: false,
            changed_at: None,
            in_flight: None,
            remote: None,
            refused: None,
            not_before: None,
            retries: 0,
            waiting: None,
            base: None,
            maybe_sent: None,
            check,
        }
    }
}

impl<T: Clone + PartialEq> SyncState<T> {
    /// Une version du serveur est connue.
    #[cfg(test)]
    pub fn is_ready(&self) -> bool {
        self.server.is_some()
    }

    /// Une modification locale n'est pas (tout à fait) arrivée au serveur.
    pub fn editing(&self) -> bool {
        self.dirty || self.in_flight.is_some() || self.refused.is_some()
    }

    pub fn remote(&self) -> Option<&T> {
        self.remote.as_ref()
    }

    /// L'utilisateur vient de modifier le document affiché.
    pub fn mark_changed(&mut self, now: Instant) {
        self.dirty = true;
        self.changed_at = Some(now);
        // Une nouvelle modification relance l'envoi, même après un refus.
        self.refused = None;
        self.not_before = None;
        self.retries = 0;
        self.waiting = None;
    }

    /// Ce qu'il faut dire à l'utilisateur d'un document qui n'est pas parti, s'il y a lieu.
    pub fn notice(&self, local: &T) -> Option<String> {
        if let Some(why) = &self.refused {
            return Some(format!(
                "Le serveur a refusé cette version : {why} Elle reste ici, non enregistrée : \
                 corrigez-la."
            ));
        }
        if self.dirty {
            if let Some(why) = (self.check)(local) {
                return Some(format!("Brouillon non envoyé : {why}."));
            }
            if let Some(why) = &self.waiting {
                return Some(format!(
                    "Pas encore enregistré ({why}) : nouvel essai automatique."
                ));
            }
        }
        None
    }

    /// Ce qui empêche, tant que rien ne change, d'enregistrer la version affichée. Sert à
    /// refuser d'attendre un enregistrement qui n'aura pas lieu.
    pub fn blocked(&self, local: &T) -> Option<String> {
        if self.remote.is_some() {
            return Some(
                "un autre client a modifié ce document : choisissez quelle version garder".into(),
            );
        }
        if let Some(why) = &self.refused {
            return Some(why.clone());
        }
        if self.dirty {
            return (self.check)(local);
        }
        None
    }

    /// Version du serveur (état complet ou événement). `local` est le document affiché.
    pub fn on_server(&mut self, incoming: T, local: &mut T) -> Incoming {
        let previous = self.server.replace(incoming.clone());
        // Première version reçue depuis une coupure, avec un brouillon gardé.
        if previous.is_none() && self.editing() {
            return self.resume(incoming, local);
        }
        let own_echo = self
            .in_flight
            .as_ref()
            .is_some_and(|(_, sent)| *sent == incoming);
        if own_echo || incoming == *local {
            self.remote = None;
            if incoming == *local && self.in_flight.is_none() {
                self.dirty = false;
                self.refused = None;
                self.waiting = None;
            }
            return Incoming::Echo;
        }
        if self.editing() {
            // Le serveur rediffuse la version que l'on connaissait déjà (resynchronisation) :
            // personne d'autre n'a rien changé.
            if previous.as_ref() == Some(&incoming) {
                return Incoming::Echo;
            }
            self.remote = Some(incoming);
            return Incoming::Conflict;
        }
        *local = incoming;
        self.remote = None;
        Incoming::Adopted
    }

    /// Première version du serveur après une coupure qui a laissé un brouillon.
    fn resume(&mut self, incoming: T, local: &mut T) -> Incoming {
        let sent = self.maybe_sent.take();
        let base = self.base.take();
        self.remote = None;
        if incoming == *local {
            // Le serveur a déjà ce que l'on affiche (notre envoi est arrivé, ou rien à dire).
            self.dirty = false;
            self.refused = None;
            self.waiting = None;
            self.in_flight = None;
            return Incoming::Echo;
        }
        if sent.as_ref() == Some(&incoming) || base.as_ref() == Some(&incoming) {
            // Le serveur est tel que nous le connaissions (ou a reçu notre envoi) : le brouillon
            // repart tout seul.
            return Incoming::Echo;
        }
        // Quelqu'un d'autre a modifié pendant la coupure : on ne l'écrase pas en silence.
        self.remote = Some(incoming);
        Incoming::Conflict
    }

    /// La liaison est coupée. S'il y a un brouillon (non envoyé, refusé ou dont on ignore le
    /// sort), il est **gardé** pour la reprise et `true` est rendu ; sinon l'appelant remet le
    /// document à zéro.
    pub fn suspend(&mut self, now: Instant) -> bool {
        if !self.editing() && self.base.is_none() {
            return false;
        }
        if let Some(server) = self.server.take() {
            self.base = Some(server);
        }
        if let Some((_, content)) = self.in_flight.take() {
            // Notre envoi est peut-être arrivé, peut-être pas.
            self.maybe_sent = Some(content);
            self.dirty = true;
            self.changed_at.get_or_insert(now);
        }
        true
    }

    /// Y a-t-il quelque chose à envoyer maintenant ? Rend le document à envoyer ; l'appelant
    /// l'envoie puis appelle [`SyncState::sent`]. Rien tant qu'un envoi est en vol, ni pendant
    /// que l'utilisateur est en train de manipuler (`holding`, souris enfoncée), ni pour un
    /// brouillon incomplet ou refusé, ni tant qu'un autre client a modifié sans que l'on ait
    /// choisi.
    pub fn due(&self, now: Instant, holding: bool, local: &T) -> Option<T> {
        let calm = self
            .changed_at
            .is_some_and(|t| now.saturating_duration_since(t) >= DEBOUNCE);
        let delayed = self.not_before.is_some_and(|t| now < t);
        (calm && !holding && !delayed)
            .then(|| self.sendable(local))
            .flatten()
    }

    /// Comme [`SyncState::due`], sans attendre le délai de calme (fermeture de la fenêtre,
    /// ou avant d'exécuter une règle qui doit voir la dernière version).
    pub fn due_now(&self, local: &T) -> Option<T> {
        self.sendable(local)
    }

    fn sendable(&self, local: &T) -> Option<T> {
        (self.dirty
            && self.in_flight.is_none()
            && self.server.is_some()
            && self.remote.is_none()
            && self.refused.is_none()
            && (self.check)(local).is_none())
        .then(|| local.clone())
    }

    /// Un envoi est en vol, ou un document prêt à partir attend son tour : l'interface doit se
    /// réveiller. Un brouillon incomplet ou refusé n'a besoin de personne.
    pub fn needs_tick(&self, local: &T) -> bool {
        self.in_flight.is_some() || self.sendable(local).is_some()
    }

    /// Un envoi est en vol.
    pub fn in_flight_id(&self) -> Option<u64> {
        self.in_flight.as_ref().map(|(id, _)| *id)
    }

    pub fn sent(&mut self, request_id: u64, content: T) {
        self.dirty = false;
        self.in_flight = Some((request_id, content));
    }

    /// Le serveur a accepté l'envoi `request_id` et renvoie le document tel qu'il l'a gardé.
    pub fn on_saved(&mut self, request_id: u64, stored: Option<T>, local: &mut T) {
        if self.in_flight_id() != Some(request_id) {
            return;
        }
        self.in_flight = None;
        self.retries = 0;
        self.not_before = None;
        self.waiting = None;
        if let Some(stored) = stored {
            self.server = Some(stored.clone());
            if self.remote.as_ref() == Some(&stored) {
                self.remote = None;
            }
            if !self.dirty && *local != stored {
                *local = stored;
            }
        }
    }

    /// L'envoi `request_id` a échoué. Rend `true` si c'était bien notre envoi.
    pub fn on_failed(
        &mut self,
        request_id: u64,
        failure: Failure,
        now: Instant,
        local: &mut T,
    ) -> bool {
        if self.in_flight_id() != Some(request_id) {
            return false;
        }
        self.in_flight = None;
        match failure {
            Failure::Refused(why) => {
                // Le brouillon reste tel quel : il n'est pas renvoyé tant qu'il ne change pas.
                self.dirty = false;
                self.refused = Some(why);
            }
            Failure::Rejected => {
                self.dirty = false;
                self.remote = None;
                if let Some(server) = &self.server {
                    *local = server.clone();
                }
            }
            Failure::Retry(why) => {
                self.dirty = true;
                self.retries += 1;
                let wait = RETRY_BASE
                    .saturating_mul(1 << self.retries.saturating_sub(1).min(4))
                    .min(RETRY_MAX);
                self.not_before = Some(now + wait);
                self.waiting = Some(why);
            }
        }
        true
    }

    /// Peut-on abandonner le brouillon pour la version du serveur ? Non sans version connue
    /// (liaison coupée), ni pendant un envoi : sa réponse réécrirait le document affiché.
    pub fn can_revert(&self) -> bool {
        self.server.is_some() && self.in_flight.is_none()
    }

    /// « Revenir à la version du serveur » : abandonne le brouillon (incomplet, refusé, en attente
    /// d'un nouvel essai, ou en conflit avec l'autre client) et affiche la dernière version connue
    /// du serveur. `false` (rien n'est touché) si [`SyncState::can_revert`] est faux.
    pub fn revert(&mut self, local: &mut T) -> bool {
        if !self.can_revert() {
            return false;
        }
        if let Some(server) = &self.server {
            *local = server.clone();
        }
        self.dirty = false;
        self.changed_at = None;
        self.remote = None;
        self.refused = None;
        self.not_before = None;
        self.retries = 0;
        self.waiting = None;
        self.base = None;
        self.maybe_sent = None;
        true
    }

    /// Bandeau « modifié ailleurs » → « Afficher sa version ».
    pub fn accept_remote(&mut self, local: &mut T) {
        if let Some(remote) = self.remote.take() {
            *local = remote;
            self.dirty = false;
            self.refused = None;
            self.waiting = None;
        }
    }

    /// Bandeau « modifié ailleurs » → « Garder la mienne » : la prochaine sauvegarde gagnera.
    pub fn keep_mine(&mut self, now: Instant) {
        if self.remote.take().is_some() {
            self.mark_changed(now);
        }
    }

    /// Envoyer dès que possible, sans attendre le délai de calme.
    pub fn save_asap(&mut self, now: Instant) {
        if self.dirty {
            self.changed_at = Some(now.checked_sub(DEBOUNCE).unwrap_or(now));
            self.not_before = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn reverting_drops_every_kind_of_draft_and_never_races_an_upload() {
        let mut s = SyncState::<String>::with_check(url_must_be_filled);
        let mut doc = "serveur http".to_string();
        s.on_server(doc.clone(), &mut doc);
        // Brouillon incomplet.
        doc = "brouillon".into();
        s.mark_changed(t0());
        assert!(s.notice(&doc).is_some() && s.editing());
        assert!(s.revert(&mut doc));
        assert_eq!(doc, "serveur http");
        assert!(!s.editing() && s.notice(&doc).is_none());
        // En conflit avec un autre client : la version de l'autre est oubliée aussi.
        doc = "ma version http".into();
        s.mark_changed(t0());
        assert_eq!(
            s.on_server("autre http".into(), &mut doc),
            Incoming::Conflict
        );
        assert!(s.remote().is_some());
        assert!(s.revert(&mut doc));
        assert_eq!(doc, "autre http", "la dernière version du serveur");
        assert!(s.remote().is_none() && !s.editing());
        // Refusé par le serveur.
        doc = "refusé http".into();
        s.mark_changed(t0());
        s.sent(7, doc.clone());
        // Un envoi est en vol : sa réponse réécrirait le document, on ne revient pas.
        assert!(!s.can_revert() && !s.revert(&mut doc));
        assert_eq!(doc, "refusé http");
        s.on_failed(7, Failure::Refused("non".into()), t0(), &mut doc);
        assert!(s.revert(&mut doc));
        assert_eq!(doc, "autre http");
        assert!(!s.editing());
        // Sans version du serveur (liaison coupée) : impossible.
        let mut cut = SyncState::<String>::default();
        let mut text = "a".to_string();
        cut.on_server("a".into(), &mut text);
        text = "b".into();
        cut.mark_changed(t0());
        cut.suspend(t0());
        assert!(!cut.can_revert() && !cut.revert(&mut text));
        assert_eq!(text, "b");
    }

    #[test]
    fn idle_documents_adopt_what_the_server_sends() {
        let mut s = SyncState::<String>::default();
        let mut local = "a".to_string();
        assert!(!s.is_ready());
        assert_eq!(s.on_server("b".into(), &mut local), Incoming::Adopted);
        assert_eq!(local, "b");
        assert!(s.is_ready());
        assert_eq!(s.on_server("b".into(), &mut local), Incoming::Echo);
    }

    #[test]
    fn a_remote_change_never_overwrites_an_edit_in_progress() {
        let mut s = SyncState::<String>::default();
        let mut local = "a".to_string();
        s.on_server("a".into(), &mut local);
        local = "mon édition".into();
        s.mark_changed(t0());
        assert_eq!(s.on_server("autre".into(), &mut local), Incoming::Conflict);
        assert_eq!(local, "mon édition");
        assert_eq!(s.remote(), Some(&"autre".to_string()));
        // « Afficher sa version » remplace.
        s.accept_remote(&mut local);
        assert_eq!(local, "autre");
        assert!(!s.editing() && s.remote().is_none());
    }

    #[test]
    fn keeping_mine_makes_the_next_save_win() {
        let mut s = SyncState::<String>::default();
        let mut local = "a".to_string();
        s.on_server("a".into(), &mut local);
        local = "mine".into();
        let start = t0();
        s.mark_changed(start);
        s.on_server("autre".into(), &mut local);
        s.keep_mine(start);
        assert!(s.remote().is_none());
        let due = s.due(start + DEBOUNCE, false, &local);
        assert_eq!(due.as_deref(), Some("mine"));
    }

    #[test]
    fn saves_are_debounced_single_flight_and_wait_for_the_pointer() {
        let mut s = SyncState::<String>::default();
        let mut local = "a".to_string();
        s.on_server("a".into(), &mut local);
        let start = t0();
        local = "ab".into();
        s.mark_changed(start);
        assert_eq!(
            s.due(start, false, &local),
            None,
            "pas avant le délai de calme"
        );
        assert_eq!(
            s.due(start + DEBOUNCE, true, &local),
            None,
            "souris enfoncée"
        );
        assert_eq!(
            s.due(start + DEBOUNCE, false, &local).as_deref(),
            Some("ab")
        );
        s.sent(7, "ab".into());
        // Pendant le vol, une nouvelle modification attend : un seul envoi à la fois.
        local = "abc".into();
        s.mark_changed(start + DEBOUNCE);
        assert_eq!(s.due(start + DEBOUNCE * 3, false, &local), None);
        s.on_saved(7, Some("ab".into()), &mut local);
        assert_eq!(
            local, "abc",
            "la saisie en cours n'est pas écrasée par la réponse"
        );
        assert_eq!(
            s.due(start + DEBOUNCE * 3, false, &local).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn own_echo_is_not_a_conflict_even_when_it_arrives_before_the_reply() {
        let mut s = SyncState::<String>::default();
        let mut local = "a".to_string();
        s.on_server("a".into(), &mut local);
        local = "b".into();
        s.mark_changed(t0());
        s.sent(1, "b".into());
        // Un autre client écrit « c » juste avant nous, puis notre écho arrive.
        assert_eq!(s.on_server("c".into(), &mut local), Incoming::Conflict);
        assert_eq!(s.on_server("b".into(), &mut local), Incoming::Echo);
        assert!(
            s.remote().is_none(),
            "la dernière version du serveur est la nôtre"
        );
        s.on_saved(1, Some("b".into()), &mut local);
        assert!(!s.editing());
    }

    #[test]
    fn a_rejected_save_goes_back_to_the_server_version() {
        let mut s = SyncState::<String>::default();
        let mut local = "ok".to_string();
        s.on_server("ok".into(), &mut local);
        local = "invalide".into();
        s.mark_changed(t0());
        s.sent(3, "invalide".into());
        let now = t0();
        assert!(
            !s.on_failed(99, Failure::Rejected, now, &mut local),
            "numéro inconnu : ignoré"
        );
        assert!(s.on_failed(3, Failure::Rejected, now, &mut local));
        assert_eq!(local, "ok");
        assert!(!s.editing());
    }

    #[test]
    fn nothing_is_sent_before_the_server_state_is_known() {
        let mut s = SyncState::<String>::default();
        s.mark_changed(t0());
        assert_eq!(s.due(t0() + DEBOUNCE * 2, false, &"x".to_string()), None);
    }

    fn edited(local: &str, now: Instant) -> (SyncState<String>, String) {
        let mut s = SyncState::<String>::default();
        let mut doc = "a".to_string();
        s.on_server("a".into(), &mut doc);
        doc = local.into();
        s.mark_changed(now);
        (s, doc)
    }

    #[allow(clippy::ptr_arg)] // `Check<String>` est un `fn(&String)`
    fn url_must_be_filled(doc: &String) -> Option<String> {
        (!doc.contains("http")).then(|| "l'adresse est vide".to_string())
    }

    #[test]
    fn an_incomplete_draft_is_never_sent_and_says_why() {
        let mut s = SyncState::<String>::with_check(url_must_be_filled);
        let mut doc = "a http".to_string();
        s.on_server(doc.clone(), &mut doc);
        let start = t0();
        doc = "a http, webhook:".into();
        s.mark_changed(start);
        assert!(s.due(start + DEBOUNCE, false, &doc).is_some());
        doc = "webhook:".into();
        s.mark_changed(start);
        assert_eq!(s.due(start + DEBOUNCE * 10, false, &doc), None);
        assert_eq!(s.due_now(&doc), None);
        assert!(s.notice(&doc).unwrap().contains("l'adresse est vide"));
        assert!(s.blocked(&doc).is_some());
        // Un serveur qui renvoie l'ancienne version ne l'écrase pas.
        assert_eq!(s.on_server("a http".into(), &mut doc), Incoming::Echo);
        assert_eq!(doc, "webhook:");
        // Complété, il part.
        doc = "webhook: http://x".into();
        s.mark_changed(start);
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc).as_deref(),
            Some("webhook: http://x")
        );
        assert!(s.notice(&doc).is_none());
    }

    #[test]
    fn a_400_keeps_the_draft_with_the_reason_until_it_changes() {
        let start = t0();
        let (mut s, mut doc) = edited("brouillon", start);
        s.sent(5, doc.clone());
        assert!(s.on_failed(
            5,
            Failure::Refused("adresse invalide.".into()),
            start,
            &mut doc
        ));
        assert_eq!(doc, "brouillon", "le brouillon est gardé");
        assert!(s.editing());
        assert!(s.notice(&doc).unwrap().contains("adresse invalide"));
        // Pas de boucle de 400 : rien n'est renvoyé tant que rien ne change.
        assert_eq!(s.due(start + DEBOUNCE * 50, false, &doc), None);
        assert_eq!(s.due_now(&doc), None);
        // Une version d'un autre client n'écrase pas le brouillon refusé : bandeau.
        assert_eq!(s.on_server("autre".into(), &mut doc), Incoming::Conflict);
        assert_eq!(doc, "brouillon");
        // Une modification relance l'envoi.
        s.keep_mine(start);
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc).as_deref(),
            Some("brouillon")
        );
    }

    #[test]
    fn only_a_definitive_4xx_goes_back_a_transient_failure_keeps_the_document() {
        let start = t0();
        let (mut s, mut doc) = edited("ma version", start);
        s.sent(1, doc.clone());
        // Délai dépassé / 503 : le document reste à envoyer, avec un nouvel essai plus tard.
        assert!(s.on_failed(1, Failure::Retry("délai".into()), start, &mut doc));
        assert_eq!(doc, "ma version");
        assert!(s.editing());
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc),
            None,
            "pas tout de suite"
        );
        assert!(s.notice(&doc).unwrap().contains("nouvel essai"));
        let later = start + Duration::from_secs(4);
        assert_eq!(s.due(later, false, &doc).as_deref(), Some("ma version"));
        // Le second échec attend plus longtemps encore.
        s.sent(2, doc.clone());
        s.on_failed(2, Failure::Retry("délai".into()), later, &mut doc);
        assert_eq!(s.due(later + Duration::from_secs(4), false, &doc), None);
        assert!(s.due(later + Duration::from_secs(7), false, &doc).is_some());
        // Un refus définitif (404, 413…) revient à la version du serveur.
        s.sent(3, doc.clone());
        s.on_failed(3, Failure::Rejected, later, &mut doc);
        assert_eq!(doc, "a");
        assert!(!s.editing());
    }

    #[test]
    fn failures_are_classified_from_the_http_status() {
        use protocol::wire::ErrorCode as C;
        let api = |status, code| NetError::Api {
            status,
            code,
            detail: None,
            retry_after_s: None,
        };
        assert!(matches!(
            Failure::of(&api(400, Some(C::BadRequest))),
            Failure::Refused(_)
        ));
        assert!(matches!(
            Failure::of(&api(400, Some(C::PlanWithoutFloor))),
            Failure::Refused(_)
        ));
        assert!(matches!(Failure::of(&api(400, None)), Failure::Refused(_)));
        for status in [401, 404, 405, 409, 413] {
            assert_eq!(
                Failure::of(&api(status, None)),
                Failure::Rejected,
                "{status}"
            );
        }
        for status in [408, 429, 500, 502, 503] {
            assert!(matches!(Failure::of(&api(status, None)), Failure::Retry(_)));
        }
        assert!(matches!(Failure::of(&NetError::Timeout), Failure::Retry(_)));
        assert!(matches!(
            Failure::of(&NetError::Unreachable("x".into())),
            Failure::Retry(_)
        ));
    }

    #[test]
    fn a_draft_survives_a_disconnection_and_is_resent_if_the_server_is_unchanged() {
        let start = t0();
        let (mut s, mut doc) = edited("brouillon", start);
        assert!(s.suspend(start), "un brouillon est gardé");
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc),
            None,
            "rien avant la reprise"
        );
        // Reprise : même version du serveur qu'avant la coupure.
        assert_eq!(s.on_server("a".into(), &mut doc), Incoming::Echo);
        assert_eq!(doc, "brouillon");
        assert!(s.remote().is_none());
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc).as_deref(),
            Some("brouillon")
        );
    }

    #[test]
    fn an_unanswered_save_is_neither_lost_nor_duplicated_across_a_disconnection() {
        let start = t0();
        // L'envoi est arrivé : le serveur a notre version, rien à renvoyer.
        let (mut s, mut doc) = edited("v2", start);
        s.sent(1, doc.clone());
        assert!(s.suspend(start));
        assert_eq!(s.on_server("v2".into(), &mut doc), Incoming::Echo);
        assert!(!s.editing());
        assert_eq!(s.due(start + DEBOUNCE, false, &doc), None);
        // L'envoi est arrivé, puis l'utilisateur a encore tapé : le reste repart.
        let (mut s, mut doc) = edited("v2", start);
        s.sent(1, doc.clone());
        doc = "v2 et la suite".into();
        s.mark_changed(start);
        assert!(s.suspend(start));
        assert_eq!(s.on_server("v2".into(), &mut doc), Incoming::Echo);
        assert_eq!(
            s.due(start + DEBOUNCE, false, &doc).as_deref(),
            Some("v2 et la suite")
        );
        // L'envoi n'est pas arrivé : il repart.
        let (mut s, mut doc) = edited("v2", start);
        s.sent(1, doc.clone());
        assert!(s.suspend(start));
        assert_eq!(s.on_server("a".into(), &mut doc), Incoming::Echo);
        assert_eq!(s.due(start + DEBOUNCE, false, &doc).as_deref(), Some("v2"));
    }

    #[test]
    fn a_server_that_changed_during_the_outage_warns_and_holds_the_draft() {
        let start = t0();
        let (mut s, mut doc) = edited("brouillon", start);
        assert!(s.suspend(start));
        assert_eq!(s.on_server("autre".into(), &mut doc), Incoming::Conflict);
        assert_eq!(doc, "brouillon");
        assert_eq!(s.remote(), Some(&"autre".to_string()));
        // Rien ne part tant que l'utilisateur n'a pas choisi.
        assert_eq!(s.due(start + DEBOUNCE * 20, false, &doc), None);
        assert_eq!(s.due_now(&doc), None);
        assert!(s.blocked(&doc).is_some());
        s.keep_mine(start);
        assert!(s.due(start + DEBOUNCE, false, &doc).is_some());
    }

    #[test]
    fn nothing_to_keep_means_nothing_suspended() {
        let mut s = SyncState::<String>::default();
        let mut doc = "a".to_string();
        s.on_server("a".into(), &mut doc);
        assert!(!s.suspend(t0()), "rien à garder : l'appelant remet à zéro");
    }

    #[test]
    fn a_resync_of_the_same_version_is_not_a_conflict() {
        let start = t0();
        let (mut s, mut doc) = edited("brouillon", start);
        assert_eq!(s.on_server("a".into(), &mut doc), Incoming::Echo);
        assert!(s.remote().is_none());
        assert_eq!(s.on_server("b".into(), &mut doc), Incoming::Conflict);
        assert_eq!(s.on_server("b".into(), &mut doc), Incoming::Echo);
        assert_eq!(s.remote(), Some(&"b".to_string()), "le bandeau reste");
    }

    #[test]
    fn exit_and_test_do_not_wait_for_the_calm_delay() {
        let start = t0();
        let (mut s, doc) = edited("fin", start);
        assert_eq!(s.due(start, false, &doc), None);
        assert_eq!(s.due_now(&doc).as_deref(), Some("fin"));
        s.save_asap(start);
        assert_eq!(s.due(start, true, &doc), None, "souris enfoncée");
        assert_eq!(s.due(start, false, &doc).as_deref(), Some("fin"));
    }
}
