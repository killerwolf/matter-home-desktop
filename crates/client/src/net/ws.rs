//! La session : le WebSocket du temps réel, avec reconnexion automatique.
//!
//! Déroulé d'une connexion : version du protocole (`GET /api/version`), puis le WebSocket avec
//! le jeton dans l'en-tête `Authorization`. Le serveur envoie `hello`, l'état complet
//! (`snapshot`), puis les événements numérotés. Un trou dans la numérotation rouvre la
//! connexion ; un client en retard (fermeture 1013) aussi, sans bruit.

use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::error::Error as WsError;
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

use protocol::wire::{ErrorBody, ErrorCode, ServerMessage, VersionInfo};

use super::backend::{Creds, Shared};
use super::error::{api_message, NetError};
use super::http::{self, Request};
use super::tls::{self, Trust};
use super::{Event, Link};

/// Rien reçu du serveur pendant ce temps : la liaison est considérée comme perdue. Le serveur
/// envoie un ping toutes les 20 s.
const SILENCE_LIMIT: Duration = Duration::from_secs(60);
/// Pas de lecture : la boucle vérifie régulièrement qu'elle n'a pas été remplacée.
const READ_TICK: Duration = Duration::from_millis(500);
/// Attente maximale entre deux tentatives de reconnexion.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Une liaison restée établie aussi longtemps remet l'attente à zéro.
const STABLE_AFTER: Duration = Duration::from_secs(10);

/// Pourquoi une connexion s'est terminée.
#[derive(Debug)]
enum Ended {
    /// Une autre session a pris la place (déconnexion, nouvelle connexion) : on s'arrête.
    Superseded,
    /// Jeton refusé ou session fermée par le serveur.
    Unauthorized,
    CertificateChanged {
        expected: String,
        found: String,
    },
    Incompatible(String),
    /// Client en retard (1013) ou trou dans les événements : on rouvre aussitôt.
    Resync,
    /// Liaison perdue : attente progressive avant de réessayer.
    Lost(String),
}

pub(super) fn run(shared: Arc<Shared>, generation: u64, creds: Creds) {
    let mut attempt: u32 = 0;
    let mut quick_resyncs: u32 = 0;
    loop {
        if shared.generation() != generation {
            return;
        }
        let started = Instant::now();
        let ended = connect_and_stream(&shared, generation, &creds);
        if shared.generation() != generation {
            return;
        }
        match ended {
            Ended::Superseded => return,
            Ended::Unauthorized => {
                shared.token_rejected(generation);
                return;
            }
            Ended::CertificateChanged { expected, found } => {
                log::warn!("le certificat du serveur a changé : connexion refusée");
                shared.link(Link::CertificateChanged {
                    url: creds.addr.url(),
                    expected,
                    found,
                });
                return;
            }
            Ended::Incompatible(message) => {
                shared.link(Link::Incompatible { message });
                return;
            }
            Ended::Resync => {
                // Quelques resynchronisations de suite en quelques secondes : le serveur est
                // débordé ou le client trop lent, on ralentit au lieu de boucler.
                if started.elapsed() < Duration::from_secs(3) {
                    quick_resyncs += 1;
                } else {
                    quick_resyncs = 0;
                }
                if quick_resyncs >= 5 {
                    quick_resyncs = 0;
                    attempt += 1;
                    let wait = backoff(attempt);
                    shared.link(Link::Reconnecting {
                        attempt,
                        retry_in_s: wait.as_secs(),
                        reason: "resynchronisations trop fréquentes.".into(),
                    });
                    if !sleep_while_current(&shared, generation, wait) {
                        return;
                    }
                    shared.link(Link::Connecting);
                    continue;
                }
                log::info!("resynchronisation avec le serveur");
                shared.link(Link::Resyncing);
                if !sleep_while_current(&shared, generation, Duration::from_millis(300)) {
                    return;
                }
            }
            Ended::Lost(reason) => {
                if started.elapsed() >= STABLE_AFTER {
                    attempt = 0;
                }
                attempt += 1;
                let wait = backoff(attempt);
                log::info!(
                    "liaison perdue ({reason}) : nouvel essai dans {} s",
                    wait.as_secs()
                );
                shared.link(Link::Reconnecting {
                    attempt,
                    retry_in_s: wait.as_secs(),
                    reason,
                });
                if !sleep_while_current(&shared, generation, wait) {
                    return;
                }
                shared.link(Link::Connecting);
            }
        }
    }
}

