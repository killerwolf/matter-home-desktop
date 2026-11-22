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
use protocol::mode::{HomeMode, ModeSource};
use protocol::wire::{
    self, ApiError, CustomizeRequest, DeviceView, ErrorCode, HistoryPoint, HistoryQuery,
    HistoryResponse, IdentityInfo, ImportIdentity, ImportSummary, LatestReading, Metric,
    RuleRunView, ServerMessage, Snapshot, TextIds, WireEvent, WireMode, WireNode, WireStatus,
};

mod cameras;
mod wifi;

pub use wifi::{People, WifiReport};

use crate::backup;
use crate::cameras::{CameraBook, ViewerRegistry, Wake};
use crate::events::{
    now_unix, Command, CoverMotion, Event, NodeInfo, NodeStatus, ReportingMode, StateKind,
};
use crate::export;
use crate::history_store::{HistorySink, HistoryStore};
use crate::home_mode;
use crate::persistence;
use crate::scrub::Scrubber;
use crate::security::user_tokens::{Change, InstallError, Kind, TokenInfo, UserTokens};
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
    /// Le mode global de la maison (voir `home_mode.rs`).
    mode: HomeMode,
}

/// Abonnement d'un client : l'état complet au moment de l'abonnement, puis les événements
/// suivants (de `seq` strictement supérieur).
pub struct Subscription {
    pub snapshot: Arc<str>,
    pub rx: broadcast::Receiver<Arc<str>>,
}

pub struct Hub {
    state: Mutex<HubState>,
    /// Les jetons de chaque personne (mode : raccourcis ; lecture : widget) : voir
    /// `security/user_tokens.rs`.
    tokens: UserTokens,
    /// Où chacun a dit être (raccourcis « Je pars » / « Je rentre ») : voir `presence.rs`. Rien n'en
    /// est déduit pour l'instant.
    presence: crate::presence::Presence,
    /// La box de la maison et les téléphones des comptes (présence par le Wi-Fi) : `wifi/`.
    wifi: crate::wifi::Wifi,
    /// Le journal d'audit (`serveur/audit/`) : voir `audit.rs`.
    audit: Arc<crate::audit::AuditLog>,
    /// Ce que chaque compte a le droit de faire : voir `rights.rs`.
    rights: crate::rights::Rights,
    /// Les raccourcis préparés pour l'iPhone (ils portent le jeton de mode de la personne), en
    /// mémoire.
    shortcut_downloads: crate::shortcuts::Downloads,
    tx: broadcast::Sender<Arc<str>>,
    commands: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    /// Sérialise les modifications du plan, des règles et des caméras (mémoire + disque + diffusion).
    write_lock: Mutex<()>,
    /// Réveille le surveillant des caméras (voir `cameras::start_monitor`) ; absent en démo et
    /// avant son démarrage.
    camera_wake: Mutex<Option<mpsc::UnboundedSender<Wake>>>,
    /// Spectateurs de chaque caméra (voir `hub/cameras.rs`).
    viewers: Arc<ViewerRegistry>,
    /// Les sessions du direct, une par caméra et par flux, partagées par les spectateurs, les
    /// vignettes et la détection (ticket n° 18).
    live: Arc<crate::cameras::live::LiveRegistry>,
    /// La détection : événements sur disque, état, réveil du superviseur (`detection/`).
    detection: Arc<crate::detection::Detection>,
    /// Un verrou par caméra : une seule connexion de vérification à la fois.
    camera_locks: Mutex<std::collections::HashMap<u64, Arc<Mutex<()>>>>,
    scrubber: Scrubber,
    /// Les notifications web push de l'app web (ticket n° 17, `push/`).
    push: Arc<crate::push::Push>,
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

/// Le droit de changer les jetons, sans attendre : une préparation en cours (signature des
/// raccourcis, quelques secondes) répond « occupé » au lieu de faire la queue.
fn try_change(store: &UserTokens) -> ApiResult<Change<'_>> {
    store.try_change().ok_or_else(|| {
        ApiError::with_detail(
            ErrorCode::Busy,
            "Une préparation (raccourcis ou widget) est déjà en cours : réessaie dans un instant.",
        )
    })
}

fn install_error(error: InstallError) -> ApiError {
    match error {
        InstallError::TooMany => ApiError::with_detail(
            ErrorCode::BadRequest,
            "Trop de raccourcis et de widgets sur ce compte : révoquez-en un d'abord.",
        ),
        InstallError::Storage => ApiError::new(ErrorCode::Internal),
    }
}

/// La requête qui a demandé la préparation est partie (délai, client fermé) : personne ne
/// recevrait le nouveau jeton, on ne remplace donc pas l'ancien.
fn abandoned() -> ApiError {
    ApiError::with_detail(
        ErrorCode::Unavailable,
        "Préparation abandonnée : rien n'a changé.",
    )
}

fn internal(context: &str, error: &anyhow::Error) -> ApiError {
    // Le détail complet va au journal, jamais au client.
    log::error!("{context} : {error:#}");
    ApiError::new(ErrorCode::Internal)
}

