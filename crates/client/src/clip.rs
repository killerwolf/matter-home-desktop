//! Lecture d'un clip d'événement dans l'application (ticket n° 22).
//!
//! Le clip (H.264 dans un MP4 du serveur) est téléchargé, lu par le démultiplexeur de
//! `matter_home_video::mp4`, puis décodé par le décodeur des flux en direct (VideoToolbox) **dans un
//! fil à part** : l'interface ne fait que tenir l'horloge de lecture et montrer l'image due.
//!
//! - **Mémoire bornée** : le fichier (compressé) reste en mémoire, mais les images décodées (RGB) ne
//!   sont jamais plus de [`QUEUE_CAP`] à la fois : le fil de décodage attend que l'interface en ait
//!   consommé.
//! - **Cadence** : l'horloge ([`Clock`]) avance en temps réel ; chaque image est montrée quand son
//!   instant dans le MP4 est atteint ([`due_count`]), les images en retard sont sautées.
//! - **Déplacement** : le fil repart de l'image clé qui précède l'instant demandé, décode vers
//!   l'avant sans convertir les images intermédiaires, et dépose les images à partir de l'instant
//!   voulu ([`plan_seek`]).
//! - **Erreurs** : fichier corrompu, décodeur absent ou en échec : un message en français
//!   ([`ClipPlayer::error`]), jamais de panique.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use matter_home_video::mp4::{self, Track};
use matter_home_video::{Decoder, FrameSink, Image};
use protocol::wire::{StreamPacket, CODEC_H264};

/// Images décodées gardées d'avance (en RGB : ~0,7 Mo chacune en 640×360, 6 Mo en 1080p).
pub const QUEUE_CAP: usize = 4;

// ---------------------------------------------------------------------------------------------
// Logique pure (testée sans décodeur ni fenêtre)
// ---------------------------------------------------------------------------------------------

/// L'horloge de lecture, en millisecondes. Le temps courant est toujours donné par l'appelant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clock {
    /// Position au moment où la lecture a (re)démarré, ou position figée si en pause.
    base_ms: u64,
    /// Instant (horloge de l'appelant) du démarrage, `None` en pause.
    started_ms: Option<u64>,
}

impl Clock {
    pub fn playing(&self) -> bool {
        self.started_ms.is_some()
    }

    /// La position, bornée à `duration_ms`.
    pub fn position(&self, now_ms: u64, duration_ms: u64) -> u64 {
        let moved = self.started_ms.map_or(0, |s| now_ms.saturating_sub(s));
        self.base_ms.saturating_add(moved).min(duration_ms)
    }

    pub fn play(&mut self, now_ms: u64, duration_ms: u64) {
        if self.started_ms.is_none() {
            self.base_ms = self.position(now_ms, duration_ms);
            self.started_ms = Some(now_ms);
        }
    }

    pub fn pause(&mut self, now_ms: u64, duration_ms: u64) {
        self.base_ms = self.position(now_ms, duration_ms);
        self.started_ms = None;
    }

    /// Saute à `ms` en gardant l'état (lecture ou pause).
    pub fn seek(&mut self, ms: u64, now_ms: u64) {
        self.base_ms = ms;
        if self.started_ms.is_some() {
            self.started_ms = Some(now_ms);
        }
    }
}

/// Combien d'images de la file sont dues à la position `pos_ms` (instants croissants) : la dernière
/// de celles-là est montrée, les précédentes sont sautées (lecture en retard).
pub fn due_count(timestamps: impl Iterator<Item = u64>, pos_ms: u64) -> usize {
    timestamps.take_while(|t| *t <= pos_ms).count()
}

/// D'où repartir pour atteindre un instant : décoder depuis `start` (image clé), ne déposer les
/// images qu'à partir de `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekPlan {
    pub start: usize,
    pub target: usize,
}

pub fn plan_seek(track: &Track, ms: u64) -> SeekPlan {
    let target = track.index_at(ms);
    SeekPlan {
        start: track.keyframe_at_or_before(target),
        target,
    }
}

/// « 0:07 » ou « 1:05 » (arrondi à la seconde inférieure).
pub fn format_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

// ---------------------------------------------------------------------------------------------
// Fil de décodage
// ---------------------------------------------------------------------------------------------