/// 1 s, 2 s, 4 s, 8 s, 16 s, puis 30 s.
fn backoff(attempt: u32) -> Duration {
    let secs = 1u64 << attempt.saturating_sub(1).min(5);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

/// Dort `total`, par petits pas. `false` si la session a été remplacée entre-temps.
fn sleep_while_current(shared: &Shared, generation: u64, total: Duration) -> bool {
    let end = Instant::now() + total;
    while Instant::now() < end {
        if shared.generation() != generation {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
    }
    shared.generation() == generation
}

fn classify(error: NetError) -> Ended {
    match error {
        NetError::CertificateChanged { expected, found } => {
            Ended::CertificateChanged { expected, found }
        }
        e if e.is_unauthorized() => Ended::Unauthorized,
        e if e.api_code() == Some(ErrorCode::VersionIncompatible) => {
            Ended::Incompatible(e.message())
        }
        e => Ended::Lost(e.message()),
    }
}

fn connect_and_stream(shared: &Shared, generation: u64, creds: &Creds) -> Ended {
    let trust = Trust::Pinned(creds.fingerprint.clone());

    // 1. Version du protocole, annoncée avant toute chose.
    let version = http::exec(&creds.addr, &trust, None, &Request::get("/api/version"))
        .and_then(http::Response::into_result)
        .and_then(|r| r.json::<VersionInfo>());
    match version {
        Ok(v) if v.protocol_version != shared.protocol() => {
            return Ended::Incompatible(format!(
                "Ce client parle la version {} du protocole, le serveur la version {} : mettez à jour {}.",
                shared.protocol(),
                v.protocol_version,
                if v.protocol_version > shared.protocol() { "l'application" } else { "le serveur" },
            ));
        }
        Ok(_) => {}
        Err(e) => return classify(e),
    }
    if shared.generation() != generation {
        return Ended::Superseded;
    }

    // 2. WebSocket.
    let connected = match tls::connect(&creds.addr, &trust) {
        Ok(c) => c,
        Err(e) => return classify(e),
    };
    let url = format!(
        "wss://{}/api/ws?protocol={}",
        creds.addr.authority(),
        shared.protocol()
    );
    let mut request = match url.into_client_request() {
        Ok(r) => r,
        Err(_) => return Ended::Lost("adresse du serveur invalide".into()),
    };
    match format!("Bearer {}", creds.token).parse() {
        Ok(value) => {
            request.headers_mut().insert("Authorization", value);
        }
        Err(_) => return Ended::Unauthorized,
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(32 * 1024 * 1024))
        .max_frame_size(Some(32 * 1024 * 1024));
    let mut socket =
        match tungstenite::client::client_with_config(request, connected.stream, Some(config)) {
            Ok((socket, _)) => socket,
            Err(HandshakeError::Failure(WsError::Http(response))) => {
                return classify(http_refusal(
                    response.status().as_u16(),
                    response.body().as_deref(),
                ));
            }
            Err(HandshakeError::Failure(WsError::Io(e))) => {
                return Ended::Lost(format!("connexion coupée ({})", tls::io_reason(&e)))
            }
            Err(_) => return Ended::Lost("échange WebSocket refusé".into()),
        };
    let _ = socket.get_mut().sock.set_read_timeout(Some(READ_TICK));

    // 3. Flux : hello, état complet, événements.
    let mut last_heard = Instant::now();
    let mut greeted = false;
    let mut announced_version: Option<String> = None;
    let mut expected_seq: Option<u64> = None;
    loop {
        if shared.generation() != generation {
            let _ = socket.close(None);
            let _ = socket.flush();
            return Ended::Superseded;
        }
        if last_heard.elapsed() > SILENCE_LIMIT {
            return Ended::Lost("le serveur ne répond plus".into());
        }
        let message = match socket.read() {
            Ok(m) => m,
            Err(WsError::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                return Ended::Lost("connexion fermée par le serveur".into())
            }
            Err(WsError::Io(e)) => {
                return Ended::Lost(format!("connexion coupée ({})", tls::io_reason(&e)))
            }
            Err(_) => return Ended::Lost("flux WebSocket invalide".into()),
        };
        last_heard = Instant::now();
        match message {
            Message::Text(text) => {
                let Ok(parsed) = serde_json::from_str::<ServerMessage>(text.as_str()) else {
                    return Ended::Lost("message du serveur incompréhensible".into());
                };
                match parsed {
                    ServerMessage::Hello {
                        protocol_version,
                        server_version,
                    } => {
                        if protocol_version != shared.protocol() {
                            return Ended::Incompatible(api_message(
                                ErrorCode::VersionIncompatible,
                                None,
                                None,
                                426,
                            ));
                        }
                        greeted = true;
                        announced_version = Some(server_version);
                    }
                    _ if !greeted => {
                        return Ended::Lost("le serveur n'a pas annoncé sa version".into())
                    }
                    ServerMessage::Snapshot(snapshot) => {
                        expected_seq = Some(snapshot.seq + 1);
                        // « Connecté » n'est annoncé qu'avec l'état complet : l'interface ne
                        // montre jamais une liste vide en attendant.
                        if let Some(server_version) = announced_version.clone() {
                            shared.link(Link::Connected { server_version });
                        }
                        shared.emit(Event::Snapshot(snapshot));
                    }
                    ServerMessage::Event { seq, event } => {
                        match check_sequence(expected_seq, seq) {
                            Sequence::Deliver => {
                                expected_seq = Some(seq + 1);
                                shared.emit(Event::Live(Box::new(event)));
                            }
                            Sequence::Duplicate => {}
                            // Trou : on a manqué des événements, on repart d'un état complet.
                            Sequence::Gap => return Ended::Resync,
                            Sequence::BeforeSnapshot => {
                                return Ended::Lost("événement avant l'état complet".into())
                            }
                        }
                    }
                }
            }
            Message::Close(frame) => return close_outcome(frame.as_ref().map(|f| f.code)),
            // Ping : tungstenite prépare le pong, `flush` l'envoie sans attendre.
            Message::Ping(_) => {
                let _ = socket.flush();
            }
            _ => {}
        }
    }
}

/// Que faire d'un événement numéroté, sachant le prochain numéro attendu.
#[derive(Debug, PartialEq, Eq)]
enum Sequence {
    Deliver,
    /// Déjà pris en compte (rejeu après un état complet) : ignoré.
    Duplicate,
    Gap,
    BeforeSnapshot,
}

fn check_sequence(expected: Option<u64>, seq: u64) -> Sequence {
    match expected {
        None => Sequence::BeforeSnapshot,
        Some(e) if seq == e => Sequence::Deliver,
        Some(e) if seq < e => Sequence::Duplicate,
        Some(_) => Sequence::Gap,
    }
}

/// Suite d'une fermeture du WebSocket par le serveur, d'après son code.
fn close_outcome(code: Option<CloseCode>) -> Ended {
    match code {
        // 1013 : client en retard, simple reconnexion (pas de message d'erreur).
        Some(CloseCode::Again) => Ended::Resync,
        // 1008 : session terminée (déconnexion, expiration).
        Some(CloseCode::Policy) => Ended::Unauthorized,
        // 1001 : le serveur s'arrête, il reviendra peut-être.
        Some(CloseCode::Away) => Ended::Lost("le serveur s'arrête".into()),
        _ => Ended::Lost("connexion fermée par le serveur".into()),
    }
}

/// Réponse HTTP refusant le passage au WebSocket (401, 426, 503…) : son corps porte le code.
fn http_refusal(status: u16, body: Option<&[u8]>) -> NetError {
    let parsed = body.and_then(|b| serde_json::from_slice::<ErrorBody>(b).ok());
    NetError::Api {
        status,
        code: parsed.as_ref().map(|b| b.error.code),
        detail: parsed.as_ref().and_then(|b| b.error.detail.clone()),
        retry_after_s: parsed.and_then(|b| b.error.retry_after_s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let secs: Vec<u64> = (1..=9).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30, 30]);
    }

    #[test]
    fn handshake_refusals_keep_the_stable_code() {
        let body = br#"{"error":{"code":"unauthorized"}}"#;
        assert!(matches!(
            classify(http_refusal(401, Some(body))),
            Ended::Unauthorized
        ));
        let body = br#"{"error":{"code":"version_incompatible","detail":"x"}}"#;
        assert!(matches!(
            classify(http_refusal(426, Some(body))),
            Ended::Incompatible(_)
        ));
        assert!(matches!(classify(http_refusal(503, None)), Ended::Lost(_)));
    }

    #[test]
    fn sequence_numbers_detect_gaps_and_replays() {
        assert_eq!(check_sequence(Some(5), 5), Sequence::Deliver);
        assert_eq!(check_sequence(Some(5), 4), Sequence::Duplicate);
        assert_eq!(check_sequence(Some(5), 0), Sequence::Duplicate);
        assert_eq!(check_sequence(Some(5), 6), Sequence::Gap);
        assert_eq!(check_sequence(None, 1), Sequence::BeforeSnapshot);
    }

    #[test]
    fn close_codes_map_to_the_right_reaction() {
        // 1013 : reconnexion simple, sans message d'erreur.
        assert!(matches!(
            close_outcome(Some(CloseCode::Again)),
            Ended::Resync
        ));
        assert!(matches!(
            close_outcome(Some(CloseCode::Policy)),
            Ended::Unauthorized
        ));
        assert!(matches!(
            close_outcome(Some(CloseCode::Away)),
            Ended::Lost(_)
        ));
        assert!(matches!(close_outcome(None), Ended::Lost(_)));
    }
}
