//! Image fixe d'une caméra (ticket n° 12) : `GET /api/cameras/{id}/snapshot`, rafraîchie toutes
//! les [`REFRESH_EVERY`] tant que la vignette est affichée.
//!
//! Pour une caméra qui n'a pas de flux réduit (ou dont la vignette n'est pas lue en direct : au-delà
//! de `MAX_LIVE_TILES`). Même principe que [`super::stream::StreamHandle`] : un fil par vignette,
//! qui s'arrête de lui-même quand l'interface ne l'appelle plus (`touch`) ou quand la session
//! change. Le serveur partage l'image entre tous les clients (cache de 15 s) : réessayer ne coûte
//! presque rien à la caméra.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use protocol::wire::camera_snapshot_path;

use super::error::NetError;
use super::http::{self, Request};
use super::stream::{current, current_generation};
use super::tls::Trust;
use matter_home_video::Image;

/// Pause entre deux images quand tout va bien.
pub const REFRESH_EVERY: Duration = Duration::from_secs(20);
/// Pause après un échec passager (caméra muette, test en cours).
pub const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Sans appel à [`SnapshotHandle::touch`] pendant ce temps, le fil s'arrête.
pub const IDLE_AFTER: Duration = Duration::from_millis(2500);
/// Délai d'une requête (le serveur borne la capture à 12 s).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Ce que la vignette sait de son image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotState {
    /// Première image en route.
    Loading,
    /// Une image est arrivée (la dernière reste affichée entre deux rafraîchissements).
    Ready,
    /// Dernier essai raté ; un autre suit.
    Failed(String),
    /// Le serveur ne connaît pas la route : plus ancien que ce client. Définitif.
    ServerTooOld,
    /// La caméra n'existe plus. Définitif.
    CameraGone,
    /// Pas de session, ou le serveur ne la reconnaît plus. Définitif.
    NoSession,
}

impl SnapshotState {
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            SnapshotState::ServerTooOld | SnapshotState::CameraGone | SnapshotState::NoSession
        )
    }
}

struct Shared {
    state: SnapshotState,
    image: Option<Image>,
}

