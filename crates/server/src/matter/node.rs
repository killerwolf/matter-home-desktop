//! Supervision d'un nœud : connexion CASE, lecture de sa description,
//! souscription (ou interrogation) aux mesures, et reconnexion automatique.
//!
//! Chaque nœud a sa propre tâche ; toutes les attentes réseau sont bornées par
//! un délai pour qu'un appareil endormi ou disparu ne bloque jamais rien.
//!
//! Un nœud « pont » (type Aggregator, ex. Somfy TaHoma) expose plusieurs appareils,
//! un par endpoint portant Bridged Device Basic Information. Chacun a son propre
//! identifiant ; une seule session et une seule souscription servent pour tous.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use matc::controller::Connection;
use matc::messages::{Message, ProtocolMessageHeader as Ph};
use tokio::sync::Notify;
use tokio::time::timeout;

use super::convert::{
    battery_low_from_tlv, battery_percent_from_tlv, cover_motion_from_tlv,
    cover_open_percent_from_tlv, energy_kwh_from_tlv, humidity_from_tlv, open_from_tlv,
    power_w_from_tlv, temperature_from_tlv,
};
use super::tlv::{parse_report_data, parse_subscribe_response_max_interval, Value};
use super::{Ctx, Interview};
use crate::events::{now_unix, Event, MeasureKind, NodeStatus, ReportingMode, StateKind};
use crate::persistence::{StoredBridgedDevice, StoredNode, TrackedEndpoints, INTERVIEW_VERSION};

const CLUSTER_ON_OFF: u32 = 0x0006;
const CLUSTER_DESCRIPTOR: u32 = 0x001D;
const CLUSTER_BASIC_INFORMATION: u32 = 0x0028;
const CLUSTER_POWER_SOURCE: u32 = 0x002F;
const CLUSTER_BRIDGED_BASIC: u32 = 0x0039;
const CLUSTER_BOOLEAN_STATE: u32 = 0x0045;
const CLUSTER_ELECTRICAL_POWER: u32 = 0x0090;
const CLUSTER_ELECTRICAL_ENERGY: u32 = 0x0091;
pub(crate) const CLUSTER_WINDOW_COVERING: u32 = 0x0102;
const CLUSTER_TEMPERATURE: u32 = 0x0402;
const CLUSTER_HUMIDITY: u32 = 0x0405;

const ATTR_MEASURED_VALUE: u32 = 0x0000;
const ATTR_ON_OFF: u32 = 0x0000;
const ATTR_STATE_VALUE: u32 = 0x0000;
const ATTR_BAT_PERCENT_REMAINING: u32 = 0x000C;
const ATTR_BAT_CHARGE_LEVEL: u32 = 0x000E;
const ATTR_ACTIVE_POWER: u32 = 0x0008;
const ATTR_CUMULATIVE_ENERGY_IMPORTED: u32 = 0x0001;
/// Bridged Device Basic Information / Reachable.
const ATTR_REACHABLE: u32 = 0x0011;
/// Window Covering / OperationalStatus et CurrentPositionLiftPercent100ths.
const ATTR_COVER_OPERATIONAL_STATUS: u32 = 0x000A;
const ATTR_COVER_CURRENT_LIFT: u32 = 0x000E;
const ATTR_FEATURE_MAP: u32 = 0xFFFC;
/// Power Source : bit « BAT » de la FeatureMap (alimentation par pile).
const POWER_SOURCE_FEATURE_BATTERY: u64 = 0x2;
/// Window Covering : bit « PA_LF » (Position Aware Lift) de la FeatureMap.
const COVER_FEATURE_POSITION_AWARE_LIFT: u64 = 0x4;
/// Type d'appareil « Aggregator » : le nœud est un pont.
const DEVICE_TYPE_AGGREGATOR: u64 = 0x000E;

/// Bornes demandées pour la souscription : au plus un rapport toutes les 10 s,
/// au moins un toutes les 5 min (un appareil sur pile peut choisir plus long).
const SUB_MIN_FLOOR_S: u16 = 10;
const SUB_MAX_CEILING_S: u16 = 300;

/// Délai d'une requête simple. Un appareil « sleepy » répond en quelques
/// secondes à quelques dizaines ; matc (copie locale) retransmet pendant 45 s.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(150);
const POLL_EVERY: Duration = Duration::from_secs(30);
const LIVENESS_EVERY: Duration = Duration::from_secs(60);
const RETRY_MIN: u64 = 5;
const RETRY_MAX: u64 = 120;

pub(crate) async fn supervise(
    ctx: Arc<Ctx>,
    node_id: u64,
    wake: Arc<Notify>,
    initial: Option<Connection>,
) {
    let mut pending = initial;
    let mut retry = RETRY_MIN;
    loop {
        let conn = match pending.take() {
            Some(c) => c,
            None => {
                status(&ctx, node_id, NodeStatus::Connecting);
                match timeout(CONNECT_TIMEOUT, connect(&ctx, node_id)).await {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        wait_retry(&ctx, node_id, &wake, format!("{e:#}"), &mut retry).await;
                        continue;
                    }
                    Err(_) => {
                        wait_retry(
                            &ctx,
                            node_id,
                            &wake,
                            "délai de connexion dépassé".into(),
                            &mut retry,
                        )
                        .await;
                        continue;
                    }
                }
            }
        };
        retry = RETRY_MIN;
        status(&ctx, node_id, NodeStatus::Online);
        seen(&ctx, node_id);

        // Connexion partagée avec les automatisations (commandes envoyées pendant l'écoute).
        let conn = Arc::new(conn);
        ctx.set_connection(node_id, Some(conn.clone()));
        let err = match run_session(&ctx, node_id, &conn).await {
            Ok(()) => anyhow!("session terminée"),
            Err(e) => e,
        };
        ctx.set_connection(node_id, None);
        log::info!("nœud {node_id:016X} : {err:#}");
        wait_retry(&ctx, node_id, &wake, format!("{err:#}"), &mut retry).await;
    }
}

