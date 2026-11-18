//! Le superviseur et la veille des caméras en Surveillance (voir `detection/mod.rs`).
//!
//! - Le **superviseur** ([`supervise`]) tourne sur le runtime réseau (celui des sessions du
//!   direct) : il charge le détecteur, nettoie les événements (au démarrage puis toutes les heures),
//!   et toutes les 2 s, ou dès que le mode ou une caméra change, compare les caméras à surveiller
//!   (`Hub::cameras_to_watch`) à celles qu'il surveille : il démarre ou arrête leur veille.
//! - La **veille** d'une caméra ([`watch_camera`]) est un spectateur du registre partagé : la même
//!   session RTSP que le direct, jamais une de plus. Elle garde le pré-enregistrement et passe les
//!   images au fil d'analyse par une file bornée ; si la session finit (caméra modifiée, éteinte),
//!   elle se réabonne, avec une attente croissante.
//! - Le **fil d'analyse** ([`analyse_loop`]), un par caméra, décode toutes les images (une image P
//!   a besoin des précédentes) mais n'en convertit que [`DetectionConfig::fps`] par seconde ;
//!   mouvement, puis détecteur (un seul pour toutes les caméras, derrière un verrou), puis passage.

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use tokio::sync::watch;

use protocol::wire::{DetectionEventView, StreamKind, StreamPacket};

use matter_home_video::{nal, Decoder, FrameSink, Image};

use super::detector::Detector;
use super::motion::{Motion, MotionConfig};
use super::mp4;
use super::prebuffer::{Clip, Frame, PrebufferConfig, Recorder};
use super::tracker::{accept, Passage, Step, Tracker, TrackerConfig};
use super::DetectionConfig;
use crate::cameras::live::Event;
use crate::cameras::snapshot::{encode_jpeg, SnapshotConfig};
use crate::hub::Hub;

/// Le détecteur, partagé par toutes les caméras (une image à la fois).
pub type SharedDetector = Arc<Mutex<Box<dyn Detector>>>;

/// Images en attente pour le fil d'analyse ; au-delà, la machine ne suit plus.
const QUEUE: usize = 90;
const RECONCILE_EVERY: Duration = Duration::from_secs(2);
const RETRY_LOAD_EVERY: Duration = Duration::from_secs(300);

/// Le décodage des images d'une caméra (VideoToolbox, ou synthétique dans les tests).
pub trait FrameDecoder: Send {
    /// Décode `packet` ; si `want`, rend l'image décodée de cette image.
    fn push(&mut self, packet: &StreamPacket, want: bool) -> Result<Option<Image>, String>;
}

struct VideoToolboxFrames {
    decoder: Decoder,
    sink: Arc<FrameSink>,
}

impl FrameDecoder for VideoToolboxFrames {
    fn push(&mut self, packet: &StreamPacket, want: bool) -> Result<Option<Image>, String> {
        self.sink.set_wanted(want);
        self.decoder.push(packet).map_err(|e| e.to_string())?;
        Ok(if want { self.sink.take() } else { None })
    }
}

pub type DecoderFactory = Arc<dyn Fn() -> Result<Box<dyn FrameDecoder>, String> + Send + Sync>;

/// Le décodeur matériel de macOS.
pub fn videotoolbox() -> DecoderFactory {
    Arc::new(|| {
        let sink = FrameSink::new(|| {});
        let decoder = Decoder::new(sink.clone()).map_err(|e| e.to_string())?;
        Ok(Box::new(VideoToolboxFrames { decoder, sink }) as Box<dyn FrameDecoder>)
    })
}

fn unix_now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

/// Le superviseur de production : charge le détecteur depuis `<données>/models/`, puis surveille.
/// Sans détecteur, aucune caméra n'est ouverte ; un nouvel essai a lieu toutes les 5 min, ou dès
/// qu'une caméra ou le mode change (un modèle déposé entre-temps est pris).
pub async fn supervise(hub: Arc<Hub>, mut shutdown: watch::Receiver<bool>) {
    let detection = hub.detection().clone();
    tokio::spawn(prune_loop(detection.clone(), shutdown.clone()));
    let mut last_reason = String::new();
    let detector = loop {
        let models = detection.models_dir().to_path_buf();
        let loaded = tokio::task::spawn_blocking(move || super::onnx::OnnxDetector::load(&models))
            .await
            .unwrap_or_else(|_| Err("chargement du détecteur interrompu".into()));
        match loaded {
            Ok(detector) => break detector,
            Err(why) => {
                if why != last_reason {
                    log::warn!(
                        "détection désactivée : {why} (les caméras en Surveillance se comportent \
                         comme en Diffusion, voir docs/detection.md)"
                    );
                    last_reason = why.clone();
                }
                detection.update_status(|s| {
                    s.available = false;
                    s.engine = format!("désactivée : {why}");
                    s.watched.clear();
                });
            }
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            () = tokio::time::sleep(RETRY_LOAD_EVERY) => {}
            () = detection.woken() => {}
        }
    };
    let engine = detector.engine();
    log::info!("détection prête : {engine}");
    detection.update_status(|s| {
        s.available = true;
        s.engine = engine;
    });
    let detector: SharedDetector = Arc::new(Mutex::new(Box::new(detector)));
    run(hub, detector, videotoolbox(), shutdown).await;
}

