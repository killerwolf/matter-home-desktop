//! Mode démo (`--demo`) : faux appareils et mesures simulées, pour tester
//! l'interface sans réseau Thread. Mêmes canaux et événements que le vrai backend.

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::Rng;
use tokio::sync::mpsc;

use crate::automation::{
    self, Action, Actuator, BoxFuture, Condition, CoverCommand, OnOffCommand, Rule, RuleSet,
    Trigger,
};
use crate::events::{
    now_unix, BridgeLink, Capabilities, Command, CoverMotion, Event, EventSink, MeasureKind,
    NodeInfo, NodeStatus, ReportingMode, StateKind,
};
use crate::matter::setup_code;
use crate::persistence::BRIDGED_ID_MIN;

const LIVING: u64 = 0xD3_0001;
const BEDROOM: u64 = 0xD3_0002;
const DOOR: u64 = 0xD3_0003;
const PLUG: u64 = 0xD3_0004;
/// Pont de démo (une TaHoma) et ses volets, avec des identifiants de la plage réservée
/// aux appareils pontés, comme en mode réel.
const BRIDGE: u64 = 0xD3_0005;
const SHUTTER_LIVING: u64 = BRIDGED_ID_MIN + 0xD3_0001;
const SHUTTER_KITCHEN: u64 = BRIDGED_ID_MIN + 0xD3_0002;
const SHUTTER_BEDROOM: u64 = BRIDGED_ID_MIN + 0xD3_0003;

/// Température simulée : cycle lent + petite oscillation + bruit.
fn simulated(t: f64, base: f64, rng: &mut impl Rng) -> f64 {
    let slow = 1.2 * (TAU * t / 3600.0).sin();
    let fast = 0.3 * (TAU * t / 420.0).sin();
    let noise: f64 = rng.gen_range(-0.05..0.05);
    ((base + slow + fast + noise) * 100.0).round() / 100.0
}

fn humidity(t: f64, rng: &mut impl Rng) -> f64 {
    let v = 45.0 + 4.0 * (TAU * t / 2700.0).cos() + rng.gen_range(-0.3..0.3);
    (v * 100.0).round() / 100.0
}

/// Prise d'un réfrigérateur : compresseur ~85 W pendant 6 min sur 15, sinon ~2 W.
fn fridge_power(t: f64, rng: &mut impl Rng) -> f64 {
    let base = if t.rem_euclid(900.0) < 360.0 {
        85.0
    } else {
        2.0
    };
    ((base + rng.gen_range(-1.5..1.5_f64)).max(0.0) * 10.0).round() / 10.0
}

/// Porte : ouverte 20 s toutes les 2 min environ.
fn door_open(t: f64) -> bool {
    t.rem_euclid(120.0) < 20.0
}

fn sensor(node_id: u64, name: &str, has_humidity: bool) -> NodeInfo {
    NodeInfo {
        node_id,
        name: name.into(),
        vendor: Some("IKEA of Sweden (démo)".into()),
        product: Some("TIMMERFLOTTE temp/hmd sensor".into()),
        device_types: if has_humidity {
            vec!["Capteur de température".into(), "Capteur d'humidité".into()]
        } else {
            vec!["Capteur de température".into()]
        },
        caps: Capabilities {
            temperature: true,
            humidity: has_humidity,
            battery: true,
            ..Default::default()
        },
        device_name: "TIMMERFLOTTE temp/hmd sensor".into(),
        ..Default::default()
    }
}

fn shutter(device_id: u64, endpoint: u16, name: &str, product: &str, position: bool) -> NodeInfo {
    NodeInfo {
        node_id: device_id,
        name: name.into(),
        device_name: name.into(),
        vendor: Some("Somfy (démo)".into()),
        product: Some(product.into()),
        device_types: vec!["Volet / store".into()],
        caps: Capabilities {
            cover: true,
            cover_position: position,
            ..Default::default()
        },
        bridge: Some(BridgeLink {
            node_id: BRIDGE,
            endpoint,
        }),
        ..Default::default()
    }
}

/// Volet simulé : position courante (% ouvert), cible, et s'il connaît sa position.
struct Shutter {
    position: f64,
    target: Option<f64>,
    position_aware: bool,
    /// Moteur sans retour : nombre de pas de mouvement restant à simuler.
    blind_steps: u32,
    motion: CoverMotion,
}

type Shutters = Arc<Mutex<HashMap<u64, Shutter>>>;

/// Vitesse de la simulation : % d'ouverture par pas de 2 s.
const SHUTTER_STEP: f64 = 25.0;

pub fn spawn(events: EventSink) -> crate::Backend {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = std::thread::Builder::new()
        .name("demo-backend".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime tokio");
            rt.block_on(run(rx, events));
        })
        .expect("thread démo");
    crate::Backend::new(tx, vec![handle])
}

