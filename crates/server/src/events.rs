//! Événements du serveur. Les types (`Command`, `Event`…) viennent du crate `protocol` ;
//! ce module y ajoute l'émetteur d'événements, qui n'a aucun lien avec une interface.

use std::sync::mpsc;

pub use protocol::events::*;

/// Côté émetteur des événements du serveur.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<Event>,
    /// Copie des événements pour le moteur d'automatisations.
    tap: Option<tokio::sync::mpsc::UnboundedSender<Event>>,
}

impl EventSink {
    pub fn new(tx: mpsc::Sender<Event>) -> Self {
        Self { tx, tap: None }
    }

    /// Même destination, avec une copie de chaque événement vers `tap`.
    pub fn with_tap(&self, tap: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
        Self {
            tx: self.tx.clone(),
            tap: Some(tap),
        }
    }

    pub fn send(&self, event: Event) {
        if let Some(tap) = &self.tap {
            let _ = tap.send(event.clone());
        }
        // Le destinataire peut avoir disparu (arrêt en cours) : on ignore l'erreur.
        let _ = self.tx.send(event);
    }
}