/// Rétention des événements : au démarrage puis toutes les heures.
async fn prune_loop(detection: Arc<super::Detection>, mut shutdown: watch::Receiver<bool>) {
    loop {
        let d = detection.clone();
        let report = tokio::task::spawn_blocking(move || d.store().prune(unix_now())).await;
        if let Ok(report) = report {
            if report.removed > 0 {
                log::info!(
                    "événements : {} retirés (7 jours ou plafond de taille), {} gardés",
                    report.removed,
                    report.kept
                );
            }
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            () = tokio::time::sleep(super::store::PRUNE_EVERY) => {}
        }
    }
}

struct Running {
    stop: watch::Sender<bool>,
    kind: StreamKind,
}

/// Démarre et arrête la veille des caméras selon le mode et les caméras (publique à la crate pour
/// les tests, qui fournissent leur détecteur et leur décodeur).
pub(crate) async fn run(
    hub: Arc<Hub>,
    detector: SharedDetector,
    decoders: DecoderFactory,
    mut shutdown: watch::Receiver<bool>,
) {
    let detection = hub.detection().clone();
    let mut running: HashMap<u64, Running> = HashMap::new();
    loop {
        let wanted: HashMap<u64, (String, StreamKind)> = hub
            .cameras_to_watch()
            .into_iter()
            .map(|(id, name, kind)| (id, (name, kind)))
            .collect();
        running.retain(|id, r| {
            let keep = wanted.get(id).is_some_and(|(_, kind)| *kind == r.kind);
            if !keep {
                let _ = r.stop.send(true);
            }
            keep
        });
        for (id, (name, kind)) in &wanted {
            if running.contains_key(id) {
                continue;
            }
            log::info!("détection : surveillance de « {name} » ({id:#x})");
            let (stop, stopped) = watch::channel(false);
            tokio::spawn(watch_camera(
                hub.clone(),
                *id,
                *kind,
                detector.clone(),
                decoders.clone(),
                stopped,
            ));
            running.insert(*id, Running { stop, kind: *kind });
        }
        detection.set_watched(&running.keys().copied().collect::<BTreeSet<u64>>());
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(RECONCILE_EVERY) => {}
            () = detection.woken() => {}
        }
    }
    for r in running.values() {
        let _ = r.stop.send(true);
    }
    detection.set_watched(&BTreeSet::new());
}