async fn run(mut commands: mpsc::UnboundedReceiver<Command>, ui: EventSink) {
    // Automatisations : même moteur que le mode réel, prise et volets simulés.
    let (tap_tx, tap_rx) = mpsc::unbounded_channel();
    let events = ui.with_tap(tap_tx);
    let plug_on = Arc::new(AtomicBool::new(true));
    let shutters: Shutters = Arc::new(Mutex::new(HashMap::from([
        (
            SHUTTER_LIVING,
            Shutter {
                position: 100.0,
                target: None,
                position_aware: true,
                blind_steps: 0,
                motion: CoverMotion::Stopped,
            },
        ),
        (
            SHUTTER_KITCHEN,
            Shutter {
                position: 35.0,
                target: None,
                position_aware: true,
                blind_steps: 0,
                motion: CoverMotion::Stopped,
            },
        ),
        (
            SHUTTER_BEDROOM,
            Shutter {
                position: 0.0,
                target: None,
                position_aware: false,
                blind_steps: 0,
                motion: CoverMotion::Stopped,
            },
        ),
    ])));
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    // Règles de démonstration fixes : le mode démo ne lit ni n'écrit aucun fichier.
    let rules = demo_rules();
    let actuator: Arc<dyn Actuator> = Arc::new(DemoActuator {
        events: events.clone(),
        plug_on: plug_on.clone(),
        shutters: shutters.clone(),
    });
    automation::spawn(rules.rules, tap_rx, engine_rx, ui, actuator.clone());

    let mut rng = rand::thread_rng();
    let measure = |node_id, kind, value, at| {
        events.send(Event::Measurement {
            node_id,
            kind,
            value,
            at,
        })
    };
    let state = |node_id, kind, value, at| {
        events.send(Event::State {
            node_id,
            kind,
            value,
            at,
        })
    };
    events.send(Event::Info(
        "Mode démo : aucune communication réseau, données simulées.".into(),
    ));

    // Nœuds de démo, gardés pour pouvoir appliquer les renommages (non sauvegardés).
    let mut infos: HashMap<u64, NodeInfo> = HashMap::new();
    for info in [
        sensor(LIVING, "Salon (démo)", true),
        sensor(BEDROOM, "Chambre — capteur endormi (démo)", false),
        NodeInfo {
            node_id: DOOR,
            name: "Porte d'entrée (démo)".into(),
            device_name: "MYGGBETT door/window sensor".into(),
            vendor: Some("IKEA of Sweden (démo)".into()),
            product: Some("MYGGBETT door/window sensor".into()),
            device_types: vec!["Capteur d'ouverture".into()],
            caps: Capabilities {
                contact: true,
                battery: true,
                ..Default::default()
            },
            ..Default::default()
        },
        NodeInfo {
            node_id: PLUG,
            name: "Prise du frigo (démo)".into(),
            device_name: "GRILLPLATS Plug".into(),
            vendor: Some("IKEA of Sweden (démo)".into()),
            product: Some("GRILLPLATS Plug".into()),
            device_types: vec!["Prise".into(), "Electrical Sensor".into()],
            caps: Capabilities {
                power: true,
                energy: true,
                on_off: true,
                ..Default::default()
            },
            ..Default::default()
        },
        NodeInfo {
            node_id: BRIDGE,
            name: "TaHoma (démo)".into(),
            device_name: "TaHoma switch".into(),
            vendor: Some("Somfy (démo)".into()),
            product: Some("TaHoma switch".into()),
            device_types: vec!["Passerelle".into()],
            ..Default::default()
        },
        shutter(SHUTTER_LIVING, 3, "Volet du salon (démo)", "Volet io", true),
        shutter(
            SHUTTER_KITCHEN,
            4,
            "Volet de la cuisine (démo)",
            "Volet io",
            true,
        ),
        shutter(
            SHUTTER_BEDROOM,
            5,
            "Volet de la chambre (démo)",
            "Volet RTS",
            false,
        ),
    ] {
        events.send(Event::Node(info.clone()));
        infos.insert(info.node_id, info);
    }

    // 90 minutes d'historique déjà « reçu », pour que les graphiques aient de quoi afficher.
    let now = now_unix();
    let mut energy_kwh = 12.4;
    let mut t = now - 90.0 * 60.0;
    let mut last_door = None;
    while t < now {
        for (id, base) in [(LIVING, 21.0), (BEDROOM, 18.5)] {
            measure(
                id,
                MeasureKind::Temperature,
                simulated(t, base, &mut rng),
                t,
            );
        }
        measure(LIVING, MeasureKind::Humidity, humidity(t, &mut rng), t);
        let p = fridge_power(t, &mut rng);
        energy_kwh += p * 30.0 / 3600.0 / 1000.0;
        measure(PLUG, MeasureKind::Power, p, t);
        // La porte change d'état à l'intérieur de chaque pas : on rejoue ses transitions.
        for k in 0..6 {
            let tk = t + k as f64 * 5.0;
            let open = door_open(tk);
            if last_door != Some(open) {
                state(DOOR, StateKind::Open, open, tk);
                last_door = Some(open);
            }
        }
        t += 30.0;
    }
    // Volets : fermés la nuit, ouverts il y a une heure (le salon), à moitié (la cuisine).
    measure(
        SHUTTER_LIVING,
        MeasureKind::Position,
        0.0,
        now - 90.0 * 60.0,
    );
    measure(
        SHUTTER_LIVING,
        MeasureKind::Position,
        100.0,
        now - 60.0 * 60.0,
    );
    measure(
        SHUTTER_KITCHEN,
        MeasureKind::Position,
        0.0,
        now - 90.0 * 60.0,
    );
    measure(
        SHUTTER_KITCHEN,
        MeasureKind::Position,
        35.0,
        now - 45.0 * 60.0,
    );
    measure(PLUG, MeasureKind::Energy, energy_kwh, now);
    state(PLUG, StateKind::OnOff, true, now);
    for (id, pct) in [(LIVING, 87.0), (BEDROOM, 14.0), (DOOR, 62.0)] {
        measure(id, MeasureKind::Battery, pct, now);
    }
    state(BEDROOM, StateKind::BatteryLow, true, now);
    for id in [SHUTTER_LIVING, SHUTTER_KITCHEN, SHUTTER_BEDROOM] {
        events.send(Event::CoverMotion {
            node_id: id,
            motion: CoverMotion::Stopped,
        });
    }
    for id in [
        LIVING,
        BEDROOM,
        DOOR,
        PLUG,
        BRIDGE,
        SHUTTER_LIVING,
        SHUTTER_KITCHEN,
        SHUTTER_BEDROOM,
    ] {
        events.send(Event::Status {
            node_id: id,
            status: NodeStatus::Online,
        });
        events.send(Event::Seen {
            node_id: id,
            at: now,
        });
    }
    for id in [
        LIVING,
        DOOR,
        PLUG,
        BRIDGE,
        SHUTTER_LIVING,
        SHUTTER_KITCHEN,
        SHUTTER_BEDROOM,
    ] {
        events.send(Event::Mode {
            node_id: id,
            mode: ReportingMode::Subscription {
                max_interval_s: 300,
            },
        });
    }
    events.send(Event::Mode {
        node_id: BEDROOM,
        mode: ReportingMode::Polling { every_s: 30 },
    });

    let mut paired: Vec<u64> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut n: u64 = 0;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                n += 1;
                let t = now_unix();
                // Salon : souscription, un rapport toutes les 2 s (accéléré pour la démo).
                for id in std::iter::once(LIVING).chain(paired.iter().copied()) {
                    let base = if id == LIVING { 21.0 } else { 23.0 };
                    measure(id, MeasureKind::Temperature, simulated(t, base, &mut rng), t);
                    events.send(Event::Seen { node_id: id, at: t });
                }
                if n.is_multiple_of(5) {
                    measure(LIVING, MeasureKind::Humidity, humidity(t, &mut rng), t);
                }
                // Porte : rapport à chaque changement d'état.
                let open = door_open(t);
                if last_door != Some(open) {
                    state(DOOR, StateKind::Open, open, t);
                    events.send(Event::Seen { node_id: DOOR, at: t });
                    last_door = Some(open);
                }
                // Prise : puissance toutes les 10 s, compteur d'énergie toutes les 30 s.
                if n.is_multiple_of(5) {
                    let p = if plug_on.load(Ordering::Relaxed) { fridge_power(t, &mut rng) } else { 0.0 };
                    energy_kwh += p * 10.0 / 3600.0 / 1000.0;
                    measure(PLUG, MeasureKind::Power, p, t);
                    events.send(Event::Seen { node_id: PLUG, at: t });
                }
                if n.is_multiple_of(15) {
                    measure(PLUG, MeasureKind::Energy, energy_kwh, t);
                }
                // Volets : un pas vers la cible, puis arrêt ; le pont rapporte pour tous.
                for (id, position, motion) in step_shutters(&shutters) {
                    if let Some(p) = position {
                        measure(id, MeasureKind::Position, p, t);
                    }
                    if let Some(m) = motion {
                        events.send(Event::CoverMotion { node_id: id, motion: m });
                    }
                    for node_id in [id, BRIDGE] {
                        events.send(Event::Seen { node_id, at: t });
                    }
                }
                // Chambre : interrogée toutes les 30 s, injoignable 40 s toutes les 3 min.
                let phase = n % 90;
                if phase == 60 {
                    events.send(Event::Status {
                        node_id: BEDROOM,
                        status: NodeStatus::Unreachable {
                            error: "pas de réponse (lecture 1/0x0402/0x0000) — simulé".into(),
                            retry_in_s: 20,
                        },
                    });
                } else if phase == 70 {
                    events.send(Event::Status { node_id: BEDROOM, status: NodeStatus::Connecting });
                } else if phase == 80 {
                    events.send(Event::Status { node_id: BEDROOM, status: NodeStatus::Online });
                }
                if (phase < 60 && n.is_multiple_of(15)) || phase == 80 {
                    measure(BEDROOM, MeasureKind::Temperature, simulated(t, 18.5, &mut rng), t);
                    events.send(Event::Seen { node_id: BEDROOM, at: t });
                }
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::Pair(code) => {
                        if let Err(e) = setup_code::parse(&code) {
                            events.send(Event::PairingFailed(e.to_string()));
                            continue;
                        }
                        events.send(Event::PairingProgress("Recherche de l'appareil en mode appairage (simulé)…".into()));
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        events.send(Event::PairingProgress("Authentification PASE et certificats (simulé)…".into()));
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        let id = 0xD3_1000 + paired.len() as u64;
                        let name = format!("Capteur appairé n°{} (démo)", paired.len() + 1);
                        paired.push(id);
                        let info = sensor(id, &name, false);
                        events.send(Event::Node(info.clone()));
                        infos.insert(id, info);
                        events.send(Event::Status { node_id: id, status: NodeStatus::Online });
                        events.send(Event::Mode { node_id: id, mode: ReportingMode::Subscription { max_interval_s: 30 } });
                        events.send(Event::PairingDone { node_id: id, name });
                    }
                    // Le mode démo n'enregistre rien sur disque : rien à effacer côté backend.
                    Command::ClearHistory(_) => {}
                    Command::SetRules(rules) => {
                        let _ = engine_tx.send(automation::EngineCommand::SetRules(rules));
                    }
                    Command::TestRule(id) => {
                        let _ = engine_tx.send(automation::EngineCommand::Test(id));
                    }
                    Command::Customize { node_id, name, icon } => {
                        if let Some(info) = infos.get_mut(&node_id) {
                            info.custom_name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                            info.name = info.custom_name.clone().unwrap_or_else(|| info.device_name.clone());
                            info.icon = icon;
                            events.send(Event::Node(info.clone()));
                        }
                    }
                    Command::SetCover { node_id, command } => {
                        let name = infos.get(&node_id).map_or_else(|| format!("{node_id:016X}"), |i| i.name.clone());
                        let what = format!("{name} : {}", command.label().to_lowercase());
                        match actuator.set_cover(node_id, command).await {
                            Ok(()) => events.send(Event::Info(format!("{what} — commande envoyée (simulé)."))),
                            Err(e) => events.send(Event::Info(format!("{what} — échec : {e:#}"))),
                        }
                    }
                    Command::SetOnOff { node_id, command } => {
                        let name = infos.get(&node_id).map_or_else(|| format!("{node_id:016X}"), |i| i.name.clone());
                        let what = format!("{name} : {}", command.label().to_lowercase());
                        match actuator.set_on_off(node_id, command).await {
                            Ok(()) => events.send(Event::Info(format!("{what} — commande envoyée (simulé)."))),
                            Err(e) => events.send(Event::Info(format!("{what} — échec : {e:#}"))),
                        }
                    }
                    Command::Retry(id) => {
                        events.send(Event::Status { node_id: id, status: NodeStatus::Connecting });
                    }
                    Command::Forget(id) => {
                        paired.retain(|p| *p != id);
                        events.send(Event::NodeRemoved(id));
                    }
                }
            }
        }
    }
}

