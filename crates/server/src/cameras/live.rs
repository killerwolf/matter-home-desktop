//! Lecture en direct d'une caméra, côté serveur : le **relais** (ticket n° 5).
//!
//! Le serveur se connecte à la caméra avec `retina` (RTSP, TCP entrelacé) **seulement tant que
//! quelqu'un regarde**, et relaie ses images H.264 telles quelles : il ne décode ni ne réencode
//! rien (un vieux Mac n'en a pas les moyens). Le client décode. Les clients ne voient jamais
//! l'adresse ni le mot de passe de la caméra.
//!
//! # Une session par caméra et par flux
//!
//! Une [`Session`] est **partagée** par tous les spectateurs de la même caméra et du même flux
//! (principal ou réduit) : une Aqara G100 échoue dès 3 canaux RTSP simultanés, la surveillance et
//! le bouton « Tester » restent donc à l'écart tant que la caméra est regardée.
//!
//! - Elle naît avec le premier spectateur ([`LiveRegistry::subscribe`]) et prend **un
//!   [`ViewerLease`](super::ViewerLease) pour toute sa vie**, délai de grâce compris : sans
//!   spectateur et passé ce délai ([`LiveConfig::grace`], 5 s), la connexion à la caméra est
//!   fermée **avant** que le témoin soit rendu. Un spectateur qui revient pendant la grâce garde la
//!   session (un clic de trop sur « Fermer » ne coupe pas la caméra).
//! - Elle se reconnecte toute seule (attente croissante) quand la caméra ou le réseau coupe ; les
//!   spectateurs reçoivent un [`StreamStatus`] (`retrying`) et gardent leur WebSocket.
//! - Une caméra **modifiée ou supprimée** pendant la lecture met fin à la session (vérifiée toutes
//!   les 2 s) : les spectateurs sont coupés ([`EndReason::Changed`]) et reviennent sur la caméra
//!   telle qu'elle est maintenant.
//! - Un codec autre que H.264 n'est jamais lu : [`StreamStatus::Unsupported`], définitif.
//!
//! # Clients lents
//!
//! Chaque spectateur a sa propre tâche et sa propre file (un `broadcast` borné) : un client qui
//! n'avance pas ne retient jamais les autres. Quand il est dépassé ([`FrameGate`]), il perd les
//! images jusqu'à la **prochaine image clé** (qui porte ses SPS/PPS) au lieu d'accumuler du
//! retard. Le délai d'écriture des WebSocket ordinaires (10 s, `client_write_timeout`) ne
//! s'applique pas ici : voir [`LiveConfig::write_stall`].
//!
//! # État de la caméra
//!
//! Tant qu'une session est saine, la caméra est « en ligne » pour tout le monde : à la première
//! image, la session l'enregistre (`Hub::record_camera_probe`), comme le ferait la surveillance,
//! qui saute la caméra tant qu'elle est regardée. Un échec de connexion est enregistré de la même
//! façon : deux d'affilée la déclarent injoignable. Seul le flux surveillé (le réduit, s'il existe)
//! décrit la caméra ; pour l'autre, seule une caméra injoignable est reportée.
//!
//! # Secrets
//!
//! Jamais de `Display` ni de `Debug` d'une erreur `retina` : son texte contient les adresses de la
//! connexion. Elle passe par `probe::classify`, qui donne un motif et une phrase sans adresse. Le
//! journal ne reçoit que le nom de la caméra et son identifiant en hexadécimal. Les journaux de
//! `retina` eux-mêmes sont coupés (`retina=off`, voir `main.rs`) pour la même raison.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use futures_util::StreamExt;
use tokio::sync::{broadcast, watch, Notify};

use protocol::wire::{
    encode_frame, CameraProblem, StreamInfo, StreamKind, StreamPacket, StreamStatus, CODEC_H264,
};

use crate::hub::Hub;

use super::probe::{classify, ProbeError, Target};
use super::ViewerLease;