/// La veille d'une caméra, jusqu'à `stop`.
async fn watch_camera(
    hub: Arc<Hub>,
    id: u64,
    kind: StreamKind,
    detector: SharedDetector,
    decoders: DecoderFactory,
    mut stop: watch::Receiver<bool>,
) {
    let config = hub.detection().config();
    let recorder = Arc::new(Mutex::new(Recorder::new(PrebufferConfig::default())));
    let (tx, rx) = sync_channel::<StreamPacket>(QUEUE);
    {
        let (hub, recorder) = (hub.clone(), recorder.clone());
        let config = config.clone();
        let spawned = std::thread::Builder::new()
            .name("detection".into())
            .spawn(move || analyse_loop(&hub, id, &rx, &recorder, &detector, &decoders, &config));
        if let Err(e) = spawned {
            log::error!("détection de {id:#x} impossible : {e}");
            return;
        }
    }
    let mut feed = Feed::new(tx, config.pause());
    let mut wait = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            break;
        }
        let mut subscription = match hub.live().subscribe(&hub, id, kind) {
            Ok(s) => s,
            Err(_) => {
                tokio::select! {
                    _ = stop.changed() => break,
                    () = tokio::time::sleep(wait) => {}
                }
                wait = (wait * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        wait = Duration::from_secs(1);
        lock(&recorder).reset();
        feed.restart();
        let ended = loop {
            let event = tokio::select! {
                _ = stop.changed() => break false,
                event = subscription.next() => event,
            };
            match event {
                Event::Status(_) => {}
                Event::End(_) => break true,
                Event::Video(bytes) => match StreamPacket::decode(&bytes) {
                    Ok(packet) => {
                        if let StreamPacket::Config {
                            width,
                            height,
                            avcc,
                            ..
                        } = &packet
                        {
                            lock(&recorder).set_config(avcc, *width, *height);
                        }
                        if let StreamPacket::Frame {
                            key,
                            timestamp_ms,
                            data,
                        } = &packet
                        {
                            lock(&recorder).push(Frame {
                                at: Instant::now(),
                                ts_ms: u64::from(*timestamp_ms),
                                key: *key,
                                data: Bytes::copy_from_slice(data),
                            });
                        }
                        // `false` : le fil d'analyse s'est arrêté (pas de décodeur). La session
                        // reste ouverte quand même : le pré-enregistrement ne sert alors à rien,
                        // mais l'état de la caméra reste suivi.
                        let _ = feed.send(packet, &hub);
                    }
                    Err(_) => continue,
                },
            }
        };
        if !ended {
            break;
        }
        tokio::select! {
            _ = stop.changed() => break,
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    // `feed` (et la file) part ici : le fil d'analyse clôt un passage en cours et s'arrête.
    drop(feed);
    hub.detection().update_status(|s| s.paused = false);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// L'envoi des images au fil d'analyse, avec la pause en cas de surcharge.
struct Feed {
    tx: SyncSender<StreamPacket>,
    /// Images sautées jusqu'à la prochaine image clé.
    waiting_for_key: bool,
    paused_until: Option<Instant>,
    pause: Duration,
    alive: bool,
}

impl Feed {
    fn new(tx: SyncSender<StreamPacket>, pause: Duration) -> Feed {
        Feed {
            tx,
            waiting_for_key: true,
            paused_until: None,
            pause,
            alive: true,
        }
    }

    fn restart(&mut self) {
        self.waiting_for_key = true;
    }

    /// `false` : le fil d'analyse ne reçoit plus rien.
    fn send(&mut self, packet: StreamPacket, hub: &Hub) -> bool {
        if !self.alive {
            return false;
        }
        let is_key = matches!(packet, StreamPacket::Frame { key: true, .. });
        let is_frame = matches!(packet, StreamPacket::Frame { .. });
        if let Some(until) = self.paused_until {
            if Instant::now() < until {
                if !is_frame {
                    let _ = self.tx.try_send(packet);
                }
                return true;
            }
            self.paused_until = None;
            self.waiting_for_key = true;
            log::info!("détection reprise");
            hub.detection().update_status(|s| s.paused = false);
        }
        if is_frame && self.waiting_for_key && !is_key {
            return true;
        }
        match self.tx.try_send(packet) {
            Ok(()) => {
                if is_key {
                    self.waiting_for_key = false;
                }
                true
            }
            Err(TrySendError::Full(_)) => {
                log::warn!(
                    "détection suspendue {} s : l'analyse ne suit plus (machine surchargée)",
                    self.pause.as_secs()
                );
                hub.detection().update_status(|s| s.paused = true);
                self.paused_until = Some(Instant::now() + self.pause);
                self.waiting_for_key = true;
                true
            }
            Err(TrySendError::Disconnected(_)) => {
                self.alive = false;
                false
            }
        }
    }
}

/// Le fil d'analyse d'une caméra : jusqu'à la fermeture de la file.
fn analyse_loop(
    hub: &Arc<Hub>,
    id: u64,
    rx: &Receiver<StreamPacket>,
    recorder: &Arc<Mutex<Recorder>>,
    detector: &SharedDetector,
    decoders: &DecoderFactory,
    config: &DetectionConfig,
) {
    let mut decoder = match decoders() {
        Ok(d) => d,
        Err(why) => {
            log::error!("détection de {id:#x} : décodeur indisponible : {why}");
            return;
        }
    };
    let interval = config.interval();
    let mut motion = Motion::new(MotionConfig::default());
    let mut tracker: Tracker<Vec<u8>> = Tracker::new(TrackerConfig {
        close_after: config.close_after_s.clamp(2.0, 60.0),
    });
    let mut settings = hub.camera_detection(id).map(|(_, s)| s);
    let mut next_due = Instant::now();
    let mut errors = 0u32;
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(packet) => {
                let want =
                    matches!(packet, StreamPacket::Frame { .. }) && Instant::now() >= next_due;
                match decoder.push(&packet, want) {
                    Ok(Some(image)) => {
                        next_due = Instant::now() + interval;
                        // Réglages relus à chaque image analysée (2 par seconde) : une zone
                        // modifiée dans la fenêtre du serveur s'applique tout de suite.
                        let fresh = hub.camera_detection(id).map(|(_, s)| s);
                        if fresh != settings {
                            motion.reset_mask();
                            settings = fresh;
                        }
                        let Some(current) = &settings else { continue };
                        let (_, moved) = motion.update(&image, current);
                        // Pendant un passage, on regarde chaque image analysée : une personne
                        // immobile reste une personne.
                        let found = if moved || tracker.is_open() {
                            match lock(detector).detect(&image) {
                                Ok(found) => accept(&found, current),
                                Err(why) => {
                                    if errors < 3 {
                                        log::warn!("détecteur : {why}");
                                    }
                                    errors += 1;
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        let snapshot = || {
                            let config = SnapshotConfig {
                                max_width: 960,
                                quality: 80,
                                ..SnapshotConfig::default()
                            };
                            encode_jpeg(&image, &config)
                                .map(|b| b.to_vec())
                                .unwrap_or_default()
                        };
                        let step = tracker.observe(unix_now(), &found, snapshot);
                        match step {
                            Step::Opened => lock(recorder).start_clip(),
                            Step::Closed(passage) => {
                                // Le verrou du tampon est lâché avant le MP4 et l'écriture : la veille y pousse chaque image.
                                let clip = lock(recorder).finish_clip();
                                finish(hub, id, passage, clip);
                                if !found.is_empty() {
                                    // L'image qui a clos le passage précédent en ouvre un autre.
                                    tracker.observe(unix_now(), &found, snapshot);
                                    lock(recorder).start_clip();
                                }
                            }
                            Step::Nothing | Step::Extended => {}
                        }
                    }
                    Ok(None) => {}
                    Err(why) => {
                        log::error!("détection de {id:#x} : décodage impossible : {why}");
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(passage) = tracker.tick(unix_now()) {
            // Le verrou du tampon est lâché avant le MP4 et l'écriture : la veille y pousse chaque image.
            let clip = lock(recorder).finish_clip();
            finish(hub, id, passage, clip);
        }
    }
    // Arrêt de la veille (changement de mode, caméra modifiée) : le passage en cours est gardé.
    if let Some(passage) = tracker.flush() {
        // Le verrou du tampon est lâché avant le MP4 et l'écriture : la veille y pousse chaque image.
        let clip = lock(recorder).finish_clip();
        finish(hub, id, passage, clip);
    }
}

/// Le clip en MP4 : images sans SPS ni PPS (ils sont dans l'`avcC`), depuis une image clé.
pub fn clip_to_mp4(clip: &Clip) -> Option<(Vec<u8>, f64)> {
    let samples: Vec<mp4::Sample> = clip
        .frames
        .iter()
        .filter_map(|f| {
            let sample = nal::prepare_sample(&f.data).ok()?;
            Some(mp4::Sample {
                ts_ms: f.ts_ms,
                key: f.key && sample.idr,
                data: sample.data,
            })
        })
        .skip_while(|s| !s.key)
        .collect();
    let bytes = mp4::mux(&clip.avcc, clip.width, clip.height, &samples).ok()?;
    Some((bytes, clip.duration().as_secs_f64()))
}

/// Enregistre et annonce un passage terminé.
fn finish(hub: &Hub, id: u64, passage: Passage<Vec<u8>>, clip: Option<Clip>) {
    let name = hub
        .camera_detection(id)
        .map(|(name, _)| name)
        .unwrap_or_default();
    let mp4 = clip.as_ref().and_then(clip_to_mp4);
    let event = DetectionEventView {
        id: format!("{}-{id}", (passage.start * 1000.0).max(0.0) as u64),
        camera_id: id,
        camera_name: name.clone(),
        classes: passage.ranked(),
        score: passage.score(),
        start: passage.start,
        end: passage.last,
        has_clip: mp4.is_some(),
        clip_s: mp4.as_ref().map_or(0.0, |(_, s)| *s),
    };
    let jpeg = passage
        .best
        .as_ref()
        .map(|(_, j)| j.as_slice())
        .filter(|j| !j.is_empty());
    if let Err(e) =
        hub.detection()
            .store()
            .save(&event, jpeg, mp4.as_ref().map(|(b, _)| b.as_slice()))
    {
        log::error!("événement de « {name} » non enregistré : {e}");
        return;
    }
    log::info!(
        "détection sur « {name} » ({id:#x}) : {} (score {:.2}, clip {:.0} s)",
        event.class().lower(),
        event.score,
        event.clip_s
    );
    hub.publish_detection(&event);
}

#[cfg(test)]
mod tests;
