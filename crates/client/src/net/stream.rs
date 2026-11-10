//! Le direct d'une caméra : un WebSocket par flux vers `GET /api/cameras/{id}/stream`, le décodage
//! (`video`) et la reprise automatique.
//!
//! Le serveur relaie le H.264 de la caméra (voir `protocol::wire::stream`) ; ici on le lit dans un
//! fil à part (jamais dans celui de l'interface), on le décode, et on garde la dernière image.
//! L'adresse, le certificat épinglé et le jeton sont ceux de la session en cours : `backend.rs`
//! les publie ici ([`publish`]) à chaque début ou fin de session, parce que le lecteur
//! (`ui::player`) n'a pas d'autre moyen de les connaître.
//!
//! # Cycle de vie
//!
//! - [`StreamHandle::open`] ouvre la liaison ; la lâcher (`Drop`) la ferme, et avec elle le flux
//!   côté serveur (le serveur coupe la caméra quand plus personne ne regarde).
//! - Le lecteur appelle [`StreamHandle::touch`] à chaque image de l'interface. Sans appel pendant
//!   [`IDLE_AFTER`], la liaison se ferme d'elle-même : une page quittée ou une fenêtre réduite (qui
//!   ne se redessine plus) ne laisse jamais la caméra allumée.
//! - La liaison, si elle se coupe, est rouverte toute seule (attente croissante), tant que la page
//!   est affichée. Les états définitifs (codec non pris en charge, caméra supprimée, serveur trop
//!   ancien, session terminée) arrêtent les tentatives.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::error::Error as WsError;
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::Message;

use protocol::wire::{
    camera_stream_path, CameraProblem, ErrorBody, StreamKind, StreamPacket, StreamStatus,
};

use super::backend::Creds;
use super::error::NetError;
use super::tls::{self, Trust};
use crate::video::{Decoder, FrameSink, Image, VideoError, VideoInfo};

/// Sans appel à [`StreamHandle::touch`] pendant ce temps, la liaison se ferme.
pub const IDLE_AFTER: Duration = Duration::from_millis(2500);
/// Pas de lecture : la boucle revérifie régulièrement qu'elle doit continuer.
const READ_TICK: Duration = Duration::from_millis(200);
/// Délai pour que la mise à niveau WebSocket aboutisse.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(10);
/// Rien reçu du serveur (images, états, pings de 20 s) pendant ce temps : la liaison est coupée.
const SILENCE_LIMIT: Duration = Duration::from_secs(45);
/// Attente maximale entre deux tentatives.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// La session en cours (publiée par `backend.rs`)
// ---------------------------------------------------------------------------

static CURRENT: Mutex<Option<(u64, Creds)>> = Mutex::new(None);

/// Retient la session en cours (`None` : plus de session). Appelée à chaque changement.
pub(super) fn publish(generation: u64, creds: Option<&Creds>) {
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) =
        creds.map(|c| (generation, c.clone()));
}

fn current() -> Option<(u64, Creds)> {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

// ---------------------------------------------------------------------------
// États
// ---------------------------------------------------------------------------

/// Ce que le lecteur affiche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// Connexion au serveur.
    Connecting,
    /// Liaison ouverte : le serveur se connecte à la caméra, ou la première image arrive.
    Waiting,
    /// Les images arrivent.
    Live,
    /// La liaison avec le serveur s'est coupée : nouvel essai dans `retry_in_s` secondes.
    Interrupted { reason: String, retry_in_s: u64 },
    /// Le serveur a perdu la caméra et réessaie tout seul.
    CameraRetrying { problem: CameraProblem },
    /// La caméra envoie un codec que le client ne lit pas (H.265…). Définitif.
    Unsupported { codec: String },
    /// Le serveur ne connaît pas cette route : il est plus ancien que ce client. Définitif.
    ServerTooOld,
    /// La caméra n'existe plus sur le serveur. Définitif.
    CameraGone,
    /// Cette caméra n'a pas ce flux (pas de flux réduit). Définitif.
    NoSuchStream,
    /// Le serveur ne reconnaît plus la session. Définitif (l'écran de connexion prend le relais).
    SessionEnded,
    /// Pas de session avec un serveur. Définitif.
    NoConnection,
    /// Pas de décodeur sur ce système. Définitif.
    DecoderUnavailable(String),
}