/// Réglages d'une session (les tests les raccourcissent).
#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// Délai entre le départ du dernier spectateur et la fermeture de la connexion à la caméra.
    pub grace: Duration,
    /// Délai pour que la caméra réponde à `DESCRIBE`, `SETUP` et `PLAY`.
    pub connect_timeout: Duration,
    /// Sans image pendant ce délai, la liaison est jugée coupée et rouverte.
    pub silence_timeout: Duration,
    /// Première attente avant une nouvelle tentative ; double à chaque échec, jusqu'à `retry_max`.
    pub retry_first: Duration,
    pub retry_max: Duration,
    /// Intervalle de vérification que la caméra n'a pas été modifiée ni supprimée.
    pub target_check: Duration,
    /// Images gardées pour un spectateur qui lit plus lentement que la caméra ; au-delà, il perd
    /// des images jusqu'à la prochaine image clé.
    pub backlog_frames: usize,
    /// Taille maximale gardée de la dernière suite d'images (depuis la dernière image clé), pour
    /// qu'un nouveau spectateur n'attende pas la clé suivante.
    pub gop_cache_bytes: usize,
    /// Un envoi vers un client qui n'avance plus depuis ce délai le déconnecte. Plus long que le
    /// délai des WebSocket ordinaires : une image clé de plusieurs centaines de ko sur un Wi-Fi
    /// faible prend du temps, et un client lent est géré par la perte d'images, pas par la coupure.
    pub write_stall: Duration,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(10),
            silence_timeout: Duration::from_secs(10),
            retry_first: Duration::from_secs(1),
            retry_max: Duration::from_secs(15),
            target_check: Duration::from_secs(2),
            backlog_frames: 90,
            gop_cache_bytes: 2 * 1024 * 1024,
            write_stall: Duration::from_secs(30),
        }
    }
}

/// Pourquoi une session a pris fin pour ses spectateurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// La caméra a été modifiée ou supprimée.
    Changed,
    /// Le serveur ferme la session (plus personne, ou arrêt).
    Closed,
}

/// Ce qu'un spectateur reçoit, dans l'ordre.
#[derive(Debug)]
pub enum Event {
    Status(StreamStatus),
    /// Un message binaire du flux (`StreamPacket` déjà encodé).
    Video(Bytes),
    End(EndReason),
}

/// Pourquoi on ne peut pas regarder une caméra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeError {
    /// Pas de caméra avec cet identifiant.
    UnknownCamera,
    /// La caméra n'a pas ce flux (pas de flux réduit configuré).
    NoSuchStream,
}

#[derive(Clone)]
struct Chunk {
    kind: ChunkKind,
    bytes: Bytes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkKind {
    Config,
    Key,
    Delta,
}

/// Ce que garde la session pour les spectateurs qui arrivent en cours de route.
#[derive(Default)]
struct Cache {
    config: Option<Bytes>,
    /// Depuis la dernière image clé (comprise) ; vide tant qu'aucune clé n'est arrivée ou si la
    /// suite dépasse [`LiveConfig::gop_cache_bytes`].
    gop: Vec<Bytes>,
    gop_bytes: usize,
    /// La suite en cours a dépassé la limite : on ne la garde plus jusqu'à la clé suivante.
    gop_overflow: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Status(StreamStatus),
    Ended(EndReason),
}

/// La décision d'un spectateur sur chaque image : tant qu'il a perdu des images, il attend la
/// prochaine image clé. Pur, pour que le comportement d'un client lent se teste sans réseau.
#[derive(Debug)]
pub struct FrameGate {
    waiting_for_key: bool,
    /// Images perdues depuis le début (diagnostic, tests).
    pub dropped: u64,
}

impl FrameGate {
    /// `waiting_for_key` : le spectateur n'a pas de point de départ (pas de clé gardée).
    pub fn new(waiting_for_key: bool) -> Self {
        Self {
            waiting_for_key,
            dropped: 0,
        }
    }

    /// Le spectateur a été dépassé : `missed` images ont disparu de sa file.
    pub fn lagged(&mut self, missed: u64) {
        self.waiting_for_key = true;
        self.dropped += missed;
    }

