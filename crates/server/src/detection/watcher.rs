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
    task: tokio::task::JoinHandle<()>,
    started: Instant,
}

/// Une veille qui s'est arrêtée seule (décodeur indisponible) : réessayée plus tard.
struct Backoff {
    until: Instant,
    wait: Duration,
}

/// Attente maximale, à l'arrêt du serveur, de l'écriture des événements en cours.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);
const RETRY_FIRST: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// Démarre et arrête la veille des caméras selon le mode et les caméras (publique à la crate pour
/// les tests, qui fournissent leur détecteur et leur décodeur). `watched` dit ce qui tourne
/// vraiment : une veille arrêtée seule en sort, et revient après une attente croissante.
pub(crate) async fn run(
    hub: Arc<Hub>,
    detector: SharedDetector,
    decoders: DecoderFactory,
    mut shutdown: watch::Receiver<bool>,
) {
    let detection = hub.detection().clone();
    let (writer, writer_thread) = Writer::spawn(hub.clone());
    let mut running: HashMap<u64, Running> = HashMap::new();
    let mut backoff: HashMap<u64, Backoff> = HashMap::new();
    loop {
        let wanted: HashMap<u64, (String, StreamKind)> = hub
            .cameras_to_watch()
            .into_iter()
            .map(|(id, name, kind)| (id, (name, kind)))
            .collect();
        running.retain(|id, r| {
            if r.task.is_finished() {
                // Arrêtée sans qu'on le demande : on attendra avant de réessayer.
                let wait = match backoff.get(id) {
                    Some(b) if r.started.elapsed() < Duration::from_secs(600) => {
                        (b.wait * 2).min(RETRY_MAX)
                    }
                    _ => RETRY_FIRST,
                };
                log::warn!(
                    "détection de {id:#x} arrêtée ; nouvel essai dans {} s",
                    wait.as_secs()
                );
                backoff.insert(
                    *id,
                    Backoff {
                        until: Instant::now() + wait,
                        wait,
                    },
                );
                return false;
            }
            let keep = wanted.get(id).is_some_and(|(_, kind)| *kind == r.kind);
            if !keep {
                let _ = r.stop.send(true);
            }
            keep
        });
        backoff.retain(|id, _| wanted.contains_key(id));
        for (id, (name, kind)) in &wanted {
            if running.contains_key(id) || backoff.get(id).is_some_and(|b| Instant::now() < b.until)
            {
                continue;
            }
            log::info!("détection : surveillance de « {name} » ({id:#x})");
            let (stop, stopped) = watch::channel(false);
            let task = tokio::spawn(watch_camera(
                hub.clone(),
                *id,
                *kind,
                detector.clone(),
                decoders.clone(),
                writer.clone(),
                stopped,
            ));
            running.insert(
                *id,
                Running {
                    stop,
                    kind: *kind,
                    task,
                    started: Instant::now(),
                },
            );
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
    drain(running, writer, writer_thread).await;
    detection.descriptions().shutdown();
}

/// Attente (bornée, [`SHUTDOWN_WAIT`] au total) de la fin des veilles, de la clôture de leurs
/// passages en cours et de l'écriture de leurs fiches : sans elle, un passage fermé par l'arrêt du
/// serveur serait perdu et son événement resterait « en cours » jusqu'au redémarrage.
async fn drain(
    running: HashMap<u64, Running>,
    writer: Writer,
    writer_thread: Option<std::thread::JoinHandle<()>>,
) {
    let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
    for (_, r) in running {
        let _ = tokio::time::timeout_at(deadline, r.task).await;
    }
    // Le fil d'écriture s'arrête quand tous les `Writer` sont lâchés : celui-ci, puis ceux des fils
    // d'analyse, qui clôturent leur passage en cours avant de s'arrêter.
    drop(writer);
    if let Some(thread) = writer_thread {
        let joined = tokio::task::spawn_blocking(move || thread.join());
        if tokio::time::timeout_at(deadline, joined).await.is_err() {
            log::warn!("détection : l'écriture des événements n'a pas fini à l'arrêt");
        }
    }
}

// ---------------------------------------------------------------------------
// L'écriture des événements : un fil à part
// ---------------------------------------------------------------------------

/// Ce que le fil d'analyse confie à l'écriture : rien de lent (MP4, JPEG sur disque, `fsync`,
/// nettoyage) ne se fait dans le fil d'analyse, dont la file se remplirait pour rien.
enum WriteJob {
    /// Un passage commence : annoncé « en cours », avec sa première capture.
    Open {
        event: DetectionEventView,
        jpeg: Vec<u8>,
    },
    /// Une classe nouvelle en cours de passage (une personne sort de la voiture) : l'événement
    /// en cours est mis à jour, et les règles qui suivent cette classe partent.
    Update {
        event: DetectionEventView,
        jpeg: Vec<u8>,
        new_classes: Vec<protocol::wire::DetectedClass>,
    },
    /// Il est fini : la capture finale et le clip.
    Close {
        event: DetectionEventView,
        jpeg: Vec<u8>,
        clip: Option<Clip>,
    },
}

#[derive(Clone)]
struct Writer(std::sync::mpsc::Sender<WriteJob>);

impl Writer {
    /// Le fil s'arrête quand toutes les veilles (et le superviseur) ont lâché leur `Writer`.
    fn spawn(hub: Arc<Hub>) -> (Writer, Option<std::thread::JoinHandle<()>>) {
        let (tx, rx) = std::sync::mpsc::channel::<WriteJob>();
        let spawned = std::thread::Builder::new()
            .name("evenements-disque".into())
            .spawn(move || {
                for job in rx {
                    write_job(&hub, job);
                }
            });
        if let Err(e) = &spawned {
            log::error!("fil d'écriture des événements impossible : {e}");
        }
        (Writer(tx), spawned.ok())
    }

    fn send(&self, job: WriteJob) {
        let _ = self.0.send(job);
    }
}

fn write_job(hub: &Arc<Hub>, job: WriteJob) {
    let (event, jpeg, clip, rules) = match job {
        WriteJob::Open { event, jpeg } => (event, jpeg, None, Some((true, Vec::new()))),
        WriteJob::Update {
            event,
            jpeg,
            new_classes,
        } => (event, jpeg, None, Some((false, new_classes))),
        WriteJob::Close { event, jpeg, clip } => (event, jpeg, clip, None),
    };
    let opening = rules.is_some();
    let mut event = event;
    let camera = event.camera_id;
    let mp4 = clip.as_ref().and_then(clip_to_mp4);
    if !opening {
        event.has_clip = mp4.is_some();
        event.clip_s = mp4.as_ref().map_or(0.0, |(_, s)| *s);
    }
    let jpeg = Some(jpeg.as_slice()).filter(|j| !j.is_empty());
    let saved =
        match hub
            .detection()
            .store()
            .save(event, jpeg, mp4.as_ref().map(|(b, _)| b.as_slice()))
        {
            Ok(saved) => saved,
            Err(e) => {
                log::error!("événement de {camera:#x} non enregistré : {e}");
                return;
            }
        };
    if opening {
        log::info!(
            "détection sur « {} » ({:#x}) : {} (score {:.2}), passage en cours",
            saved.camera_name,
            saved.camera_id,
            saved.class().lower(),
            saved.score
        );
    } else {
        log::info!(
            "détection sur « {} » ({:#x}) terminée : {} (score {:.2}, clip {:.0} s)",
            saved.camera_name,
            saved.camera_id,
            saved.class().lower(),
            saved.score,
            saved.clip_s
        );
    }
    // Les automatisations partent à l'ouverture (on veut être prévenu tout de suite), et pour une
    // classe nouvelle en cours de passage ; la fin ne fait que compléter l'événement.
    // Description (ticket n° 19) : à l'ouverture, on dit aux règles d'attendre si elle va venir
    // (avant de publier : la description ne doit pas arriver avant l'annonce) ; la demande à l'API
    // ne part qu'après la publication. À la fin du passage, la meilleure capture est envoyée.
    let descriptions = hub.detection().descriptions();
    let jpeg = jpeg.unwrap_or_default();
    match &rules {
        Some((true, _)) => {
            let awaiting = descriptions.on_open(hub, &saved, jpeg);
            hub.publish_detection(&saved, rules, awaiting);
            descriptions.describe_opening(hub, &saved, jpeg);
        }
        // Une classe nouvelle en cours de passage : les règles qui emploient la description
        // attendent aussi (elles partent à l'arrivée de la description, une seule fois).
        Some((false, _)) => {
            let awaiting = descriptions.is_awaiting(&saved.id);
            hub.publish_detection(&saved, rules, awaiting);
        }
        None => {
            hub.publish_detection(&saved, None, false);
            descriptions.on_close(hub, &saved, jpeg);
        }
    }
}

/// L'événement d'un passage (en cours ou fini). L'identifiant vient du début : le même à
/// l'ouverture et à la fin.
fn event_of<T>(hub: &Hub, id: u64, passage: &Passage<T>, in_progress: bool) -> DetectionEventView {
    let name = hub
        .camera_detection(id)
        .map(|(name, _)| name)
        .unwrap_or_default();
    DetectionEventView {
        id: format!("{}-{id}", (passage.start * 1000.0).max(0.0) as u64),
        camera_id: id,
        camera_name: name,
        classes: passage.ranked(),
        score: passage.score(),
        start: passage.start,
        end: passage.last,
        has_clip: false,
        clip_s: 0.0,
        in_progress,
        description: None,
        niveau: None,
    }
}

// ---------------------------------------------------------------------------
// La veille d'une caméra
// ---------------------------------------------------------------------------

/// La veille d'une caméra, jusqu'à `stop`, ou jusqu'à l'arrêt de son fil d'analyse (pas de
/// décodeur) : elle lâche alors la session RTSP et le superviseur réessaiera plus tard.
async fn watch_camera(
    hub: Arc<Hub>,
    id: u64,
    kind: StreamKind,
    detector: SharedDetector,
    decoders: DecoderFactory,
    writer: Writer,
    mut stop: watch::Receiver<bool>,
) {
    let recorder = Arc::new(Mutex::new(Recorder::new(PrebufferConfig::default())));
    let (tx, rx) = sync_channel::<StreamPacket>(QUEUE);
    {
        let (hub, recorder) = (hub.clone(), recorder.clone());
        let spawned = std::thread::Builder::new()
            .name("detection".into())
            .spawn(move || analyse_loop(&hub, id, &rx, &recorder, &detector, &decoders, &writer));
        if let Err(e) = spawned {
            log::error!("détection de {id:#x} impossible : {e}");
            return;
        }
    }
    let mut feed = Feed::new(tx);
    let mut wait = Duration::from_secs(1);
    'session: loop {
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
                Event::Video(bytes) => {
                    let Ok(packet) = StreamPacket::decode(&bytes) else {
                        continue;
                    };
                    match &packet {
                        StreamPacket::Config {
                            width,
                            height,
                            avcc,
                            ..
                        } => lock(&recorder).set_config(avcc, *width, *height),
                        StreamPacket::Frame {
                            key,
                            timestamp_ms,
                            data,
                        } => lock(&recorder).push(Frame {
                            at: Instant::now(),
                            ts_ms: u64::from(*timestamp_ms),
                            key: *key,
                            data: Bytes::copy_from_slice(data),
                        }),
                    }
                    if !feed.send(packet, &hub, id) {
                        // Le fil d'analyse s'est arrêté : la session est lâchée (le témoin de
                        // spectateur part avec elle), le superviseur réessaiera.
                        break 'session;
                    }
                }
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
    hub.detection().set_paused(id, QUEUE_FULL, None);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Les raisons d'une pause (une caméra peut en avoir plusieurs à la fois).
const QUEUE_FULL: &str = "file";
const SLOW: &str = "lent";
const HOT: &str = "chaud";

/// L'envoi des images au fil d'analyse, avec la pause en cas de surcharge.
struct Feed {
    tx: SyncSender<StreamPacket>,
    /// Images sautées jusqu'à la prochaine image clé.
    waiting_for_key: bool,
    paused_until: Option<Instant>,
    alive: bool,
}

impl Feed {
    fn new(tx: SyncSender<StreamPacket>) -> Feed {
        Feed {
            tx,
            waiting_for_key: true,
            paused_until: None,
            alive: true,
        }
    }

    fn restart(&mut self) {
        self.waiting_for_key = true;
    }

    /// `false` : le fil d'analyse ne reçoit plus rien (il s'est arrêté).
    fn send(&mut self, packet: StreamPacket, hub: &Hub, id: u64) -> bool {
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
            log::info!("détection de {id:#x} reprise");
            hub.detection().set_paused(id, QUEUE_FULL, None);
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
                let pause = hub.detection().config().pause();
                log::warn!(
                    "détection de {id:#x} suspendue {} s : l'analyse ne suit plus (machine surchargée)",
                    pause.as_secs()
                );
                hub.detection().set_paused(
                    id,
                    QUEUE_FULL,
                    Some("l'analyse ne suit plus le flux".into()),
                );
                self.paused_until = Some(Instant::now() + pause);
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

// ---------------------------------------------------------------------------
// Le fil d'analyse
// ---------------------------------------------------------------------------

/// Combien d'images analysées trop lentes d'affilée avant la pause.
const SLOW_FRAMES: u32 = 10;
/// La température est relue toutes les…
const THERMAL_EVERY: Duration = Duration::from_secs(10);

/// La charge du fil d'analyse : le temps passé par image analysée (décodage de l'image voulue,
/// mouvement, attente et travail du détecteur) comparé au budget (l'intervalle entre deux images
/// analysées), et la température du Mac.
struct Load {
    slow_streak: u32,
    slow_until: Option<Instant>,
    hot: bool,
    thermal_checked: Option<Instant>,
}

impl Load {
    fn new() -> Self {
        Self {
            slow_streak: 0,
            slow_until: None,
            hot: false,
            thermal_checked: None,
        }
    }

    /// `true` : on n'analyse pas en ce moment (trop lent récemment, ou trop chaud).
    fn paused(&mut self, hub: &Hub, id: u64) -> bool {
        if self
            .thermal_checked
            .is_none_or(|t| t.elapsed() >= THERMAL_EVERY)
        {
            self.thermal_checked = Some(Instant::now());
            let hot = super::too_hot();
            if hot != self.hot {
                self.hot = hot;
                if hot {
                    log::warn!("détection de {id:#x} suspendue : le Mac est trop chaud");
                    hub.detection()
                        .set_paused(id, HOT, Some("le Mac est trop chaud".into()));
                } else {
                    log::info!("détection de {id:#x} reprise : le Mac a refroidi");
                    hub.detection().set_paused(id, HOT, None);
                }
            }
        }
        if let Some(until) = self.slow_until {
            if Instant::now() < until {
                return true;
            }
            self.slow_until = None;
            log::info!("détection de {id:#x} reprise");
            hub.detection().set_paused(id, SLOW, None);
        }
        self.hot
    }

    fn record(&mut self, hub: &Hub, id: u64, spent: Duration, budget: Duration, pause: Duration) {
        if spent > budget {
            self.slow_streak += 1;
        } else {
            self.slow_streak = 0;
        }
        if self.slow_streak >= SLOW_FRAMES {
            self.slow_streak = 0;
            self.slow_until = Some(Instant::now() + pause);
            log::warn!(
                "détection de {id:#x} suspendue {} s : {} ms par image analysée pour {} ms de budget",
                pause.as_secs(),
                spent.as_millis(),
                budget.as_millis()
            );
            hub.detection().set_paused(
                id,
                SLOW,
                Some("analyse trop lente pour cette machine".into()),
            );
        }
    }
}

/// Le fil d'analyse d'une caméra : jusqu'à la fermeture de la file, ou un décodeur indisponible.
fn analyse_loop(
    hub: &Arc<Hub>,
    id: u64,
    rx: &Receiver<StreamPacket>,
    recorder: &Arc<Mutex<Recorder>>,
    detector: &SharedDetector,
    decoders: &DecoderFactory,
    writer: &Writer,
) {
    let mut decoder = match decoders() {
        Ok(d) => d,
        Err(why) => {
            log::error!("détection de {id:#x} : décodeur indisponible : {why}");
            return;
        }
    };
    let tracker_config = |c: &DetectionConfig| TrackerConfig {
        close_after: c.close_after_s.clamp(2.0, 60.0),
        max_duration: PrebufferConfig::default().clip_max.as_secs_f64(),
        ..TrackerConfig::default()
    };
    let mut config = hub.detection().config();
    let mut motion = Motion::new(MotionConfig::default());
    let mut tracker: Tracker<Vec<u8>> = Tracker::new(tracker_config(&config));
    let mut settings = hub.camera_detection(id).map(|(_, s)| s);
    let mut next_due = Instant::now();
    let mut errors = 0u32;
    let mut load = Load::new();
    let open = |tracker: &Tracker<Vec<u8>>| {
        lock(recorder).start_clip();
        if let Some(p) = tracker.current() {
            writer.send(WriteJob::Open {
                event: event_of(hub, id, p, true),
                jpeg: p.best.as_ref().map(|(_, j)| j.clone()).unwrap_or_default(),
            });
        }
    };
    let close = |passage: Passage<Vec<u8>>| {
        // Le verrou du tampon n'est tenu que le temps de prendre les images.
        let clip = lock(recorder).finish_clip();
        writer.send(WriteJob::Close {
            event: event_of(hub, id, &passage, false),
            jpeg: passage.best.map(|(_, j)| j).unwrap_or_default(),
            clip,
        });
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(packet) => {
                let started = Instant::now();
                let want = matches!(packet, StreamPacket::Frame { .. }) && started >= next_due;
                match decoder.push(&packet, want) {
                    Ok(Some(image)) => {
                        // Réglages relus à chaque image analysée (2 par seconde) : une zone ou un
                        // réglage général modifiés s'appliquent tout de suite.
                        let fresh_config = hub.detection().config();
                        if fresh_config != config {
                            tracker.set_config(tracker_config(&fresh_config));
                            config = fresh_config;
                        }
                        let interval = config.interval();
                        next_due = Instant::now() + interval;
                        let fresh = hub.camera_detection(id).map(|(_, s)| s);
                        if fresh != settings {
                            motion.reset_mask();
                            settings = fresh;
                        }
                        let Some(current) = &settings else { continue };
                        if load.paused(hub, id) {
                            continue;
                        }
                        let (_, moved) = motion.update(&image, current);
                        // Rien ne bouge : le détecteur ne tourne pas. Une boîte où rien n'a bougé
                        // (voiture garée vue parce qu'une branche bouge ailleurs) ne compte pas.
                        let found = if moved {
                            match lock(detector).detect(&image) {
                                Ok(found) => accept(&found, current)
                                    .into_iter()
                                    .filter(|d| motion.moved_in(&d.bbox))
                                    .collect(),
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
                            // 960 px de large, et jamais plus de 1024 px de côté long (une
                            // image en portrait) : la capture part telle quelle vers l'API de
                            // description (ticket n° 19).
                            let max_side = crate::descriptions::api::MAX_SIDE;
                            let config = SnapshotConfig {
                                max_width: 960.min(
                                    (u64::from(max_side) * u64::from(image.width)
                                        / u64::from(image.height.max(1)))
                                    .clamp(16, 960) as u32,
                                ),
                                quality: 80,
                                ..SnapshotConfig::default()
                            };
                            encode_jpeg(&image, &config)
                                .map(|b| b.to_vec())
                                .unwrap_or_default()
                        };
                        match tracker.observe(unix_now(), &found, snapshot) {
                            Step::Opened => open(&tracker),
                            Step::Closed(passage) => {
                                close(passage);
                                // L'image qui a clos le passage précédent en ouvre un autre.
                                if !found.is_empty()
                                    && tracker.observe(unix_now(), &found, snapshot) == Step::Opened
                                {
                                    open(&tracker);
                                }
                            }
                            Step::Extended => {
                                let new_classes = tracker.take_new_classes();
                                if let (false, Some(p)) =
                                    (new_classes.is_empty(), tracker.current())
                                {
                                    writer.send(WriteJob::Update {
                                        event: event_of(hub, id, p, true),
                                        jpeg: p
                                            .best
                                            .as_ref()
                                            .map(|(_, j)| j.clone())
                                            .unwrap_or_default(),
                                        new_classes,
                                    });
                                }
                            }
                            Step::Nothing => {}
                        }
                        load.record(hub, id, started.elapsed(), interval, config.pause());
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
            close(passage);
        }
    }
    // Arrêt de la veille (changement de mode, caméra modifiée) : le passage en cours est gardé.
    if let Some(passage) = tracker.flush() {
        close(passage);
    }
    hub.detection().set_paused(id, SLOW, None);
    hub.detection().set_paused(id, HOT, None);
}

/// Le clip en MP4 : images sans SPS ni PPS, depuis une image clé. L'`avcC` vient des SPS et PPS de
/// la première image clé quand ils diffèrent de la configuration annoncée (la caméra fait foi).
pub fn clip_to_mp4(clip: &Clip) -> Option<(Vec<u8>, f64)> {
    let mut in_band = None;
    let samples: Vec<mp4::Sample> = clip
        .frames
        .iter()
        .filter_map(|f| {
            let sample = nal::prepare_sample(&f.data).ok()?;
            let key = f.key && sample.idr;
            if key && in_band.is_none() {
                in_band = sample.parameters.clone();
            }
            Some(mp4::Sample {
                ts_ms: f.ts_ms,
                key,
                data: sample.data,
            })
        })
        .skip_while(|s| !s.key)
        .collect();
    let configured = nal::parse_avcc(&clip.avcc).ok();
    let avcc = match in_band {
        Some(ps) if configured.as_ref() != Some(&ps) => nal::build_avcc(&ps.sps, &ps.pps),
        _ => clip.avcc.clone(),
    };
    let bytes = mp4::mux(&avcc, clip.width, clip.height, &samples).ok()?;
    Some((bytes, clip.duration().as_secs_f64()))
}

#[cfg(test)]
mod tests;