struct Live {
    shared: Mutex<Shared>,
    epoch: Instant,
    touched_ms: AtomicU64,
    stop: AtomicBool,
    finished: AtomicBool,
    generation: Option<u64>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Live {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn should_stop(&self) -> bool {
        let idle = self
            .epoch
            .elapsed()
            .as_millis()
            .saturating_sub(u128::from(self.touched_ms.load(Ordering::Relaxed)))
            > IDLE_AFTER.as_millis();
        self.stop.load(Ordering::SeqCst) || idle || current_generation() != self.generation
    }

    fn set_state(&self, state: SnapshotState) {
        self.lock().state = state;
        (self.notify)();
    }
}

/// Une vignette qui se rafraîchit. La lâcher arrête le fil.
pub struct SnapshotHandle {
    live: Arc<Live>,
}

impl SnapshotHandle {
    pub fn open(camera_id: u64, notify: impl Fn() + Send + Sync + 'static) -> SnapshotHandle {
        let live = Arc::new(Live {
            shared: Mutex::new(Shared {
                state: SnapshotState::Loading,
                image: None,
            }),
            epoch: Instant::now(),
            touched_ms: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            generation: current_generation(),
            notify: Arc::new(notify),
        });
        let thread_live = live.clone();
        let spawned = std::thread::Builder::new()
            .name("matter-snapshot".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&thread_live, camera_id)
                }));
                if result.is_err() {
                    thread_live.set_state(SnapshotState::Failed("erreur interne".into()));
                }
                thread_live.finished.store(true, Ordering::SeqCst);
                (thread_live.notify)();
            });
        if spawned.is_err() {
            live.set_state(SnapshotState::Failed("ressources épuisées".into()));
            live.finished.store(true, Ordering::SeqCst);
        }
        let handle = SnapshotHandle { live };
        handle.touch();
        handle
    }

    /// La vignette est affichée : à appeler à chaque image de l'interface.
    pub fn touch(&self) {
        self.live.touched_ms.store(
            self.live.epoch.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn state(&self) -> SnapshotState {
        self.live.lock().state.clone()
    }

    /// La dernière image reçue, une seule fois.
    pub fn take_image(&self) -> Option<Image> {
        self.live.lock().image.take()
    }

    /// Le fil est terminé (page quittée trop longtemps, état définitif, autre session).
    pub fn is_finished(&self) -> bool {
        self.live.finished.load(Ordering::SeqCst)
    }

    /// Cette liaison date d'une autre session (reconnexion) : elle ne vaut plus rien.
    pub fn is_stale(&self) -> bool {
        self.live.generation != current_generation()
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        self.live.stop.store(true, Ordering::SeqCst);
    }
}

fn run(live: &Arc<Live>, camera_id: u64) {
    while !live.should_stop() {
        let wait = match fetch(camera_id) {
            Ok(image) => {
                {
                    let mut shared = live.lock();
                    shared.image = Some(image);
                    shared.state = SnapshotState::Ready;
                }
                (live.notify)();
                REFRESH_EVERY
            }
            Err(Failure::Final(state)) => {
                live.set_state(state);
                return;
            }
            Err(Failure::Retry(reason)) => {
                // La dernière image reste affichée ; seul l'état change.
                live.set_state(SnapshotState::Failed(reason));
                RETRY_AFTER
            }
        };
        let end = Instant::now() + wait;
        while Instant::now() < end {
            if live.should_stop() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

enum Failure {
    Final(SnapshotState),
    Retry(String),
}

fn fetch(camera_id: u64) -> Result<Image, Failure> {
    let Some((_, creds)) = current() else {
        return Err(Failure::Final(SnapshotState::NoSession));
    };
    let request = Request::get(camera_snapshot_path(camera_id)).with_timeout(REQUEST_TIMEOUT);
    let trust = Trust::Pinned(creds.fingerprint.clone());
    let response = http::exec(&creds.addr, &trust, Some(&creds.token), &request)
        .map_err(|e| Failure::Retry(e.message()))?;
    if response.status < 400 {
        return decode_jpeg(&response.body).ok_or_else(|| Failure::Retry("image illisible".into()));
    }
    match response.into_result() {
        Ok(_) => Err(Failure::Retry("réponse inattendue".into())),
        Err(NetError::Api {
            status: 404,
            detail: None,
            ..
        }) => Err(Failure::Final(SnapshotState::ServerTooOld)),
        Err(NetError::Api { status: 404, .. }) => Err(Failure::Final(SnapshotState::CameraGone)),
        Err(NetError::Api { status: 401, .. }) => Err(Failure::Final(SnapshotState::NoSession)),
        Err(NetError::Api { detail, status, .. }) => {
            Err(Failure::Retry(detail.unwrap_or_else(|| {
                format!("le serveur a répondu {status}")
            })))
        }
        Err(other) => Err(Failure::Retry(other.message())),
    }
}

/// JPEG vers pixels RGB.
pub fn decode_jpeg(bytes: &[u8]) -> Option<Image> {
    let mut decoder = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(bytes));
    let rgb = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (usize::from(info.width), usize::from(info.height));
    if width == 0 || height == 0 || rgb.len() != width * height * 3 {
        return None;
    }
    Some(Image {
        width: u32::from(info.width),
        height: u32::from(info.height),
        rgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_decodes_to_rgb_pixels() {
        // Un petit JPEG gris uni, fabriqué par `jpeg-encoder` (la caisse du serveur).
        let mut jpeg = Vec::new();
        jpeg_bytes(&mut jpeg);
        let image = decode_jpeg(&jpeg).expect("image");
        assert_eq!((image.width, image.height), (16, 8));
        assert_eq!(image.rgb.len(), 16 * 8 * 3);
        assert!(image.rgb.iter().all(|v| (120..=136).contains(v)));
    }

    fn jpeg_bytes(out: &mut Vec<u8>) {
        let pixels = vec![128u8; 16 * 8 * 3];
        jpeg_encoder::Encoder::new(out, 90)
            .encode(&pixels, 16, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
    }

    #[test]
    fn garbage_is_not_an_image() {
        assert!(decode_jpeg(b"pas un jpeg").is_none());
        assert!(decode_jpeg(&[]).is_none());
    }

    #[test]
    fn only_these_states_are_final() {
        assert!(SnapshotState::ServerTooOld.is_final());
        assert!(SnapshotState::CameraGone.is_final());
        assert!(SnapshotState::NoSession.is_final());
        assert!(!SnapshotState::Loading.is_final());
        assert!(!SnapshotState::Failed("x".into()).is_final());
    }
}