struct Frame {
    ts_ms: u64,
    image: Image,
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<Frame>,
    /// Déplacement demandé (instant), pris par le fil de décodage.
    seek: Option<u64>,
    stop: bool,
    /// Toutes les images du clip ont été décodées (depuis le dernier déplacement).
    finished: bool,
    error: Option<String>,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn decode_loop(
    shared: &Arc<Shared>,
    file: &[u8],
    track: &Track,
    notify: &(dyn Fn() + Send + Sync),
    sink: &Arc<FrameSink>,
    mut decoder: Decoder,
) {
    let config = StreamPacket::Config {
        codec: CODEC_H264,
        width: u16::try_from(track.width).unwrap_or(u16::MAX),
        height: u16::try_from(track.height).unwrap_or(u16::MAX),
        pixel_aspect: (1, 1),
        avcc: track.avcc.clone(),
    };
    let mut next = 0usize;
    let mut first_shown = 0usize;
    let mut produced = false;
    let mut announce_config = true;
    loop {
        // Attente d'un travail : un déplacement, ou de la place dans la file.
        {
            let mut inner = shared.lock();
            loop {
                if inner.stop {
                    return;
                }
                if let Some(ms) = inner.seek.take() {
                    inner.queue.clear();
                    inner.finished = false;
                    let plan = plan_seek(track, ms);
                    next = plan.start;
                    first_shown = plan.target;
                    // Une nouvelle suite repart d'une image clé, comme au premier lancement.
                    decoder.reset();
                    announce_config = true;
                    continue;
                }
                // Le lecteur jeté (plus personne ne tient `shared` que ce fil) : on s'arrête.
                if Arc::strong_count(shared) == 1 {
                    return;
                }
                if inner.finished || inner.queue.len() >= QUEUE_CAP {
                    inner = shared
                        .wake
                        .wait_timeout(inner, std::time::Duration::from_millis(250))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                    continue;
                }
                break;
            }
        }
        if announce_config {
            if let Err(e) = decoder.push(&config) {
                fail(shared, notify, e.to_string());
                return;
            }
            announce_config = false;
        }
        let sample = track.samples[next];
        let Some(data) = track.sample_data(file, next) else {
            fail(shared, notify, "le clip est tronqué".into());
            return;
        };
        sink.set_wanted(next >= first_shown);
        let packet = StreamPacket::Frame {
            key: sample.key,
            timestamp_ms: u32::try_from(sample.ts_ms).unwrap_or(u32::MAX),
            data: data.to_vec(),
        };
        if let Err(e) = decoder.push(&packet) {
            fail(shared, notify, e.to_string());
            return;
        }
        let image = sink.take();
        let mut inner = shared.lock();
        if inner.seek.is_none() {
            if let (Some(image), true) = (image, next >= first_shown) {
                produced = true;
                inner.queue.push_back(Frame {
                    ts_ms: sample.ts_ms,
                    image,
                });
            }
            next += 1;
            if next >= track.samples.len() {
                inner.finished = true;
                if !produced && inner.queue.is_empty() {
                    inner.error = Some("Le clip n'a pas pu être décodé.".into());
                }
            }
        }
        drop(inner);
        notify();
    }
}

fn fail(shared: &Shared, notify: &(dyn Fn() + Send + Sync), message: String) {
    shared.lock().error = Some(format!("Lecture impossible : {message}."));
    notify();
}

// ---------------------------------------------------------------------------------------------
// Lecteur (côté interface)
// ---------------------------------------------------------------------------------------------

enum Phase {
    /// Téléchargement et lecture du fichier.
    Loading,
    Ready {
        shared: Arc<Shared>,
        duration_ms: u64,
    },
    Failed(String),
}

/// Le lecteur d'un clip. Créé au clic sur « Lire le clip », jeté à la fermeture (le fil s'arrête).
pub struct ClipPlayer {
    pub id: String,
    phase: Arc<Mutex<Phase>>,
    epoch: Instant,
    clock: Clock,
    pub looping: bool,
    /// La lecture démarre dès que le clip est prêt.
    autoplay: bool,
}

impl ClipPlayer {
    /// Lance le téléchargement (fil réseau) puis le décodage (fil du décodeur). `notify` redessine
    /// l'interface.
    pub fn open(id: &str, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let phase = Arc::new(Mutex::new(Phase::Loading));
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let (p, n, clip_id) = (phase.clone(), notify.clone(), id.to_string());
        let spawned = std::thread::Builder::new()
            .name("lecteur-clip".into())
            .spawn(move || {
                let fetched = crate::net::events::fetch_clip(&clip_id);
                // Lecteur fermé pendant le téléchargement : le résultat est jeté.
                if Arc::strong_count(&p) == 1 {
                    return;
                }
                let outcome = fetched.and_then(|bytes| start(bytes, n.clone()));
                *p.lock().unwrap_or_else(PoisonError::into_inner) = match outcome {
                    Ok(ready) => ready,
                    Err(why) => Phase::Failed(why),
                };
                n();
            });
        if spawned.is_err() {
            *phase.lock().unwrap_or_else(PoisonError::into_inner) =
                Phase::Failed("Lecture impossible : fil d'exécution indisponible.".into());
        }
        Self::with_phase(id, phase)
    }

    fn with_phase(id: &str, phase: Arc<Mutex<Phase>>) -> Self {
        Self {
            id: id.to_string(),
            phase,
            epoch: Instant::now(),
            clock: Clock::default(),
            looping: false,
            autoplay: true,
        }
    }