impl StreamState {
    /// Plus aucune nouvelle tentative : le lecteur n'a pas à rouvrir la liaison tout seul.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            StreamState::Unsupported { .. }
                | StreamState::ServerTooOld
                | StreamState::CameraGone
                | StreamState::NoSuchStream
                | StreamState::SessionEnded
                | StreamState::NoConnection
                | StreamState::DecoderUnavailable(_)
        )
    }
}

enum Conn {
    Connecting,
    Open,
    Interrupted { reason: String, retry_in_s: u64 },
    Final(StreamState),
}

struct Shared {
    conn: Conn,
    server: StreamStatus,
    info: Option<VideoInfo>,
}

struct Live {
    shared: Mutex<Shared>,
    sink: Arc<FrameSink>,
    epoch: Instant,
    touched_ms: AtomicU64,
    stop: AtomicBool,
    finished: AtomicBool,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Live {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_conn(&self, conn: Conn) {
        self.lock().conn = conn;
        (self.notify)();
    }

    fn idle(&self) -> bool {
        let now = self.epoch.elapsed().as_millis() as u64;
        now.saturating_sub(self.touched_ms.load(Ordering::Relaxed)) > IDLE_AFTER.as_millis() as u64
    }

    /// La liaison doit s'arrêter : le lecteur est parti, ou la session n'est plus la même.
    fn should_stop(&self, generation: Option<u64>) -> bool {
        self.stop.load(Ordering::SeqCst)
            || self.idle()
            || generation.is_some_and(|g| current().map(|(c, _)| c) != Some(g))
    }
}

/// Une liaison vers le flux d'une caméra. La lâcher la ferme.
pub struct StreamHandle {
    live: Arc<Live>,
}

impl StreamHandle {
    /// Ouvre le flux `kind` de la caméra `camera_id`. `notify` est appelée (depuis un fil réseau)
    /// à chaque image décodée et chaque changement d'état : l'interface s'en sert pour se
    /// redessiner.
    pub fn open(
        camera_id: u64,
        kind: StreamKind,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> StreamHandle {
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let sink = {
            let notify = notify.clone();
            FrameSink::new(move || notify())
        };
        let live = Arc::new(Live {
            shared: Mutex::new(Shared {
                conn: Conn::Connecting,
                server: StreamStatus::Connecting,
                info: None,
            }),
            sink,
            epoch: Instant::now(),
            touched_ms: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            notify,
        });
        let thread_live = live.clone();
        let spawned = std::thread::Builder::new()
            .name("matter-stream".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&thread_live, camera_id, kind)
                }));
                if result.is_err() {
                    log::error!("erreur interne dans la lecture vidéo");
                    thread_live.set_conn(Conn::Interrupted {
                        reason: "erreur interne du client".into(),
                        retry_in_s: 0,
                    });
                }
                thread_live.finished.store(true, Ordering::SeqCst);
                (thread_live.notify)();
            });
        if spawned.is_err() {
            live.set_conn(Conn::Final(StreamState::DecoderUnavailable(
                "ressources du système épuisées".into(),
            )));
            live.finished.store(true, Ordering::SeqCst);
        }
        let handle = StreamHandle { live };
        handle.touch();
        handle
    }

    /// Le lecteur est affiché : à appeler à chaque image de l'interface.
    pub fn touch(&self) {
        self.live.touched_ms.store(
            self.live.epoch.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn state(&self) -> StreamState {
        let shared = self.live.lock();
        match &shared.conn {
            Conn::Connecting => StreamState::Connecting,
            Conn::Interrupted { reason, retry_in_s } => StreamState::Interrupted {
                reason: reason.clone(),
                retry_in_s: *retry_in_s,
            },
            Conn::Final(state) => state.clone(),
            Conn::Open => match &shared.server {
                StreamStatus::Retrying { problem } => {
                    StreamState::CameraRetrying { problem: *problem }
                }
                StreamStatus::Unsupported { codec } => StreamState::Unsupported {
                    codec: codec.clone(),
                },
                StreamStatus::Live if self.live.sink.frames() > 0 => StreamState::Live,
                _ => StreamState::Waiting,
            },
        }
    }

    /// La dernière image décodée, une seule fois.
    pub fn take_image(&self) -> Option<Image> {
        self.live.sink.take()
    }

    /// Taille et forme des pixels annoncées par le serveur.
    pub fn info(&self) -> Option<VideoInfo> {
        self.live.lock().info
    }

    /// Images décodées depuis l'ouverture.
    pub fn frames(&self) -> u64 {
        self.live.sink.frames()
    }

    /// Le fil de lecture est terminé (état définitif, ou page quittée trop longtemps) : le lecteur
    /// doit en ouvrir un autre s'il veut reprendre.
    pub fn is_finished(&self) -> bool {
        self.live.finished.load(Ordering::SeqCst)
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.live.stop.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Le fil de lecture
// ---------------------------------------------------------------------------

/// Comment une liaison s'est terminée.
enum Ended {
    Stopped,
    Final(StreamState),
    /// À rouvrir après une attente.
    Retry {
        reason: String,
        quick: bool,
    },
}

fn run(live: &Arc<Live>, camera_id: u64, kind: StreamKind) {
    // Pas de décodeur : inutile de déranger le serveur.
    let mut decoder = match Decoder::new(live.sink.clone()) {
        Ok(decoder) => decoder,
        Err(e) => {
            live.set_conn(Conn::Final(StreamState::DecoderUnavailable(e.to_string())));
            return;
        }
    };
    let mut attempt: u32 = 0;
    loop {
        if live.should_stop(None) {
            return;
        }
        let Some((generation, creds)) = current() else {
            live.set_conn(Conn::Final(StreamState::NoConnection));
            return;
        };
        live.set_conn(Conn::Connecting);
        let started = Instant::now();
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            session(live, &mut decoder, generation, &creds, camera_id, kind)
        }))
        .unwrap_or_else(|_| Ended::Retry {
            reason: "erreur interne du client".into(),
            quick: false,
        });
        match ended {
            Ended::Stopped => return,
            Ended::Final(state) => {
                live.set_conn(Conn::Final(state));
                return;
            }
            Ended::Retry { reason, quick } => {
                if started.elapsed() > Duration::from_secs(10) {
                    attempt = 0;
                }
                attempt += 1;
                let wait = if quick {
                    Duration::from_millis(300)
                } else {
                    Duration::from_secs(u64::from(attempt.min(5))).min(MAX_RETRY_WAIT)
                };
                log::info!(
                    "flux de la caméra {camera_id:#x} interrompu ({reason}) : nouvel essai dans {} ms",
                    wait.as_millis()
                );
                live.set_conn(Conn::Interrupted {
                    reason,
                    retry_in_s: wait.as_secs().max(1),
                });
                if !sleep_while_wanted(live, generation, wait) {
                    return;
                }
            }
        }
    }
}

