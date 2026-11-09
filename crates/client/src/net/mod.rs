//! Connexion au serveur Matter Home : HTTPS pour les requêtes, WebSocket pour le temps réel.
//!
//! Tout le travail réseau (résolution du nom, connexion TCP, TLS, requêtes, WebSocket) se fait
//! dans des fils d'exécution à part : l'interface n'attend jamais le réseau. Elle envoie des
//! demandes à [`Backend`] (qui rend la main aussitôt) et lit des [`Event`] à chaque image.
//!
//! Voir `docs/protocole.md` pour le contrat avec le serveur.

pub mod addr;
mod backend;
pub mod error;
pub mod http;
pub mod store;
pub mod tls;
mod ws;

use std::sync::Arc;

pub use addr::ServerAddr;
pub use backend::Backend;
pub use error::NetError;
pub use http::{Body, Request, Response};
pub use store::{Saved, Store};

use protocol::wire::{Snapshot, WireEvent};

/// Appelée à chaque événement mis en file pour réveiller l'interface (`request_repaint`).
pub type Notifier = Arc<dyn Fn() + Send + Sync>;

/// État de la liaison avec le serveur.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Pas de session : écran de connexion. `url` est préremplie ; `pinned` dit que le
    /// certificat de cette adresse est déjà approuvé (jeton expiré ou révoqué : le mot de passe
    /// suffit). `reason` explique pourquoi on est ici.
    LoggedOut {
        url: Option<String>,
        pinned: bool,
        reason: Option<String>,
    },
    /// Première tentative après le démarrage ou la connexion.
    Connecting,
    /// Reconnexion immédiate (client en retard, trou dans les événements) : les données
    /// affichées restent, un nouvel état complet arrive aussitôt.
    Resyncing,
    /// Liaison perdue : nouvelle tentative dans `retry_in_s` secondes.
    Reconnecting {
        attempt: u32,
        retry_in_s: u64,
        reason: String,
    },
    Connected {
        server_version: String,
    },
    /// Le certificat présenté n'est pas celui qui a été approuvé. Aucune donnée n'est envoyée.
    CertificateChanged {
        url: String,
        expected: String,
        found: String,
    },
    /// Le client et le serveur ne parlent pas la même version du protocole.
    Incompatible {
        message: String,
    },
}

/// Ce que le réseau remonte à l'interface.
#[derive(Debug)]
pub enum Event {
    Link(Link),
    /// État complet : remplace tout ce qui était affiché.
    Snapshot(Box<Snapshot>),
    /// Événement suivant, numérotation continue garantie (voir `ws.rs`).
    Live(Box<WireEvent>),
    /// Réponse à une requête de [`Backend::call`].
    Reply {
        id: u64,
        result: Result<Response, NetError>,
    },
    /// Empreinte lue pendant une première connexion : à faire approuver par l'utilisateur.
    Probed {
        url: String,
        fingerprint: String,
    },
    ProbeFailed(NetError),
    LoginFailed(NetError),
    /// Information à afficher dans le journal (déconnexion, jeton non révoqué…).
    Notice(String),
}
