//! Partie réseau / Matter. Tourne entièrement dans un runtime tokio sur un thread
//! dédié ; l'interface lui parle uniquement par canaux (voir `events`).

pub mod certs;
pub mod convert;
pub mod discovery;
mod node;
pub mod setup_code;
pub mod tlv;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use matc::certmanager::CertManager;
use matc::controller::{Connection, Controller};
use matc::transport::Transport;
use rand::Rng;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::automation::{self, Actuator, BoxFuture, CoverCommand, OnOffCommand};
use crate::events::{BridgeLink, Capabilities, Command, Event, EventSink, NodeInfo};
use crate::history_store::{self, HistorySink, HistoryStore};
use crate::home::Symbol;
use crate::persistence::{
    self, StoredBridgedDevice, StoredNode, StoredState, TrackedEndpoints, BRIDGED_ID_MAX,
    BRIDGED_ID_MIN, INTERVIEW_VERSION,
};
use discovery::Discovery;

/// Socket UDP local : toutes les interfaces, IPv6 (double pile), port éphémère.
/// Les appareils Thread ne sont joignables qu'en IPv6.
const LOCAL_ADDRESS: &str = "[::]:0";
const PAIRING_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(40);
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// État partagé par toutes les tâches de fond.
pub(crate) struct Ctx {
    pub controller: Arc<Controller>,
    pub transport: Arc<Transport>,
    pub discovery: Arc<Discovery>,
    pub controller_node_id: u64,
    /// Identifiant de fabric compressé (hex majuscules) pour les noms mDNS opérationnels.
    pub compressed_fabric: String,
    pub events: EventSink,
    pub history: HistorySink,
    /// Sessions CASE ouvertes, partagées avec les automatisations.
    connections: Mutex<HashMap<u64, Arc<Connection>>>,
    /// Appareils que leur pont déclare injoignables (attribut Reachable).
    bridge_unreachable: Mutex<HashSet<u64>>,
    state: Mutex<StoredState>,
    state_path: PathBuf,
}

/// Résultat de la lecture de structure d'un nœud (voir `node::interview`).
pub(crate) struct Interview {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub device_types: Vec<String>,
    pub endpoints: TrackedEndpoints,
    /// `device_id` à 0 : attribué ici, ou repris de l'ancienne lecture (même endpoint).
    pub bridged: Vec<StoredBridgedDevice>,
}

/// Un appareil résolu : le nœud qui le porte et ses endpoints suivis.
pub(crate) struct Resolved {
    pub node_id: u64,
    pub name: String,
    pub endpoints: TrackedEndpoints,
}