fn sleep_while_wanted(live: &Live, generation: u64, total: Duration) -> bool {
    let end = Instant::now() + total;
    while Instant::now() < end {
        if live.should_stop(Some(generation)) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
    }
    !live.should_stop(Some(generation))
}

/// Une liaison : connexion, mise à niveau WebSocket, lecture jusqu'à la coupure.
fn session(
    live: &Arc<Live>,
    decoder: &mut Decoder,
    generation: u64,
    creds: &Creds,
    camera_id: u64,
    kind: StreamKind,
) -> Ended {
    let trust = Trust::Pinned(creds.fingerprint.clone());
    let mut connected = match tls::connect(&creds.addr, &trust) {
        Ok(c) => c,
        Err(NetError::CertificateChanged { .. }) => {
            return Ended::Final(StreamState::NoConnection);
        }
        Err(e) => {
            return Ended::Retry {
                reason: e.message(),
                quick: false,
            }
        }
    };
    {
        let live = live.clone();
        connected
            .stream
            .sock
            .set_cancel(Some(Arc::new(move || live.should_stop(Some(generation)))));
        connected
            .stream
            .sock
            .set_deadline(Instant::now() + UPGRADE_TIMEOUT);
    }
    let url = format!(
        "wss://{}{}",
        creds.addr.authority(),
        camera_stream_path(camera_id, kind)
    );
    let Ok(mut request) = url.into_client_request() else {
        return Ended::Final(StreamState::NoConnection);
    };
    match format!("Bearer {}", creds.token).parse::<tungstenite::http::HeaderValue>() {
        Ok(mut value) => {
            value.set_sensitive(true);
            request.headers_mut().insert("Authorization", value);
        }
        Err(_) => return Ended::Final(StreamState::SessionEnded),
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024));
    let mut socket =
        match tungstenite::client::client_with_config(request, connected.stream, Some(config)) {
            Ok((socket, _)) => socket,
            Err(HandshakeError::Failure(WsError::Http(response))) => {
                return refusal(response.status().as_u16(), response.body().as_deref());
            }
            Err(HandshakeError::Failure(WsError::Io(e))) => {
                if live.should_stop(Some(generation)) {
                    return Ended::Stopped;
                }
                return Ended::Retry {
                    reason: format!("connexion coupée ({})", tls::io_reason(&e)),
                    quick: false,
                };
            }
            Err(_) => {
                return Ended::Retry {
                    reason: "échange WebSocket refusé".into(),
                    quick: false,
                }
            }
        };
    socket.get_mut().sock.set_tick(Some(READ_TICK));
    decoder.reset();
    {
        let mut shared = live.lock();
        shared.conn = Conn::Open;
        shared.server = StreamStatus::Connecting;
    }
    (live.notify)();

    let mut last_heard = Instant::now();
    loop {
        if live.should_stop(Some(generation)) {
            let _ = socket.close(None);
            let _ = socket.flush();
            return Ended::Stopped;
        }
        if last_heard.elapsed() >= SILENCE_LIMIT {
            return Ended::Retry {
                reason: "le serveur ne répond plus".into(),
                quick: false,
            };
        }
        socket
            .get_mut()
            .sock
            .set_deadline(last_heard + SILENCE_LIMIT);
        let message = match socket.read() {
            Ok(m) => m,
            Err(WsError::Io(e)) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(WsError::Io(e)) if e.kind() == ErrorKind::TimedOut => {
                return Ended::Retry {
                    reason: "le serveur ne répond plus".into(),
                    quick: false,
                }
            }
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                return Ended::Retry {
                    reason: "connexion fermée par le serveur".into(),
                    quick: true,
                }
            }
            Err(WsError::Io(e)) => {
                if live.should_stop(Some(generation)) {
                    return Ended::Stopped;
                }
                return Ended::Retry {
                    reason: format!("connexion coupée ({})", tls::io_reason(&e)),
                    quick: false,
                };
            }
            Err(_) => {
                return Ended::Retry {
                    reason: "flux vidéo invalide".into(),
                    quick: false,
                }
            }
        };
        last_heard = Instant::now();
        match message {
            Message::Binary(bytes) => {
                let packet = match StreamPacket::decode(&bytes) {
                    Ok(packet) => packet,
                    Err(_) => {
                        return Ended::Retry {
                            reason: "flux vidéo invalide".into(),
                            quick: false,
                        }
                    }
                };
                if let StreamPacket::Config { .. } = &packet {
                    if let Err(e) = decoder.push(&packet) {
                        return Ended::Final(decode_failure(e));
                    }
                    live.lock().info = decoder.info();
                    (live.notify)();
                } else if let Err(e) = decoder.push(&packet) {
                    return Ended::Final(decode_failure(e));
                }
            }
            Message::Text(text) => {
                let status = serde_json::from_str::<StreamStatus>(text.as_str())
                    .unwrap_or(StreamStatus::Unknown);
                if let StreamStatus::Unsupported { codec } = &status {
                    return Ended::Final(StreamState::Unsupported {
                        codec: codec.clone(),
                    });
                }
                live.lock().server = status;
                (live.notify)();
            }
            Message::Close(frame) => {
                return match frame.as_ref().map(|f| f.code) {
                    Some(CloseCode::Policy) => Ended::Final(StreamState::SessionEnded),
                    // Caméra modifiée, arrêt du serveur : on rouvre vite.
                    Some(CloseCode::Away) | Some(CloseCode::Normal) | None => Ended::Retry {
                        reason: "le serveur a fermé le flux".into(),
                        quick: true,
                    },
                    _ => Ended::Retry {
                        reason: "connexion fermée par le serveur".into(),
                        quick: false,
                    },
                };
            }
            Message::Ping(_) => {
                let _ = socket.flush();
            }
            _ => {}
        }
    }
}

