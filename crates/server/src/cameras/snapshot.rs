//! Image fixe d'une caméra (ticket n° 12) : `GET /api/cameras/{id}/snapshot`.
//!
//! Une caméra sans flux réduit n'est pas ouverte dans la mosaïque (une G100 supporte mal plusieurs
//! flux à la fois) : les clients affichent à la place une **image fixe**, rafraîchie de temps en
//! temps. Le serveur la fabrique ici, sans FFmpeg : le H.264 est décodé par VideoToolbox (caisse
//! `matter-home-video`, la même que le client) puis encodé en JPEG par `jpeg-encoder` (Rust pur).
//!
//! # Règles
//!
//! - **Cache partagé** : une image par caméra, valable [`SnapshotConfig::max_age`] (15 s). Tous les
//!   clients, toutes les vignettes la partagent : la caméra n'est jamais plus sollicitée parce qu'on
//!   est plus nombreux à regarder.
//! - **Une capture à la fois par caméra** : les demandes qui arrivent pendant une capture font la
//!   queue et reçoivent son résultat (image, ou échec gardé [`SnapshotConfig::failure_ttl`]). Le
//!   tout est borné par [`SnapshotConfig::timeout`].
//! - **Spectateurs** : on passe par le registre de la lecture ([`LiveRegistry::subscribe_any`]). Si
//!   la caméra est déjà regardée (n'importe quel flux), on se joint à la session en cours : aucune
//!   connexion de plus, et l'image vient de la dernière image clé gardée. Sinon une session s'ouvre
//!   (un [`ViewerLease`](super::ViewerLease) pour toute sa vie : la surveillance et « Tester » la
//!   sautent), on attend la **première image clé**, puis on se retire : la session se ferme après
//!   sa grâce, comme pour un spectateur qui part. Jamais plus de deux connexions à la caméra.
//! - **Verrou de la caméra** : avant d'ouvrir ou de rejoindre une session, on prend (sans attendre)
//!   `Hub::camera_probe_lock`, le verrou de la surveillance et du testeur. Pris (un test ou une
//!   vérification est en cours) : [`SnapshotError::Busy`], rien n'est ouvert. Le verrou n'est tenu
//!   que le temps de prendre le témoin de spectateur : ensuite, c'est le compte des spectateurs qui
//!   tient la surveillance et le testeur à l'écart.
//! - **Démo** : pas de vraie caméra, l'image est synthétique.
//!
//! # Secrets
//!
//! Aucune adresse, aucun identifiant de caméra dans une erreur : seulement des motifs fixes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use protocol::wire::{StreamPacket, StreamStatus};

use matter_home_video::{Decoder, FrameSink, Image, VideoError};

use super::live::{Event, LiveRegistry, SubscribeError};
use crate::hub::Hub;

/// Réglages (les tests les raccourcissent).
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Âge maximal d'une image servie depuis le cache.
    pub max_age: Duration,
    /// Délai maximal d'une demande, file d'attente comprise.
    pub timeout: Duration,
    /// Un échec (caméra muette, éteinte) est gardé ce temps, pour que dix vignettes ne fassent pas
    /// dix essais l'un après l'autre.
    pub failure_ttl: Duration,
    /// Largeur maximale du JPEG (une vignette n'a pas besoin de plus).
    pub max_width: u32,
    /// Qualité JPEG, de 1 à 100.
    pub quality: u8,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(15),
            timeout: Duration::from_secs(12),
            failure_ttl: Duration::from_secs(3),
            max_width: 640,
            quality: 72,
        }
    }
}

/// Pourquoi on ne peut pas donner d'image. Pas de texte libre : rien de ce qui vient de la caméra
/// ou de son adresse ne passe par ici.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    /// Pas de caméra avec cet identifiant.
    UnknownCamera,
    /// Un test ou une vérification de la caméra est en cours : réessayer plus tard.
    Busy,
    /// La caméra n'a pas donné d'image clé à temps, ou ne répond pas.
    NoImage,
    /// La caméra a été modifiée ou supprimée pendant la capture.
    Changed,
    /// Codec que le serveur ne décode pas (H.265…).
    Unsupported,
    /// Pas de décodeur sur ce système, ou il refuse de démarrer.
    DecoderUnavailable,
}