    /// `true` si l'image doit lui être envoyée.
    fn accept(&mut self, kind: ChunkKind) -> bool {
        match kind {
            // La configuration ne dépend d'aucune image.
            ChunkKind::Config => true,
            ChunkKind::Key => {
                self.waiting_for_key = false;
                true
            }
            ChunkKind::Delta => {
                if self.waiting_for_key {
                    self.dropped += 1;
                    false
                } else {
                    true
                }
            }
        }
    }
}

/// Une session : la connexion à une caméra, partagée par ses spectateurs.
pub struct Session {
    id: u64,
    kind: StreamKind,
    name: String,
    config: LiveConfig,
    tx: broadcast::Sender<Chunk>,
    cache: Mutex<Cache>,
    phase: watch::Sender<Phase>,
    viewers: AtomicUsize,
    /// Réveille la surveillance du départ des spectateurs.
    activity: Notify,
    started: Instant,
}

impl Session {
    fn new(id: u64, kind: StreamKind, name: String, config: LiveConfig) -> Arc<Session> {
        let (tx, _) = broadcast::channel(config.backlog_frames.max(2));
        let (phase, _) = watch::channel(Phase::Status(StreamStatus::Connecting));
        Arc::new(Session {
            id,
            kind,
            name,
            config,
            tx,
            cache: Mutex::new(Cache::default()),
            phase,
            viewers: AtomicUsize::new(0),
            activity: Notify::new(),
            started: Instant::now(),
        })
    }

    /// Nombre de spectateurs de cette session.
    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::SeqCst)
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_status(&self, status: StreamStatus) {
        self.phase.send_replace(Phase::Status(status));
    }

    fn end(&self, reason: EndReason) {
        self.phase.send_replace(Phase::Ended(reason));
    }

    fn ended(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Ended(_))
    }

    /// Un spectateur de plus. À appeler sous le verrou du registre (voir `try_close`).
    fn join(self: &Arc<Self>) -> Subscription {
        self.viewers.fetch_add(1, Ordering::SeqCst);
        self.activity.notify_one();
        // Le verrou du cache sert aussi à `push_frame` : l'abonnement et la lecture de la suite
        // gardée sont atomiques (ni image perdue, ni image en double).
        let cache = self.cache();
        let rx = self.tx.subscribe();
        let mut backlog = VecDeque::new();
        if let Some(config) = &cache.config {
            backlog.push_back(config.clone());
        }
        backlog.extend(cache.gop.iter().cloned());
        let waiting_for_key = cache.gop.is_empty();
        drop(cache);
        Subscription {
            session: self.clone(),
            rx,
            phase: self.phase.subscribe(),
            announced: false,
            backlog,
            gate: FrameGate::new(waiting_for_key),
        }
    }

    // -----------------------------------------------------------------------
    // Ce que publie la connexion à la caméra
    // -----------------------------------------------------------------------

    fn publish_config(&self, packet: &StreamPacket) {
        let bytes = Bytes::from(packet.encode());
        let mut cache = self.cache();
        if cache.config.as_ref() == Some(&bytes) {
            return;
        }
        cache.config = Some(bytes.clone());
        let _ = self.tx.send(Chunk {
            kind: ChunkKind::Config,
            bytes,
        });
    }

    /// Une image (données AVCC). La première image clé d'une suite repart de zéro la suite gardée.
    fn push_frame(&self, key: bool, timestamp_ms: u32, data: &[u8]) {
        let bytes = Bytes::from(encode_frame(key, timestamp_ms, data));
        let mut cache = self.cache();
        if key {
            cache.gop.clear();
            cache.gop_bytes = 0;
            cache.gop_overflow = false;
        }
        // Sans clé d'abord (le flux démarre en cours de suite), rien n'est gardé : un nouveau
        // spectateur attendra la prochaine clé.
        let started = key || !cache.gop.is_empty();
        if started && !cache.gop_overflow {
            if cache.gop_bytes + bytes.len() > self.config.gop_cache_bytes {
                cache.gop.clear();
                cache.gop_bytes = 0;
                cache.gop_overflow = true;
            } else {
                cache.gop_bytes += bytes.len();
                cache.gop.push(bytes.clone());
            }
        }
        let _ = self.tx.send(Chunk {
            kind: if key {
                ChunkKind::Key
            } else {
                ChunkKind::Delta
            },
            bytes,
        });
    }

    /// Oublie la suite gardée (pas la configuration) : une nouvelle connexion à la caméra repart de
    /// sa première image clé, un spectateur qui arrive n'en reçoit pas de l'ancienne.
    fn clear_gop(&self) {
        let mut cache = self.cache();
        cache.gop.clear();
        cache.gop_bytes = 0;
        cache.gop_overflow = false;
    }

    fn clear_cache(&self) {
        *self.cache() = Cache::default();
    }
}