fn decode_failure(error: VideoError) -> StreamState {
    match error {
        VideoError::UnsupportedCodec(_) => StreamState::Unsupported {
            codec: "inconnu".into(),
        },
        VideoError::Unavailable(why) => StreamState::DecoderUnavailable(why),
    }
}

/// Réponse HTTP qui refuse la mise à niveau WebSocket. Un `404` **sans détail** est un serveur qui
/// n'a pas la route (trop ancien) ; avec un détail, c'est une caméra inconnue.
fn refusal(status: u16, body: Option<&[u8]>) -> Ended {
    let parsed = body.and_then(|b| serde_json::from_slice::<ErrorBody>(b).ok());
    let detail = parsed.as_ref().and_then(|b| b.error.detail.clone());
    classify_refusal(status, detail.as_deref())
}

fn classify_refusal(status: u16, detail: Option<&str>) -> Ended {
    match status {
        404 if detail.is_some() => Ended::Final(StreamState::CameraGone),
        404 | 405 | 426 => Ended::Final(StreamState::ServerTooOld),
        400 => Ended::Final(StreamState::NoSuchStream),
        401 => Ended::Final(StreamState::SessionEnded),
        _ => Ended::Retry {
            reason: format!("le serveur a refusé le flux (code {status})"),
            quick: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn final_state(ended: Ended) -> Option<StreamState> {
        match ended {
            Ended::Final(state) => Some(state),
            _ => None,
        }
    }

    #[test]
    fn refusals_tell_an_old_server_from_a_missing_camera() {
        // Un serveur sans la route répond 404 sans détail ; une caméra inconnue, avec un détail.
        assert_eq!(
            final_state(classify_refusal(404, None)),
            Some(StreamState::ServerTooOld)
        );
        assert_eq!(
            final_state(classify_refusal(404, Some("caméra inconnue"))),
            Some(StreamState::CameraGone)
        );
        assert_eq!(
            final_state(classify_refusal(400, Some("pas de flux réduit"))),
            Some(StreamState::NoSuchStream)
        );
        assert_eq!(
            final_state(classify_refusal(401, None)),
            Some(StreamState::SessionEnded)
        );
        // Un serveur occupé ou en arrêt : on réessaie.
        assert!(final_state(classify_refusal(503, None)).is_none());
        assert!(StreamState::ServerTooOld.is_final());
        assert!(!StreamState::Waiting.is_final());
    }

    #[test]
    fn the_refusal_body_is_read_for_its_detail() {
        let old = br#"{"error":{"code":"not_found"}}"#;
        assert_eq!(
            final_state(refusal(404, Some(old))),
            Some(StreamState::ServerTooOld)
        );
        let gone = r#"{"error":{"code":"not_found","detail":"caméra inconnue"}}"#.as_bytes();
        assert_eq!(
            final_state(refusal(404, Some(gone))),
            Some(StreamState::CameraGone)
        );
        assert_eq!(
            final_state(refusal(404, None)),
            Some(StreamState::ServerTooOld)
        );
    }
}
