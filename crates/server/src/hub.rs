//! Le centre du serveur : il agrège les événements du backend en un état complet des
//! appareils (ce que l'interface faisait dans `apply_events`), tient le plan et les règles
//! (le serveur est le seul à écrire `maison.json` et `regles.json`), diffuse chaque
//! changement aux clients connectés et traduit leurs demandes en commandes.
//!
//! Aucune attente côté moteur : `apply` ne bloque jamais sur un client. La diffusion passe par
//! un `broadcast` borné ; un client en retard le découvre à la lecture (`Lagged`) et reçoit
//! alors l'état complet, ou est déconnecté s'il ne suit toujours pas.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

use protocol::automation::{CoverCommand, OnOffCommand, Rule, RuleSet};
use protocol::home::HomePlan;
use protocol::wire::{
    self, ApiError, CameraStatus, CameraView, CustomizeRequest, DeviceView, ErrorCode,
    HistoryPoint, HistoryQuery, HistoryResponse, IdentityInfo, ImportIdentity, ImportSummary,
    LatestReading, Metric, RuleRunView, ServerMessage, Snapshot, StreamInfo, TextIds, WireEvent,
    WireMode, WireNode, WireStatus,
};

use crate::backup;
use crate::cameras::{
    self, CameraAdmin, CameraBook, CameraDraft, CameraError, ProbeError, StreamKind, StreamTest,
    Target, Wake,
};
use crate::events::{
    now_unix, Command, CoverMotion, Event, NodeInfo, NodeStatus, ReportingMode, StateKind,
};
use crate::export;
use crate::history_store::{HistorySink, HistoryStore};
use crate::persistence;
use crate::scrub::Scrubber;
use crate::validate;

/// Pourquoi le serveur doit s'arrêter de lui-même.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Une sauvegarde a été importée : le prochain lancement l'applique.
    ImportStaged,
}

pub struct HubConfig {
    pub data_dir: PathBuf,
    /// Mode démo : plan et règles de démonstration, rien n'est écrit pour eux.
    pub demo: bool,
    /// Dossier de l'historique à relire (`history`, ou `history-demo` en démo).
    pub history_dir: PathBuf,
    /// Historique tenu par le hub lui-même (démo seulement ; le vrai backend écrit le sien).
    pub demo_history: Option<HistorySink>,
    /// Messages gardés pour un client lent avant qu'il ne soit resynchronisé.
    pub broadcast_capacity: usize,
    pub commands: mpsc::UnboundedSender<Command>,
}

/// Un appareil tel que le serveur le connaît.
struct Dev {
    info: NodeInfo,
    status: NodeStatus,
    last_seen: Option<f64>,
    mode: Option<ReportingMode>,
    /// Dernière valeur (valeur, instant) de chaque grandeur.
    latest: BTreeMap<Metric, (f64, f64)>,
    open_since: Option<f64>,
    cover_motion: Option<CoverMotion>,
}

impl Dev {
    fn new(info: NodeInfo) -> Self {
        Self {
            info,
            status: NodeStatus::Connecting,
            last_seen: None,
            mode: None,
            latest: BTreeMap::new(),
            open_since: None,
            cover_motion: None,
        }
    }

    fn view(&self, clean: &dyn Fn(&str) -> String) -> DeviceView {
        DeviceView {
            device: WireNode::from(&self.info),
            status: WireStatus::from_status(&self.status, clean),
            last_seen: self.last_seen,
            mode: self.mode.as_ref().map(WireMode::from),
            latest: self
                .latest
                .iter()
                .map(|(metric, (value, at))| LatestReading {
                    metric: *metric,
                    value: *value,
                    at: *at,
                })
                .collect(),
            open_since: self.open_since,
            cover_motion: self.cover_motion,
        }
    }

    /// Retient une valeur si elle n'est pas plus ancienne que la précédente (l'historique
    /// rejoué au démarrage arrive dans le désordre par rapport aux premières mesures).
    fn set_latest(&mut self, metric: Metric, value: f64, at: f64) -> bool {
        match self.latest.get(&metric) {
            Some((_, old_at)) if *old_at > at => false,
            _ => {
                self.latest.insert(metric, (value, at));
                true
            }
        }
    }
}

struct HubState {
    seq: u64,
    devices: BTreeMap<u64, Dev>,
    plan: HomePlan,
    rules: RuleSet,
    /// Les caméras et leur état (voir `cameras.rs`).
    cameras: CameraBook,
    runs: VecDeque<RuleRunView>,
    pairing_busy: bool,
}

/// Abonnement d'un client : l'état complet au moment de l'abonnement, puis les événements
/// suivants (de `seq` strictement supérieur).
pub struct Subscription {
    pub snapshot: Arc<str>,
    pub rx: broadcast::Receiver<Arc<str>>,
}

pub struct Hub {
    state: Mutex<HubState>,
    tx: broadcast::Sender<Arc<str>>,
    commands: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    /// Sérialise les modifications du plan, des règles et des caméras (mémoire + disque + diffusion).
    write_lock: Mutex<()>,
    /// Réveille le surveillant des caméras (voir `cameras::start_monitor`) ; absent en démo et
    /// avant son démarrage.
    camera_wake: Mutex<Option<mpsc::UnboundedSender<Wake>>>,
    scrubber: Scrubber,
    data_dir: PathBuf,
    history_dir: PathBuf,
    demo: bool,
    demo_history: Mutex<Option<HistorySink>>,
    exit: Mutex<Option<ExitReason>>,
    stopping: AtomicBool,
    /// Un import validé attend le redémarrage : un second est refusé.
    import_staged: AtomicBool,
}

type ApiResult<T> = Result<T, ApiError>;

fn internal(context: &str, error: &anyhow::Error) -> ApiError {
    // Le détail complet va au journal, jamais au client.
    log::error!("{context} : {error:#}");
    ApiError::new(ErrorCode::Internal)
}