impl Hub {
    pub fn new(config: HubConfig) -> Self {
        let (tx, _) = broadcast::channel(config.broadcast_capacity.max(2));
        let (plan, rules, cameras, mode) = if config.demo {
            (
                crate::demo::demo_plan(),
                crate::demo::demo_rules(),
                CameraBook::demo(),
                HomeMode::default(),
            )
        } else {
            (
                load_plan(&config.data_dir.join(PLAN_FILE)),
                load_document::<RuleSet>(&config.data_dir.join(RULES_FILE), "règles"),
                CameraBook::new(crate::cameras::load(&config.data_dir)),
                home_mode::load(&config.data_dir),
            )
        };
        // Les jetons et la présence vivent dans `serveur/` (jamais exporté). Un dossier inutilisable
        // ne doit pas empêcher de démarrer : ils ne vivront alors qu'en mémoire.
        let server_dir = crate::security::server_dir(&config.data_dir);
        let tokens = match &server_dir {
            Ok(dir) => UserTokens::open(dir).unwrap_or_else(|e| {
                log::error!("jetons inutilisables, en mémoire seulement : {e:#}");
                UserTokens::in_memory()
            }),
            Err(e) => {
                log::error!("dossier des jetons inutilisable, en mémoire seulement : {e:#}");
                UserTokens::in_memory()
            }
        };
        let presence = match &server_dir {
            Ok(dir) => crate::presence::Presence::open(dir),
            Err(_) => crate::presence::Presence::in_memory(),
        };
        // La démonstration ne sort jamais de la boucle locale : sa « box » est fictive (sans réseau),
        // mais elle lit les mêmes fichiers (`box.json`, `telephones.json`) que le vrai serveur.
        let box_api = || -> Box<dyn crate::wifi::bbox::BoxApi> {
            if config.demo {
                Box::new(crate::wifi::DemoBox)
            } else {
                Box::new(crate::wifi::bbox::CurlBox::default())
            }
        };
        let wifi = match &server_dir {
            Ok(dir) => crate::wifi::Wifi::open(dir, box_api()),
            Err(_) => crate::wifi::Wifi::in_memory(box_api()),
        };
        let audit = match &server_dir {
            Ok(dir) => crate::audit::AuditLog::open(dir),
            Err(_) => crate::audit::AuditLog::disabled(),
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
                mode,
            }),
            tokens,
            presence,
            wifi,
            audit: Arc::new(audit),
            rights: crate::rights::Rights::new(),
            shortcut_downloads: Default::default(),
            tx,
            commands: Mutex::new(Some(config.commands)),
            write_lock: Mutex::new(()),
            camera_wake: Mutex::new(None),
            viewers: Arc::new(ViewerRegistry::default()),
            live: crate::cameras::live::LiveRegistry::new(Default::default()),
            detection: Arc::new(crate::detection::Detection::new(
                &config.data_dir,
                config.demo,
            )),
            camera_locks: Mutex::new(std::collections::HashMap::new()),
            scrubber: Scrubber::new(&config.data_dir),
            push: Arc::new(crate::push::Push::new(&config.data_dir, config.demo)),
            data_dir: config.data_dir,
            history_dir: config.history_dir,
            demo: config.demo,
            demo_history: Mutex::new(config.demo_history),
            exit: Mutex::new(None),
            stopping: AtomicBool::new(false),
            import_staged: AtomicBool::new(false),
        }
    }

    /// Les droits des comptes (V1 : tout le monde peut tout faire).
    pub fn rights(&self) -> &crate::rights::Rights {
        &self.rights
    }

    /// Le journal d'audit.
    pub fn audit(&self) -> &Arc<crate::audit::AuditLog> {
        &self.audit
    }

    /// Le nom d'un équipement, nettoyé, pour le journal d'audit (`None` : inconnu).
    pub fn device_name(&self, node_id: u64) -> Option<String> {
        let name = self
            .lock()
            .devices
            .get(&node_id)
            .map(|d| d.info.name.clone())?;
        Some(self.clean(&name))
    }

    /// Le registre des sessions du direct (voir `cameras/live.rs`).
    pub fn live(&self) -> &Arc<crate::cameras::live::LiveRegistry> {
        &self.live
    }

    /// La détection (ticket n° 18) : événements, état, réglages.
    pub fn detection(&self) -> &Arc<crate::detection::Detection> {
        &self.detection
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn clean(&self, text: &str) -> String {
        self.scrubber.clean(text)
    }

    /// Les notifications web push (ticket n° 17).
    pub fn push(&self) -> &Arc<crate::push::Push> {
        &self.push
    }

    /// Le nettoyage du texte d'une notification : les hôtes, identifiants et mots de passe des
    /// caméras masqués tels quels, puis adresses, identifiants de nœud et chemins (voir
    /// `push/content.rs`).
    pub fn push_cleaner(&self) -> impl Fn(&str) -> String + '_ {
        let secrets: Vec<String> = {
            let st = self.lock();
            st.cameras
                .cameras()
                .iter()
                .flat_map(|c| {
                    let hosts = [Some(&c.url), c.sub_url.as_ref()]
                        .into_iter()
                        .flatten()
                        .filter_map(|u| url::Url::parse(u).ok())
                        .filter_map(|u| u.host_str().map(str::to_string))
                        .collect::<Vec<_>>();
                    hosts
                        .into_iter()
                        .chain(c.username.clone())
                        .chain(c.password.clone())
                })
                .collect()
        };
        let sanitizer = crate::push::content::Sanitizer::new(secrets);
        move |text: &str| self.scrubber.clean(&sanitizer.clean(text))
    }

    /// Un passage détecté vient de se terminer (après `Descriptions::on_close`) : son message part
    /// maintenant, ou à l'arrivée de sa description si elle est attendue.
    pub fn push_detection_closed(&self, event: &protocol::wire::DetectionEventView) {
        let clean = self.push_cleaner();
        let descriptions = self.detection.descriptions();
        self.push
            .detection_closed(event, || descriptions.is_awaiting(&event.id), &clean);
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
        let (retained, name) = {
            let mut st = self.lock();
            // Le nom avant l'agrégation : un appareil oublié n'en a plus après.
            let name = event_node(event)
                .and_then(|id| st.devices.get(&id))
                .map(|d| d.info.name.clone());
            // Une mesure écartée (plus ancienne que la dernière retenue : historique rejoué au
            // démarrage) n'est pas diffusée non plus : le client ne verrait pas la valeur
            // reculer, alors que l'état complet, lui, ne la contient pas.
            let retained = self.aggregate(&mut st, event);
            if let (true, Some(wire)) = (retained, wire) {
                self.publish(&mut st, wire);
            }
            (retained, name)
        };
        // Notifications web push (ticket n° 17) : rien n'est envoyé ici, tout passe par la file.
        if let Event::Notification { title, message, .. } = event {
            let clean = self.push_cleaner();
            self.push.notify(
                crate::push::Category::Rules,
                crate::push::content::payload(
                    &clean,
                    title,
                    message,
                    crate::push::content::HOME_URL.into(),
                    None,
                ),
            );
        } else if crate::push::concerns(event) {
            let name = name.map(|n| self.push_cleaner()(&n));
            self.push.observe(event, name.as_deref(), retained);
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
            // Le hub le publie lui-même (`set_mode`) : un événement venu du backend ne change rien.
            Event::HomeModeChanged { .. }
            | Event::HomeModeRequested { .. }
            | Event::PairingProgress(_)
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
            cameras: st.cameras.views_in(Some(st.mode)),
            mode: Some(st.mode),
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

    /// Le mode global de la maison.
    pub fn mode(&self) -> HomeMode {
        self.lock().mode
    }

    /// Change le mode : l'enregistre (`mode.json`), le diffuse (`mode_changed`), prévient le moteur
    /// d'automatisations (règles « Quand le mode devient… ») ; les lectures de caméras que le
    /// nouveau mode éteint se ferment d'elles-mêmes (voir `camera_target`). `Ok(false)` : c'était
    /// déjà le mode actuel, rien n'est enregistré, diffusé ni déclenché. Si l'écriture échoue, le
    /// mode ne change pas (il commande les caméras : on ne l'annonce pas sans pouvoir le retenir).
    /// `source` : d'où vient la demande ; écrite au journal, diffusée avec `mode_changed` et
    /// donnée aux règles (`{source}`).
    pub fn set_mode(&self, mode: HomeMode, source: ModeSource) -> ApiResult<bool> {
        let cause = crate::audit::ModeCause::from_source(&source);
        self.set_mode_as(mode, source, cause)
    }

    /// Comme [`Hub::set_mode`], en disant au journal d'audit qui le change et de quelle façon. Une
    /// ligne n'y entre que si le mode change vraiment.
    pub fn set_mode_as(
        &self,
        mode: HomeMode,
        source: ModeSource,
        cause: crate::audit::ModeCause,
    ) -> ApiResult<bool> {
        let changed = self.change_mode(mode, source)?;
        if changed {
            let mut entry = crate::audit::Entry::new(crate::audit::Action::ModeChanged, &cause.who)
                .target(mode.key())
                .source(cause.origin);
            if let Some(detail) = &cause.detail {
                entry = entry.detail(detail);
            }
            self.audit.record(entry);
        }
        Ok(changed)
    }

    fn change_mode(&self, mode: HomeMode, source: ModeSource) -> ApiResult<bool> {
        self.change_mode_with(|_| Some(mode), source)
    }

    /// Comme [`Hub::set_mode_as`], mais le nouveau mode se décide **sous le verrou**, d'après le mode
    /// du moment (`decide` rend `None` : on ne change rien). Un choix fait à la main entre-temps
    /// n'est jamais écrasé par une décision prise sur un mode périmé.
    pub fn set_mode_if(
        &self,
        decide: impl FnOnce(HomeMode) -> Option<HomeMode>,
        source: ModeSource,
        cause: crate::audit::ModeCause,
    ) -> ApiResult<Option<HomeMode>> {
        let mut chosen = None;
        let changed = self.change_mode_with(
            |current| {
                chosen = decide(current);
                chosen
            },
            source,
        )?;
        let Some(mode) = chosen.filter(|_| changed) else {
            return Ok(None);
        };
        let mut entry = crate::audit::Entry::new(crate::audit::Action::ModeChanged, &cause.who)
            .target(mode.key())
            .source(cause.origin);
        if let Some(detail) = &cause.detail {
            entry = entry.detail(detail);
        }
        self.audit.record(entry);
        Ok(Some(mode))
    }

    fn change_mode_with(
        &self,
        decide: impl FnOnce(HomeMode) -> Option<HomeMode>,
        source: ModeSource,
    ) -> ApiResult<bool> {
        let _write = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mode) = decide(self.mode()) else {
            return Ok(false);
        };
        if self.mode() == mode {
            return Ok(false);
        }
        // L'écriture (fsync) se fait sans le verrou de l'état : les lectures des clients et du
        // moteur ne l'attendent pas. `write_lock` empêche toute autre modification du mode.
        if !self.demo {
            if let Err(e) = home_mode::save(&self.data_dir, mode) {
                return Err(internal("enregistrement du mode", &e));
            }
        }
        let mut st = self.lock();
        let previous = std::mem::replace(&mut st.mode, mode);
        self.publish(
            &mut st,
            WireEvent::ModeChanged {
                mode,
                source: Some(source.clone()),
            },
        );
        // Les caméras éteintes changent d'état affiché (`Off`) avec le mode.
        self.publish_cameras(&mut st);
        drop(st);
        log::info!(
            "mode : {} → {} ({})",
            previous.label(),
            mode.label(),
            source.label()
        );
        // Les caméras en Surveillance changent avec le mode : le superviseur revoit sa liste.
        self.detection.wake();
        // Le moteur peut avoir disparu (arrêt en cours) : le mode est enregistré, c'est l'essentiel.
        let _ = self.send(Command::SetHomeMode { mode, source });
        Ok(true)
    }

    /// Un nouveau jeton pour cette personne (celui de même type et de même libellé est remplacé,
    /// les autres restent), en clair : à montrer une seule fois. Ne fait jamais la queue : `busy` si
    /// une préparation est en cours (la fenêtre du serveur ne se fige pas).
    fn issue_token(&self, user_id: &str, kind: Kind, label: &str) -> ApiResult<String> {
        let change = try_change(&self.tokens)?;
        let token = Change::draw();
        change
            .install(user_id, kind, label, None, &token)
            .map_err(install_error)?;
        if kind == Kind::Mode {
            self.shortcut_downloads.clear(user_id);
        }
        Ok(token)
    }

    /// Un nouveau jeton de mode pour cette personne (voir [`Hub::issue_token`]). Les liens des
    /// raccourcis qu'elle avait préparés avant disparaissent.
    pub fn generate_mode_token(&self, user_id: &str, label: &str) -> ApiResult<String> {
        self.issue_token(user_id, Kind::Mode, label)
    }

    /// Un nouveau jeton de lecture pour cette personne (voir [`Hub::issue_token`]).
    pub fn generate_read_token(&self, user_id: &str, label: &str) -> ApiResult<String> {
        self.issue_token(user_id, Kind::Read, label)
    }

    /// Un nouveau jeton de mode et les raccourcis qui le portent, en un seul changement :
    /// `build` fabrique les fichiers (signés) avec le jeton tiré ; puis, **seulement si**
    /// `still_wanted()` (la requête n'a pas été abandonnée) et que tout a réussi, le jeton est
    /// enregistré (au nom de cette personne, pour ce libellé : celui du même téléphone est remplacé,
    /// jamais ceux des autres) et les fichiers remplacent ses liens d'avant. Sinon rien ne change.
    /// Rend les identifiants des liens, dans l'ordre des fichiers.
    pub fn prepare_mode_shortcuts(
        &self,
        user_id: &str,
        label: &str,
        installation_id: Option<&str>,
        build: impl FnOnce(&str) -> Result<Vec<(String, Vec<u8>)>, ApiError>,
        still_wanted: impl Fn() -> bool,
    ) -> ApiResult<Vec<String>> {
        let change = try_change(&self.tokens)?;
        let token = Change::draw();
        let files = build(&token)?;
        if !still_wanted() {
            return Err(abandoned());
        }
        change
            .install(user_id, Kind::Mode, label, installation_id, &token)
            .map_err(install_error)?;
        Ok(self.shortcut_downloads.replace(user_id, files))
    }

    /// Un nouveau jeton de lecture et ce qui le porte (le script du widget), comme
    /// `prepare_mode_shortcuts` : rien ne change si `build` échoue ou si la requête est abandonnée.
    pub fn prepare_widget<T>(
        &self,
        user_id: &str,
        label: &str,
        installation_id: Option<&str>,
        build: impl FnOnce(&str) -> Result<T, ApiError>,
        still_wanted: impl Fn() -> bool,
    ) -> ApiResult<T> {
        let change = try_change(&self.tokens)?;
        let token = Change::draw();
        let built = build(&token)?;
        if !still_wanted() {
            return Err(abandoned());
        }
        change
            .install(user_id, Kind::Read, label, installation_id, &token)
            .map_err(install_error)?;
        Ok(built)
    }

    /// Un raccourci préparé (lien encore valable), par identifiant et nom de fichier.
    pub fn shortcut_download(&self, id: &str, file_name: &str) -> Option<Vec<u8>> {
        self.shortcut_downloads.get(id, file_name)
    }

    /// Révoque un jeton de cette personne (`NotFound` : elle n'en a pas sous cet identifiant, y
    /// compris un jeton d'une autre personne). Les liens de raccourcis préparés sont oubliés.
    pub fn revoke_token(&self, user_id: &str, id: &str) -> ApiResult<()> {
        let change = try_change(&self.tokens)?;
        if !change.revoke(user_id, id).map_err(install_error)? {
            return Err(ApiError::with_detail(ErrorCode::NotFound, "jeton inconnu"));
        }
        self.shortcut_downloads.clear(user_id);
        Ok(())
    }

    /// Révoque les raccourcis et le widget préparés depuis une connexion qu'on vient de couper,
    /// et oublie les liens de téléchargement de ces personnes. `user_id` : limite à une personne
    /// (l'API) ; `None` : la fenêtre du serveur. Rend le nombre de jetons révoqués.
    ///
    /// Attend son tour (une signature en cours dure quelques secondes) : une coupure ne doit
    /// jamais laisser un jeton valable. À appeler hors d'une tâche asynchrone.
    pub fn revoke_installation_tokens(
        &self,
        user_id: Option<&str>,
        installation_id: &str,
    ) -> ApiResult<usize> {
        let change = self.tokens.change();
        let (users, n) = change
            .revoke_installation(user_id, installation_id)
            .map_err(install_error)?;
        for user in users {
            self.shortcut_downloads.clear(&user);
        }
        Ok(n)
    }

    /// Révoque tous les jetons de mode (raccourcis) ou tous ceux de lecture (widgets) de cette
    /// personne ; rend leur nombre.
    pub fn revoke_tokens_of(&self, user_id: &str, kind: Kind) -> ApiResult<usize> {
        let change = try_change(&self.tokens)?;
        let n = change.revoke_kind(user_id, kind).map_err(install_error)?;
        if kind == Kind::Mode {
            self.shortcut_downloads.clear(user_id);
        }
        Ok(n)
    }

    /// Les jetons de cette personne (jamais ceux des autres), le plus récent d'abord.
    pub fn tokens_of(&self, user_id: &str) -> Vec<TokenInfo> {
        self.tokens.list(user_id)
    }

    /// Existe-t-il un jeton de lecture (de n'importe qui) ? Sans widget installé, un refus n'est
    /// pas retardé.
    pub fn any_read_token(&self) -> bool {
        self.tokens.any(Kind::Read)
    }

    // -----------------------------------------------------------------------
    // La fenêtre du serveur (Ostal Server) : le code secret et le widget du premier compte, celui
    // de la personne qui a installé le Mac. Les autres comptes gèrent les leurs depuis l'app web.
    // -----------------------------------------------------------------------

    /// Le libellé du code secret créé depuis la fenêtre du serveur.
    const WINDOW_LABEL: &'static str = "Code secret (fenêtre du serveur)";

    fn first_user(&self) -> Option<String> {
        crate::security::account::first_user_id(&self.data_dir.join(crate::persistence::SERVER_DIR))
    }

    /// Les jetons de mode du premier compte qui sont « le code secret » de la fenêtre : celui
    /// qu'elle a créé, ou le jeton de mode repris de la 2.0.0 (qui pouvait venir d'elle).
    fn window_mode_tokens(&self) -> Vec<TokenInfo> {
        let Some(first) = self.first_user() else {
            return Vec::new();
        };
        self.tokens
            .list(&first)
            .into_iter()
            .filter(|t| {
                t.kind == Kind::Mode
                    && (t.label == Self::WINDOW_LABEL
                        || t.label == crate::security::user_tokens::LEGACY_MODE_LABEL)
            })
            .collect()
    }

    /// La fenêtre du serveur a-t-elle un code secret ?
    pub fn window_has_mode_token(&self) -> bool {
        !self.window_mode_tokens().is_empty()
    }

    /// Un nouveau code secret pour le premier compte, en clair : à montrer une seule fois. Le
    /// précédent (celui de la fenêtre, ou repris de la 2.0.0) est révoqué.
    pub fn window_generate_mode_token(&self) -> ApiResult<String> {
        let Some(first) = self.first_user() else {
            return Err(ApiError::with_detail(
                ErrorCode::Unavailable,
                "Aucun compte : créez-en un d'abord.",
            ));
        };
        let old = self.window_mode_tokens();
        let change = try_change(&self.tokens)?;
        let token = Change::draw();
        change
            .install(&first, Kind::Mode, Self::WINDOW_LABEL, None, &token)
            .map_err(install_error)?;
        for t in old.iter().filter(|t| t.label != Self::WINDOW_LABEL) {
            change.revoke(&first, &t.id).map_err(install_error)?;
        }
        self.shortcut_downloads.clear(&first);
        Ok(token)
    }

    /// Supprime le code secret de la fenêtre (plus aucun accès par ce code).
    pub fn window_revoke_mode_token(&self) -> ApiResult<()> {
        let old = self.window_mode_tokens();
        let change = try_change(&self.tokens)?;
        for t in old {
            change.revoke(&t.user_id, &t.id).map_err(install_error)?;
            self.shortcut_downloads.clear(&t.user_id);
        }
        Ok(())
    }

    /// Le premier compte a-t-il un widget (jeton de lecture) ?
    pub fn window_has_read_token(&self) -> bool {
        self.first_user().is_some_and(|first| {
            self.tokens
                .list(&first)
                .iter()
                .any(|t| t.kind == Kind::Read)
        })
    }

    /// Révoque les widgets du premier compte (ceux des autres comptes ne bougent pas).
    pub fn window_revoke_read_tokens(&self) -> ApiResult<()> {
        if let Some(first) = self.first_user() {
            self.revoke_tokens_of(&first, Kind::Read)?;
        }
        Ok(())
    }

    /// À qui est ce jeton de lecture ? `None` : inconnu ou révoqué.
    pub fn verify_read_token(&self, token: &str) -> Option<TokenInfo> {
        self.tokens.verify(Kind::Read, token)
    }

    /// À qui est ce jeton de mode ? `None` : inconnu ou révoqué.
    pub fn verify_mode_token(&self, token: &str) -> Option<TokenInfo> {
        self.tokens.verify(Kind::Mode, token)
    }

    /// Enregistre où cette personne dit être. `true` : cela change son état. Le mode de la maison
    /// n'en change pas : le serveur garde seulement ce que chacun a déclaré.
    pub fn declare_presence(
        &self,
        user_id: &str,
        state: crate::presence::Whereabouts,
        source: crate::presence::Source,
    ) -> ApiResult<bool> {
        self.presence
            .declare(user_id, state, source)
            .map_err(|e| internal("présence", &e))
    }

    /// Après la déclaration de `user_id` par un raccourci : met la maison dans le mode que la règle
    /// de [`crate::presence::auto_mode`] en tire, ou la laisse telle quelle. **Rien ne change** tant
    /// que l'interrupteur « Changer le mode de la maison selon la présence » est éteint, ou
    /// qu'aucun téléphone n'est relié à un compte actif (la règle est la même que pour le Wi-Fi).
    /// `is_active` : le compte existe-t-il et est-il actif ? (Une personne désactivée ne compte pas
    /// comme « à la maison ».) Rend le mode final et ce qui l'a retenu, pour le dire à la personne.
    pub fn follow_presence(
        &self,
        user_id: &str,
        state: crate::presence::Whereabouts,
        is_active: impl Fn(&str) -> bool,
        who: &crate::audit::Who,
    ) -> ApiResult<crate::presence::Followed> {
        use crate::presence::{auto_mode, Movement, Whereabouts};
        let declared = self.presence.all();
        let others_home = crate::presence::others_at_home(user_id, &declared, &is_active);
        let auto =
            self.wifi.auto_mode() && self.wifi.phones().iter().any(|p| is_active(&p.user_id));
        let movement = match state {
            Whereabouts::Home => Some(Movement::Arrival),
            Whereabouts::Away if others_home.is_empty() => Some(Movement::LastDeparture),
            Whereabouts::Away => None,
        };
        // Box injoignable ou mot de passe refusé : « qui est là » vient de relevés peut-être périmés
        // (quelqu'un a pu rentrer sans que la box le voie). On n'arme jamais la maison sur eux : le
        // mode est figé. Une arrivée, elle, ne fait que désarmer.
        let box_down = auto
            && movement == Some(Movement::LastDeparture)
            && self.wifi.status().link != crate::wifi::Link::Ok;
        let changed = match movement {
            _ if box_down => false,
            // Le mode change tout seul, par la règle de présence : le journal l'attribue à celui
            // qui a dit partir ou rentrer, d'où l'origine « automatique ».
            Some(movement) if auto => self
                .set_mode_if(
                    |current| auto_mode(movement, current),
                    ModeSource::Shortcut,
                    crate::audit::ModeCause {
                        who: who.clone(),
                        origin: crate::audit::Origin::Automatic,
                        detail: None,
                    },
                )?
                .is_some(),
            _ => false,
        };
        Ok(crate::presence::Followed {
            mode: self.mode(),
            changed,
            others_home,
            auto,
            box_down,
        })
    }

    /// Ce que chacun a déclaré en dernier.
    pub fn declared_presence(&self) -> Vec<crate::presence::Declared> {
        self.presence.all()
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
        self.replace_document(Document::Plan, move |st| {
            // Un client plus ancien efface `opening` sans le savoir : on garde le réglage connu.
            let mut plan = plan;
            plan.keep_openings_from(&st.plan);
            st.plan = plan;
        })
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

    /// Sauvegarde `.ostal` complète : contient les clés privées en clair.
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

/// L'appareil d'un événement (pour le nom dans une notification).
fn event_node(event: &Event) -> Option<u64> {
    match event {
        Event::Measurement { node_id, .. }
        | Event::State { node_id, .. }
        | Event::Status { node_id, .. } => Some(*node_id),
        Event::NodeRemoved(id) => Some(*id),
        _ => None,
    }
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
pub(crate) mod tests {
    use super::*;
    use crate::events::{MeasureKind, NodeStatus};

    pub(crate) fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-hub-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub(crate) fn hub_with(dir: &Path, capacity: usize) -> (Hub, mpsc::UnboundedReceiver<Command>) {
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
            assert!(!dir.join("import-en-attente.ostal").exists());
        }
        assert!(hub.exit_reason().is_none());

        hub.stage_import(&text, false).unwrap();
        assert_eq!(hub.exit_reason(), Some(ExitReason::ImportStaged));
        let pending = std::fs::read(dir.join("import-en-attente.ostal")).unwrap();
        let second = hub.stage_import(&text, false).unwrap_err();
        assert_eq!(second.code, ErrorCode::ImportPending);
        assert_eq!(second.code.http_status(), 409);
        // Le premier dépôt n'a pas été touché.
        assert_eq!(
            std::fs::read(dir.join("import-en-attente.ostal")).unwrap(),
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
        assert!(!dir.join("import-en-attente.ostal").exists());
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

    /// Chaque vrai changement de mode laisse une ligne au journal d'audit, selon sa source ; un mode
    /// inchangé n'en laisse aucune.
    #[test]
    fn a_mode_change_is_audited_by_its_origin_and_an_unchanged_mode_is_not() {
        use crate::audit::{Action, Filter, Origin};
        let dir = temp("mode-audit");
        let (hub, _commands) = hub_with(&dir, 16);
        let rule = ModeSource::Rule {
            name: "Nuit à 23 h".into(),
        };
        assert!(hub.set_mode(HomeMode::Nuit, rule).unwrap());
        assert!(hub
            .set_mode(HomeMode::Absent, ModeSource::ServerApp)
            .unwrap());
        assert!(!hub
            .set_mode(HomeMode::Absent, ModeSource::Shortcut)
            .unwrap());
        assert!(hub.set_mode(HomeMode::Present, ModeSource::Web).unwrap());
        let entries = hub.audit().read(&Filter::default());
        assert!(entries.iter().all(|e| e.action == Action::ModeChanged));
        let seen: Vec<(Option<&str>, Option<Origin>, Option<&str>)> = entries
            .iter()
            .rev()
            .map(|e| (e.target.as_deref(), e.source, e.detail.as_deref()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (Some("nuit"), Some(Origin::Automation), Some("Nuit à 23 h")),
                (Some("absent"), Some(Origin::Person), Some("Ostal Server")),
                (Some("present"), Some(Origin::Person), Some("l'app web")),
            ]
        );
        // La règle n'a pas de personne ; la fenêtre du serveur est une connexion « serveur ».
        let oldest = entries.last().unwrap();
        assert!(oldest.user.is_none() && oldest.connection.is_none());
        assert_eq!(entries[1].connection.as_ref().unwrap().kind, "server");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_mode_is_saved_broadcast_and_announced_once_per_change() {
        let dir = temp("mode");
        let (hub, mut commands) = hub_with(&dir, 16);
        let mut sub = hub.subscribe();
        assert_eq!(hub.mode(), HomeMode::Present);
        assert_eq!(hub.current_snapshot().mode, Some(HomeMode::Present));

        // La fenêtre du serveur (sous-menu « Mode ») : sa source suit le changement partout.
        assert!(hub
            .set_mode(HomeMode::Absent, ModeSource::ServerApp)
            .unwrap());
        // Le même mode redemandé (Raccourcis, à l'arrivée) : rien n'est écrit, diffusé ni lancé.
        assert!(!hub
            .set_mode(HomeMode::Absent, ModeSource::Shortcut)
            .unwrap());
        assert_eq!(hub.current_snapshot().mode, Some(HomeMode::Absent));
        assert_eq!(home_mode::load(&dir), HomeMode::Absent);
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::SetHomeMode {
                mode: HomeMode::Absent,
                source: ModeSource::ServerApp
            })
        ));
        assert!(commands.try_recv().is_err());
        let mut changes = 0;
        while let Ok(m) = sub.rx.try_recv() {
            let ServerMessage::Event { event, .. } = serde_json::from_str(&m).unwrap() else {
                panic!()
            };
            if let WireEvent::ModeChanged { mode, source } = event {
                assert_eq!(mode, HomeMode::Absent);
                assert_eq!(source, Some(ModeSource::ServerApp));
                changes += 1;
            }
        }
        assert_eq!(changes, 1);

        // Au redémarrage, le mode enregistré revient (le moteur le reçoit par `automation::spawn`).
        drop(hub);
        let (again, mut commands) = hub_with(&dir, 16);
        assert_eq!(again.mode(), HomeMode::Absent);
        assert!(
            commands.try_recv().is_err(),
            "rien n'est envoyé au démarrage"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mode_tokens_are_personal_replaced_per_phone_and_revoked_one_by_one() {
        let dir = temp("mode-token");
        let (hub, _commands) = hub_with(&dir, 16);
        assert!(hub.verify_mode_token("rien").is_none());
        let first = hub.generate_mode_token("lea", "iPhone").unwrap();
        assert_eq!(hub.verify_mode_token(&first).unwrap().user_id, "lea");
        // Le même téléphone qui réinstalle remplace son jeton ; un autre téléphone, une autre
        // personne : rien ne bouge.
        let ipad = hub.generate_mode_token("lea", "iPad").unwrap();
        let adam = hub.generate_mode_token("adam", "iPhone").unwrap();
        let second = hub.generate_mode_token("lea", "iPhone").unwrap();
        assert!(hub.verify_mode_token(&first).is_none());
        assert!(hub.verify_mode_token(&second).is_some());
        assert!(hub.verify_mode_token(&ipad).is_some() && hub.verify_mode_token(&adam).is_some());
        // Les jetons survivent au redémarrage (leur empreinte est dans `serveur/`).
        drop(hub);
        let (again, _commands) = hub_with(&dir, 16);
        assert!(again.verify_mode_token(&second).is_some());
        // Révoquer un jeton ne touche ni les autres de la personne ni ceux des autres ; on ne
        // révoque pas celui de quelqu'un d'autre.
        let id = again.verify_mode_token(&second).unwrap().id;
        assert_eq!(
            again.revoke_token("adam", &id).unwrap_err().code,
            ErrorCode::NotFound
        );
        again.revoke_token("lea", &id).unwrap();
        assert!(again.verify_mode_token(&second).is_none());
        assert!(
            again.verify_mode_token(&ipad).is_some() && again.verify_mode_token(&adam).is_some()
        );
        assert_eq!(again.revoke_tokens_of("lea", Kind::Mode).unwrap(), 1);
        assert!(
            again.verify_mode_token(&ipad).is_none() && again.verify_mode_token(&adam).is_some()
        );
        // Le jeton de lecture vit à part : aucun ne vaut pour l'autre.
        let read = again.generate_read_token("lea", "iPhone").unwrap();
        let mode = again.generate_mode_token("lea", "iPhone").unwrap();
        assert!(
            again.verify_read_token(&read).is_some() && again.verify_mode_token(&read).is_none()
        );
        assert!(again.verify_read_token(&mode).is_none());
        // Une préparation refusée (signature) ou abandonnée (requête partie) garde l'ancien jeton
        // et ne laisse aucun lien.
        let refused = again
            .prepare_mode_shortcuts(
                "lea",
                "iPhone",
                None,
                |_| Err(ApiError::with_detail(ErrorCode::Unavailable, "signature")),
                || true,
            )
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Unavailable);
        assert!(again.verify_mode_token(&mode).is_some());
        let mut drawn = String::new();
        let gone = again
            .prepare_mode_shortcuts(
                "lea",
                "iPhone",
                None,
                |token| {
                    drawn = token.to_string();
                    Ok(vec![("x".into(), token.as_bytes().to_vec())])
                },
                || false,
            )
            .unwrap_err();
        assert!(gone.detail.unwrap().contains("abandonnée"));
        assert!(
            again.verify_mode_token(&mode).is_some() && again.verify_mode_token(&drawn).is_none()
        );
        // Réussie : le jeton de ce téléphone change, le lien porte le nouveau ; un nouveau jeton
        // ou une révocation fait disparaître les liens de cette personne, pas ceux des autres.
        let ids = again
            .prepare_mode_shortcuts(
                "lea",
                "iPhone",
                None,
                |t| Ok(vec![("x".into(), t.as_bytes().to_vec())]),
                || true,
            )
            .unwrap();
        let others = again
            .prepare_mode_shortcuts(
                "adam",
                "iPhone",
                None,
                |t| Ok(vec![("y".into(), t.as_bytes().to_vec())]),
                || true,
            )
            .unwrap();
        assert!(
            again.shortcut_download(&ids[0], "x").is_some(),
            "ceux de Léa restent"
        );
        let carried = String::from_utf8(again.shortcut_download(&ids[0], "x").unwrap()).unwrap();
        assert_eq!(again.verify_mode_token(&carried).unwrap().user_id, "lea");
        assert!(again.verify_mode_token(&mode).is_none());
        again.generate_mode_token("lea", "iPad").unwrap();
        assert!(again.shortcut_download(&ids[0], "x").is_none());
        assert!(again.shortcut_download(&others[0], "y").is_some());
        // Pendant une préparation, tout autre changement répond « occupé » tout de suite (la
        // fenêtre du serveur ne se fige pas), la vérification continue.
        let again = std::sync::Arc::new(again);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let worker = {
            let hub = again.clone();
            std::thread::spawn(move || {
                hub.prepare_mode_shortcuts(
                    "lea",
                    "iPhone",
                    None,
                    |_| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(vec![])
                    },
                    || true,
                )
            })
        };
        entered_rx.recv().unwrap();
        let some_id = again.tokens_of("adam")[0].id.clone();
        for busy in [
            again.generate_mode_token("adam", "x").map(|_| ()),
            again.revoke_token("adam", &some_id),
            again
                .prepare_mode_shortcuts("adam", "x", None, |_| Ok(vec![]), || true)
                .map(|_| ()),
        ] {
            assert_eq!(busy.unwrap_err().code, ErrorCode::Busy);
        }
        assert!(
            again.verify_read_token(&read).is_some(),
            "la vérification n'attend pas"
        );
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap().is_ok());
        let again = std::sync::Arc::try_unwrap(again).ok().unwrap();
        let script = again
            .prepare_widget("lea", "iPhone", None, |t| Ok(format!("TOKEN={t}")), || true)
            .unwrap();
        // Le même téléphone : l'ancien jeton de lecture est remplacé.
        assert!(again.verify_read_token(&read).is_none());
        assert!(again
            .verify_read_token(script.trim_start_matches("TOKEN="))
            .is_some());
        assert!(again
            .prepare_widget("lea", "iPhone", None, |_| Ok(()), || false)
            .is_err());
        assert!(again
            .verify_read_token(script.trim_start_matches("TOKEN="))
            .is_some());
        let read = script.trim_start_matches("TOKEN=").to_string();
        assert_eq!(again.revoke_tokens_of("lea", Kind::Read).unwrap(), 1);
        assert!(!again.any_read_token() && again.verify_read_token(&read).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_server_window_manages_the_first_accounts_secret_code_and_widget_only() {
        use sha2::Digest;
        let dir = temp("window-token");
        let server = dir.join(crate::persistence::SERVER_DIR);
        std::fs::create_dir_all(&server).unwrap();
        // Sans compte : pas de code à créer.
        let (hub, _commands) = hub_with(&dir, 16);
        assert_eq!(
            hub.window_generate_mode_token().unwrap_err().code,
            ErrorCode::Unavailable
        );
        drop(hub);
        // Un compte de la 2.0.0 et son jeton de mode : c'est le code secret de la fenêtre.
        std::fs::write(
            server.join("compte.json"),
            r#"{"version":1,"id":"premier","username":"hamdi","password_hash":"x","created":1.0}"#,
        )
        .unwrap();
        let old = "dd44".repeat(16);
        std::fs::write(
            server.join("jeton-mode.json"),
            format!(
                r#"{{"hash":"{}","created":5.0}}"#,
                hex::encode(sha2::Sha256::digest(old.as_bytes()))
            ),
        )
        .unwrap();
        let (hub, _commands) = hub_with(&dir, 16);
        assert!(hub.window_has_mode_token());
        assert!(!hub.window_has_read_token());
        let other = hub.generate_mode_token("autre", "iPhone").unwrap();
        // Un nouveau code remplace l'ancien (repris de la 2.0.0), sans toucher aux autres comptes.
        let new = hub.window_generate_mode_token().unwrap();
        assert!(hub.verify_mode_token(&old).is_none());
        assert_eq!(hub.verify_mode_token(&new).unwrap().user_id, "premier");
        assert!(hub.verify_mode_token(&other).is_some());
        // Un raccourci du premier compte (autre libellé) n'est pas « le code secret ».
        let phone = hub.generate_mode_token("premier", "iPhone").unwrap();
        hub.window_revoke_mode_token().unwrap();
        assert!(!hub.window_has_mode_token());
        assert!(hub.verify_mode_token(&new).is_none());
        assert!(hub.verify_mode_token(&phone).is_some() && hub.verify_mode_token(&other).is_some());
        // Le widget : seuls ceux du premier compte.
        hub.generate_read_token("premier", "iPhone").unwrap();
        let theirs = hub.generate_read_token("autre", "iPhone").unwrap();
        assert!(hub.window_has_read_token());
        hub.window_revoke_read_tokens().unwrap();
        assert!(!hub.window_has_read_token());
        assert!(hub.verify_read_token(&theirs).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn declared_presence_is_kept_per_person_without_touching_the_mode() {
        use crate::presence::{Source, Whereabouts};
        let dir = temp("presence");
        let (hub, mut commands) = hub_with(&dir, 16);
        assert!(hub.declared_presence().is_empty());
        assert!(hub
            .declare_presence("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        assert!(!hub
            .declare_presence("lea", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        assert!(hub
            .declare_presence("adam", Whereabouts::Home, Source::Shortcut)
            .unwrap());
        // Même quand tout le monde est parti, le mode ne bouge pas (c'est l'étape suivante).
        assert!(hub
            .declare_presence("adam", Whereabouts::Away, Source::Shortcut)
            .unwrap());
        assert_eq!(hub.mode(), HomeMode::Present);
        assert!(commands.try_recv().is_err());
        drop(hub);
        let (again, _commands) = hub_with(&dir, 16);
        assert_eq!(again.declared_presence().len(), 2);
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
                opening: None,
            },
            x: 1.0,
            y: 1.0,
            rotation: 0.0,
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
}