/// Avance chaque volet d'un pas vers sa cible. Retourne, par volet, la position à
/// publier (si connue) et le mouvement s'il a changé.
fn step_shutters(shutters: &Shutters) -> Vec<(u64, Option<f64>, Option<CoverMotion>)> {
    let mut out = Vec::new();
    let mut map = shutters.lock().unwrap_or_else(|e| e.into_inner());
    for (id, s) in map.iter_mut() {
        let before = s.motion;
        let mut position = None;
        if s.position_aware {
            if let Some(target) = s.target {
                let delta = target - s.position;
                if delta.abs() <= SHUTTER_STEP {
                    s.position = target;
                    s.target = None;
                    s.motion = CoverMotion::Stopped;
                } else {
                    s.position += SHUTTER_STEP * delta.signum();
                }
                position = Some(s.position);
            }
        } else if s.blind_steps > 0 {
            s.blind_steps -= 1;
            if s.blind_steps == 0 {
                s.motion = CoverMotion::Stopped;
            }
        }
        let motion = (s.motion != before).then_some(s.motion);
        if position.is_some() || motion.is_some() {
            out.push((*id, position, motion));
        }
    }
    out
}

/// Plan de démonstration : deux étages, appareils déjà placés.
pub fn demo_plan() -> crate::home::HomePlan {
    use crate::home::{Floor, HomePlan, Item, ItemKind, Room, Symbol};
    let room = |id, name: &str, x, y, w, h| Room {
        id,
        name: name.into(),
        x,
        y,
        w,
        h,
    };
    let device = |id, node_id, symbol, x, y| Item {
        id,
        kind: ItemKind::Device { node_id, symbol },
        x,
        y,
    };
    let decor = |id, symbol, label: &str, x, y| Item {
        id,
        kind: ItemKind::Decor {
            symbol,
            label: label.into(),
        },
        x,
        y,
    };
    HomePlan {
        version: 1,
        floors: vec![
            Floor {
                id: 1,
                name: "Rez-de-chaussée".into(),
                rooms: vec![
                    room(2, "Salon", 0.0, 0.0, 6.0, 4.5),
                    room(3, "Cuisine", 6.0, 0.0, 4.0, 3.0),
                    room(4, "Entrée", 6.0, 3.0, 4.0, 1.5),
                ],
                items: vec![
                    device(10, LIVING, None, 2.5, 2.0),
                    device(11, DOOR, None, 9.0, 4.25),
                    device(12, PLUG, None, 9.0, 1.25),
                    decor(13, Symbol::Window, "Baie vitrée", 3.0, 4.25),
                    decor(14, Symbol::Light, "Suspension", 4.5, 1.0),
                    device(15, SHUTTER_LIVING, None, 1.0, 4.25),
                    device(16, SHUTTER_KITCHEN, None, 7.5, 0.25),
                    device(17, BRIDGE, None, 8.0, 2.25),
                ],
            },
            Floor {
                id: 20,
                name: "Étage".into(),
                rooms: vec![
                    room(21, "Chambre", 0.0, 0.0, 4.0, 3.5),
                    room(22, "Bureau", 4.0, 0.0, 3.0, 3.5),
                ],
                items: vec![
                    device(23, BEDROOM, None, 2.0, 1.75),
                    decor(24, Symbol::Window, "Velux", 5.5, 0.25),
                    device(25, SHUTTER_BEDROOM, None, 2.0, 3.25),
                ],
            },
        ],
    }
}