impl Ctx {
    pub fn node(&self, node_id: u64) -> Option<StoredNode> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.nodes.iter().find(|n| n.node_id == node_id).cloned()
    }

    /// Modifie un nœud puis sauvegarde le fichier JSON.
    pub fn update_node(&self, node_id: u64, f: impl FnOnce(&mut StoredNode)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = state.nodes.iter_mut().find(|n| n.node_id == node_id) {
            let before = n.clone();
            f(n);
            if *n != before {
                if let Err(e) = persistence::save(&self.state_path, &state) {
                    log::error!("sauvegarde impossible : {e:#}");
                }
            }
        }
    }

    /// Nœud et appareils qu'il expose, le nœud en premier.
    pub fn device_ids(&self, node_id: u64) -> Vec<u64> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids = vec![node_id];
        if let Some(n) = state.nodes.iter().find(|n| n.node_id == node_id) {
            ids.extend(n.bridged.iter().map(|b| b.device_id));
        }
        ids
    }

    /// Nœud qui porte cet appareil : lui-même, ou son pont.
    pub fn parent_node(&self, device_id: u64) -> Option<u64> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .nodes
            .iter()
            .find(|n| n.node_id == device_id || n.bridged(device_id).is_some())
            .map(|n| n.node_id)
    }

    pub fn resolve(&self, device_id: u64) -> Option<Resolved> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for n in &state.nodes {
            if n.node_id == device_id {
                return Some(Resolved {
                    node_id: n.node_id,
                    name: n.custom_name.clone().unwrap_or_else(|| n.name.clone()),
                    endpoints: n.endpoints.clone(),
                });
            }
            if let Some(b) = n.bridged(device_id) {
                return Some(Resolved {
                    node_id: n.node_id,
                    name: b.custom_name.clone().unwrap_or_else(|| b.name.clone()),
                    endpoints: b.endpoints.clone(),
                });
            }
        }
        None
    }

    /// Applique une lecture de structure. Les appareils pontés gardent leur identifiant,
    /// leur nom et leur icône choisis d'une lecture à l'autre (même endpoint) ; ceux qui
    /// ont disparu sont retirés et leurs identifiants retournés.
    pub fn set_interview(&self, node_id: u64, iv: Interview) -> Vec<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut taken: HashSet<u64> = state.device_ids().into_iter().collect();
        let Some(n) = state.nodes.iter_mut().find(|n| n.node_id == node_id) else {
            return Vec::new();
        };
        let mut bridged = iv.bridged;
        for b in &mut bridged {
            if let Some(old) = n.bridged.iter().find(|o| o.endpoint == b.endpoint) {
                b.device_id = old.device_id;
                b.custom_name = old.custom_name.clone();
                b.icon = old.icon;
            } else {
                let mut rng = rand::thread_rng();
                loop {
                    let id = rng.gen_range(BRIDGED_ID_MIN..=BRIDGED_ID_MAX);
                    if taken.insert(id) {
                        b.device_id = id;
                        break;
                    }
                }
            }
        }
        let removed: Vec<u64> = n
            .bridged
            .iter()
            .filter(|o| !bridged.iter().any(|b| b.endpoint == o.endpoint))
            .map(|o| o.device_id)
            .collect();
        if let Some(name) = iv.name {
            n.name = name;
        }
        n.vendor = iv.vendor;
        n.product = iv.product;
        n.device_types = iv.device_types;
        n.endpoints = iv.endpoints;
        n.bridged = bridged;
        n.interviewed = true;
        n.interview_version = INTERVIEW_VERSION;
        if let Err(e) = persistence::save(&self.state_path, &state) {
            log::error!("sauvegarde impossible : {e:#}");
        }
        removed
    }

    /// Nom et icône choisis pour un appareil (nœud ou ponté) ; retourne sa fiche à jour.
    pub fn customize(
        &self,
        device_id: u64,
        name: Option<String>,
        icon: Option<Symbol>,
    ) -> Option<NodeInfo> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut info = None;
        for n in &mut state.nodes {
            if n.node_id == device_id {
                n.custom_name = name.clone();
                n.icon = icon;
                info = Some(node_info(n));
                break;
            }
            let link = n.node_id;
            if let Some(b) = n.bridged_mut(device_id) {
                b.custom_name = name.clone();
                b.icon = icon;
                info = Some(bridged_info(link, b));
                break;
            }
        }
        if info.is_some() {
            if let Err(e) = persistence::save(&self.state_path, &state) {
                log::error!("sauvegarde impossible : {e:#}");
            }
        }
        info
    }

    fn add_node(&self, node: StoredNode) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.nodes.retain(|n| n.node_id != node.node_id);
        state.nodes.push(node);
        persistence::save(&self.state_path, &state)
    }

    fn remove_node(&self, node_id: u64) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.nodes.retain(|n| n.node_id != node_id);
        persistence::save(&self.state_path, &state)
    }

    pub fn set_connection(&self, node_id: u64, conn: Option<Arc<Connection>>) {
        let mut map = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        match conn {
            Some(c) => map.insert(node_id, c),
            None => map.remove(&node_id),
        };
    }

    fn connection(&self, node_id: u64) -> Option<Arc<Connection>> {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&node_id)
            .cloned()
    }

    pub fn set_bridge_reachable(&self, device_id: u64, reachable: bool) {
        let mut set = self
            .bridge_unreachable
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if reachable {
            set.remove(&device_id);
        } else {
            set.insert(device_id);
        }
    }

    pub fn is_bridge_unreachable(&self, device_id: u64) -> bool {
        self.bridge_unreachable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&device_id)
    }

    pub fn instance_name(&self, node_id: u64) -> String {
        format!("{}-{:016X}", self.compressed_fabric, node_id)
    }
}

