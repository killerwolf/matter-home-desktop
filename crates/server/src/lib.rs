//! Serveur Matter Home : tout ce qui pilote les appareils et garde les données, sans
//! aucune interface graphique. Le binaire `matter-home-server` (voir `main.rs`) l'assemble.

pub mod automation;
pub mod backup;
pub mod demo;
pub mod events;
pub mod history_store;
pub mod matter;
pub mod persistence;

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use events::Command;

/// Tâche de fond lancée par `matter::spawn` ou `demo::spawn` : canal de commandes et threads.
pub struct Backend {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    threads: Vec<JoinHandle<()>>,
}

impl Backend {
    pub fn new(
        commands: tokio::sync::mpsc::UnboundedSender<Command>,
        threads: Vec<JoinHandle<()>>,
    ) -> Self {
        Self { commands, threads }
    }

    pub fn send(
        &self,
        command: Command,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Command>> {
        self.commands.send(command)
    }

    /// Ferme le canal de commandes (ce qui demande l'arrêt du backend), puis attend la fin
    /// des threads au plus `timeout` au total. `true` si tous se sont arrêtés à temps.
    pub fn shutdown(self, timeout: Duration) -> bool {
        drop(self.commands);
        let deadline = Instant::now() + timeout;
        let mut all_done = true;
        for handle in self.threads {
            let name = handle.thread().name().unwrap_or("?").to_string();
            let (tx, rx) = mpsc::channel();
            // `JoinHandle::join` n'a pas de délai : un thread auxiliaire joint et prévient.
            std::thread::spawn(move || {
                let _ = tx.send(handle.join().is_ok());
            });
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(true) => {}
                Ok(false) => {
                    log::error!("le thread {name} s'est terminé par une panique");
                    all_done = false;
                }
                Err(_) => {
                    log::error!("le thread {name} ne s'est pas arrêté dans les temps");
                    all_done = false;
                }
            }
        }
        all_done
    }
}

/// Plan de la maison : le modèle vient du crate `protocol`.
pub mod home {
    pub use protocol::home::*;
}