impl Hub {
    pub fn new(config: HubConfig) -> Self {
        let (tx, _) = broadcast::channel(config.broadcast_capacity.max(2));
        let (plan, rules, cameras) = if config.demo {
            (
                crate::demo::demo_plan(),
                crate::demo::demo_rules(),
                CameraBook::demo(),
            )
        } else {
            (
                load_plan(&config.data_dir.join(PLAN_FILE)),
                load_document::<RuleSet>(&config.data_dir.join(RULES_FILE), "règles"),
                CameraBook::new(cameras::load(&config.data_dir)),
            )
        };
        Self {
            state: Mutex::new(HubState {
                seq: 0,
                devices: BTreeMap::new(),
                plan,
                rules,
                cameras,
                runs: VecDeque::new(),
                pairing_busy: false,
            }),
            tx,
            commands: Mutex::new(Some(config.commands)),
            write_lock: Mutex::new(()),
            camera_wake: Mutex::new(None),
            scrubber: Scrubber::new(&config.data_dir),
            data_dir: config.data_dir,
            history_dir: config.history_dir,
            demo: config.demo,
            demo_history: Mutex::new(config.demo_history),
            exit: Mutex::new(None),
            stopping: AtomicBool::new(false),
            import_staged: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn clean(&self, text: &str) -> String {
        self.scrubber.clean(text)
    }

    // -----------------------------------------------------------------------
    // Agrégation des événements du backend
    // -----------------------------------------------------------------------

    /// Applique un événement du backend à l'état et le diffuse. Ne bloque sur aucun client.
    pub fn apply(&self, event: &Event) {
        if let Some(history) = self
            .demo_history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            history.record(event);
        }
        let wire = WireEvent::from_event(event, &|s| self.scrubber.clean(s));
        {
            let mut st = self.lock();
            // Une mesure écartée (plus ancienne que la dernière retenue : historique rejoué au
            // démarrage) n'est pas diffusée non plus : le client ne verrait pas la valeur
            // reculer, alors que l'état complet, lui, ne la contient pas.
            let retained = self.aggregate(&mut st, event);
            if let (true, Some(wire)) = (retained, wire) {
                self.publish(&mut st, wire);
            }
        }
        if let Event::NodeRemoved(id) = event {
            // Le plan ne garde pas un appareil oublié (c'est l'interface qui le faisait).
            self.update_document(
                |st| st.plan.remove_device(*id).then_some(()),
                Document::Plan,
            );
        }
    }

    /// Retient l'événement dans l'état. `false` : il est écarté et ne doit pas être diffusé.
    fn aggregate(&self, st: &mut HubState, event: &Event) -> bool {
        match event {
            Event::Node(info) => match st.devices.get_mut(&info.node_id) {
                Some(d) => d.info = info.clone(),
                None => {
                    st.devices.insert(info.node_id, Dev::new(info.clone()));
                }
            },
            Event::NodeRemoved(id) => {
                st.devices.remove(id);
            }
            Event::Status { node_id, status } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    d.status = status.clone();
                }
            }
            Event::Seen { node_id, at } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    d.last_seen = Some(d.last_seen.map_or(*at, |s| s.max(*at)));
                }
            }
            Event::Mode { node_id, mode } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    d.mode = Some(mode.clone());
                }
            }
            Event::Measurement {
                node_id,
                kind,
                value,
                at,
            } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    return d.set_latest(Metric::from(*kind), *value, *at);
                }
            }
            Event::State {
                node_id,
                kind,
                value,
                at,
            } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    let metric = Metric::from(*kind);
                    let changed = d
                        .latest
                        .get(&metric)
                        .is_none_or(|(old, _)| (*old != 0.0) != *value);
                    let retained = d.set_latest(metric, f64::from(u8::from(*value)), *at);
                    if retained && *kind == StateKind::Open && changed {
                        d.open_since = Some(*at);
                    }
                    return retained;
                }
            }
            Event::CoverMotion { node_id, motion } => {
                if let Some(d) = st.devices.get_mut(node_id) {
                    d.cover_motion = Some(*motion);
                }
            }
            Event::RuleRun {
                rule_id,
                name,
                at,
                ok,
                detail,
            } => {
                st.runs.push_back(RuleRunView {
                    rule_id: *rule_id,
                    name: name.clone(),
                    at: *at,
                    ok: *ok,
                    detail: self.scrubber.clean(detail),
                });
                while st.runs.len() > wire::RULE_RUNS_KEPT {
                    st.runs.pop_front();
                }
            }
            Event::PairingDone { .. } | Event::PairingFailed(_) => st.pairing_busy = false,
            Event::PairingProgress(_)
            | Event::Notification { .. }
            | Event::Info(_)
            | Event::Fatal(_) => {}
        }
        true
    }

    /// Numérote et diffuse un événement. Sans abonné, on ne le sérialise même pas.
    fn publish(&self, st: &mut HubState, event: WireEvent) {
        st.seq += 1;
        if self.tx.receiver_count() == 0 {
            return;
        }
        let message = ServerMessage::Event { seq: st.seq, event };
        if let Some(json) = to_json(&message) {
            // `send` ne bloque jamais : au pire, les lecteurs trop lents perdent des messages.
            let _ = self.tx.send(json);
        }
    }

    // -----------------------------------------------------------------------
    // État complet et abonnements
    // -----------------------------------------------------------------------

    fn snapshot(&self, st: &HubState) -> Snapshot {
        let clean = |s: &str| self.scrubber.clean(s);
        Snapshot {
            seq: st.seq,
            server_time: now_unix(),
            devices: st.devices.values().map(|d| d.view(&clean)).collect(),
            plan: TextIds(st.plan.clone()),
            rules: TextIds(st.rules.clone()),
            rule_runs: st.runs.iter().cloned().collect(),
            pairing_busy: st.pairing_busy,
            cameras: st.cameras.views(),
        }
    }

    pub fn current_snapshot(&self) -> Snapshot {
        self.snapshot(&self.lock())
    }

    /// Abonnement atomique : l'état et le point de départ des événements sont pris sous le
    /// même verrou, aucun événement ne tombe entre les deux.
    pub fn subscribe(&self) -> Subscription {
        let st = self.lock();
        let rx = self.tx.subscribe();
        let snapshot = to_json(&ServerMessage::Snapshot(Box::new(self.snapshot(&st))))
            .unwrap_or_else(|| Arc::from("{}"));
        Subscription { snapshot, rx }
    }

    pub fn rule_runs(&self) -> Vec<RuleRunView> {
        self.lock().runs.iter().cloned().collect()
    }

    pub fn plan(&self) -> HomePlan {
        self.lock().plan.clone()
    }

    pub fn rules(&self) -> RuleSet {
        self.lock().rules.clone()
    }

    /// Les caméras vues par les clients : sans adresse ni mot de passe.
    pub fn cameras(&self) -> Vec<CameraView> {
        self.lock().cameras.views()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    // -----------------------------------------------------------------------
    // Commandes vers le backend
    // -----------------------------------------------------------------------

    fn send(&self, command: Command) -> ApiResult<()> {
        let guard = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(tx) => tx
                .send(command)
                .map_err(|_| ApiError::new(ErrorCode::Unavailable)),
            None => Err(ApiError::new(ErrorCode::Unavailable)),
        }
    }

    /// Ferme l'envoi de commandes : à l'arrêt, le backend ne doit plus rien recevoir et son
    /// canal doit pouvoir se fermer.
    pub fn close_commands(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // Le fil d'écriture de l'historique de démonstration s'arrête quand son émetteur tombe.
        self.demo_history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    fn device_info(&self, node_id: u64) -> ApiResult<NodeInfo> {
        self.lock()
            .devices
            .get(&node_id)
            .map(|d| d.info.clone())
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound))
    }

    pub fn pair(&self, code: &str) -> ApiResult<()> {
        let code = code.trim();
        if crate::matter::setup_code::parse(code).is_err() {
            return Err(ApiError::with_detail(
                ErrorCode::BadRequest,
                "code d'appairage invalide",
            ));
        }
        {
            let mut st = self.lock();
            if st.pairing_busy {
                return Err(ApiError::new(ErrorCode::PairingBusy));
            }
            st.pairing_busy = true;
        }
        let sent = self.send(Command::Pair(code.to_string()));
        if sent.is_err() {
            self.lock().pairing_busy = false;
        }
        sent
    }

    pub fn retry(&self, node_id: u64) -> ApiResult<()> {
        self.device_info(node_id)?;
        self.send(Command::Retry(node_id))
    }

    pub fn forget(&self, node_id: u64) -> ApiResult<()> {
        self.device_info(node_id)?;
        self.send(Command::Forget(node_id))
    }

    pub fn clear_history(&self, node_id: u64) -> ApiResult<()> {
        self.device_info(node_id)?;
        if self.demo {
            // En démo, l'historique est tenu ici (le faux backend n'enregistre rien).
            if let Some(history) = self
                .demo_history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                history.remove(node_id);
            }
            return Ok(());
        }
        self.send(Command::ClearHistory(node_id))
    }

    pub fn customize(&self, node_id: u64, request: CustomizeRequest) -> ApiResult<()> {
        self.device_info(node_id)?;
        if request
            .name
            .as_ref()
            .is_some_and(|n| n.chars().count() > 80)
        {
            return Err(ApiError::with_detail(
                ErrorCode::BadRequest,
                "nom trop long",
            ));
        }
        self.send(Command::Customize {
            node_id,
            name: request.name,
            icon: request.icon,
        })
    }

    pub fn set_on_off(&self, node_id: u64, command: OnOffCommand) -> ApiResult<()> {
        if !self.device_info(node_id)?.caps.on_off {
            return Err(ApiError::with_detail(
                ErrorCode::BadRequest,
                "cet appareil n'a pas de marche/arrêt",
            ));
        }
        self.send(Command::SetOnOff { node_id, command })
    }

    pub fn set_cover(&self, node_id: u64, command: CoverCommand) -> ApiResult<()> {
        let info = self.device_info(node_id)?;
        if !info.caps.cover {
            return Err(ApiError::with_detail(
                ErrorCode::BadRequest,
                "cet appareil n'est pas un volet",
            ));
        }
        if matches!(command, CoverCommand::Position(p) if p > 100) {
            return Err(ApiError::with_detail(
                ErrorCode::BadRequest,
                "position invalide (0 à 100)",
            ));
        }
        self.send(Command::SetCover { node_id, command })
    }

    pub fn test_rule(&self, rule_id: u64) -> ApiResult<()> {
        if !self.lock().rules.rules.iter().any(|r| r.id == rule_id) {
            return Err(ApiError::new(ErrorCode::NotFound));
        }
        self.send(Command::TestRule(rule_id))
    }

    // -----------------------------------------------------------------------
    // Plan et règles : la dernière modification gagne, tout le monde la reçoit
    // -----------------------------------------------------------------------

    pub fn set_plan(&self, plan: HomePlan) -> ApiResult<()> {
        validate::plan(&plan)?;
        self.replace_document(Document::Plan, move |st| st.plan = plan)
    }

    pub fn set_rules(&self, rules: RuleSet) -> ApiResult<()> {
        validate::rules(&rules)?;
        let list: Vec<Rule> = rules.rules.clone();
        self.replace_document(Document::Rules, move |st| st.rules = rules)?;
        self.send(Command::SetRules(list))
    }

    // -----------------------------------------------------------------------
    // Caméras : seule la fenêtre du serveur les modifie (aucune route d'écriture), et le serveur
    // est le seul à connaître leurs adresses et mots de passe. Voir `cameras.rs`.
    // -----------------------------------------------------------------------

    /// Les caméras pour la fenêtre du serveur (adresses et identifiant, jamais le mot de passe).
    pub fn camera_admin_list(&self) -> Vec<CameraAdmin> {
        self.lock().cameras.admin_list()
    }

    pub fn camera_ids(&self) -> Vec<u64> {
        self.lock().cameras.ids()
    }

    pub fn add_camera(&self, draft: &CameraDraft) -> Result<u64, CameraError> {
        let id = self.change_cameras(|book| book.add(draft))?;
        self.wake_cameras(Wake::Camera(id));
        log::info!("caméra ajoutée : {id:#x}");
        Ok(id)
    }

    pub fn update_camera(&self, id: u64, draft: &CameraDraft) -> Result<(), CameraError> {
        self.change_cameras(|book| book.update(id, draft))?;
        self.wake_cameras(Wake::Camera(id));
        log::info!("caméra modifiée : {id:#x}");
        Ok(())
    }

    /// Supprime une caméra. #6 : c'est ici qu'il faudra aussi la retirer du plan
    /// (`update_document` avec `Document::Plan`, comme pour un appareil oublié).
    pub fn delete_camera(&self, id: u64) -> Result<(), CameraError> {
        self.change_cameras(|book| book.remove(id))?;
        log::info!("caméra supprimée : {id:#x}");
        Ok(())
    }

    /// Adresse et identifiants d'un flux, pour le serveur seulement (lecture, #5). Ne sort
    /// jamais vers un client.
    pub fn camera_target(&self, id: u64, kind: StreamKind) -> Option<Target> {
        self.lock().cameras.get(id)?.target(kind)
    }

    /// Le flux à vérifier d'une caméra : le réduit s'il existe, sinon le principal.
    pub(crate) fn camera_probe_target(&self, id: u64) -> Option<(StreamKind, Target)> {
        let st = self.lock();
        let camera = st.cameras.get(id)?;
        let kind = camera.preferred_kind();
        Some((kind, camera.target(kind)?))
    }

    /// Enregistre le résultat d'une vérification, et le diffuse s'il change quelque chose. Un
    /// résultat périmé (caméra supprimée ou modifiée pendant la vérification) est écarté.
    pub(crate) fn record_camera_probe(
        &self,
        id: u64,
        kind: StreamKind,
        probed: &Target,
        outcome: Result<StreamInfo, ProbeError>,
    ) {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let Some(camera) = st.cameras.get(id) else {
            return;
        };
        if camera.target(kind).as_ref() != Some(probed) {
            return;
        }
        let name = camera.name.clone();
        let was_online = st
            .cameras
            .views()
            .iter()
            .find(|v| v.id == id)
            .is_some_and(|v| v.status == CameraStatus::Online);
        let (status, stream) = match outcome {
            Ok(info) => (CameraStatus::Online, Some(info)),
            Err(e) => (
                CameraStatus::Unreachable {
                    problem: e.problem,
                    detail: Some(e.message),
                },
                None,
            ),
        };
        match (&status, was_online) {
            (CameraStatus::Online, false) => log::info!("caméra « {name} » joignable"),
            (CameraStatus::Unreachable { detail, .. }, true) => {
                log::warn!(
                    "caméra « {name} » injoignable : {}",
                    detail.as_deref().unwrap_or("")
                )
            }
            _ => {}
        }
        if st.cameras.set_health(id, status, stream) {
            self.publish_cameras(&mut st);
        }
    }

    /// Teste une saisie du formulaire (flux principal puis flux réduit), sans rien enregistrer.
    /// `editing` : la caméra modifiée, dont le mot de passe enregistré sert si le champ est
    /// laissé vide. **Bloque** jusqu'au résultat (8 s par flux au plus) : à lancer hors du fil
    /// de l'interface. En démo, aucun trafic : la réponse est fabriquée.
    pub fn test_camera_draft(
        &self,
        draft: &CameraDraft,
        editing: Option<u64>,
    ) -> Result<Vec<StreamTest>, CameraError> {
        let camera = {
            let st = self.lock();
            let existing = editing.and_then(|id| st.cameras.get(id));
            draft.build(editing.unwrap_or(protocol::wire::CAMERA_ID_MIN), existing)?
        };
        if self.demo {
            return Ok([StreamKind::Main, StreamKind::Sub]
                .into_iter()
                .filter(|kind| camera.target(*kind).is_some())
                .map(|kind| StreamTest {
                    kind,
                    outcome: Ok(StreamInfo {
                        codec: "h264".into(),
                        width: Some(if kind == StreamKind::Main { 2304 } else { 1280 }),
                        height: Some(if kind == StreamKind::Main { 1296 } else { 720 }),
                    }),
                })
                .collect());
        }
        Ok(cameras::test_camera(&camera))
    }

    pub(crate) fn set_camera_wake(&self, wake: Option<mpsc::UnboundedSender<Wake>>) {
        *self.camera_wake.lock().unwrap_or_else(|e| e.into_inner()) = wake;
    }

    fn wake_cameras(&self, wake: Wake) {
        if let Some(tx) = &*self.camera_wake.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = tx.send(wake);
        }
    }

    /// Modifie les caméras, les enregistre (0600, atomique) et diffuse la liste. Si
    /// l'enregistrement échoue, la modification est annulée : la fenêtre du serveur ne doit pas
    /// montrer une caméra qui n'existera plus au prochain lancement.
    fn change_cameras<T>(
        &self,
        change: impl FnOnce(&mut CameraBook) -> Result<T, CameraError>,
    ) -> Result<T, CameraError> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        let backup = st.cameras.clone();
        let out = change(&mut st.cameras)?;
        if !self.demo {
            if let Err(e) = cameras::save(&self.data_dir, st.cameras.cameras()) {
                log::error!("enregistrement des caméras : {e:#}");
                st.cameras = backup;
                return Err(CameraError(
                    "Les caméras n'ont pas pu être enregistrées (droits ou espace disque) : rien n'a changé."
                        .into(),
                ));
            }
        }
        self.publish_cameras(&mut st);
        Ok(out)
    }

    fn publish_cameras(&self, st: &mut HubState) {
        let cameras = st.cameras.views();
        self.publish(st, WireEvent::CamerasChanged { cameras });
    }

    fn replace_document(
        &self,
        document: Document,
        set: impl FnOnce(&mut HubState),
    ) -> ApiResult<()> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        set(&mut st);
        self.persist_and_publish(&mut st, document)
    }

    /// Modifie un document si `change` retourne `Some` ; l'écrit et le diffuse.
    fn update_document(
        &self,
        change: impl FnOnce(&mut HubState) -> Option<()>,
        document: Document,
    ) {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = self.lock();
        if change(&mut st).is_some() {
            let _ = self.persist_and_publish(&mut st, document);
        }
    }

    fn persist_and_publish(&self, st: &mut HubState, document: Document) -> ApiResult<()> {
        let saved = if self.demo {
            Ok(())
        } else {
            match document {
                Document::Plan => persistence::save_json(&self.data_dir.join(PLAN_FILE), &st.plan),
                Document::Rules => {
                    persistence::save_json(&self.data_dir.join(RULES_FILE), &st.rules)
                }
            }
        };
        // Diffusé même si l'écriture échoue : la mémoire fait foi tant que le serveur tourne.
        let event = match document {
            Document::Plan => WireEvent::PlanChanged {
                plan: TextIds(st.plan.clone()),
            },
            Document::Rules => WireEvent::RulesChanged {
                rules: TextIds(st.rules.clone()),
            },
        };
        self.publish(st, event);
        saved.map_err(|e| internal("enregistrement du plan ou des règles", &e))
    }

    // -----------------------------------------------------------------------
    // Historique, export, import
    // -----------------------------------------------------------------------

    /// Lit l'historique d'un appareil (bloquant : à lancer hors du runtime).
    pub fn history(&self, node_id: u64, query: &HistoryQuery) -> ApiResult<HistoryResponse> {
        self.read_history(
            node_id,
            query,
            wire::HISTORY_MAX_LIMIT,
            wire::HISTORY_DEFAULT_LIMIT,
        )
    }

    fn read_history(
        &self,
        node_id: u64,
        query: &HistoryQuery,
        max_limit: usize,
        default_limit: usize,
    ) -> ApiResult<HistoryResponse> {
        self.device_info(node_id)?;
        let bad = |detail: &str| ApiError::with_detail(ErrorCode::BadRequest, detail);
        let now = now_unix();
        let until = query.until.unwrap_or(now);
        let default_window = if default_limit == wire::HISTORY_DEFAULT_LIMIT {
            3600.0
        } else {
            wire::HISTORY_MAX_WINDOW_S as f64
        };
        let mut since = query.since.unwrap_or(until - default_window);
        if !since.is_finite() || !until.is_finite() || since > until {
            return Err(bad("fenêtre de temps invalide"));
        }
        // Fenêtre bornée à la rétention : au-delà, il n'y a rien à lire.
        since = since.max(until - wire::HISTORY_MAX_WINDOW_S as f64);
        let limit = match query.limit {
            Some(0) => return Err(bad("limite invalide")),
            Some(n) => n.min(max_limit),
            None => default_limit,
        };
        let wanted: Option<Vec<Metric>> = match query.metrics.as_deref() {
            None | Some("") => None,
            Some(list) => Some(
                list.split(',')
                    .map(|k| Metric::from_key(k.trim()).ok_or_else(|| bad("grandeur inconnue")))
                    .collect::<Result<_, _>>()?,
            ),
        };
        let buckets = match query.buckets {
            Some(0) => return Err(bad("nombre de tranches invalide")),
            Some(n) => Some(n.min(wire::HISTORY_MAX_BUCKETS)),
            None => None,
        };
        let store = HistoryStore::new(self.history_dir.clone());
        let mut points: Vec<HistoryPoint> = store
            .load(node_id, since)
            .into_iter()
            .filter(|r| r.t <= until)
            .filter_map(|r| {
                let metric = Metric::from_key(&r.k)?;
                Some(HistoryPoint {
                    at: r.t,
                    metric,
                    value: r.v,
                })
            })
            .filter(|p| wanted.as_ref().is_none_or(|w| w.contains(&p.metric)))
            .collect();
        points.sort_by(|a, b| a.at.total_cmp(&b.at));
        let mut truncated = false;
        if let Some(buckets) = buckets {
            // `limit` s'applique à chaque grandeur : sinon, avec plusieurs grandeurs, la coupe
            // « les plus anciens d'abord » de l'ensemble ne couvrirait plus la fenêtre.
            (points, truncated) = reduce_points(points, since, until, buckets, limit);
        } else if points.len() > limit {
            truncated = true;
            points.drain(..points.len() - limit);
        }
        Ok(HistoryResponse {
            node_id,
            since,
            until,
            truncated,
            points,
        })
    }

    /// CSV (température et humidité) d'un appareil, sur la fenêtre demandée (7 jours par défaut).
    pub fn history_csv(&self, node_id: u64, query: &HistoryQuery) -> ApiResult<String> {
        let mut query = query.clone();
        query.metrics = Some("temperature,humidity".into());
        // Un export garde toutes les mesures : jamais de réduction.
        query.buckets = None;
        let response = self.read_history(node_id, &query, CSV_MAX_POINTS, CSV_MAX_POINTS)?;
        Ok(export::temperature_csv(&response.points))
    }

    /// Sauvegarde `.matterhome` complète : contient les clés privées en clair.
    pub fn export(&self) -> ApiResult<(String, String)> {
        match backup::export_to_string(&self.data_dir) {
            Ok((text, _)) => Ok((backup::default_file_name(), text)),
            Err(e) if !self.data_dir.join(persistence::FILE_NAME).exists() => {
                log::info!("export impossible : {e:#}");
                Err(ApiError::new(ErrorCode::NoData))
            }
            Err(e) => Err(internal("export", &e)),
        }
    }

    /// Valide une sauvegarde sans rien engager : ni fichier écrit, ni arrêt (l'inspection d'un
    /// import). Les mêmes contrôles que l'import réel.
    pub fn inspect_import(&self, text: &str) -> ApiResult<(ImportSummary, ImportIdentity)> {
        match backup::inspect_text(text) {
            Ok(summary) => Ok((import_summary(summary), self.import_identity(text)?)),
            Err(e) => {
                log::info!("sauvegarde à inspecter refusée : {e}");
                Err(self.invalid_import(&e.to_string()))
            }
        }
    }

    /// L'identité de la sauvegarde face à celle du serveur. Une identité du serveur illisible
    /// compte comme « différente » : dans le doute, rien n'est remplacé sans accord explicite.
    fn import_identity(&self, text: &str) -> ApiResult<ImportIdentity> {
        let info = |b: &backup::IdentityBrief| IdentityInfo {
            fabric_id: format!("{:#x}", b.fabric_id),
            devices: b.devices,
        };
        let incoming = backup::identity_in_text(text).map_err(|e| self.invalid_import(&e.0))?;
        let (current, differs) = match backup::identity_in_dir(&self.data_dir) {
            Ok(None) => (None, false),
            Ok(Some(c)) => {
                let differs = c.fabric_id != incoming.fabric_id;
                (Some(info(&c)), differs)
            }
            Err(e) => {
                log::warn!("identité actuelle illisible : {e:#}");
                (None, true)
            }
        };
        Ok(ImportIdentity {
            incoming: info(&incoming),
            current,
            differs,
        })
    }

    fn invalid_import(&self, why: &str) -> ApiError {
        // Les messages de `backup` décrivent le fichier reçu ; on retire tout chemin.
        ApiError::with_detail(ErrorCode::InvalidImport, self.scrubber.clean(why))
    }

    /// Valide une sauvegarde et la met de côté ; le serveur doit ensuite s'arrêter pour que
    /// son superviseur le relance et l'applique. Un seul import à la fois : tant qu'un import
    /// validé attend le redémarrage, un autre est refusé (`import_pending`).
    ///
    /// Une sauvegarde d'une autre identité (fabric) que celle du serveur n'est acceptée qu'avec
    /// `replace_identity` : sinon `identity_mismatch` (409), rien n'est écrit ni arrêté.
    pub fn stage_import(
        &self,
        text: &str,
        replace_identity: bool,
    ) -> ApiResult<(ImportSummary, ImportIdentity)> {
        if let Err(e) = backup::inspect_text(text) {
            log::info!("import refusé : {e}");
            return Err(self.invalid_import(&e.to_string()));
        }
        let identity = self.import_identity(text)?;
        if identity.differs && !replace_identity {
            log::warn!(
                "import refusé : fabric {} (sauvegarde) différente de celle du serveur",
                identity.incoming.fabric_id
            );
            return Err(ApiError::with_detail(
                ErrorCode::IdentityMismatch,
                "la sauvegarde porte une autre identité Matter que le serveur",
            ));
        }
        if self.import_staged.swap(true, Ordering::SeqCst) {
            return Err(ApiError::new(ErrorCode::ImportPending));
        }
        match backup::stage_import_text(&self.data_dir, text) {
            Ok(summary) => {
                log::warn!(
                    "import validé ({}) : arrêt demandé pour l'appliquer.",
                    summary.describe()
                );
                self.request_exit(ExitReason::ImportStaged);
                Ok((import_summary(summary), identity))
            }
            Err(e) => {
                self.import_staged.store(false, Ordering::SeqCst);
                match e {
                    backup::StageError::Invalid(why) => {
                        log::info!("import refusé : {why}");
                        Err(self.invalid_import(&why))
                    }
                    backup::StageError::Io(e) => Err(internal("import", &e)),
                }
            }
        }
    }

    pub fn request_exit(&self, reason: ExitReason) {
        *self.exit.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
    }

    pub fn exit_reason(&self) -> Option<ExitReason> {
        *self.exit.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

fn import_summary(summary: backup::Summary) -> ImportSummary {
    ImportSummary {
        created: summary.created,
        devices: summary.devices,
        rooms: summary.rooms,
        rules: summary.rules,
        cameras: summary.cameras,
        history_files: summary.history_files,
        has_plan: summary.has_plan,
        has_rules: summary.has_rules,
        disabled_rules: summary.disabled_rules,
    }
}

/// Réduit chaque grandeur à `buckets` tranches de temps (minimum et maximum de chacune), garde
/// au plus `limit` points récents par grandeur, puis remet les points dans l'ordre chronologique.
/// Rend aussi `true` si une grandeur a dû être coupée. Voir `protocol::downsample::min_max`.
fn reduce_points(
    points: Vec<HistoryPoint>,
    since: f64,
    until: f64,
    buckets: usize,
    limit: usize,
) -> (Vec<HistoryPoint>, bool) {
    let mut by_metric: std::collections::BTreeMap<Metric, Vec<(f64, f64)>> =
        std::collections::BTreeMap::new();
    for p in points {
        by_metric.entry(p.metric).or_default().push((p.at, p.value));
    }
    let mut truncated = false;
    let mut out: Vec<HistoryPoint> = Vec::new();
    for (metric, series) in by_metric {
        let mut reduced = protocol::downsample::min_max(&series, since, until, buckets);
        if reduced.len() > limit {
            truncated = true;
            reduced.drain(..reduced.len() - limit);
        }
        out.extend(
            reduced
                .into_iter()
                .map(|(at, value)| HistoryPoint { at, metric, value }),
        );
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    (out, truncated)
}

/// Plafond de lignes d'un export CSV.
const CSV_MAX_POINTS: usize = 200_000;

const PLAN_FILE: &str = "maison.json";
const RULES_FILE: &str = "regles.json";

#[derive(Clone, Copy)]
enum Document {
    Plan,
    Rules,
}

fn to_json<T: Serialize>(message: &T) -> Option<Arc<str>> {
    match serde_json::to_string(message) {
        Ok(text) => Some(Arc::from(text)),
        Err(e) => {
            log::error!("message impossible à sérialiser : {e}");
            None
        }
    }
}

/// Lit `maison.json` ou `regles.json`. Un fichier illisible est mis de côté (jamais écrasé
/// sans trace) et le document par défaut prend sa place.
/// Le plan enregistré. Un plan sans étage (ancien fichier, édition à la main) est remplacé en
/// mémoire par le plan par défaut : les clients supposent qu'il y a toujours un étage.
fn load_plan(path: &Path) -> HomePlan {
    let mut plan = load_document::<HomePlan>(path, "plan");
    if plan.ensure_floor() {
        log::warn!(
            "plan sans étage dans {} : plan par défaut utilisé",
            path.display()
        );
    }
    plan
}

fn load_document<T: serde::de::DeserializeOwned + Default>(path: &Path, what: &str) -> T {
    match persistence::load_json::<T>(path) {
        Ok(Some(doc)) => doc,
        Ok(None) => T::default(),
        Err(e) => {
            let aside = path.with_extension("json.illisible");
            log::error!(
                "{what} illisible(s), valeurs par défaut utilisées ({e:#}) ; fichier conservé sous {}",
                aside.display()
            );
            let _ = std::fs::rename(path, aside);
            T::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{MeasureKind, NodeStatus};

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-hub-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn hub_with(dir: &Path, capacity: usize) -> (Hub, mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let hub = Hub::new(HubConfig {
            data_dir: dir.to_path_buf(),
            demo: false,
            history_dir: dir.join("history"),
            demo_history: None,
            broadcast_capacity: capacity,
            commands: tx,
        });
        (hub, rx)
    }

    fn node(id: u64) -> Event {
        Event::Node(NodeInfo {
            node_id: id,
            name: format!("Appareil {id}"),
            caps: crate::events::Capabilities {
                temperature: true,
                cover: true,
                on_off: true,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn temperature(id: u64, value: f64, at: f64) -> Event {
        Event::Measurement {
            node_id: id,
            kind: MeasureKind::Temperature,
            value,
            at,
        }
    }

    #[test]
    fn aggregates_the_full_state() {
        let dir = temp("agg");
        let (hub, _rx) = hub_with(&dir, 16);
        hub.apply(&node(7));
        hub.apply(&Event::Status {
            node_id: 7,
            status: NodeStatus::Online,
        });
        hub.apply(&temperature(7, 21.0, 100.0));
        // Une mesure plus ancienne (historique rejoué) ne remplace pas la plus récente.
        hub.apply(&temperature(7, 15.0, 50.0));
        hub.apply(&Event::State {
            node_id: 7,
            kind: StateKind::Open,
            value: true,
            at: 120.0,
        });
        hub.apply(&Event::State {
            node_id: 7,
            kind: StateKind::Open,
            value: true,
            at: 130.0,
        });
        let snap = hub.current_snapshot();
        assert_eq!(snap.devices.len(), 1);
        let d = &snap.devices[0];
        assert_eq!(d.status, WireStatus::Online);
        let t = d
            .latest
            .iter()
            .find(|l| l.metric == Metric::Temperature)
            .unwrap();
        assert_eq!((t.value, t.at), (21.0, 100.0));
        // `open_since` = premier passage à « ouvert », pas le dernier rapport identique.
        assert_eq!(d.open_since, Some(120.0));
        // La mesure écartée (historique rejoué) n'a pas été diffusée : pas de numéro consommé.
        assert_eq!(snap.seq, 5);
        hub.apply(&Event::NodeRemoved(7));
        assert!(hub.current_snapshot().devices.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Les mesures rejouées au démarrage, plus anciennes que la dernière retenue, ne sont pas
    /// diffusées : l'état complet ne les contient pas non plus.
    #[test]
    fn discarded_replayed_measurements_are_not_broadcast() {
        let dir = temp("replay");
        let (hub, _rx) = hub_with(&dir, 16);
        hub.apply(&node(7));
        let mut sub = hub.subscribe();
        hub.apply(&temperature(7, 21.0, 100.0));
        hub.apply(&temperature(7, 15.0, 50.0)); // rejouée : plus ancienne
        hub.apply(&Event::State {
            node_id: 7,
            kind: StateKind::Open,
            value: true,
            at: 200.0,
        });
        hub.apply(&Event::State {
            node_id: 7,
            kind: StateKind::Open,
            value: false,
            at: 150.0, // rejouée : plus ancienne
        });
        let mut seen = Vec::new();
        while let Ok(text) = sub.rx.try_recv() {
            let ServerMessage::Event { seq, event } = serde_json::from_str(&text).unwrap() else {
                panic!()
            };
            seen.push((seq, serde_json::to_value(&event).unwrap()));
        }
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].0 + 1, seen[1].0, "pas de trou dans les numéros");
        assert_eq!(seen[0].1["value"], 21.0);
        assert_eq!(seen[1].1["value"], 1.0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn backup_text(dir: &Path) -> String {
        let state = persistence::StoredState {
            version: 1,
            identity: crate::matter::certs::test_identity(&"01".repeat(32)),
            nodes: Vec::new(),
        };
        std::fs::write(
            dir.join(persistence::FILE_NAME),
            serde_json::to_string(&state).unwrap(),
        )
        .unwrap();
        backup::export_to_string(dir).unwrap().0
    }

    /// Un second import est refusé tant qu'un premier attend le redémarrage ; un import refusé
    /// ne bloque pas le suivant ; l'inspection ne bloque rien et n'écrit rien.
    #[test]
    fn a_second_import_is_refused_while_one_is_pending() {
        let dir = temp("imports");
        let (hub, _rx) = hub_with(&dir, 16);
        let text = backup_text(&dir);

        // Refusé (pas une sauvegarde) : le verrou est rendu.
        assert_eq!(
            hub.stage_import("n'importe quoi", false).unwrap_err().code,
            ErrorCode::InvalidImport
        );
        // Inspection : rien d'écrit, répétable.
        for _ in 0..2 {
            let (summary, identity) = hub.inspect_import(&text).unwrap();
            assert!(!identity.differs && identity.current.is_some());
            assert_eq!((summary.rooms, summary.rules), (0, 0));
            assert!(!dir.join("import-en-attente.matterhome").exists());
        }
        assert!(hub.exit_reason().is_none());

        hub.stage_import(&text, false).unwrap();
        assert_eq!(hub.exit_reason(), Some(ExitReason::ImportStaged));
        let pending = std::fs::read(dir.join("import-en-attente.matterhome")).unwrap();
        let second = hub.stage_import(&text, false).unwrap_err();
        assert_eq!(second.code, ErrorCode::ImportPending);
        assert_eq!(second.code.http_status(), 409);
        // Le premier dépôt n'a pas été touché.
        assert_eq!(
            std::fs::read(dir.join("import-en-attente.matterhome")).unwrap(),
            pending
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Une sauvegarde d'une autre fabric n'est mise de côté qu'avec `replace_identity` ; sans
    /// lui, refus `identity_mismatch` sans rien écrire ni arrêter le serveur.
    #[test]
    fn a_different_identity_needs_an_explicit_replace_identity() {
        let dir = temp("identity");
        let (hub, _rx) = hub_with(&dir, 16);
        let own = backup_text(&dir);
        // Une autre fabric : même clés, autre numéro.
        let other = own.replace("\\\"fabric_id\\\":1,", "\\\"fabric_id\\\":255,");
        assert_ne!(own, other, "la sauvegarde d'essai doit changer");

        let (_, identity) = hub.inspect_import(&other).unwrap();
        assert!(identity.differs);
        assert_eq!(identity.incoming.fabric_id, "0xff");
        assert_eq!(identity.current.as_ref().unwrap().fabric_id, "0x1");
        let err = hub.stage_import(&other, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        assert_eq!(err.code.http_status(), 409);
        assert!(!dir.join("import-en-attente.matterhome").exists());
        assert!(hub.exit_reason().is_none());
        // Le refus ne bloque pas la suite : avec l'option, c'est accepté.
        hub.stage_import(&other, true).unwrap();
        assert_eq!(hub.exit_reason(), Some(ExitReason::ImportStaged));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn subscription_has_no_gap_and_slow_readers_resync() {
        let dir = temp("lag");
        let (hub, _rx) = hub_with(&dir, 4);
        hub.apply(&node(7));
        let mut slow = hub.subscribe();
        let snap: ServerMessage = serde_json::from_str(&slow.snapshot).unwrap();
        let ServerMessage::Snapshot(snap) = snap else {
            panic!()
        };
        assert_eq!(snap.seq, 1);
        // Un lecteur qui ne lit pas ne bloque jamais l'émetteur : 50 événements passent.
        for i in 0..50 {
            hub.apply(&temperature(7, 20.0 + f64::from(i), 100.0 + f64::from(i)));
        }
        assert!(matches!(
            slow.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        // Il repart de l'état complet le plus récent, puis des événements suivants.
        let fresh = hub.subscribe();
        let ServerMessage::Snapshot(snap) = serde_json::from_str(&fresh.snapshot).unwrap() else {
            panic!()
        };
        assert_eq!(snap.seq, 51);
        let t = snap.devices[0].latest[0];
        assert_eq!(t.value, 69.0);
        let mut fresh_rx = fresh.rx;
        hub.apply(&temperature(7, 1.0, 500.0));
        let next: ServerMessage = serde_json::from_str(&fresh_rx.try_recv().unwrap()).unwrap();
        assert!(matches!(next, ServerMessage::Event { seq: 52, .. }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stored_plan_without_floor_loads_as_the_default_plan() {
        let dir = temp("nofloor");
        std::fs::write(dir.join(PLAN_FILE), r#"{"version":1,"floors":[]}"#).unwrap();
        let (hub, _commands) = hub_with(&dir, 16);
        assert_eq!(hub.plan(), HomePlan::default());
        // Et un client ne peut pas en renvoyer un.
        let empty = HomePlan {
            floors: vec![],
            ..HomePlan::default()
        };
        assert_eq!(
            hub.set_plan(empty).unwrap_err().code,
            protocol::wire::ErrorCode::PlanWithoutFloor
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plan_and_rules_are_persisted_broadcast_and_last_write_wins() {
        let dir = temp("docs");
        let (hub, mut commands) = hub_with(&dir, 16);
        let mut sub = hub.subscribe();

        let mut plan = HomePlan::default();
        plan.floors[0].name = "Premier".into();
        hub.set_plan(plan.clone()).unwrap();
        plan.floors[0].name = "Second".into();
        hub.set_plan(plan.clone()).unwrap();
        let on_disk: HomePlan = persistence::load_json(&dir.join(PLAN_FILE))
            .unwrap()
            .unwrap();
        assert_eq!(on_disk.floors[0].name, "Second");
        assert_eq!(hub.plan().floors[0].name, "Second");

        let mut rules = RuleSet::default();
        rules.rules.push(Rule::new(9));
        hub.set_rules(rules).unwrap();
        assert!(dir.join(RULES_FILE).exists());
        assert!(matches!(commands.try_recv(), Ok(Command::SetRules(r)) if r.len() == 1));
        // Règle de test : seulement pour une règle connue.
        hub.test_rule(9).unwrap();
        assert!(hub.test_rule(10).is_err());

        let mut kinds = Vec::new();
        while let Ok(m) = sub.rx.try_recv() {
            let ServerMessage::Event { event, .. } = serde_json::from_str(&m).unwrap() else {
                panic!()
            };
            kinds.push(match event {
                WireEvent::PlanChanged { .. } => "plan",
                WireEvent::RulesChanged { .. } => "rules",
                _ => "autre",
            });
        }
        assert_eq!(kinds, ["plan", "plan", "rules"]);

        // Oublier un appareil le retire du plan, enregistré et diffusé.
        let mut plan = HomePlan::default();
        plan.floors[0].items.push(protocol::home::Item {
            id: 5,
            kind: protocol::home::ItemKind::Device {
                node_id: 7,
                symbol: None,
            },
            x: 1.0,
            y: 1.0,
        });
        hub.apply(&node(7));
        hub.set_plan(plan).unwrap();
        hub.apply(&Event::NodeRemoved(7));
        assert!(hub.plan().placed_devices().is_empty());
        let on_disk: HomePlan = persistence::load_json(&dir.join(PLAN_FILE))
            .unwrap()
            .unwrap();
        assert!(on_disk.placed_devices().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_is_windowed_and_bounded() {
        let dir = temp("hist");
        let (hub, _rx) = hub_with(&dir, 16);
        hub.apply(&node(7));
        let mut store = HistoryStore::new(dir.join("history"));
        for i in 0..100 {
            let record = crate::history_store::Record {
                t: 1000.0 + f64::from(i),
                k: if i % 2 == 0 {
                    "temperature"
                } else {
                    "humidity"
                }
                .into(),
                v: f64::from(i),
            };
            store.append(7, &record).unwrap();
        }
        let q = |since, until, metrics: Option<&str>, limit| HistoryQuery {
            since: Some(since),
            until: Some(until),
            metrics: metrics.map(str::to_string),
            limit,
            buckets: None,
        };
        let all = hub.history(7, &q(0.0, 2000.0, None, None)).unwrap();
        assert_eq!(all.points.len(), 100);
        assert!(!all.truncated);
        // Fenêtre et filtre de grandeur.
        let t = hub
            .history(7, &q(1010.0, 1019.0, Some("temperature"), None))
            .unwrap();
        assert_eq!(t.points.len(), 5);
        // Borne de volume : les plus récentes, avec l'indicateur.
        let cut = hub.history(7, &q(0.0, 2000.0, None, Some(10))).unwrap();
        assert!(cut.truncated);
        assert_eq!(cut.points.len(), 10);
        assert_eq!(cut.points.last().unwrap().value, 99.0);
        // Erreurs : appareil inconnu, grandeur inconnue, fenêtre à l'envers, limite nulle.
        assert_eq!(
            hub.history(8, &q(0.0, 1.0, None, None)).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert!(hub.history(7, &q(0.0, 1.0, Some("couleur"), None)).is_err());
        assert!(hub.history(7, &q(5.0, 1.0, None, None)).is_err());
        assert!(hub.history(7, &q(0.0, 1.0, None, Some(0))).is_err());
        // Fenêtre plafonnée à la rétention.
        let wide = hub.history(7, &q(0.0, 1e9, None, None)).unwrap();
        assert_eq!(wide.until - wide.since, wire::HISTORY_MAX_WINDOW_S as f64);
        // Plafond de la limite.
        let big = hub
            .history(7, &q(0.0, 2000.0, None, Some(usize::MAX)))
            .unwrap();
        assert_eq!(big.points.len(), 100);
        let csv = hub.history_csv(7, &q(0.0, 2000.0, None, None)).unwrap();
        assert_eq!(csv.lines().count(), 51);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_buckets_cover_the_window_and_keep_extremes() {
        let dir = temp("hist-buckets");
        let (hub, _rx) = hub_with(&dir, 16);
        hub.apply(&node(7));
        let mut store = HistoryStore::new(dir.join("history"));
        // Un capteur bavard : 30 000 mesures (toutes les 20 s sur 7 jours), un pic et un creux.
        let n = 30_000;
        for i in 0..n {
            let v = match i {
                4_000 => 35.0,
                21_000 => -3.0,
                _ => 20.0,
            };
            let record = crate::history_store::Record {
                t: 1_000.0 + f64::from(i) * 20.0,
                k: "temperature".into(),
                v,
            };
            store.append(7, &record).unwrap();
        }
        let last = 1_000.0 + f64::from(n - 1) * 20.0;
        let q = |buckets| HistoryQuery {
            since: Some(0.0),
            until: Some(last),
            metrics: Some("temperature".into()),
            limit: Some(wire::HISTORY_MAX_LIMIT),
            buckets,
        };
        // Sans réduction : la limite ne garde que les plus récentes (comportement d'avant).
        let raw = hub.history(7, &q(None)).unwrap();
        assert!(raw.truncated);
        assert!(raw.points.first().unwrap().at > 1_000.0 + 9_000.0 * 20.0);
        // Avec réduction : toute la fenêtre est couverte, peu de points, extrêmes gardés.
        let reduced = hub.history(7, &q(Some(600))).unwrap();
        assert!(!reduced.truncated);
        assert!(
            reduced.points.len() <= 2 * 600 + 2,
            "{}",
            reduced.points.len()
        );
        assert_eq!(reduced.points.first().unwrap().at, 1_000.0);
        assert_eq!(reduced.points.last().unwrap().at, last);
        assert!(reduced.points.iter().any(|p| p.value == 35.0));
        assert!(reduced.points.iter().any(|p| p.value == -3.0));
        assert!(reduced.points.windows(2).all(|w| w[0].at <= w[1].at));
        // Tranches : zéro refusé, plafond appliqué sans erreur.
        assert!(hub.history(7, &q(Some(0))).is_err());
        assert!(hub.history(7, &q(Some(usize::MAX))).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_buckets_with_several_metrics_apply_the_limit_per_metric() {
        let dir = temp("hist-buckets-multi");
        let (hub, _rx) = hub_with(&dir, 16);
        hub.apply(&node(7));
        let mut store = HistoryStore::new(dir.join("history"));
        // Température et humidité, 6 000 mesures chacune sur 6 000 s.
        for i in 0..6_000 {
            for (k, v) in [("temperature", 20.0), ("humidity", 50.0)] {
                let record = crate::history_store::Record {
                    t: 1_000.0 + f64::from(i),
                    k: k.into(),
                    v,
                };
                store.append(7, &record).unwrap();
            }
        }
        let q = |buckets, limit| HistoryQuery {
            since: Some(0.0),
            until: Some(7_000.0),
            metrics: None,
            limit,
            buckets,
        };
        let r = hub.history(7, &q(Some(1_000), Some(2_100))).unwrap();
        assert!(
            !r.truncated,
            "2 002 points par grandeur tiennent dans la limite"
        );
        for metric in [Metric::Temperature, Metric::Humidity] {
            let of: Vec<f64> = r
                .points
                .iter()
                .filter(|p| p.metric == metric)
                .map(|p| p.at)
                .collect();
            assert!(
                of.len() <= 2_002 && of.len() > 800,
                "{metric:?} : {}",
                of.len()
            );
            // Les deux grandeurs couvrent toute la fenêtre.
            assert_eq!(of[0], 1_000.0, "{metric:?}");
            assert_eq!(*of.last().unwrap(), 6_999.0, "{metric:?}");
        }
        // Limite plus basse que la réduction : coupe par grandeur, signalée.
        let cut = hub.history(7, &q(Some(1_000), Some(500))).unwrap();
        assert!(cut.truncated);
        for metric in [Metric::Temperature, Metric::Humidity] {
            assert_eq!(
                cut.points.iter().filter(|p| p.metric == metric).count(),
                500
            );
        }
        // L'export CSV ignore `buckets`.
        let csv = hub.history_csv(7, &q(Some(10), None)).unwrap();
        assert_eq!(csv.lines().count(), 6_001);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_are_checked_before_being_sent() {
        let dir = temp("cmd");
        let (hub, mut rx) = hub_with(&dir, 16);
        assert_eq!(hub.retry(1).unwrap_err().code, ErrorCode::NotFound);
        hub.apply(&node(1));
        hub.retry(1).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Command::Retry(1))));
        assert!(hub.set_cover(1, CoverCommand::Position(101)).is_err());
        hub.set_cover(1, CoverCommand::Position(40)).unwrap();
        hub.set_on_off(1, OnOffCommand::Toggle).unwrap();
        // Appairage : code invalide refusé, un seul à la fois.
        assert_eq!(hub.pair("abc").unwrap_err().code, ErrorCode::BadRequest);
        hub.pair("3497-011-2332").unwrap();
        assert_eq!(
            hub.pair("3497-011-2332").unwrap_err().code,
            ErrorCode::PairingBusy
        );
        hub.apply(&Event::PairingFailed("échec".into()));
        hub.pair("3497-011-2332").unwrap();
        // À l'arrêt, plus aucune commande n'est transmise.
        hub.close_commands();
        assert_eq!(hub.retry(1).unwrap_err().code, ErrorCode::Unavailable);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // -----------------------------------------------------------------------
    // Caméras
    // -----------------------------------------------------------------------

    const CAM_SECRET: &str = "mdp-camera-tres-secret";
    const CAM_HOST: &str = "192.0.2.55";

    fn cam_draft(name: &str) -> CameraDraft {
        CameraDraft {
            name: name.into(),
            url: format!("rtsp://{CAM_HOST}:554/live/ch00_0"),
            sub_url: format!("rtsp://{CAM_HOST}:554/live/ch00_1"),
            username: "admin".into(),
            password: Some(CAM_SECRET.into()),
        }
    }

    /// Journal capturé : tout ce que le serveur écrit pendant les tests de caméras.
    fn captured_logs() -> &'static Mutex<Vec<String>> {
        use std::sync::OnceLock;
        static LOGS: OnceLock<&'static Mutex<Vec<String>>> = OnceLock::new();
        LOGS.get_or_init(|| {
            struct Capture(&'static Mutex<Vec<String>>);
            impl log::Log for Capture {
                fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                    true
                }
                fn log(&self, record: &log::Record<'_>) {
                    self.0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("{}", record.args()));
                }
                fn flush(&self) {}
            }
            let lines: &'static Mutex<Vec<String>> = Box::leak(Box::new(Mutex::new(Vec::new())));
            let _ = log::set_logger(Box::leak(Box::new(Capture(lines))));
            log::set_max_level(log::LevelFilter::Trace);
            lines
        })
    }

    fn drain(sub: &mut Subscription) -> Vec<String> {
        std::iter::from_fn(|| sub.rx.try_recv().ok().map(|m| m.to_string())).collect()
    }

    #[test]
    fn cameras_are_persisted_broadcast_and_reloaded() {
        let dir = temp("cam-flow");
        let (hub, _rx) = hub_with(&dir, 32);
        let mut sub = hub.subscribe();
        let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
        assert!(protocol::wire::is_camera_id(id));
        assert!(dir.join(cameras::FILE_NAME).exists());
        // Le client reçoit la liste complète, sans secret.
        let events = drain(&mut sub);
        assert_eq!(events.len(), 1);
        let ServerMessage::Event {
            event: WireEvent::CamerasChanged { cameras },
            ..
        } = serde_json::from_str(&events[0]).unwrap()
        else {
            panic!("{}", events[0])
        };
        assert_eq!(cameras.len(), 1);
        assert_eq!((cameras[0].id, cameras[0].name.as_str()), (id, "Entrée"));
        assert!(cameras[0].has_sub_stream);
        assert_eq!(cameras[0].status, CameraStatus::Unknown);
        // Un nouveau hub (redémarrage) retrouve la caméra, avec ses secrets côté serveur.
        let (again, _rx2) = hub_with(&dir, 32);
        assert_eq!(again.cameras(), cameras);
        let target = again.camera_target(id, StreamKind::Sub).unwrap();
        assert_eq!(target.url.path(), "/live/ch00_1");
        assert_eq!(target.credentials.unwrap().password, CAM_SECRET);
        assert!(again.camera_target(id, StreamKind::Main).is_some());
        // Modifier, puis supprimer : chaque changement est diffusé et enregistré.
        let mut edit = cam_draft("Entrée 2");
        edit.password = None;
        hub.update_camera(id, &edit).unwrap();
        assert_eq!(cameras::load(&dir)[0].password.as_deref(), Some(CAM_SECRET));
        hub.delete_camera(id).unwrap();
        assert!(cameras::load(&dir).is_empty());
        assert!(hub.cameras().is_empty());
        assert_eq!(drain(&mut sub).len(), 2);
        assert!(hub.delete_camera(id).is_err());
        assert!(hub.update_camera(id, &edit).is_err());
        // Aucune caméra n'a touché aux appareils ni au plan.
        assert!(hub.current_snapshot().devices.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_save_leaves_the_cameras_unchanged() {
        let dir = temp("cam-rollback");
        let (hub, _rx) = hub_with(&dir, 32);
        let id = hub.add_camera(&cam_draft("Garage")).unwrap();
        let mut sub = hub.subscribe();
        // Un dossier à la place du fichier temporaire : l'écriture échoue.
        std::fs::create_dir(dir.join("cameras.tmp")).unwrap();
        assert!(hub.add_camera(&cam_draft("Jardin")).is_err());
        assert!(hub.update_camera(id, &cam_draft("Renommée")).is_err());
        assert!(hub.delete_camera(id).is_err());
        assert_eq!(hub.cameras().len(), 1);
        assert_eq!(hub.cameras()[0].name, "Garage");
        assert!(
            drain(&mut sub).is_empty(),
            "rien n'est diffusé si rien n'a changé"
        );
        let message = hub.add_camera(&cam_draft("Jardin")).unwrap_err().0;
        assert!(!message.contains(CAM_SECRET) && !message.contains(CAM_HOST));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn probe_results_update_the_state_and_stale_ones_are_dropped() {
        let dir = temp("cam-probe");
        let (hub, _rx) = hub_with(&dir, 32);
        let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
        let mut sub = hub.subscribe();
        let (kind, target) = hub.camera_probe_target(id).unwrap();
        assert_eq!(
            kind,
            StreamKind::Sub,
            "on surveille le flux réduit s'il existe"
        );
        let info = StreamInfo {
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
        };
        hub.record_camera_probe(id, kind, &target, Ok(info.clone()));
        let view = hub.cameras().remove(0);
        assert_eq!(
            (view.status, view.stream),
            (CameraStatus::Online, Some(info.clone()))
        );
        assert_eq!(drain(&mut sub).len(), 1);
        // Même résultat : rien à diffuser.
        hub.record_camera_probe(id, kind, &target, Ok(info));
        assert!(drain(&mut sub).is_empty());
        // Échec : injoignable, avec la phrase de l'utilisateur.
        let down = ProbeError {
            problem: protocol::wire::CameraProblem::Unauthorized,
            message: "La caméra refuse l'identifiant ou le mot de passe.".into(),
        };
        hub.record_camera_probe(id, kind, &target, Err(down));
        assert!(matches!(
            hub.cameras()[0].status,
            CameraStatus::Unreachable {
                problem: protocol::wire::CameraProblem::Unauthorized,
                ..
            }
        ));
        // La caméra est modifiée pendant une vérification : le résultat obsolète est écarté.
        let mut edit = cam_draft("Entrée");
        edit.sub_url = format!("rtsp://{CAM_HOST}:554/live/autre");
        edit.password = None;
        hub.update_camera(id, &edit).unwrap();
        assert_eq!(hub.cameras()[0].status, CameraStatus::Unknown);
        drain(&mut sub);
        hub.record_camera_probe(
            id,
            kind,
            &target,
            Ok(StreamInfo {
                codec: "h265".into(),
                width: None,
                height: None,
            }),
        );
        assert_eq!(hub.cameras()[0].status, CameraStatus::Unknown);
        assert!(drain(&mut sub).is_empty());
        // Caméra supprimée pendant la vérification : rien non plus.
        hub.delete_camera(id).unwrap();
        hub.record_camera_probe(
            id,
            kind,
            &target,
            Ok(StreamInfo {
                codec: "h264".into(),
                width: None,
                height: None,
            }),
        );
        assert!(hub.cameras().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Critère d'acceptation : ni l'adresse ni le mot de passe dans les réponses de l'API (l'état
    /// complet), les événements, ni les journaux.
    #[test]
    fn neither_the_address_nor_the_password_leave_the_server() {
        let logs = captured_logs();
        let dir = temp("cam-secrets");
        let (hub, _rx) = hub_with(&dir, 32);
        let mut sub = hub.subscribe();
        let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
        let mut edit = cam_draft("Entrée");
        edit.url = format!("rtsp://admin:{CAM_SECRET}@{CAM_HOST}:554/live/ch00_9");
        edit.password = Some(String::new());
        hub.update_camera(id, &edit).unwrap();
        // Un refus ne répète pas non plus ce qui a été saisi.
        let mut bad = cam_draft("");
        bad.url = format!("http://admin:{CAM_SECRET}@{CAM_HOST}/x");
        let refused = hub.add_camera(&bad).unwrap_err();
        let (kind, target) = hub.camera_probe_target(id).unwrap();
        let down = ProbeError {
            problem: protocol::wire::CameraProblem::Unreachable,
            message: "La caméra ne répond pas.".into(),
        };
        hub.record_camera_probe(id, kind, &target, Err(down));
        // Test du formulaire (mode réel, mais vers une adresse qui échoue vite : refus de la saisie).
        let _ = hub.test_camera_draft(&bad, None);
        hub.delete_camera(id).unwrap();

        let mut seen = drain(&mut sub);
        seen.push(refused.0);
        seen.push(serde_json::to_string(&hub.current_snapshot()).unwrap());
        seen.push(serde_json::to_string(&hub.cameras()).unwrap());
        seen.push(sub.snapshot.to_string());
        // Pendant que la caméra existe : l'état complet la montre aussi.
        let id2 = hub.add_camera(&cam_draft("Autre")).unwrap();
        seen.push(serde_json::to_string(&hub.current_snapshot()).unwrap());
        seen.push(hub.subscribe().snapshot.to_string());
        assert!(hub.current_snapshot().cameras.iter().any(|c| c.id == id2));
        seen.extend(logs.lock().unwrap().iter().cloned());
        for text in &seen {
            for needle in [CAM_SECRET, CAM_HOST, "rtsp://", "ch00_"] {
                assert!(!text.contains(needle), "{needle} dans : {text}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_demo_hub_has_fake_cameras_that_are_never_written() {
        let dir = temp("cam-demo");
        let (tx, _rx) = mpsc::unbounded_channel();
        let hub = Hub::new(HubConfig {
            data_dir: dir.clone(),
            demo: true,
            history_dir: dir.join("history-demo"),
            demo_history: None,
            broadcast_capacity: 8,
            commands: tx,
        });
        assert_eq!(hub.cameras().len(), 3);
        assert_eq!(hub.current_snapshot().cameras.len(), 3);
        let id = hub.add_camera(&cam_draft("Nouvelle")).unwrap();
        hub.delete_camera(id).unwrap();
        assert!(!dir.join(cameras::FILE_NAME).exists());
        // Le test du formulaire ne fait aucun trafic en démo.
        let results = hub.test_camera_draft(&cam_draft("x"), None).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.outcome.is_ok()));
        // Il refuse quand même une saisie invalide.
        assert!(hub.test_camera_draft(&cam_draft(""), None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