async fn wait_retry(ctx: &Ctx, node_id: u64, wake: &Notify, error: String, retry: &mut u64) {
    status(
        ctx,
        node_id,
        NodeStatus::Unreachable {
            error,
            retry_in_s: *retry,
        },
    );
    let _ = timeout(Duration::from_secs(*retry), wake.notified()).await;
    *retry = (*retry * 2).min(RETRY_MAX);
}

/// Statut du nœud, répercuté sur les appareils qu'il expose. Un appareil que le pont
/// lui-même déclare injoignable ne repasse pas « en ligne » avec le pont.
fn status(ctx: &Ctx, node_id: u64, status: NodeStatus) {
    let online = status == NodeStatus::Online;
    for id in ctx.device_ids(node_id) {
        if online && id != node_id && ctx.is_bridge_unreachable(id) {
            continue;
        }
        ctx.events.send(Event::Status {
            node_id: id,
            status: status.clone(),
        });
    }
}

fn seen(ctx: &Ctx, node_id: u64) {
    let at = now_unix();
    for id in ctx.device_ids(node_id) {
        ctx.events.send(Event::Seen { node_id: id, at });
    }
}

fn mode(ctx: &Ctx, node_id: u64, mode: ReportingMode) {
    for id in ctx.device_ids(node_id) {
        ctx.events.send(Event::Mode {
            node_id: id,
            mode: mode.clone(),
        });
    }
}

/// Délai avant de lancer la redécouverte mDNS pendant que l'adresse mémorisée est
/// encore à l'essai (une session saine s'ouvre bien plus vite).
const DISCOVERY_GRACE: Duration = Duration::from_secs(3);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
/// Décalage entre les départs des adresses fraîches : une adresse morte coûte jusqu'à
/// 45 s de retransmissions, les suivantes ne doivent pas les attendre.
const FRESH_STAGGER: Duration = Duration::from_secs(3);

/// Ouvre une session CASE : adresse connue d'abord, redécouverte mDNS en parallèle.
async fn connect(ctx: &Ctx, node_id: u64) -> Result<Connection> {
    let stored = ctx.node(node_id).context("nœud inconnu")?;
    let instance = ctx.instance_name(node_id);
    let (conn, address) = connect_race(
        &stored.address,
        DISCOVERY_GRACE,
        FRESH_STAGGER,
        |address| async move {
            case(ctx, node_id, &address).await.inspect_err(|e| {
                log::debug!("CASE {node_id:016X} @ {address} : {e:#}");
            })
        },
        || async {
            ctx.discovery
                .find_operational(&instance, DISCOVERY_TIMEOUT)
                .await
                .map(|found| found.addresses)
        },
    )
    .await?;
    if address != stored.address {
        ctx.update_node(node_id, |n| n.address = address.clone());
    }
    Ok(conn)
}

