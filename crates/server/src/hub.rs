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
    self, ApiError, CustomizeRequest, DeviceView, ErrorCode, HistoryPoint, HistoryQuery,
    HistoryResponse, ImportSummary, LatestReading, Metric, RuleRunView, ServerMessage, Snapshot,
    TextIds, WireEvent, WireMode, WireNode, WireStatus,
};

use crate::backup;
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
    /// Sérialise les modifications du plan et des règles (mémoire + disque + diffusion).
    write_lock: Mutex<()>,
    scrubber: Scrubber,
    data_dir: PathBuf,
    history_dir: PathBuf,
    demo: bool,
    demo_history: Mutex<Option<HistorySink>>,
    exit: Mutex<Option<ExitReason>>,
    stopping: AtomicBool,
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
        let (plan, rules) = if config.demo {
            (crate::demo::demo_plan(), crate::demo::demo_rules())
        } else {
            (
                load_document::<HomePlan>(&config.data_dir.join(PLAN_FILE), "plan"),
                load_document::<RuleSet>(&config.data_dir.join(RULES_FILE), "règles"),
            )
        };
        Self {
            state: Mutex::new(HubState {
                seq: 0,
                devices: BTreeMap::new(),
                plan,
                rules,
                runs: VecDeque::new(),
                pairing_busy: false,
            }),
            tx,
            commands: Mutex::new(Some(config.commands)),
            write_lock: Mutex::new(()),
            scrubber: Scrubber::new(&config.data_dir),
            data_dir: config.data_dir,
            history_dir: config.history_dir,
            demo: config.demo,
            demo_history: Mutex::new(config.demo_history),
            exit: Mutex::new(None),
            stopping: AtomicBool::new(false),
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
            self.aggregate(&mut st, event);
            if let Some(wire) = wire {
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

    fn aggregate(&self, st: &mut HubState, event: &Event) {
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
                    d.set_latest(Metric::from(*kind), *value, *at);
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
                    if d.set_latest(metric, f64::from(u8::from(*value)), *at)
                        && *kind == StateKind::Open
                        && changed
                    {
                        d.open_since = Some(*at);
                    }
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
        let truncated = points.len() > limit;
        if truncated {
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

    /// Valide une sauvegarde et la met de côté ; le serveur doit ensuite s'arrêter pour que
    /// son superviseur le relance et l'applique.
    pub fn stage_import(&self, text: &str) -> ApiResult<ImportSummary> {
        match backup::stage_import_text(&self.data_dir, text) {
            Ok(summary) => {
                log::warn!(
                    "import validé ({}) : arrêt demandé pour l'appliquer.",
                    summary.describe()
                );
                self.request_exit(ExitReason::ImportStaged);
                Ok(ImportSummary {
                    created: summary.created,
                    devices: summary.devices,
                    history_files: summary.history_files,
                    has_plan: summary.has_plan,
                    has_rules: summary.has_rules,
                })
            }
            Err(e) => {
                // Les messages de `backup` décrivent le fichier reçu ; on retire tout chemin.
                log::info!("import refusé : {e:#}");
                Err(ApiError::with_detail(
                    ErrorCode::InvalidImport,
                    self.scrubber.clean(&e.to_string()),
                ))
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
        assert_eq!(snap.seq, 6);
        hub.apply(&Event::NodeRemoved(7));
        assert!(hub.current_snapshot().devices.is_empty());
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
}