/// Une image prête à servir.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub jpeg: Bytes,
    /// Depuis combien de temps elle a été prise.
    pub age: Duration,
}

#[derive(Default)]
struct SlotState {
    image: Option<(Instant, Bytes)>,
    failure: Option<(Instant, SnapshotError)>,
}

#[derive(Default)]
struct Slot {
    /// Une capture à la fois ; les autres demandes attendent ici.
    gate: tokio::sync::Mutex<()>,
    state: Mutex<SlotState>,
}

impl Slot {
    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Le cache et la file d'attente des images fixes, une entrée par caméra.
pub struct SnapshotService {
    config: SnapshotConfig,
    slots: Mutex<HashMap<u64, Arc<Slot>>>,
}

impl SnapshotService {
    pub fn new(config: SnapshotConfig) -> Arc<SnapshotService> {
        Arc::new(SnapshotService {
            config,
            slots: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &SnapshotConfig {
        &self.config
    }

    fn slot(&self, id: u64) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.entry(id).or_default().clone()
    }

    /// L'image de la caméra : celle du cache si elle est assez récente, sinon une capture.
    pub async fn get(
        &self,
        live: &Arc<LiveRegistry>,
        hub: &Arc<Hub>,
        id: u64,
    ) -> Result<Snapshot, SnapshotError> {
        if hub.camera_probe_target(id).is_none() {
            // Une caméra supprimée n'a plus d'image, même gardée.
            self.slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(SnapshotError::UnknownCamera);
        }
        let slot = self.slot(id);
        let whole = async {
            if let Some(snapshot) = self.cached(&slot)? {
                return Ok(snapshot);
            }
            let _turn = slot.gate.lock().await;
            // La capture qu'on attendait a peut-être déjà répondu.
            if let Some(snapshot) = self.cached(&slot)? {
                return Ok(snapshot);
            }
            let started = Instant::now();
            let outcome = self.capture(live, hub, id).await;
            let mut state = slot.state();
            match &outcome {
                Ok(jpeg) => {
                    state.image = Some((started, jpeg.clone()));
                    state.failure = None;
                }
                // Un refus passager (test en cours) ne se garde pas : la caméra n'y est pour rien.
                Err(SnapshotError::Busy) => {}
                Err(error) => state.failure = Some((Instant::now(), *error)),
            }
            outcome.map(|jpeg| Snapshot {
                jpeg,
                age: started.elapsed(),
            })
        };
        match tokio::time::timeout(self.config.timeout, whole).await {
            Ok(result) => result,
            Err(_) => Err(SnapshotError::NoImage),
        }
    }

    /// `Ok(Some)` : une image assez récente. `Err` : un échec récent gardé. `Ok(None)` : il faut
    /// capturer.
    fn cached(&self, slot: &Slot) -> Result<Option<Snapshot>, SnapshotError> {
        let state = slot.state();
        if let Some((at, jpeg)) = &state.image {
            let age = at.elapsed();
            if age <= self.config.max_age {
                return Ok(Some(Snapshot {
                    jpeg: jpeg.clone(),
                    age,
                }));
            }
        }
        if let Some((at, error)) = state.failure {
            if at.elapsed() <= self.config.failure_ttl {
                return Err(error);
            }
        }
        Ok(None)
    }

    async fn capture(
        &self,
        live: &Arc<LiveRegistry>,
        hub: &Arc<Hub>,
        id: u64,
    ) -> Result<Bytes, SnapshotError> {
        if hub.is_demo() {
            return encode_jpeg(&demo_image(id), &self.config).ok_or(SnapshotError::NoImage);
        }
        let mut subscription = {
            let lock = hub.camera_probe_lock(id);
            // Sans attendre : un test ou une vérification en cours garde la caméra pour lui.
            let _held = match lock.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return Err(SnapshotError::Busy),
            };
            live.subscribe_any(hub, id).map_err(|e| match e {
                SubscribeError::UnknownCamera => SnapshotError::UnknownCamera,
                SubscribeError::NoSuchStream => SnapshotError::NoImage,
            })?
        };
        let deadline = tokio::time::Instant::now()
            + self
                .config
                .timeout
                .saturating_sub(Duration::from_millis(500));
        let mut config: Option<StreamPacket> = None;
        loop {
            let event = tokio::time::timeout_at(deadline, subscription.next())
                .await
                .map_err(|_| SnapshotError::NoImage)?;
            match event {
                Event::Status(StreamStatus::Unsupported { .. }) => {
                    return Err(SnapshotError::Unsupported)
                }
                // Connexion, reprise : on attend l'image clé, le délai tranchera.
                Event::Status(_) => {}
                Event::End(super::live::EndReason::Changed) => return Err(SnapshotError::Changed),
                Event::End(_) => return Err(SnapshotError::NoImage),
                Event::Video(bytes) => match StreamPacket::decode(&bytes) {
                    Ok(packet @ StreamPacket::Config { .. }) => config = Some(packet),
                    Ok(packet @ StreamPacket::Frame { key: true, .. }) => {
                        let Some(config) = config.clone() else {
                            continue;
                        };
                        let image =
                            tokio::task::spawn_blocking(move || decode_key_frame(&config, &packet))
                                .await
                                .map_err(|_| SnapshotError::NoImage)?;
                        match image {
                            Ok(Some(image)) => {
                                return encode_jpeg(&image, &self.config)
                                    .ok_or(SnapshotError::NoImage)
                            }
                            // Image clé illisible : la suivante peut l'être.
                            Ok(None) => {}
                            Err(_) => return Err(SnapshotError::DecoderUnavailable),
                        }
                    }
                    _ => {}
                },
            }
        }
    }
}

/// Décode une image clé avec un décodeur tout neuf (rien ne reste entre deux captures : pas de
/// session VideoToolbox gardée pour rien). `Ok(None)` : image refusée par le décodeur.
fn decode_key_frame(
    config: &StreamPacket,
    frame: &StreamPacket,
) -> Result<Option<Image>, VideoError> {
    let sink = FrameSink::new(|| {});
    let mut decoder = Decoder::new(sink.clone())?;
    decoder.push(config)?;
    decoder.push(frame)?;
    let started = Instant::now();
    while sink.frames() == 0 && started.elapsed() < Duration::from_millis(400) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Fermer le décodeur vide ses images en attente : elles arrivent dans `sink`.
    drop(decoder);
    Ok(sink.take())
}

/// Réduit si besoin (au plus `max_width`, plus proche voisin : assez pour une vignette) puis
/// encode en JPEG.
pub fn encode_jpeg(image: &Image, config: &SnapshotConfig) -> Option<Bytes> {
    let (width, height) = (image.width as usize, image.height as usize);
    if width == 0 || height == 0 || image.rgb.len() != width * height * 3 {
        return None;
    }
    let max = config.max_width.max(16) as usize;
    let (out_w, out_h, pixels): (usize, usize, std::borrow::Cow<[u8]>) = if width > max {
        let out_w = max;
        let out_h = (height * max / width).max(1);
        let mut scaled = Vec::with_capacity(out_w * out_h * 3);
        for y in 0..out_h {
            let sy = y * height / out_h;
            for x in 0..out_w {
                let sx = x * width / out_w;
                let at = (sy * width + sx) * 3;
                scaled.extend_from_slice(&image.rgb[at..at + 3]);
            }
        }
        (out_w, out_h, scaled.into())
    } else {
        (width, height, (&image.rgb[..]).into())
    };
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, config.quality.clamp(1, 100));
    encoder
        .encode(
            &pixels,
            u16::try_from(out_w).ok()?,
            u16::try_from(out_h).ok()?,
            jpeg_encoder::ColorType::Rgb,
        )
        .ok()?;
    Some(Bytes::from(out))
}

/// Image de la démo : un dégradé qui change d'une caméra à l'autre et d'une minute à l'autre.
fn demo_image(id: u64) -> Image {
    let (width, height) = (320u32, 180u32);
    let seed = (id % 251) as u32 + 40 * (chrono::Utc::now().timestamp() / 60 % 6) as u32;
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            rgb.push(((x * 255 / width + seed) % 256) as u8);
            rgb.push(((y * 255 / height + seed * 2) % 256) as u8);
            rgb.push(((seed * 3 + 90) % 256) as u8);
        }
    }
    Image { width, height, rgb }
}

#[cfg(test)]
mod tests;
