//! Serveur Matter Home : tout ce qui pilote les appareils et garde les données, sans
//! aucune interface graphique. Le binaire `matter-home-server` (voir `main.rs`) l'assemble.

pub mod automation;
pub mod backup;
pub mod demo;
pub mod events;
pub mod history_store;
pub mod matter;
pub mod persistence;

/// Plan de la maison : le modèle vient du crate `protocol`.
pub mod home {
    pub use protocol::home::*;
}