    /// Lit un clip déjà en mémoire (sans réseau).
    pub fn from_bytes(id: &str, bytes: Vec<u8>, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let phase = match start(bytes, Arc::new(notify)) {
            Ok(ready) => ready,
            Err(why) => Phase::Failed(why),
        };
        Self::with_phase(id, Arc::new(Mutex::new(phase)))
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn phase(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Le message d'erreur à montrer, s'il y en a un.
    pub fn error(&self) -> Option<String> {
        match &*self.phase() {
            Phase::Failed(why) => Some(why.clone()),
            Phase::Ready { shared, .. } => shared.lock().error.clone(),
            Phase::Loading => None,
        }
    }

    pub fn loading(&self) -> bool {
        matches!(&*self.phase(), Phase::Loading)
    }

    pub fn duration_ms(&self) -> u64 {
        match &*self.phase() {
            Phase::Ready { duration_ms, .. } => *duration_ms,
            _ => 0,
        }
    }

    pub fn playing(&self) -> bool {
        self.clock.playing()
    }

    pub fn position_ms(&self) -> u64 {
        self.clock.position(self.now_ms(), self.duration_ms())
    }

    /// La lecture n'a pas encore commencé : elle démarre à la première image décodée.
    pub fn waiting_for_start(&self) -> bool {
        self.autoplay
    }

    pub fn toggle_play(&mut self) {
        self.autoplay = false;
        let (now, duration) = (self.now_ms(), self.duration_ms());
        if self.clock.playing() {
            self.clock.pause(now, duration);
        } else {
            if self.clock.position(now, duration) >= duration && duration > 0 {
                self.seek(0);
            }
            self.clock.play(now, duration);
        }
    }

    /// Déplace la lecture à `ms` (image clé précédente, puis décodage vers l'avant).
    pub fn seek(&mut self, ms: u64) {
        let duration = self.duration_ms();
        let ms = ms.min(duration);
        self.clock.seek(ms, self.now_ms());
        if let Phase::Ready { shared, .. } = &*self.phase() {
            let mut inner = shared.lock();
            inner.seek = Some(ms);
            // Le clip n'est plus « fini » : le fil de décodage repart (et `update` ne conclut pas
            // à la fin sur l'état d'avant le déplacement).
            inner.finished = false;
            inner.queue.clear();
            shared.wake.notify_all();
        }
    }

    /// À appeler à chaque image de l'interface : avance la lecture, boucle ou s'arrête à la fin, et
    /// rend l'image à montrer si une nouvelle est due (l'interface se redessine tant que ça joue).
    pub fn update(&mut self) -> Option<Arc<Image>> {
        let (now, duration) = (self.now_ms(), self.duration_ms());
        let mut shown = None;
        let mut first_frame = false;
        let ready = match &*self.phase() {
            Phase::Ready { shared, .. } => {
                let mut inner = shared.lock();
                let pos = self.clock.position(now, duration);
                let due = due_count(inner.queue.iter().map(|f| f.ts_ms), pos);
                if due > 0 {
                    let frame = inner.queue.drain(..due).next_back();
                    shared.wake.notify_all();
                    shown = frame;
                }
                // La lecture démarre à la première image décodée, pas au téléchargement.
                first_frame = shown.is_some() || !inner.queue.is_empty();
                true
            }
            _ => false,
        };
        if self.autoplay && first_frame {
            self.autoplay = false;
            self.clock.play(now, duration);
        }
        if ready && self.clock.playing() {
            // Fin : quand l'horloge atteint la durée, donc après la dernière image montrée pendant
            // sa durée ; la position reste alors à la durée du clip.
            let pos = self.clock.position(now, duration);
            if pos >= duration {
                if self.looping {
                    self.seek(0);
                } else {
                    self.clock.pause(now, duration);
                }
            }
        }
        shown.map(|f| Arc::new(f.image))
    }
}

impl Drop for ClipPlayer {
    fn drop(&mut self) {
        if let Phase::Ready { shared, .. } = &*self.phase() {
            shared.lock().stop = true;
            shared.wake.notify_all();
        }
    }
}

/// Lit le fichier et lance le fil de décodage.
fn start(bytes: Vec<u8>, notify: Arc<dyn Fn() + Send + Sync>) -> Result<Phase, String> {
    let track = mp4::parse(&bytes).map_err(|e| format!("Clip illisible : {e}."))?;
    let notify_sink = notify.clone();
    let sink = FrameSink::new(move || notify_sink());
    let decoder = Decoder::new(sink.clone()).map_err(|e| format!("Lecture impossible : {e}."))?;
    let duration_ms = track.duration_ms;
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner::default()),
        wake: Condvar::new(),
    });
    let worker = shared.clone();
    std::thread::Builder::new()
        .name("decodage-clip".into())
        .spawn(move || {
            // Les images déposées dans le puits réveillent l'interface : inutile ici, la file le fait.
            decode_loop(&worker, &bytes, &track, &*notify, &sink, decoder);
        })
        .map_err(|_| "Lecture impossible : fil d'exécution indisponible.".to_string())?;
    Ok(Phase::Ready {
        shared,
        duration_ms,
    })
}

#[cfg(test)]
mod tests;
