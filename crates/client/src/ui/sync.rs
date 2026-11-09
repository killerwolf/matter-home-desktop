//! Un document partagé (le plan, les règles) modifié ici et par d'autres clients.
//!
//! La règle est « dernière modification gagnante » (docs/protocole.md) : chaque client envoie
//! le document entier, le serveur le garde et le diffuse à tous. Ce suivi décide quoi faire de
//! la version qui arrive :
//! - rien n'est en cours d'édition ici : on l'adopte telle quelle ;
//! - c'est l'écho de notre propre envoi : rien à faire ;
//! - un autre client a modifié pendant que l'on éditait : on **garde** notre édition et on
//!   propose de voir l'autre version (jamais d'écrasement par surprise).
//!
//! Un seul envoi est en vol à la fois par document, et les modifications rapprochées sont
//! regroupées (délai de calme) : deux requêtes simultanées pourraient arriver dans le désordre
//! chez le serveur, et chaque frappe ne doit pas faire une requête.

use std::time::{Duration, Instant};

/// Délai de calme avant d'envoyer une modification.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Adoptée : le document affiché a été remplacé.
    Adopted,
    /// Écho de notre envoi, ou version identique : rien à faire.
    Echo,
    /// Un autre client a modifié pendant notre édition : bandeau « modifié ailleurs ».
    Conflict,
}

#[derive(Debug)]
pub struct SyncState<T> {
    server: Option<T>,
    dirty: bool,
    changed_at: Option<Instant>,
    in_flight: Option<(u64, T)>,
    /// Version d'un autre client reçue pendant une édition locale.
    remote: Option<T>,
}

impl<T> Default for SyncState<T> {
    fn default() -> Self {
        Self {
            server: None,
            dirty: false,
            changed_at: None,
            in_flight: None,
            remote: None,
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
        self.dirty || self.in_flight.is_some()
    }

    pub fn remote(&self) -> Option<&T> {
        self.remote.as_ref()
    }

    /// L'utilisateur vient de modifier le document affiché.
    pub fn mark_changed(&mut self, now: Instant) {
        self.dirty = true;
        self.changed_at = Some(now);
    }

    /// Version du serveur (état complet ou événement). `local` est le document affiché.
    pub fn on_server(&mut self, incoming: T, local: &mut T) -> Incoming {
        self.server = Some(incoming.clone());
        let own_echo = self
            .in_flight
            .as_ref()
            .is_some_and(|(_, sent)| *sent == incoming);
        if own_echo || incoming == *local {
            self.remote = None;
            return Incoming::Echo;
        }
        if self.editing() {
            self.remote = Some(incoming);
            return Incoming::Conflict;
        }
        *local = incoming;
        self.remote = None;
        Incoming::Adopted
    }

    /// Y a-t-il quelque chose à envoyer maintenant ? Rend le document à envoyer ; l'appelant
    /// l'envoie puis appelle [`SyncState::sent`]. Rien tant qu'un envoi est en vol, ni pendant
    /// que l'utilisateur est en train de manipuler (`holding`, souris enfoncée).
    pub fn due(&self, now: Instant, holding: bool, local: &T) -> Option<T> {
        let calm = self
            .changed_at
            .is_some_and(|t| now.saturating_duration_since(t) >= DEBOUNCE);
        (self.dirty && self.in_flight.is_none() && calm && !holding && self.server.is_some())
            .then(|| local.clone())
    }

    pub fn sent(&mut self, request_id: u64, content: T) {
        self.dirty = false;
        self.in_flight = Some((request_id, content));
    }

    /// Le serveur a accepté l'envoi `request_id` et renvoie le document tel qu'il l'a gardé.
    pub fn on_saved(&mut self, request_id: u64, stored: Option<T>, local: &mut T) {
        if self.in_flight.as_ref().map(|(id, _)| *id) != Some(request_id) {
            return;
        }
        self.in_flight = None;
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

    /// Le serveur a refusé l'envoi : on revient à sa version, rien ne reste en attente.
    /// Rend `true` si c'était bien notre envoi.
    pub fn on_rejected(&mut self, request_id: u64, local: &mut T) -> bool {
        if self.in_flight.as_ref().map(|(id, _)| *id) != Some(request_id) {
            return false;
        }
        self.in_flight = None;
        self.dirty = false;
        self.remote = None;
        if let Some(server) = &self.server {
            *local = server.clone();
        }
        true
    }

    /// Bandeau « modifié ailleurs » → « Afficher sa version ».
    pub fn accept_remote(&mut self, local: &mut T) {
        if let Some(remote) = self.remote.take() {
            *local = remote;
            self.dirty = false;
        }
    }

    /// Bandeau « modifié ailleurs » → « Garder la mienne » : la prochaine sauvegarde gagnera.
    pub fn keep_mine(&mut self, now: Instant) {
        if self.remote.take().is_some() {
            self.mark_changed(now);
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
        assert!(!s.on_rejected(99, &mut local), "numéro inconnu : ignoré");
        assert!(s.on_rejected(3, &mut local));
        assert_eq!(local, "ok");
        assert!(!s.editing());
    }

    #[test]
    fn nothing_is_sent_before_the_server_state_is_known() {
        let mut s = SyncState::<String>::default();
        s.mark_changed(t0());
        assert_eq!(s.due(t0() + DEBOUNCE * 2, false, &"x".to_string()), None);
    }
}