fn caps_of(e: &TrackedEndpoints) -> Capabilities {
    Capabilities {
        temperature: e.temperature_endpoint.is_some(),
        humidity: e.humidity_endpoint.is_some(),
        contact: e.contact_endpoint.is_some(),
        battery: e.battery_endpoint.is_some(),
        power: e.power_endpoint.is_some(),
        energy: e.energy_endpoint.is_some(),
        on_off: e.on_off_endpoint.is_some(),
        cover: e.cover_endpoint.is_some(),
        cover_position: e.cover_endpoint.is_some() && e.cover_position,
    }
}

pub fn node_info(n: &StoredNode) -> NodeInfo {
    NodeInfo {
        node_id: n.node_id,
        name: n.custom_name.clone().unwrap_or_else(|| n.name.clone()),
        device_name: n.name.clone(),
        custom_name: n.custom_name.clone(),
        icon: n.icon,
        vendor: n.vendor.clone(),
        product: n.product.clone(),
        device_types: n.device_types.clone(),
        caps: caps_of(&n.endpoints),
        bridge: None,
    }
}

/// Fiche d'un appareil exposé par le pont `bridge_node`.
pub fn bridged_info(bridge_node: u64, b: &StoredBridgedDevice) -> NodeInfo {
    NodeInfo {
        node_id: b.device_id,
        name: b.custom_name.clone().unwrap_or_else(|| b.name.clone()),
        device_name: b.name.clone(),
        custom_name: b.custom_name.clone(),
        icon: b.icon,
        vendor: b.vendor.clone(),
        product: b.product.clone(),
        device_types: b.device_types.clone(),
        caps: caps_of(&b.endpoints),
        bridge: Some(BridgeLink {
            node_id: bridge_node,
            endpoint: b.endpoint,
        }),
    }
}

/// Envoie la fiche du nœud, puis celles des appareils qu'il expose.
pub(crate) fn announce(ctx: &Ctx, n: &StoredNode) {
    ctx.events.send(Event::Node(node_info(n)));
    for b in &n.bridged {
        ctx.events.send(Event::Node(bridged_info(n.node_id, b)));
    }
}

/// Démarre la tâche de fond sur son propre thread et retourne le canal de commandes.
pub fn spawn(events: EventSink) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("matter-backend".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    events.send(Event::Fatal(format!("runtime tokio : {e}")));
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = run(rx, events.clone()).await {
                    events.send(Event::Fatal(format!("{e:#}")));
                }
            });
        })
        .expect("thread backend");
    tx
}

struct NodeHandle {
    task: JoinHandle<()>,
    wake: Arc<Notify>,
}

/// Messages internes (tâches de fond → boucle principale).
enum Internal {
    Paired(u64, Connection),
}