/// Prise et volets simulés : seule la « prise du frigo » se commande en mode démo.
struct DemoActuator {
    events: EventSink,
    plug_on: Arc<AtomicBool>,
    shutters: Shutters,
}

impl Actuator for DemoActuator {
    fn set_on_off(&self, node_id: u64, command: OnOffCommand) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            if node_id != PLUG {
                anyhow::bail!("cet appareil ne se commande pas (démo)");
            }
            let on = match command {
                OnOffCommand::On => true,
                OnOffCommand::Off => false,
                OnOffCommand::Toggle => !self.plug_on.load(Ordering::Relaxed),
            };
            self.plug_on.store(on, Ordering::Relaxed);
            let at = now_unix();
            self.events.send(Event::State {
                node_id,
                kind: StateKind::OnOff,
                value: on,
                at,
            });
            if !on {
                self.events.send(Event::Measurement {
                    node_id,
                    kind: MeasureKind::Power,
                    value: 0.0,
                    at,
                });
            }
            Ok(())
        })
    }

    fn set_cover(&self, node_id: u64, command: CoverCommand) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            let mut map = self.shutters.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = map.get_mut(&node_id) else {
                anyhow::bail!("cet appareil n'est pas un volet (démo)");
            };
            let target = match command {
                CoverCommand::Open => 100.0,
                CoverCommand::Close => 0.0,
                CoverCommand::Position(p) => f64::from(p.min(100)),
                CoverCommand::Stop => {
                    s.target = None;
                    s.blind_steps = 0;
                    s.motion = CoverMotion::Stopped;
                    self.events.send(Event::CoverMotion {
                        node_id,
                        motion: CoverMotion::Stopped,
                    });
                    return Ok(());
                }
            };
            if s.position_aware {
                s.target = (target != s.position).then_some(target);
                s.motion = match s.target {
                    Some(t) if t > s.position => CoverMotion::Opening,
                    Some(_) => CoverMotion::Closing,
                    None => CoverMotion::Stopped,
                };
            } else {
                // Moteur sans retour : le pont ne sait que « ça bouge », quelques secondes.
                s.blind_steps = 3;
                s.motion = if target > 50.0 {
                    CoverMotion::Opening
                } else {
                    CoverMotion::Closing
                };
            }
            self.events.send(Event::CoverMotion {
                node_id,
                motion: s.motion,
            });
            Ok(())
        })
    }
}