/// Un spectateur d'une session. Le lâcher le retire du compte ; sans spectateur, la session se
/// ferme après la grâce.
pub struct Subscription {
    session: Arc<Session>,
    rx: broadcast::Receiver<Chunk>,
    phase: watch::Receiver<Phase>,
    announced: bool,
    /// Configuration puis suite d'images gardées, à envoyer avant le direct.
    backlog: VecDeque<Bytes>,
    gate: FrameGate,
}

impl Subscription {
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Images perdues pour ce spectateur (il était trop lent).
    pub fn dropped(&self) -> u64 {
        self.gate.dropped
    }

    /// L'événement suivant. Ne rend jamais la main tant qu'il n'y a rien de neuf.
    pub async fn next(&mut self) -> Event {
        if !self.announced {
            self.announced = true;
            match self.phase.borrow_and_update().clone() {
                Phase::Ended(reason) => return Event::End(reason),
                Phase::Status(status) => return Event::Status(status),
            }
        }
        loop {
            if let Some(bytes) = self.backlog.pop_front() {
                return Event::Video(bytes);
            }
            tokio::select! {
                biased;
                changed = self.phase.changed() => {
                    if changed.is_err() {
                        return Event::End(EndReason::Closed);
                    }
                    match self.phase.borrow_and_update().clone() {
                        Phase::Ended(reason) => return Event::End(reason),
                        Phase::Status(status) => return Event::Status(status),
                    }
                }
                received = self.rx.recv() => match received {
                    Ok(chunk) => {
                        if self.gate.accept(chunk.kind) {
                            return Event::Video(chunk.bytes);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => self.recover(missed),
                    Err(broadcast::error::RecvError::Closed) => {
                        return Event::End(EndReason::Closed);
                    }
                },
            }
        }
    }

    /// Dépassé : on repart de l'instant présent (pas des vieilles images qui restent dans la
    /// file : ce serait du retard), en attendant la prochaine image clé. La configuration, qui
    /// a pu être perdue avec elles, est renvoyée.
    fn recover(&mut self, missed: u64) {
        self.gate.lagged(missed);
        let cache = self.session.cache();
        self.rx = self.session.tx.subscribe();
        self.backlog.clear();
        if let Some(config) = &cache.config {
            self.backlog.push_back(config.clone());
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.session.viewers.fetch_sub(1, Ordering::SeqCst);
        self.session.activity.notify_one();
    }
}

// ---------------------------------------------------------------------------
// Le registre
// ---------------------------------------------------------------------------

/// Les sessions en cours, une par caméra et par flux.
pub struct LiveRegistry {
    config: LiveConfig,
    sessions: Mutex<HashMap<(u64, StreamKind), Arc<Session>>>,
}

impl LiveRegistry {
    pub fn new(config: LiveConfig) -> Arc<LiveRegistry> {
        Arc::new(LiveRegistry {
            config,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &LiveConfig {
        &self.config
    }

    /// Nombre de sessions ouvertes (donc de caméras avec une connexion, ou en train d'en
    /// rouvrir une).
    pub fn sessions(&self) -> usize {
        self.map().len()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<(u64, StreamKind), Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Un spectateur de plus pour ce flux de cette caméra : rejoint la session en cours, ou en
    /// ouvre une (la connexion à la caméra se fait dans une tâche, pas ici). À appeler depuis un
    /// runtime tokio.
    pub fn subscribe(
        self: &Arc<Self>,
        hub: &Arc<Hub>,
        id: u64,
        kind: StreamKind,
    ) -> Result<Subscription, SubscribeError> {
        let mut map = self.map();
        if let Some(session) = map.get(&(id, kind)) {
            if !session.ended() {
                return Ok(session.join());
            }
            map.remove(&(id, kind));
        }
        // Le témoin dit aussi si la caméra existe.
        let lease = hub
            .acquire_camera_viewer(id)
            .ok_or(SubscribeError::UnknownCamera)?;
        let target = hub
            .camera_target(id, kind)
            .ok_or(SubscribeError::NoSuchStream)?;
        let name = hub
            .cameras()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.name)
            .unwrap_or_default();
        let session = Session::new(id, kind, name, self.config.clone());
        let subscription = session.join();
        map.insert((id, kind), session.clone());
        drop(map);
        log::info!(
            "lecture de « {} » ({id:#x}, {}) : ouverte",
            session.name,
            kind.key()
        );
        tokio::spawn(run_session(
            session,
            Arc::downgrade(self),
            hub.clone(),
            target,
            lease,
        ));
        Ok(subscription)
    }

    /// Retire la session du registre si plus personne ne la regarde. `true` : elle est fermée (un
    /// nouveau spectateur en ouvrira une autre). Pris sous le même verrou que `subscribe` : un
    /// spectateur ne rejoint jamais une session qui se ferme.
    fn try_close(&self, session: &Arc<Session>) -> bool {
        let mut map = self.map();
        if session.viewers() > 0 {
            return false;
        }
        session.end(EndReason::Closed);
        if map
            .get(&(session.id, session.kind))
            .is_some_and(|s| Arc::ptr_eq(s, session))
        {
            map.remove(&(session.id, session.kind));
        }
        true
    }

    /// Retire la session et la termine pour ses spectateurs (caméra modifiée ou supprimée).
    fn remove(&self, session: &Arc<Session>, reason: EndReason) {
        let mut map = self.map();
        session.end(reason);
        if map
            .get(&(session.id, session.kind))
            .is_some_and(|s| Arc::ptr_eq(s, session))
        {
            map.remove(&(session.id, session.kind));
        }
    }
}

// ---------------------------------------------------------------------------
// La tâche d'une session
// ---------------------------------------------------------------------------

/// Comment la connexion s'est terminée.
enum Outcome {
    /// La caméra a changé ou disparu.
    Changed,
    /// Codec non relayé : définitif.
    Unsupported,
}

async fn run_session(
    session: Arc<Session>,
    registry: Weak<LiveRegistry>,
    hub: Arc<Hub>,
    target: Target,
    lease: ViewerLease,
) {
    let name = session.name.clone();
    let (id, kind) = (session.id, session.kind);
    let outcome = {
        let connection = connect_loop(&session, &hub, &target);
        let idle = idle_watch(&session, &registry);
        tokio::pin!(connection, idle);
        tokio::select! {
            outcome = &mut connection => Some(outcome),
            () = &mut idle => None,
        }
    };
    match outcome {
        // Plus personne : la connexion à la caméra a déjà été lâchée (fin du `select!`).
        None => {}
        Some(Outcome::Changed) => {
            if let Some(registry) = registry.upgrade() {
                registry.remove(&session, EndReason::Changed);
            }
            log::info!(
                "lecture de « {name} » ({id:#x}, {}) : caméra modifiée ou supprimée",
                kind.key()
            );
        }
        // On garde la session le temps de la grâce : un client qui réessaie aussitôt apprend la
        // même chose sans rouvrir de connexion à la caméra.
        Some(Outcome::Unsupported) => {
            idle_watch(&session, &registry).await;
        }
    }
    session.clear_cache();
    // Le témoin est rendu en dernier : la connexion à la caméra est déjà fermée.
    drop(lease);
    log::info!("lecture de « {name} » ({id:#x}, {}) : fermée", kind.key());
}

/// Attend que plus personne ne regarde, puis ferme la session après la grâce.
async fn idle_watch(session: &Arc<Session>, registry: &Weak<LiveRegistry>) {
    loop {
        if session.viewers() == 0 {
            tokio::select! {
                () = tokio::time::sleep(session.config.grace) => {
                    match registry.upgrade() {
                        Some(registry) => {
                            if registry.try_close(session) {
                                return;
                            }
                        }
                        None => return,
                    }
                }
                () = session.activity.notified() => {}
            }
        } else {
            session.activity.notified().await;
        }
    }
}

/// Ouvre la connexion, relaie, et recommence quand elle coupe. Ne rend la main que si la caméra
/// a changé ou si son codec n'est pas relayé.
async fn connect_loop(session: &Arc<Session>, hub: &Arc<Hub>, target: &Target) -> Outcome {
    let mut wait = session.config.retry_first;
    loop {
        session.set_status(StreamStatus::Connecting);
        let started = Instant::now();
        let failure = match relay(session, hub, target).await {
            Relayed::Changed => return Outcome::Changed,
            Relayed::Unsupported(codec) => {
                session.set_status(StreamStatus::Unsupported { codec });
                return Outcome::Unsupported;
            }
            Relayed::Lost(error) => error,
        };
        log::warn!(
            "lecture de « {} » ({:#x}, {}) : {}",
            session.name,
            session.id,
            session.kind.key(),
            failure.message
        );
        report_failure(session, hub, target, &failure);
        session.set_status(StreamStatus::Retrying {
            problem: failure.problem,
        });
        // Une liaison qui a tenu un moment repart d'une courte attente.
        if started.elapsed() > Duration::from_secs(30) {
            wait = session.config.retry_first;
        }
        let pause = match failure.problem {
            // Ne pas marteler une caméra qui refuse nos identifiants (verrouillage, journaux).
            CameraProblem::Unauthorized => wait.max(Duration::from_secs(10)),
            _ => wait,
        };
        if !wait_for_change(session, hub, target, pause).await {
            return Outcome::Changed;
        }
        wait = (wait * 2).min(session.config.retry_max);
    }
}

/// Attend `pause` en surveillant la caméra. `false` : elle a changé.
async fn wait_for_change(session: &Session, hub: &Hub, target: &Target, pause: Duration) -> bool {
    let end = tokio::time::Instant::now() + pause;
    loop {
        let step = session
            .config
            .target_check
            .min(end.saturating_duration_since(tokio::time::Instant::now()));
        if step.is_zero() {
            return still_the_same(session, hub, target);
        }
        tokio::time::sleep(step).await;
        if !still_the_same(session, hub, target) {
            return false;
        }
    }
}

fn still_the_same(session: &Session, hub: &Hub, target: &Target) -> bool {
    hub.camera_target(session.id, session.kind).as_ref() == Some(target)
}

/// Un état « injoignable » ne vient que du flux surveillé, ou d'un échec de réseau (qui vaut pour
/// toute la caméra).
fn report_failure(session: &Session, hub: &Hub, target: &Target, failure: &ProbeError) {
    let preferred = hub.camera_probe_target(session.id).map(|(kind, _)| kind);
    if preferred == Some(session.kind) || failure.problem == CameraProblem::Unreachable {
        hub.record_camera_probe(session.id, session.kind, target, Err(failure.clone()));
    }
}

fn report_success(session: &Session, hub: &Hub, target: &Target, info: StreamInfo) {
    let preferred = hub.camera_probe_target(session.id).map(|(kind, _)| kind);
    let online = hub
        .cameras()
        .iter()
        .any(|c| c.id == session.id && c.status == protocol::wire::CameraStatus::Online);
    if preferred == Some(session.kind) || !online {
        hub.record_camera_probe(session.id, session.kind, target, Ok(info));
    }
}

enum Relayed {
    Changed,
    Unsupported(String),
    Lost(ProbeError),
}

fn lost(problem: CameraProblem, message: &str) -> Relayed {
    Relayed::Lost(ProbeError {
        problem,
        message: message.to_string(),
    })
}

/// Une connexion à la caméra, du `DESCRIBE` à la première erreur.
async fn relay(session: &Arc<Session>, hub: &Arc<Hub>, target: &Target) -> Relayed {
    use retina::client::{PlayOptions, SessionOptions, SetupOptions};
    use retina::codec::h26x::Framing;
    use retina::codec::{CodecItem, FrameFormat, ParameterSetInsertion, ParametersRef};

    session.clear_gop();
    let timeout = session.config.connect_timeout;
    let mut options = SessionOptions::default()
        .user_agent(format!("MatterHomeServer/{}", env!("CARGO_PKG_VERSION")));
    if let Some(c) = &target.credentials {
        options = options.creds(Some(retina::client::Credentials {
            username: c.username.clone(),
            password: c.password.clone(),
        }));
    }

    let connecting = async {
        let mut described = retina::client::Session::describe(target.url.clone(), options).await?;
        let Some(index) = described
            .streams()
            .iter()
            .position(|s| s.media() == "video")
        else {
            return Ok(Err(None));
        };
        let codec = described.streams()[index]
            .encoding_name()
            .to_ascii_lowercase();
        if codec != "h264" {
            return Ok(Err(Some(codec)));
        }
        // Les NAL sont précédées de leur longueur (format AVCC, celui de VideoToolbox), et chaque
        // image clé porte ses SPS et PPS : un client qui a perdu des images reprend à la clé
        // suivante sans autre information.
        described
            .setup(
                index,
                SetupOptions::default().frame_format(FrameFormat {
                    h26x_framing: Framing::FourByteLength,
                    parameter_set_insertion: ParameterSetInsertion::EachKeyFrame,
                    ..FrameFormat::default()
                }),
            )
            .await?;
        let playing = described.play(PlayOptions::default()).await?;
        Ok::<_, retina::Error>(Ok((index, playing.demuxed()?)))
    };
    let (index, mut stream) = match tokio::time::timeout(timeout, connecting).await {
        Err(_) => {
            return lost(
                CameraProblem::Unreachable,
                "La caméra n'a pas répondu à temps.",
            )
        }
        Ok(Err(e)) => return Relayed::Lost(classify(&e)),
        Ok(Ok(Err(None))) => {
            return lost(
                CameraProblem::Protocol,
                "Aucun flux vidéo dans la réponse de la caméra.",
            )
        }
        Ok(Ok(Err(Some(codec)))) => return Relayed::Unsupported(codec),
        Ok(Ok(Ok(opened))) => opened,
    };

    let mut check = tokio::time::interval(session.config.target_check);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    check.tick().await;
    let mut announced = false;
    loop {
        tokio::select! {
            item = tokio::time::timeout(session.config.silence_timeout, stream.next()) => {
                let item = match item {
                    Err(_) => return lost(CameraProblem::Unreachable, "La caméra n'envoie plus d'images."),
                    Ok(None) => return lost(CameraProblem::Unreachable, "La connexion avec la caméra s'est interrompue."),
                    Ok(Some(Err(e))) => return Relayed::Lost(classify(&e)),
                    Ok(Some(Ok(item))) => item,
                };
                let CodecItem::VideoFrame(frame) = item else { continue };
                if frame.stream_id() != index {
                    continue;
                }
                if frame.has_new_parameters() || !announced {
                    if let Some(ParametersRef::Video(v)) = stream.streams()[index].parameters() {
                        let (width, height) = v.pixel_dimensions();
                        let (n, d) = v.pixel_aspect_ratio().unwrap_or((1, 1));
                        let clamp = |x: u32| u16::try_from(x).unwrap_or(u16::MAX);
                        session.publish_config(&StreamPacket::Config {
                            codec: CODEC_H264,
                            width: clamp(width),
                            height: clamp(height),
                            pixel_aspect: (clamp(n.max(1)), clamp(d.max(1))),
                            avcc: v.extra_data().to_vec(),
                        });
                        if !announced {
                            report_success(session, hub, target, StreamInfo {
                                codec: "h264".into(),
                                width: Some(width),
                                height: Some(height),
                            });
                        }
                    } else if !announced {
                        // Pas de paramètres : le décodeur ne saurait rien faire de l'image.
                        continue;
                    }
                }
                if !announced {
                    announced = true;
                    session.set_status(StreamStatus::Live);
                }
                let ms = u32::try_from(session.started.elapsed().as_millis() & 0xFFFF_FFFF)
                    .unwrap_or(0);
                session.push_frame(frame.is_random_access_point(), ms, frame.data());
            }
            _ = check.tick() => {
                if !still_the_same(session, hub, target) {
                    return Relayed::Changed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