async fn run(mut commands: mpsc::UnboundedReceiver<Command>, ui: EventSink) -> Result<()> {
    // Tous les événements du backend passent aussi par le moteur d'automatisations.
    let (tap_tx, tap_rx) = mpsc::unbounded_channel();
    let events = ui.with_tap(tap_tx);
    let state_path = persistence::state_path();
    let state = match persistence::load(&state_path)? {
        Some(s) => s,
        None => {
            let s = StoredState {
                version: 1,
                identity: certs::generate_identity()?,
                nodes: Vec::new(),
            };
            persistence::save(&state_path, &s)?;
            events.send(Event::Info(format!(
                "Nouvelle fabric créée (ID {:#x}).",
                s.identity.fabric_id
            )));
            s
        }
    };

    let cm: Arc<dyn CertManager> =
        Arc::new(certs::JsonCertManager::from_identity(&state.identity)?);
    let transport = Transport::new(LOCAL_ADDRESS)
        .await
        .context("ouverture du socket UDP IPv6")?;
    let controller = Controller::new(&cm, &transport, state.identity.fabric_id)?;
    let fabric = matc::fabric::Fabric::new(state.identity.fabric_id, 1, &cm.get_ca_public_key()?);
    let compressed_fabric = hex::encode_upper(fabric.compressed()?);
    let discovery = Discovery::start().await.context("démarrage mDNS")?;

    events.send(Event::Info(format!(
        "Données : {} — fabric {:#x} (compressée {compressed_fabric}).",
        state_path.display(),
        state.identity.fabric_id
    )));

    let nodes = state.nodes.clone();
    let device_ids = state.device_ids();
    for n in &nodes {
        events.send(Event::Node(node_info(n)));
        for b in &n.bridged {
            events.send(Event::Node(bridged_info(n.node_id, b)));
        }
    }

    // Historique sur disque : nettoyage (rétention, taille), puis relecture vers l'interface.
    let mut store = HistoryStore::new(history_store::dir());
    let now = crate::events::now_unix();
    let pruned = store.prune(now);
    let since = now - history_store::RETENTION.as_secs_f64();
    let mut replayed = 0;
    for id in &device_ids {
        for event in store
            .load(*id, since)
            .iter()
            .filter_map(|r| r.to_event(*id))
        {
            events.send(event);
            replayed += 1;
        }
    }
    // Historique d'appareils oubliés entre-temps : supprimé.
    for orphan in store
        .nodes()
        .into_iter()
        .filter(|id| !device_ids.contains(id))
    {
        let _ = store.remove(orphan);
    }
    let summary = format!(
        "Historique : {replayed} mesures rechargées ({} jours max) ; {} anciennes retirées au nettoyage.",
        history_store::RETENTION.as_secs() / 86_400,
        pruned.removed_lines
    );
    log::info!("{summary}");
    events.send(Event::Info(summary));
    let history = history_store::start(store);

    let ctx = Arc::new(Ctx {
        controller,
        transport,
        discovery,
        controller_node_id: state.identity.controller_node_id,
        compressed_fabric,
        events: events.clone(),
        history,
        connections: Mutex::new(HashMap::new()),
        bridge_unreachable: Mutex::new(HashSet::new()),
        state: Mutex::new(state),
        state_path,
    });

    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let (rules, rules_error) = automation::load(false, automation::RuleSet::default);
    if let Some(e) = rules_error {
        events.send(Event::Info(e));
    }
    let actuator: Arc<dyn Actuator> = Arc::new(MatterActuator(ctx.clone()));
    automation::spawn(rules.rules, tap_rx, engine_rx, ui, actuator.clone());

    let mut handles: HashMap<u64, NodeHandle> = HashMap::new();
    for n in &nodes {
        handles.insert(n.node_id, spawn_node(&ctx, n.node_id, None));
    }

    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Internal>();
    let pairing = Arc::new(AtomicBool::new(false));

    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::Pair(code) => {
                        if pairing.swap(true, Ordering::SeqCst) {
                            events.send(Event::PairingFailed("Un appairage est déjà en cours.".into()));
                            continue;
                        }
                        let ctx = ctx.clone();
                        let pairing = pairing.clone();
                        let internal_tx = internal_tx.clone();
                        tokio::spawn(async move {
                            match pair(&ctx, &code).await {
                                Ok((node_id, name, conn)) => {
                                    ctx.events.send(Event::PairingDone { node_id, name });
                                    let _ = internal_tx.send(Internal::Paired(node_id, conn));
                                }
                                Err(msg) => ctx.events.send(Event::PairingFailed(msg)),
                            }
                            pairing.store(false, Ordering::SeqCst);
                        });
                    }
                    Command::ClearHistory(node_id) => ctx.history.remove(node_id),
                    Command::SetRules(rules) => {
                        let _ = engine_tx.send(automation::EngineCommand::SetRules(rules));
                    }
                    Command::TestRule(id) => {
                        let _ = engine_tx.send(automation::EngineCommand::Test(id));
                    }
                    Command::Customize { node_id, name, icon } => {
                        let name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                        if let Some(info) = ctx.customize(node_id, name, icon) {
                            events.send(Event::Node(info));
                        }
                    }
                    Command::SetCover { node_id, command } => {
                        let ctx = ctx.clone();
                        let actuator = actuator.clone();
                        tokio::spawn(async move {
                            let name = ctx.resolve(node_id).map_or_else(
                                || format!("{node_id:016X}"),
                                |r| r.name,
                            );
                            let what = format!("{name} : {}", command.label().to_lowercase());
                            match actuator.set_cover(node_id, command).await {
                                Ok(()) => ctx.events.send(Event::Info(format!("{what} — commande envoyée."))),
                                Err(e) => ctx.events.send(Event::Info(format!("{what} — échec : {e:#}"))),
                            }
                        });
                    }
                    Command::Retry(node_id) => {
                        let parent = ctx.parent_node(node_id).unwrap_or(node_id);
                        if let Some(h) = handles.get(&parent) {
                            h.wake.notify_one();
                        }
                    }
                    Command::Forget(node_id) => {
                        if ctx.parent_node(node_id).is_some_and(|p| p != node_id) {
                            events.send(Event::Info(
                                "Cet appareil est exposé par un pont : pour le retirer, oubliez le pont.".into(),
                            ));
                            continue;
                        }
                        if let Some(h) = handles.remove(&node_id) {
                            h.task.abort();
                        }
                        let ids = ctx.device_ids(node_id);
                        ctx.set_connection(node_id, None);
                        if let Err(e) = ctx.remove_node(node_id) {
                            events.send(Event::Info(format!("Sauvegarde impossible : {e:#}")));
                        }
                        for id in ids {
                            ctx.history.remove(id);
                            ctx.set_bridge_reachable(id, true);
                            events.send(Event::NodeRemoved(id));
                        }
                    }
                }
            }
            Some(msg) = internal_rx.recv() => match msg {
                Internal::Paired(node_id, conn) => {
                    if let Some(old) = handles.remove(&node_id) {
                        old.task.abort();
                    }
                    handles.insert(node_id, spawn_node(&ctx, node_id, Some(conn)));
                }
            },
        }
    }
    Ok(())
}