/// Règles d'exemple du mode démo.
pub fn demo_rules() -> RuleSet {
    RuleSet {
        version: 1,
        rules: vec![
            Rule {
                id: 1,
                name: "Porte d'entrée ouverte".into(),
                enabled: true,
                trigger: Trigger::Contact {
                    node_id: DOOR,
                    open: true,
                },
                conditions: Vec::new(),
                actions: vec![Action::Notify {
                    title: "{appareil}".into(),
                    message: "Porte {valeur} à {heure}".into(),
                }],
                // La porte de démo s'ouvre toutes les 2 min : une notification par quart d'heure suffit.
                cooldown_s: 900,
            },
            Rule {
                id: 2,
                name: "Frigo : consommation élevée".into(),
                enabled: true,
                trigger: Trigger::Threshold {
                    node_id: PLUG,
                    measure: MeasureKind::Power,
                    above: true,
                    value: 80.0,
                    for_s: 120,
                },
                conditions: Vec::new(),
                actions: vec![Action::Notify {
                    title: "{regle}".into(),
                    message: "{appareil} consomme {valeur} depuis 2 min".into(),
                }],
                cooldown_s: 900,
            },
            Rule {
                id: 3,
                name: "Couper la prise la nuit".into(),
                enabled: false,
                trigger: Trigger::Time {
                    hour: 23,
                    minute: 0,
                    days: [true; 7],
                },
                conditions: vec![Condition::Switch {
                    node_id: PLUG,
                    on: true,
                }],
                actions: vec![Action::SetOnOff {
                    node_id: PLUG,
                    command: OnOffCommand::Off,
                }],
                cooldown_s: 60,
            },
            Rule {
                id: 4,
                name: "Fermer les volets le soir".into(),
                enabled: false,
                trigger: Trigger::Time {
                    hour: 21,
                    minute: 30,
                    days: [true; 7],
                },
                conditions: Vec::new(),
                actions: vec![
                    Action::SetCover {
                        node_id: SHUTTER_LIVING,
                        command: CoverCommand::Close,
                    },
                    Action::SetCover {
                        node_id: SHUTTER_KITCHEN,
                        command: CoverCommand::Position(20),
                    },
                    Action::SetCover {
                        node_id: SHUTTER_BEDROOM,
                        command: CoverCommand::Close,
                    },
                ],
                cooldown_s: 60,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::home::Symbol;

    /// Chaîne complète sans interface : « Tester » → moteur → actionneur → compte rendu.
    #[test]
    fn test_rule_switches_simulated_plug() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (ui_tx, ui_rx) = std::sync::mpsc::channel();
            let ui = EventSink::new(ui_tx);
            let (tap_tx, tap_rx) = mpsc::unbounded_channel();
            let events = ui.with_tap(tap_tx);
            let plug_on = Arc::new(AtomicBool::new(true));
            let shutters: Shutters = Arc::new(Mutex::new(HashMap::from([(
                SHUTTER_LIVING,
                Shutter {
                    position: 100.0,
                    target: None,
                    position_aware: true,
                    blind_steps: 0,
                    motion: CoverMotion::Stopped,
                },
            )])));
            let (engine_tx, engine_rx) = mpsc::unbounded_channel();
            let rule = Rule {
                id: 9,
                name: "Couper".into(),
                enabled: true,
                trigger: Trigger::Time {
                    hour: 3,
                    minute: 0,
                    days: [true; 7],
                },
                conditions: Vec::new(),
                actions: vec![
                    Action::SetOnOff {
                        node_id: PLUG,
                        command: OnOffCommand::Toggle,
                    },
                    Action::SetCover {
                        node_id: SHUTTER_LIVING,
                        command: CoverCommand::Position(40),
                    },
                    Action::SetOnOff {
                        node_id: DOOR,
                        command: OnOffCommand::On,
                    },
                ],
                cooldown_s: 0,
            };
            let actuator = DemoActuator {
                events: events.clone(),
                plug_on: plug_on.clone(),
                shutters: shutters.clone(),
            };
            automation::spawn(vec![rule], tap_rx, engine_rx, ui, Arc::new(actuator));
            engine_tx.send(automation::EngineCommand::Test(9)).unwrap();

            let mut report = None;
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                while let Ok(ev) = ui_rx.try_recv() {
                    if let Event::RuleRun { ok, detail, .. } = ev {
                        report = Some((ok, detail));
                    }
                }
                if report.is_some() {
                    break;
                }
            }
            let (ok, detail) = report.expect("compte rendu de la règle");
            assert!(
                !plug_on.load(Ordering::Relaxed),
                "la prise a été éteinte (inversée)"
            );
            {
                let map = shutters.lock().unwrap();
                let s = &map[&SHUTTER_LIVING];
                assert_eq!((s.target, s.motion), (Some(40.0), CoverMotion::Closing));
            }
            // Pas à pas vers la cible, puis arrêt.
            let steps = step_shutters(&shutters);
            assert_eq!(steps[0].1, Some(75.0));
            step_shutters(&shutters);
            let last = step_shutters(&shutters);
            assert_eq!(
                last[0],
                (SHUTTER_LIVING, Some(40.0), Some(CoverMotion::Stopped))
            );
            assert!(step_shutters(&shutters).is_empty(), "plus rien à publier");
            assert!(!ok, "la troisième action (porte) doit échouer : {detail}");
            assert!(
                detail.contains("inverser")
                    && detail.contains("ouvrir à 40 %")
                    && detail.contains("échec"),
                "{detail}"
            );
        });
    }

    /// Attend, en lisant `rx`, qu'une condition soit vraie sur les événements reçus depuis
    /// la position `from` de `seen` (50 ms entre deux lectures).
    fn wait_until(
        rx: &std::sync::mpsc::Receiver<Event>,
        seen: &mut Vec<Event>,
        from: usize,
        polls: u32,
        cond: impl Fn(&[Event]) -> bool,
    ) -> bool {
        for _ in 0..polls {
            seen.extend(rx.try_iter());
            if cond(&seen[from..]) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn nodes_of(events: &[Event]) -> HashMap<u64, NodeInfo> {
        let mut nodes = HashMap::new();
        for e in events {
            if let Event::Node(info) = e {
                nodes.insert(info.node_id, info.clone());
            }
        }
        nodes
    }

    fn measures(events: &[Event], id: u64, wanted: MeasureKind) -> Vec<f64> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Measurement {
                    node_id,
                    kind,
                    value,
                    ..
                } if *node_id == id && *kind == wanted => Some(*value),
                _ => None,
            })
            .collect()
    }

    /// Mode démo de bout en bout, sans interface : commandes → événements. Pendant, côté
    /// serveur, de l'ancien test de l'interface (qui lisait ce même flux d'événements).
    #[test]
    fn demo_backend_end_to_end() {
        let (tx, rx) = std::sync::mpsc::channel();
        let commands = spawn(EventSink::new(tx));
        let mut seen: Vec<Event> = Vec::new();

        // La démo envoie les statuts « en ligne » après tout l'historique pré-rempli :
        // 4 appareils, un pont et ses 3 volets.
        assert!(wait_until(&rx, &mut seen, 0, 100, |ev| {
            // Appareils distincts en ligne (un même appareil peut l'être plusieurs fois).
            let online: std::collections::HashSet<u64> = ev
                .iter()
                .filter_map(|e| match e {
                    Event::Status {
                        node_id,
                        status: NodeStatus::Online,
                    } => Some(*node_id),
                    _ => None,
                })
                .collect();
            nodes_of(ev).len() == 8 && online.len() == 8
        }));
        let nodes = nodes_of(&seen);
        let living = nodes.values().find(|n| n.caps.humidity).unwrap().node_id;
        assert!(
            measures(&seen, living, MeasureKind::Temperature).len() > 150,
            "historique pré-rempli"
        );
        assert!(!measures(&seen, living, MeasureKind::Humidity).is_empty());
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::Measurement {
                kind: MeasureKind::Battery,
                value,
                ..
            } if *value == 87.0
        )));

        // Porte : transitions rejouées.
        let door = nodes.values().find(|n| n.caps.contact).unwrap().node_id;
        let door_states: Vec<bool> = seen
            .iter()
            .filter_map(|e| match e {
                Event::State {
                    node_id,
                    kind: StateKind::Open,
                    value,
                    ..
                } if *node_id == door => Some(*value),
                _ => None,
            })
            .collect();
        assert!(door_states.len() > 10, "historique des ouvertures");
        assert!(door_states.windows(2).all(|w| w[0] != w[1]));

        // Prise : puissance, énergie, marche/arrêt.
        let plug = nodes.values().find(|n| n.caps.power).unwrap().node_id;
        assert!(measures(&seen, plug, MeasureKind::Power).len() > 150);
        assert!(measures(&seen, plug, MeasureKind::Energy)
            .last()
            .is_some_and(|e| *e > 12.0));
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::State { node_id, kind: StateKind::OnOff, value: true, .. } if *node_id == plug
        )));
        // Pile faible signalée.
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::State {
                kind: StateKind::BatteryLow,
                value: true,
                ..
            }
        )));

        // Volets derrière le pont : position connue pour deux, inconnue pour le moteur RTS.
        let bridge = nodes
            .values()
            .find(|n| n.device_types.iter().any(|t| t == "Passerelle"))
            .unwrap()
            .node_id;
        let shutters: Vec<&NodeInfo> = nodes
            .values()
            .filter(|n| n.bridge.is_some_and(|b| b.node_id == bridge))
            .collect();
        assert_eq!(shutters.len(), 3);
        assert!(shutters.iter().all(|n| n.caps.cover));
        assert_eq!(shutters.iter().filter(|n| n.caps.cover_position).count(), 2);
        let open_shutter = shutters
            .iter()
            .find(|n| {
                measures(&seen, n.node_id, MeasureKind::Position)
                    .last()
                    .is_some_and(|p| *p >= 100.0)
            })
            .expect("un volet tout ouvert")
            .node_id;
        let rts = shutters
            .iter()
            .find(|n| !n.caps.cover_position)
            .unwrap()
            .node_id;
        assert!(measures(&seen, rts, MeasureKind::Position).is_empty());

        // Fermer un volet : il descend par paliers puis s'arrête fermé.
        let mark = seen.len();
        commands
            .send(Command::SetCover {
                node_id: open_shutter,
                command: CoverCommand::Close,
            })
            .unwrap();
        let motion = |ev: &[Event], id: u64, wanted: CoverMotion| {
            ev.iter().any(|e| {
                matches!(e, Event::CoverMotion { node_id, motion }
                    if *node_id == id && *motion == wanted)
            })
        };
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| motion(
            ev,
            open_shutter,
            CoverMotion::Closing
        )));
        // 100 % → 0 % par pas de 25 % toutes les 2 s : une dizaine de secondes.
        assert!(wait_until(&rx, &mut seen, mark, 300, |ev| {
            motion(ev, open_shutter, CoverMotion::Stopped)
                && measures(ev, open_shutter, MeasureKind::Position)
                    .last()
                    .is_some_and(|p| *p <= 0.0)
        }));
        // Puis à mi-hauteur.
        let mark = seen.len();
        commands
            .send(Command::SetCover {
                node_id: open_shutter,
                command: CoverCommand::Position(50),
            })
            .unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 300, |ev| {
            measures(ev, open_shutter, MeasureKind::Position)
                .last()
                .is_some_and(|p| *p == 50.0)
        }));
        // Le moteur RTS bouge sans jamais donner de position.
        let mark = seen.len();
        commands
            .send(Command::SetCover {
                node_id: rts,
                command: CoverCommand::Open,
            })
            .unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| motion(
            ev,
            rts,
            CoverMotion::Opening
        )));
        assert!(measures(&seen, rts, MeasureKind::Position).is_empty());

        // Nom et icône choisis, puis retour à l'automatique.
        let mark = seen.len();
        commands
            .send(Command::Customize {
                node_id: plug,
                name: Some("  Lampe du salon ".into()),
                icon: Some(Symbol::Light),
            })
            .unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| {
            nodes_of(ev).get(&plug).is_some_and(|n| {
                n.name == "Lampe du salon"
                    && n.icon == Some(Symbol::Light)
                    && n.device_name == "GRILLPLATS Plug"
            })
        }));
        let mark = seen.len();
        commands
            .send(Command::Customize {
                node_id: plug,
                name: Some("   ".into()),
                icon: None,
            })
            .unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| {
            nodes_of(ev)
                .get(&plug)
                .is_some_and(|n| n.custom_name.is_none() && n.name == "GRILLPLATS Plug")
        }));

        // Code invalide : message d'erreur clair.
        let mark = seen.len();
        commands.send(Command::Pair("12345".into())).unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| ev
            .iter()
            .any(|e| matches!(e, Event::PairingFailed(_)))));

        // Code valide : un appareil de plus.
        let mark = seen.len();
        commands
            .send(Command::Pair("3497-011-2332".into()))
            .unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| ev
            .iter()
            .any(|e| matches!(e, Event::PairingDone { .. }))));
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| nodes_of(ev)
            .keys()
            .any(|id| !nodes.contains_key(id))));
        let new_id = *nodes_of(&seen[mark..])
            .keys()
            .find(|id| !nodes.contains_key(id))
            .unwrap();

        // Oublier.
        let mark = seen.len();
        commands.send(Command::Forget(new_id)).unwrap();
        assert!(wait_until(&rx, &mut seen, mark, 100, |ev| ev
            .iter()
            .any(|e| matches!(e, Event::NodeRemoved(id) if *id == new_id))));
        // Aucune erreur fatale pendant tout le scénario.
        assert!(!seen.iter().any(|e| matches!(e, Event::Fatal(_))));
    }
}
