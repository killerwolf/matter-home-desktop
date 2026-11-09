//! Journal du client (`RUST_LOG`) : même avec `RUST_LOG=trace`, le jeton de session ne doit
//! jamais s'écrire.
//!
//! La bibliothèque WebSocket (`tungstenite`) journalise, au niveau `trace`, la requête de mise
//! à niveau telle qu'elle part, **en-tête `Authorization` compris**, puis chaque trame reçue.
//! On enveloppe donc `env_logger` d'un filtre : rien de `tungstenite` au-delà de `debug` (niveau
//! où elle ne montre ni en-têtes ni contenu), et, en dernier recours pour toute source, rien
//! de ce dont le texte mentionne un en-tête d'autorisation.

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Niveau le plus bavard accepté pour les bibliothèques qui peuvent journaliser des en-têtes.
const QUIET_LIBRARIES: LevelFilter = LevelFilter::Debug;

fn is_websocket_library(target: &str) -> bool {
    target == "tungstenite" || target.starts_with("tungstenite::")
}

/// Cette source a-t-elle le droit de parler à ce niveau ? (Avant même de formater le message.)
pub fn allowed_target(target: &str, level: Level) -> bool {
    !is_websocket_library(target) || level <= QUIET_LIBRARIES
}

/// Le texte du message laisse-t-il voir un en-tête d'autorisation ou un jeton porteur ?
pub fn leaks_credentials(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("authorization") || lower.contains("bearer ")
}

struct Filtered<L: Log> {
    inner: L,
}

impl<L: Log> Log for Filtered<L> {
    fn enabled(&self, metadata: &Metadata) -> bool {
        allowed_target(metadata.target(), metadata.level()) && self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Le texte n'est formaté que pour les niveaux détaillés, où un en-tête peut passer.
        if record.level() >= Level::Debug && leaks_credentials(&record.args().to_string()) {
            return;
        }
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Installe le journal (`RUST_LOG`, `info` par défaut) derrière le filtre ci-dessus.
pub fn init() {
    let inner =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let max = inner.filter();
    if log::set_boxed_logger(Box::new(Filtered { inner })).is_ok() {
        log::set_max_level(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Journal de test : garde ce qu'on lui confie.
    struct Sink(Mutex<Vec<String>>);

    impl Log for &'static Sink {
        fn enabled(&self, _: &Metadata) -> bool {
            true
        }
        fn log(&self, record: &Record) {
            self.0.lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }

    #[test]
    fn the_websocket_library_never_speaks_below_debug() {
        assert!(!allowed_target(
            "tungstenite::handshake::client",
            Level::Trace
        ));
        assert!(!allowed_target("tungstenite", Level::Trace));
        assert!(allowed_target(
            "tungstenite::handshake::client",
            Level::Debug
        ));
        assert!(allowed_target("matter_home_client::net::ws", Level::Trace));
        // Un nom qui commence pareil sans être la bibliothèque n'est pas touché.
        assert!(allowed_target("tungstenitelike", Level::Trace));
    }

    #[test]
    fn authorization_headers_are_dropped_whatever_the_source() {
        let request = "Request: \"GET /api/ws HTTP/1.1\\r\\nauthorization: Bearer abc123\\r\\n\"";
        assert!(leaks_credentials(request));
        assert!(leaks_credentials("Authorization: x"));
        assert!(leaks_credentials("jeton Bearer abc"));
        assert!(!leaks_credentials("liaison perdue : délai dépassé"));

        let sink: &'static Sink = Box::leak(Box::new(Sink(Mutex::new(Vec::new()))));
        let logger = Filtered { inner: sink };
        let emit = |target: &str, level: Level, text: &str| {
            logger.log(
                &Record::builder()
                    .target(target)
                    .level(level)
                    .args(format_args!("{text}"))
                    .build(),
            );
        };
        emit("tungstenite::handshake::client", Level::Trace, request);
        emit("autre", Level::Trace, request);
        emit("autre", Level::Debug, request);
        emit("tungstenite", Level::Debug, "Client handshake done.");
        emit("matter_home_client", Level::Info, "connexion acceptée");
        let kept = sink.0.lock().unwrap().clone();
        assert_eq!(kept, vec!["Client handshake done.", "connexion acceptée"]);
    }
}