fn spawn_node(ctx: &Arc<Ctx>, node_id: u64, conn: Option<Connection>) -> NodeHandle {
    let wake = Arc::new(Notify::new());
    let task = tokio::spawn(node::supervise(ctx.clone(), node_id, wake.clone(), conn));
    NodeHandle { task, wake }
}

/// Appairage on-network (multi-admin) : découverte mDNS, PASE, certificats, CASE.
/// L'erreur retournée est un message destiné à l'utilisateur.
async fn pair(
    ctx: &Arc<Ctx>,
    code: &str,
) -> std::result::Result<(u64, String, Connection), String> {
    let setup = setup_code::parse(code).map_err(|e| e.to_string())?;
    let progress = |m: &str| ctx.events.send(Event::PairingProgress(m.to_string()));

    progress("Recherche de l'appareil en mode appairage (mDNS _matterc._udp)…");
    let candidates = match ctx
        .discovery
        .find_commissionable(&setup, PAIRING_DISCOVERY_TIMEOUT)
        .await
    {
        Ok(found) => vec![found],
        Err(_) => {
            // Certains capteurs sur pile ouvrent bien leur fenêtre mais ne mettent pas à
            // jour leur annonce mDNS (CM=0, ancien discriminateur). Repli : essayer PASE
            // directement sur les appareils qui s'annoncent comme appairables.
            let paired: Vec<String> = {
                let state = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
                state.nodes.iter().map(|n| n.address.clone()).collect()
            };
            let mut others = ctx.discovery.recent_commissionable().await;
            // Inutile de réessayer un appareil que l'on vient d'appairer.
            others.retain(|f| !f.addresses.iter().any(|a| paired.contains(a)));
            if others.is_empty() {
                return Err(format!(
                    "Appareil introuvable : aucun appareil en mode appairage ne correspond à ce code \
                     après {} s. Vérifiez que le mode appairage est toujours actif dans l'app Maison \
                     (le code expire au bout de quelques minutes) et que le Mac est sur le même réseau.",
                    PAIRING_DISCOVERY_TIMEOUT.as_secs()
                ));
            }
            progress(&format!(
                "Aucune annonce ne correspond au code ; essai direct sur {} appareil(s) qui s'annonce(nt) appairable(s)…",
                others.len()
            ));
            others
        }
    };

    let node_id = {
        let mut rng = rand::thread_rng();
        loop {
            // Plage des identifiants opérationnels : 0x1 à 0xFFFF_FFEF_FFFF_FFFF.
            let id = rng.gen_range(1..=0xFFFF_FFEF_FFFF_FFFFu64);
            if ctx.node(id).is_none() {
                break id;
            }
        }
    };

    let mut last_err = String::from("aucune adresse IP utilisable annoncée");
    for found in &candidates {
        for address in &found.addresses {
            progress(&format!(
            "Appareil trouvé ({}). Authentification par code (PASE) et installation des certificats sur {address}…",
            found.info.name.as_deref().unwrap_or("sans nom")
        ));
            let conn = ctx.transport.create_connection(address).await;
            let attempt = tokio::time::timeout(
                COMMISSION_TIMEOUT,
                ctx.controller
                    .commission(&conn, setup.passcode, node_id, ctx.controller_node_id),
            )
            .await;
            match attempt {
                Ok(Ok(connection)) => {
                    let name = found
                        .info
                        .name
                        .clone()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| format!("Nœud {node_id:016X}"));
                    let stored = StoredNode {
                        node_id,
                        name: name.clone(),
                        address: address.clone(),
                        ..Default::default()
                    };
                    ctx.add_node(stored.clone())
                        .map_err(|e| format!("Appairé, mais sauvegarde impossible : {e:#}"))?;
                    ctx.events.send(Event::Node(node_info(&stored)));
                    return Ok((node_id, name, connection));
                }
                Ok(Err(e)) => {
                    log::warn!("commissioning sur {address} : {e:#}");
                    last_err = format!("{e:#}");
                }
                Err(_) => {
                    last_err = format!("pas de réponse après {} s", COMMISSION_TIMEOUT.as_secs())
                }
            }
        }
    }
    Err(explain_commission_error(&last_err))
}