/// Adresses annoncées en mDNS à essayer en plus de l'adresse mémorisée.
fn fresh_addresses(stored: &str, found: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in found {
        if a != stored && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

/// Met en course l'adresse mémorisée et les adresses annoncées en mDNS. L'adresse
/// mémorisée part tout de suite et n'est jamais abandonnée : un appareil à sommeil peut
/// mettre plus de 3 s à répondre, et mDNS peut annoncer des adresses mortes (cache,
/// réponse partielle). La découverte démarre après `grace`, ou dès que l'essai mémorisé
/// échoue ; les adresses fraîches partent ensuite avec `stagger` d'écart. Le premier
/// succès gagne ; les autres essais sont lâchés (leur connexion est libérée à la chute
/// de la future : aucune seconde session n'est conservée). Renvoie la connexion et
/// l'adresse qui a servi ; l'appelant ne réécrit l'adresse qu'après ce succès.
async fn connect_race<C, A, AF, D, DF>(
    stored: &str,
    grace: Duration,
    stagger: Duration,
    attempt: A,
    discover: D,
) -> Result<(C, String)>
where
    A: Fn(String) -> AF,
    AF: std::future::Future<Output = Result<C>>,
    D: FnOnce() -> DF,
    DF: std::future::Future<Output = Result<Vec<String>>>,
{
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let mut errors: Vec<String> = Vec::new();
    let failed = Notify::new();
    let mut attempts = FuturesUnordered::new();
    if stored.is_empty() {
        failed.notify_one();
    } else {
        attempts.push(timed_attempt(
            &attempt,
            true,
            stored.to_string(),
            Duration::ZERO,
        ));
    }
    let mut disc_fut = std::pin::pin!(async {
        tokio::select! {
            _ = tokio::time::sleep(grace) => {}
            _ = failed.notified() => {}
        }
        discover().await
    });
    let mut disc_active = true;
    while disc_active || !attempts.is_empty() {
        tokio::select! {
            Some((is_stored, address, r)) = attempts.next(), if !attempts.is_empty() => {
                match r {
                    Ok(c) => return Ok((c, address)),
                    Err(e) => errors.push(format!("{e:#}")),
                }
                if is_stored {
                    failed.notify_one();
                }
            }
            r = &mut disc_fut, if disc_active => {
                disc_active = false;
                let found = match r {
                    Ok(addresses) => addresses,
                    Err(e) => {
                        errors.push(format!("{e:#}"));
                        Vec::new()
                    }
                };
                for (i, address) in fresh_addresses(stored, &found).into_iter().enumerate() {
                    attempts.push(timed_attempt(&attempt, false, address, stagger * i as u32));
                }
            }
        }
    }
    if errors.is_empty() {
        bail!("aucune adresse joignable");
    }
    Err(anyhow!(errors.join(" ; ")))
}

/// Un essai de connexion après `delay`, étiqueté pour la course de `connect_race`.
async fn timed_attempt<C, A, AF>(
    attempt: &A,
    is_stored: bool,
    address: String,
    delay: Duration,
) -> (bool, String, Result<C>)
where
    A: Fn(String) -> AF,
    AF: std::future::Future<Output = Result<C>>,
{
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let r = attempt(address.clone()).await;
    (is_stored, address, r)
}

async fn case(ctx: &Ctx, node_id: u64, address: &str) -> Result<Connection> {
    let conn = ctx.transport.create_connection(address).await;
    for attempt in 0..3 {
        match timeout(
            REQUEST_TIMEOUT,
            ctx.controller
                .auth_sigma(&conn, node_id, ctx.controller_node_id),
        )
        .await
        {
            Ok(Ok(c)) => return Ok(c),
            Ok(Err(e)) => {
                if let Some(busy) = e.downcast_ref::<matc::controller::SigmaBusy>() {
                    if attempt < 2 {
                        let ms = busy.wait_ms.unwrap_or(3000).min(30_000);
                        tokio::time::sleep(Duration::from_millis(ms.into())).await;
                        continue;
                    }
                }
                return Err(e.context("ouverture de session CASE"));
            }
            Err(_) => bail!("pas de réponse à l'ouverture de session (CASE) sur {address}"),
        }
    }
    bail!("appareil occupé (BUSY)")
}

/// Attributs suivis pour un appareil : (endpoint, cluster, attribut).
fn paths_of(e: &TrackedEndpoints) -> Vec<(u16, u32, u32)> {
    let mut paths = Vec::new();
    let mut add = |ep: Option<u16>, cluster: u32, attrs: &[u32]| {
        if let Some(ep) = ep {
            paths.extend(attrs.iter().map(|a| (ep, cluster, *a)));
        }
    };
    add(
        e.temperature_endpoint,
        CLUSTER_TEMPERATURE,
        &[ATTR_MEASURED_VALUE],
    );
    add(
        e.humidity_endpoint,
        CLUSTER_HUMIDITY,
        &[ATTR_MEASURED_VALUE],
    );
    add(
        e.contact_endpoint,
        CLUSTER_BOOLEAN_STATE,
        &[ATTR_STATE_VALUE],
    );
    add(
        e.battery_endpoint,
        CLUSTER_POWER_SOURCE,
        &[ATTR_BAT_PERCENT_REMAINING, ATTR_BAT_CHARGE_LEVEL],
    );
    add(
        e.power_endpoint,
        CLUSTER_ELECTRICAL_POWER,
        &[ATTR_ACTIVE_POWER],
    );
    add(
        e.energy_endpoint,
        CLUSTER_ELECTRICAL_ENERGY,
        &[ATTR_CUMULATIVE_ENERGY_IMPORTED],
    );
    add(e.on_off_endpoint, CLUSTER_ON_OFF, &[ATTR_ON_OFF]);
    // La position n'existe que sur un volet qui la connaît : la demander ailleurs
    // ferait refuser la souscription par certains appareils.
    if e.cover_position {
        add(
            e.cover_endpoint,
            CLUSTER_WINDOW_COVERING,
            &[ATTR_COVER_OPERATIONAL_STATUS, ATTR_COVER_CURRENT_LIFT],
        );
    } else {
        add(
            e.cover_endpoint,
            CLUSTER_WINDOW_COVERING,
            &[ATTR_COVER_OPERATIONAL_STATUS],
        );
    }
    paths
}

/// Attributs suivis pour le nœud et tous les appareils qu'il expose.
fn tracked_paths(n: &StoredNode) -> Vec<(u16, u32, u32)> {
    let mut paths = paths_of(&n.endpoints);
    for b in &n.bridged {
        paths.extend(paths_of(&b.endpoints));
        paths.push((b.endpoint, CLUSTER_BRIDGED_BASIC, ATTR_REACHABLE));
    }
    paths
}

/// Quel appareil (identifiant) reçoit les rapports de chaque endpoint du nœud.
struct Routes {
    node_id: u64,
    by_endpoint: HashMap<u16, u64>,
}

impl Routes {
    fn of(n: &StoredNode) -> Self {
        let mut by_endpoint = HashMap::new();
        for b in &n.bridged {
            by_endpoint.insert(b.endpoint, b.device_id);
            for (ep, _, _) in paths_of(&b.endpoints) {
                by_endpoint.insert(ep, b.device_id);
            }
        }
        Self {
            node_id: n.node_id,
            by_endpoint,
        }
    }

    fn device(&self, endpoint: u16) -> u64 {
        self.by_endpoint
            .get(&endpoint)
            .copied()
            .unwrap_or(self.node_id)
    }
}

async fn run_session(ctx: &Ctx, node_id: u64, conn: &Connection) -> Result<()> {
    let up_to_date = ctx
        .node(node_id)
        .is_some_and(|n| n.interviewed && n.interview_version >= INTERVIEW_VERSION);
    if !up_to_date {
        interview(ctx, node_id, conn).await?;
        seen(ctx, node_id);
    }
    let node = ctx.node(node_id).context("nœud inconnu")?;
    let routes = Routes::of(&node);
    let paths = tracked_paths(&node);
    if paths.is_empty() {
        return liveness_loop(ctx, node_id, conn).await;
    }
    match subscribe(ctx, &routes, conn, &paths).await {
        Ok(max_interval) => {
            mode(
                ctx,
                node_id,
                ReportingMode::Subscription {
                    max_interval_s: max_interval,
                },
            );
            listen(ctx, &routes, conn, max_interval).await
        }
        Err(e) => {
            log::warn!(
                "souscription impossible sur {node_id:016X} : {e:#} ; interrogation périodique"
            );
            ctx.events.send(Event::Info(format!(
                "Souscription impossible pour le nœud {node_id:016X} ({e:#}) : interrogation toutes les {} s.",
                POLL_EVERY.as_secs()
            )));
            mode(
                ctx,
                node_id,
                ReportingMode::Polling {
                    every_s: POLL_EVERY.as_secs(),
                },
            );
            poll_loop(ctx, &routes, conn, &paths).await
        }
    }
}

/// Lecture d'un attribut, avec décodage TLV maison.
async fn read_attr(conn: &Connection, endpoint: u16, cluster: u32, attr: u32) -> Result<Value> {
    let msg = timeout(REQUEST_TIMEOUT, conn.read_request(endpoint, cluster, attr))
        .await
        .map_err(|_| anyhow!("pas de réponse (lecture {endpoint}/{cluster:#06x}/{attr:#06x})"))??;
    if let Some(sr) = &msg.status_report_info {
        bail!("statut {sr:?}");
    }
    if msg.protocol_header.opcode != Ph::INTERACTION_OPCODE_REPORT_DATA {
        bail!(
            "réponse inattendue (opcode {:#x})",
            msg.protocol_header.opcode
        );
    }
    let reports = parse_report_data(&msg.payload)?;
    let report = reports
        .into_iter()
        .find(|r| r.endpoint == endpoint && r.cluster == cluster && r.attribute == attr)
        .context("attribut absent de la réponse")?;
    report.data.map_err(|s| anyhow!("statut IM {s:#04x}"))
}

/// Chaîne non vide d'un attribut texte, sinon `None`.
fn text(v: Result<Value>) -> Option<String> {
    v.ok()
        .and_then(|v| v.as_str().map(str::trim).map(str::to_owned))
        .filter(|s| !s.is_empty())
}

fn u64_list(v: &Value) -> Vec<u64> {
    v.items().iter().filter_map(|(_, v)| v.as_u64()).collect()
}

/// Description d'un endpoint : types d'appareil, clusters serveurs, sous-endpoints.
struct EndpointDesc {
    ep: u16,
    device_types: Vec<u64>,
    servers: Vec<u64>,
}

async fn describe_endpoints(conn: &Connection) -> Result<Vec<EndpointDesc>> {
    let parts = read_attr(conn, 0, CLUSTER_DESCRIPTOR, 0x0003)
        .await
        .context("lecture de la liste des endpoints")?;
    // L'endpoint 0 (nœud racine) n'est pas dans PartsList mais porte souvent Power Source.
    let mut endpoints: Vec<u16> = vec![0];
    for ep in u64_list(&parts) {
        let ep = ep as u16;
        if !endpoints.contains(&ep) {
            endpoints.push(ep);
        }
    }
    let mut out = Vec::new();
    for ep in endpoints {
        let device_types = if ep == 0 {
            Vec::new()
        } else {
            read_attr(conn, ep, CLUSTER_DESCRIPTOR, 0x0000)
                .await
                .map(|list| {
                    list.items()
                        .iter()
                        .filter_map(|(_, entry)| entry.field(0).and_then(Value::as_u64))
                        .collect()
                })
                .unwrap_or_default()
        };
        let servers = u64_list(&read_attr(conn, ep, CLUSTER_DESCRIPTOR, 0x0001).await?);
        out.push(EndpointDesc {
            ep,
            device_types,
            servers,
        });
    }
    Ok(out)
}

/// Repère, parmi ces endpoints, ceux qui portent les clusters suivis.
async fn scan(conn: &Connection, descs: &[&EndpointDesc]) -> TrackedEndpoints {
    let mut found = TrackedEndpoints::default();
    for d in descs {
        let ep = d.ep;
        let has = |c: u32| d.servers.contains(&(c as u64));
        let pick = |slot: &mut Option<u16>, cluster: u32| {
            if slot.is_none() && has(cluster) {
                *slot = Some(ep);
            }
        };
        pick(&mut found.temperature_endpoint, CLUSTER_TEMPERATURE);
        pick(&mut found.humidity_endpoint, CLUSTER_HUMIDITY);
        pick(&mut found.contact_endpoint, CLUSTER_BOOLEAN_STATE);
        pick(&mut found.power_endpoint, CLUSTER_ELECTRICAL_POWER);
        pick(&mut found.energy_endpoint, CLUSTER_ELECTRICAL_ENERGY);
        pick(&mut found.on_off_endpoint, CLUSTER_ON_OFF);
        // Power Source existe aussi sur les appareils secteur : on ne garde que les piles.
        if found.battery_endpoint.is_none() && has(CLUSTER_POWER_SOURCE) {
            let features = read_attr(conn, ep, CLUSTER_POWER_SOURCE, ATTR_FEATURE_MAP)
                .await
                .ok()
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if features & POWER_SOURCE_FEATURE_BATTERY != 0 {
                found.battery_endpoint = Some(ep);
            }
        }
        if found.cover_endpoint.is_none() && has(CLUSTER_WINDOW_COVERING) {
            found.cover_endpoint = Some(ep);
            let features = read_attr(conn, ep, CLUSTER_WINDOW_COVERING, ATTR_FEATURE_MAP)
                .await
                .ok()
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            found.cover_position = features & COVER_FEATURE_POSITION_AWARE_LIFT != 0;
        }
    }
    found
}

/// Noms des types d'appareil de ces endpoints, sans doublon.
fn type_names(descs: &[&EndpointDesc]) -> Vec<String> {
    let mut names = Vec::new();
    for d in descs {
        for dt in &d.device_types {
            if let Some(name) = device_type_name(*dt as u32) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// Lit le nom, le type et les endpoints de l'appareil, et repère les clusters suivis.
/// Pour un pont, décrit aussi chaque appareil exposé.
async fn interview(ctx: &Ctx, node_id: u64, conn: &Connection) -> Result<()> {
    let vendor = text(read_attr(conn, 0, CLUSTER_BASIC_INFORMATION, 0x0001).await);
    let product = text(read_attr(conn, 0, CLUSTER_BASIC_INFORMATION, 0x0003).await);
    let label = text(read_attr(conn, 0, CLUSTER_BASIC_INFORMATION, 0x0005).await);

    let descs = describe_endpoints(conn).await?;
    let is_bridge = descs
        .iter()
        .any(|d| d.device_types.contains(&DEVICE_TYPE_AGGREGATOR));

    let mut bridged = Vec::new();
    let mut claimed: HashSet<u16> = HashSet::new();
    if is_bridge {
        let bridged_eps: Vec<u16> = descs
            .iter()
            .filter(|d| d.servers.contains(&(CLUSTER_BRIDGED_BASIC as u64)))
            .map(|d| d.ep)
            .collect();
        for ep in bridged_eps {
            // Un appareil ponté peut être composé : ses sous-endpoints sont dans sa PartsList.
            let children: Vec<u16> = read_attr(conn, ep, CLUSTER_DESCRIPTOR, 0x0003)
                .await
                .map(|v| u64_list(&v).into_iter().map(|e| e as u16).collect())
                .unwrap_or_default();
            let members: Vec<&EndpointDesc> = descs
                .iter()
                .filter(|d| d.ep == ep || children.contains(&d.ep))
                .collect();
            let b_label = text(read_attr(conn, ep, CLUSTER_BRIDGED_BASIC, 0x0005).await);
            let b_product = text(read_attr(conn, ep, CLUSTER_BRIDGED_BASIC, 0x0003).await);
            let b_vendor = text(read_attr(conn, ep, CLUSTER_BRIDGED_BASIC, 0x0001).await);
            let endpoints = scan(conn, &members).await;
            let device_types = type_names(&members);
            claimed.extend(members.iter().map(|d| d.ep));
            bridged.push(StoredBridgedDevice {
                device_id: 0, // attribué (ou conservé) par `set_interview`
                endpoint: ep,
                name: b_label
                    .or_else(|| b_product.clone())
                    .unwrap_or_else(|| format!("Appareil {ep}")),
                vendor: b_vendor,
                product: b_product,
                device_types,
                endpoints,
                custom_name: None,
                icon: None,
            });
        }
    }
    let own: Vec<&EndpointDesc> = descs.iter().filter(|d| !claimed.contains(&d.ep)).collect();
    let endpoints = scan(conn, &own).await;
    let device_types = type_names(&own);

    let removed = ctx.set_interview(
        node_id,
        Interview {
            name: label.or_else(|| product.clone()),
            vendor,
            product,
            device_types,
            endpoints,
            bridged,
        },
    );
    for id in removed {
        ctx.history.remove(id);
        ctx.events.send(Event::NodeRemoved(id));
    }
    if let Some(n) = ctx.node(node_id) {
        super::announce(ctx, &n);
        if !n.bridged.is_empty() {
            ctx.events.send(Event::Info(format!(
                "Pont « {} » : {} appareil(s) exposé(s).",
                n.custom_name.as_deref().unwrap_or(&n.name),
                n.bridged.len()
            )));
        }
    }
    Ok(())
}

/// Noms des types d'appareil (types « utilitaires » ignorés : nœud racine, alimentation…).
fn device_type_name(id: u32) -> Option<String> {
    let name = match id {
        0x0016 | 0x0011 | 0x0012 | 0x0014 | 0x0013 => return None,
        0x0302 => "Capteur de température",
        0x0307 => "Capteur d'humidité",
        0x0100 => "Lampe on/off",
        0x0101 => "Lampe à variateur",
        0x010C => "Lampe à température de couleur",
        0x010D => "Lampe couleur",
        0x010A => "Prise",
        0x0015 => "Capteur d'ouverture",
        0x0107 => "Capteur de présence",
        0x0106 => "Capteur de luminosité",
        0x002C => "Capteur de qualité de l'air",
        0x000F => "Interrupteur",
        0x0202 => "Volet / store",
        0x000E => "Passerelle",
        _ => {
            return Some(
                matc::clusters::dt_names::get_device_type_name(id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Type {id:#06x}")),
            )
        }
    };
    Some(name.to_owned())
}

/// Transforme la valeur d'un attribut suivi en événement pour l'interface.
fn publish(
    ctx: &Ctx,
    routes: &Routes,
    endpoint: u16,
    cluster: u32,
    attribute: u32,
    value: &Value,
    at: f64,
) {
    let node_id = routes.device(endpoint);
    // Chaque mesure part vers l'interface et vers l'historique sur disque.
    let emit = |event: Event| {
        ctx.history.record(&event);
        ctx.events.send(event);
    };
    let measure = |kind, v: Option<f64>| {
        if let Some(value) = v {
            emit(Event::Measurement {
                node_id,
                kind,
                value,
                at,
            });
        }
    };
    let state = |kind, v: Option<bool>| {
        if let Some(value) = v {
            emit(Event::State {
                node_id,
                kind,
                value,
                at,
            });
        }
    };
    match (cluster, attribute) {
        (CLUSTER_TEMPERATURE, ATTR_MEASURED_VALUE) => {
            measure(MeasureKind::Temperature, temperature_from_tlv(value))
        }
        (CLUSTER_HUMIDITY, ATTR_MEASURED_VALUE) => {
            measure(MeasureKind::Humidity, humidity_from_tlv(value))
        }
        (CLUSTER_POWER_SOURCE, ATTR_BAT_PERCENT_REMAINING) => {
            measure(MeasureKind::Battery, battery_percent_from_tlv(value))
        }
        (CLUSTER_ELECTRICAL_POWER, ATTR_ACTIVE_POWER) => {
            measure(MeasureKind::Power, power_w_from_tlv(value))
        }
        (CLUSTER_ELECTRICAL_ENERGY, ATTR_CUMULATIVE_ENERGY_IMPORTED) => {
            measure(MeasureKind::Energy, energy_kwh_from_tlv(value))
        }
        (CLUSTER_WINDOW_COVERING, ATTR_COVER_CURRENT_LIFT) => {
            measure(MeasureKind::Position, cover_open_percent_from_tlv(value))
        }
        (CLUSTER_WINDOW_COVERING, ATTR_COVER_OPERATIONAL_STATUS) => {
            if let Some(motion) = cover_motion_from_tlv(value) {
                ctx.events.send(Event::CoverMotion { node_id, motion });
            }
        }
        (CLUSTER_BOOLEAN_STATE, ATTR_STATE_VALUE) => state(StateKind::Open, open_from_tlv(value)),
        (CLUSTER_POWER_SOURCE, ATTR_BAT_CHARGE_LEVEL) => {
            state(StateKind::BatteryLow, battery_low_from_tlv(value))
        }
        (CLUSTER_ON_OFF, ATTR_ON_OFF) => state(
            StateKind::OnOff,
            matches!(value, Value::Bool(_)).then(|| *value == Value::Bool(true)),
        ),
        (CLUSTER_BRIDGED_BASIC, ATTR_REACHABLE) => {
            if let Value::Bool(reachable) = value {
                ctx.set_bridge_reachable(node_id, *reachable);
                ctx.events.send(Event::Status {
                    node_id,
                    status: if *reachable {
                        NodeStatus::Online
                    } else {
                        NodeStatus::Unreachable {
                            error: "déclaré injoignable par le pont".into(),
                            retry_in_s: 0,
                        }
                    },
                });
            }
        }
        _ => {}
    }
}

fn handle_reports(ctx: &Ctx, routes: &Routes, msg: &Message) {
    let Ok(reports) = parse_report_data(&msg.payload) else {
        return;
    };
    let at = now_unix();
    for r in reports {
        if let Ok(value) = &r.data {
            publish(ctx, routes, r.endpoint, r.cluster, r.attribute, value, at);
        }
    }
}

/// Accusé de réception applicatif (StatusResponse) d'un ReportData.
async fn ack_report(conn: &Connection, msg: &Message) -> Result<()> {
    let flags = if msg.protocol_header.exchange_flags & Ph::FLAG_INITIATOR == 0 {
        Ph::FLAG_INITIATOR | Ph::FLAG_ACK
    } else {
        Ph::FLAG_ACK
    };
    timeout(
        REQUEST_TIMEOUT,
        conn.im_status_response(
            msg.protocol_header.exchange_id,
            flags,
            msg.message_header.message_counter,
        ),
    )
    .await
    .map_err(|_| anyhow!("délai dépassé à l'accusé de réception"))?
}

/// Une seule souscription pour tous les attributs suivis ; retourne le MaxInterval négocié (s).
async fn subscribe(
    ctx: &Ctx,
    routes: &Routes,
    conn: &Connection,
    paths: &[(u16, u32, u32)],
) -> Result<u64> {
    let node_id = routes.node_id;
    let priming = timeout(
        REQUEST_TIMEOUT,
        conn.im_subscribe_request_paths(paths, false, SUB_MIN_FLOOR_S, SUB_MAX_CEILING_S),
    )
    .await
    .map_err(|_| anyhow!("pas de réponse à la souscription"))??;
    if priming.protocol_header.opcode != Ph::INTERACTION_OPCODE_REPORT_DATA {
        bail!(
            "souscription refusée (opcode {:#x})",
            priming.protocol_header.opcode
        );
    }
    handle_reports(ctx, routes, &priming);
    seen(ctx, node_id);
    ack_report(conn, &priming).await?;

    // Le rapport initial peut être découpé ; le SubscribeResponse (avec le MaxInterval) suit.
    let mut max_interval = SUB_MAX_CEILING_S as u64;
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    loop {
        let ev = match tokio::time::timeout_at(deadline, conn.recv_event()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => bail!("connexion fermée pendant la souscription"),
            Err(_) => {
                log::warn!("SubscribeResponse non reçu, MaxInterval supposé {max_interval} s");
                break;
            }
        };
        match ev.protocol_header.opcode {
            Ph::INTERACTION_OPCODE_SUBSCRIBE_RESP => {
                if let Some(mi) = parse_subscribe_response_max_interval(&ev.payload) {
                    max_interval = mi.max(1);
                }
                break;
            }
            Ph::INTERACTION_OPCODE_REPORT_DATA => {
                handle_reports(ctx, routes, &ev);
                ack_report(conn, &ev).await?;
            }
            _ => {}
        }
    }
    Ok(max_interval)
}

/// Écoute les rapports. Sans nouvelles au-delà de MaxInterval + marge, la
/// session est considérée perdue et le superviseur se reconnecte.
async fn listen(ctx: &Ctx, routes: &Routes, conn: &Connection, max_interval: u64) -> Result<()> {
    let node_id = routes.node_id;
    let silence = Duration::from_secs(max_interval + 30);
    loop {
        let ev = match timeout(silence, conn.recv_event()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => bail!("connexion fermée"),
            Err(_) => bail!("aucun rapport depuis {} s", silence.as_secs()),
        };
        match ev.protocol_header.opcode {
            Ph::INTERACTION_OPCODE_REPORT_DATA => {
                handle_reports(ctx, routes, &ev);
                seen(ctx, node_id);
                status(ctx, node_id, NodeStatus::Online);
                ack_report(conn, &ev).await?;
            }
            Ph::INTERACTION_OPCODE_SUBSCRIBE_RESP => {}
            other => log::debug!("nœud {node_id:016X} : opcode {other:#x} ignoré"),
        }
    }
}

async fn poll_loop(
    ctx: &Ctx,
    routes: &Routes,
    conn: &Connection,
    paths: &[(u16, u32, u32)],
) -> Result<()> {
    let node_id = routes.node_id;
    let mut failures = 0;
    loop {
        let mut ok = false;
        let mut last_err = None;
        for (ep, cluster, attr) in paths {
            match read_attr(conn, *ep, *cluster, *attr).await {
                Ok(v) => {
                    ok = true;
                    publish(ctx, routes, *ep, *cluster, *attr, &v, now_unix());
                }
                Err(e) => last_err = Some(e),
            }
        }
        if ok {
            failures = 0;
            seen(ctx, node_id);
            status(ctx, node_id, NodeStatus::Online);
        } else {
            failures += 1;
            if failures >= 2 {
                return Err(last_err.unwrap_or_else(|| anyhow!("lecture impossible")));
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Appareils sans attribut suivi : simple lecture périodique pour suivre l'état.
async fn liveness_loop(ctx: &Ctx, node_id: u64, conn: &Connection) -> Result<()> {
    let mut failures = 0;
    loop {
        tokio::time::sleep(LIVENESS_EVERY).await;
        // Basic Information / DataModelRevision : petit attribut toujours présent.
        match read_attr(conn, 0, CLUSTER_BASIC_INFORMATION, 0x0000).await {
            Ok(_) => {
                failures = 0;
                seen(ctx, node_id);
            }
            Err(e) => {
                failures += 1;
                if failures >= 2 {
                    return Err(e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_with_bridge() -> StoredNode {
        StoredNode {
            node_id: 0x10,
            endpoints: TrackedEndpoints {
                battery_endpoint: Some(0),
                ..Default::default()
            },
            bridged: vec![
                StoredBridgedDevice {
                    device_id: 0xFFFF_FFF0_0000_0001,
                    endpoint: 3,
                    endpoints: TrackedEndpoints {
                        cover_endpoint: Some(3),
                        cover_position: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                StoredBridgedDevice {
                    device_id: 0xFFFF_FFF0_0000_0002,
                    endpoint: 4,
                    endpoints: TrackedEndpoints {
                        cover_endpoint: Some(5), // volet composé : moteur sur un sous-endpoint
                        cover_position: false,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn bridged_paths_and_routes() {
        let n = node_with_bridge();
        let paths = tracked_paths(&n);
        assert!(paths.contains(&(0, CLUSTER_POWER_SOURCE, ATTR_BAT_PERCENT_REMAINING)));
        assert!(paths.contains(&(3, CLUSTER_WINDOW_COVERING, ATTR_COVER_CURRENT_LIFT)));
        assert!(paths.contains(&(3, CLUSTER_WINDOW_COVERING, ATTR_COVER_OPERATIONAL_STATUS)));
        assert!(paths.contains(&(3, CLUSTER_BRIDGED_BASIC, ATTR_REACHABLE)));
        // Sans position connue : pas de souscription à la position.
        assert!(!paths.contains(&(5, CLUSTER_WINDOW_COVERING, ATTR_COVER_CURRENT_LIFT)));
        assert!(paths.contains(&(5, CLUSTER_WINDOW_COVERING, ATTR_COVER_OPERATIONAL_STATUS)));
        assert!(paths.contains(&(4, CLUSTER_BRIDGED_BASIC, ATTR_REACHABLE)));

        let routes = Routes::of(&n);
        assert_eq!(routes.device(0), 0x10);
        assert_eq!(routes.device(3), 0xFFFF_FFF0_0000_0001);
        assert_eq!(routes.device(4), 0xFFFF_FFF0_0000_0002);
        assert_eq!(routes.device(5), 0xFFFF_FFF0_0000_0002);
        assert_eq!(routes.device(9), 0x10);
    }

    #[test]
    fn device_type_names_for_covers_and_bridges() {
        assert_eq!(device_type_name(0x0202).as_deref(), Some("Volet / store"));
        assert_eq!(device_type_name(0x000E).as_deref(), Some("Passerelle"));
        assert_eq!(device_type_name(0x0013), None, "Bridged Node : utilitaire");
    }

    #[test]
    fn fresh_addresses_skip_the_stored_one() {
        let found = vec!["[fdb2::1]:5540".to_string(), "[2001::1]:5540".to_string()];
        assert_eq!(
            fresh_addresses("[2001::1]:5540", &found),
            vec!["[fdb2::1]:5540"]
        );
        assert_eq!(fresh_addresses("", &found).len(), 2);
        let twice = vec!["[fdb2::1]:5540".to_string(), "[fdb2::1]:5540".to_string()];
        assert_eq!(fresh_addresses("", &twice).len(), 1, "doublons écartés");
    }

    /// Faux transport : `plan` donne, par adresse, le délai puis le résultat.
    struct Fake {
        plan: Vec<(&'static str, u64, bool)>,
        started: std::sync::Mutex<Vec<String>>,
        dropped: std::sync::atomic::AtomicUsize,
    }
    struct DropGuard<'a>(&'a Fake);
    impl Drop for DropGuard<'_> {
        fn drop(&mut self) {
            self.0
                .dropped
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl Fake {
        fn new(plan: Vec<(&'static str, u64, bool)>) -> Self {
            Self {
                plan,
                started: Default::default(),
                dropped: Default::default(),
            }
        }
        async fn attempt(&self, address: String) -> Result<u32> {
            self.started.lock().unwrap().push(address.clone());
            let guard = DropGuard(self);
            let (_, ms, ok) = *self.plan.iter().find(|p| p.0 == address).unwrap();
            tokio::time::sleep(Duration::from_millis(ms)).await;
            std::mem::forget(guard);
            if ok {
                Ok(1)
            } else {
                bail!("échec {address}")
            }
        }
        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
        fn dropped(&self) -> usize {
            self.dropped.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    const DEAD: &str = "[2001::1]:5540";
    const ULA: &str = "[fdb2::1]:5540";

    #[tokio::test]
    async fn healthy_stored_address_never_starts_discovery() {
        let fake = Fake::new(vec![(DEAD, 5, true)]);
        let called = std::sync::atomic::AtomicBool::new(false);
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_secs(10),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async {
                called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![ULA.to_string()])
            },
        )
        .await
        .unwrap();
        assert_eq!(addr, DEAD);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dead_stored_address_is_cancelled_when_mdns_differs() {
        // L'adresse mémorisée ne répond jamais (10 s) ; mDNS annonce l'ULA.
        let fake = Fake::new(vec![(DEAD, 10_000, false), (ULA, 5, true)]);
        let t = std::time::Instant::now();
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_millis(30),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![ULA.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, ULA);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(fake.started(), vec![DEAD, ULA]);
        assert_eq!(
            fake.dropped(),
            1,
            "l'essai mémorisé est annulé, pas orphelin"
        );
    }

    #[tokio::test]
    async fn failing_stored_address_starts_discovery_without_grace() {
        let fake = Fake::new(vec![(DEAD, 5, false), (ULA, 5, true)]);
        let t = std::time::Instant::now();
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_secs(30),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![ULA.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, ULA);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn slow_but_announced_stored_address_still_wins() {
        // mDNS confirme l'adresse mémorisée : l'appareil à sommeil a le temps de répondre.
        let fake = Fake::new(vec![(DEAD, 200, true), (ULA, 10_000, false)]);
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_millis(20),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![DEAD.to_string(), ULA.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, DEAD);
        assert_eq!(fake.started(), vec![DEAD, ULA]);
        assert_eq!(
            fake.dropped(),
            1,
            "la fraîche lâchée, sans session conservée"
        );
    }

    #[tokio::test]
    async fn announced_stored_address_failing_falls_back_to_the_others() {
        let fake = Fake::new(vec![(DEAD, 80, false), (ULA, 5, true)]);
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_millis(10),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![DEAD.to_string(), ULA.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, ULA);
        assert_eq!(fake.started(), vec![DEAD, ULA]);
    }

    #[tokio::test]
    async fn slow_stored_address_survives_dead_mdns_addresses() {
        // Appareil à sommeil : la mémorisée répond en 400 ms, bien après la découverte ;
        // mDNS ne connaît que des adresses mortes (cache, réponse partielle).
        let dead2 = "[fdb2::2]:5540";
        let fake = Fake::new(vec![
            (DEAD, 400, true),
            (ULA, 10_000, false),
            (dead2, 10_000, false),
        ]);
        let t = std::time::Instant::now();
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_millis(20),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![ULA.to_string(), dead2.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, DEAD);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(fake.dropped(), 2, "les fraîches mortes sont lâchées");
    }

    #[tokio::test]
    async fn dead_first_fresh_address_does_not_block_the_second() {
        // Mémorisée en échec rapide, première fraîche muette (10 s), seconde bonne.
        let good = "[fdb2::2]:5540";
        let fake = Fake::new(vec![
            (DEAD, 5, false),
            (ULA, 10_000, false),
            (good, 5, true),
        ]);
        let t = std::time::Instant::now();
        let (_, addr) = connect_race(
            DEAD,
            Duration::from_secs(30),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![ULA.to_string(), good.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, good);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(fake.started(), vec![DEAD, ULA, good]);
        assert_eq!(fake.dropped(), 1);
    }

    #[tokio::test]
    async fn no_stored_address_goes_straight_to_discovery() {
        let fake = Fake::new(vec![(ULA, 5, true)]);
        let (_, addr) = connect_race(
            "",
            Duration::from_secs(30),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { Ok(vec![ULA.to_string()]) },
        )
        .await
        .unwrap();
        assert_eq!(addr, ULA);
    }

    #[tokio::test]
    async fn errors_are_combined_when_everything_fails() {
        let fake = Fake::new(vec![(DEAD, 5, false)]);
        let err = connect_race(
            DEAD,
            Duration::from_millis(5),
            Duration::from_millis(50),
            |a| fake.attempt(a),
            || async { bail!("introuvable en mDNS") },
        )
        .await
        .err()
        .unwrap();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("échec") && msg.contains("introuvable en mDNS"),
            "{msg}"
        );
    }
}
