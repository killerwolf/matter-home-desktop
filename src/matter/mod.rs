//! Partie réseau / Matter. Tourne entièrement dans un runtime tokio sur un thread
//! dédié ; l'interface lui parle uniquement par canaux (voir `events`).

pub mod certs;
pub mod convert;
pub mod discovery;
mod node;
pub mod setup_code;
pub mod tlv;

use std::collections::HashMap;
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

use crate::events::{Capabilities, Command, Event, EventSink, NodeInfo};
use crate::history_store::{self, HistorySink, HistoryStore};
use crate::persistence::{self, StoredNode, StoredState};
use discovery::Discovery;

/// Socket UDP local : toutes les interfaces, IPv6 (double pile), port éphémère.
/// Les appareils Thread ne sont joignables qu'en IPv6.
const LOCAL_ADDRESS: &str = "[::]:0";
const PAIRING_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(40);
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

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
    state: Mutex<StoredState>,
    state_path: PathBuf,
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

    pub fn instance_name(&self, node_id: u64) -> String {
        format!("{}-{:016X}", self.compressed_fabric, node_id)
    }
}

pub fn node_info(n: &StoredNode) -> NodeInfo {
    NodeInfo {
        node_id: n.node_id,
        name: n.name.clone(),
        vendor: n.vendor.clone(),
        product: n.product.clone(),
        device_types: n.device_types.clone(),
        caps: Capabilities {
            temperature: n.temperature_endpoint.is_some(),
            humidity: n.humidity_endpoint.is_some(),
            contact: n.contact_endpoint.is_some(),
            battery: n.battery_endpoint.is_some(),
            power: n.power_endpoint.is_some(),
            energy: n.energy_endpoint.is_some(),
            on_off: n.on_off_endpoint.is_some(),
        },
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

async fn run(mut commands: mpsc::UnboundedReceiver<Command>, events: EventSink) -> Result<()> {
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
    for n in &nodes {
        events.send(Event::Node(node_info(n)));
    }

    // Historique sur disque : nettoyage (rétention, taille), puis relecture vers l'interface.
    let mut store = HistoryStore::new(history_store::dir());
    let now = crate::events::now_unix();
    let pruned = store.prune(now);
    let since = now - history_store::RETENTION.as_secs_f64();
    let mut replayed = 0;
    for n in &nodes {
        for event in store.load(n.node_id, since).iter().filter_map(|r| r.to_event(n.node_id)) {
            events.send(event);
            replayed += 1;
        }
    }
    // Historique d'appareils oubliés entre-temps : supprimé.
    for orphan in store.nodes().into_iter().filter(|id| !nodes.iter().any(|n| n.node_id == *id)) {
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
        state: Mutex::new(state),
        state_path,
    });

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
                    Command::Retry(node_id) => {
                        if let Some(h) = handles.get(&node_id) {
                            h.wake.notify_one();
                        }
                    }
                    Command::Forget(node_id) => {
                        if let Some(h) = handles.remove(&node_id) {
                            h.task.abort();
                        }
                        ctx.history.remove(node_id);
                        if let Err(e) = ctx.remove_node(node_id) {
                            events.send(Event::Info(format!("Sauvegarde impossible : {e:#}")));
                        }
                        events.send(Event::NodeRemoved(node_id));
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