/// Commandes Matter des automatisations et de la fiche, envoyées sur la session
/// ouverte du nœud (ou du pont) qui porte l'appareil.
struct MatterActuator(Arc<Ctx>);

impl MatterActuator {
    /// Envoie une commande sur la session du nœud `parent` (l'appareil lui-même ou son pont).
    async fn invoke(
        &self,
        parent: u64,
        endpoint: u16,
        cluster: u32,
        command: u32,
        payload: &[u8],
    ) -> Result<()> {
        let conn = self
            .0
            .connection(parent)
            .context("appareil non connecté pour l'instant")?;
        let msg = tokio::time::timeout(
            COMMAND_TIMEOUT,
            conn.invoke_request(endpoint, cluster, command, payload),
        )
        .await
        .map_err(|_| anyhow::anyhow!("pas de réponse à la commande"))??;
        invoke_status(&msg.payload)
    }
}

impl Actuator for MatterActuator {
    fn set_on_off(&self, node_id: u64, command: OnOffCommand) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let device = self.0.resolve(node_id).context("appareil inconnu")?;
            let endpoint = device
                .endpoints
                .on_off_endpoint
                .context("cet appareil ne se commande pas (pas de cluster On/Off)")?;
            self.invoke(device.node_id, endpoint, 0x0006, command.command_id(), &[])
                .await
        })
    }

    fn set_cover(&self, node_id: u64, command: CoverCommand) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let device = self.0.resolve(node_id).context("appareil inconnu")?;
            let endpoint = device
                .endpoints
                .cover_endpoint
                .context("cet appareil n'est pas un volet (pas de cluster Window Covering)")?;
            let payload = cover_payload(command)?;
            self.invoke(
                device.node_id,
                endpoint,
                node::CLUSTER_WINDOW_COVERING,
                command.command_id(),
                &payload,
            )
            .await
        })
    }
}

/// Champs TLV de la commande Window Covering : vide, sauf GoToLiftPercentage
/// (champ 0 : LiftPercent100thsValue, 0 = ouvert, 10000 = fermé).
fn cover_payload(command: CoverCommand) -> Result<Vec<u8>> {
    match command {
        CoverCommand::Position(open) => {
            let item = matc::tlv::TlvItemEnc {
                tag: 0,
                value: matc::tlv::TlvItemValueEnc::UInt16(convert::lift100ths_from_open_percent(
                    open,
                )),
            };
            item.encode().context("encodage de la position")
        }
        _ => Ok(Vec::new()),
    }
}

/// Statut d'une InvokeResponse : `Ok` si la commande a réussi.
/// Structure : {1: [InvokeResponseIB {1: CommandStatusIB {1: StatusIB {0: statut}}}]}.
fn invoke_status(payload: &[u8]) -> Result<()> {
    let root = tlv::decode(payload).context("réponse de commande illisible")?;
    let response = root
        .field(1)
        .and_then(|list| list.items().first().map(|(_, v)| v))
        .context("réponse de commande vide")?;
    match response.path(&[1, 1, 0]).and_then(tlv::Value::as_u64) {
        None | Some(0) => Ok(()),
        Some(status) => anyhow::bail!("commande refusée par l'appareil (statut {status:#04x})"),
    }
}

fn explain_commission_error(err: &str) -> String {
    let lower = err.to_lowercase();
    let hint = if lower.contains("pake") || lower.contains("spake") || lower.contains("pase") {
        "Le code a été refusé : il est probablement expiré ou déjà utilisé. Générez-en un nouveau dans l'app Maison."
    } else if lower.contains("noc") || lower.contains("fabric") {
        "L'appareil a refusé nos certificats (nombre maximal de contrôleurs atteint ?)."
    } else if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("pas de réponse")
    {
        "L'appareil ne répond pas (endormi ou hors de portée du réseau Thread). Réessayez avec un nouveau code."
    } else {
        "Réessayez avec un nouveau code d'appairage."
    };
    format!("Échec de l'appairage : {hint}\nDétail technique : {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::Symbol;

    /// InvokeResponse : {0: false, 1: [{1: {0: Path{0:1,1:6,2:1}, 1: Status{0: statut}}}], 0xFF: 11}
    fn invoke_response(status: u8) -> Vec<u8> {
        vec![
            0x15, 0x28, 0x00, 0x36, 0x01, 0x15, 0x35, 0x01, 0x37, 0x00, 0x24, 0x00, 0x01, 0x24,
            0x01, 0x06, 0x24, 0x02, 0x01, 0x18, 0x35, 0x01, 0x24, 0x00, status, 0x18, 0x18, 0x18,
            0x18, 0x24, 0xFF, 0x0B, 0x18,
        ]
    }

    #[test]
    fn invoke_status_success_and_failure() {
        assert!(invoke_status(&invoke_response(0)).is_ok());
        let err = invoke_status(&invoke_response(0x81))
            .unwrap_err()
            .to_string();
        assert!(err.contains("0x81"), "{err}");
        assert!(invoke_status(&[0x15, 0x18]).is_err());
    }

    #[test]
    fn cover_payloads() {
        assert!(cover_payload(CoverCommand::Open).unwrap().is_empty());
        assert!(cover_payload(CoverCommand::Stop).unwrap().is_empty());
        // Ouvert à 40 % → 6000 centièmes fermés = 0x1770, uint16 avec tag de contexte 0.
        assert_eq!(
            cover_payload(CoverCommand::Position(40)).unwrap(),
            vec![0x25, 0x00, 0x70, 0x17]
        );
        assert_eq!(
            cover_payload(CoverCommand::Position(100)).unwrap(),
            vec![0x25, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn node_info_prefers_custom_name_and_icon() {
        let mut n = StoredNode {
            node_id: 1,
            name: "GRILLPLATS Plug".into(),
            endpoints: TrackedEndpoints {
                power_endpoint: Some(2),
                ..Default::default()
            },
            ..Default::default()
        };
        let info = node_info(&n);
        assert_eq!((info.name.as_str(), info.icon), ("GRILLPLATS Plug", None));
        assert!(info.caps.power);
        assert!(info.bridge.is_none());
        n.custom_name = Some("Lampe du salon".into());
        n.icon = Some(Symbol::Light);
        let info = node_info(&n);
        assert_eq!(info.name, "Lampe du salon");
        assert_eq!(info.device_name, "GRILLPLATS Plug");
        assert_eq!(info.icon, Some(Symbol::Light));
    }

    #[test]
    fn bridged_info_links_to_bridge() {
        let b = StoredBridgedDevice {
            device_id: BRIDGED_ID_MIN + 1,
            endpoint: 4,
            name: "Volet cuisine".into(),
            endpoints: TrackedEndpoints {
                cover_endpoint: Some(4),
                cover_position: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let info = bridged_info(0x42, &b);
        assert_eq!(info.node_id, BRIDGED_ID_MIN + 1);
        assert_eq!(
            info.bridge,
            Some(BridgeLink {
                node_id: 0x42,
                endpoint: 4
            })
        );
        assert!(info.caps.cover && !info.caps.cover_position);
    }
}
