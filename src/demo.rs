//! Mode démo (`--demo`) : faux appareils et mesures simulées, pour tester
//! l'interface sans réseau Thread. Mêmes canaux et événements que le vrai backend.

use std::f64::consts::TAU;
use std::time::Duration;

use rand::Rng;
use tokio::sync::mpsc;

use crate::events::{
    now_unix, Capabilities, Command, Event, EventSink, MeasureKind, NodeInfo, NodeStatus, ReportingMode,
    StateKind,
};
use crate::matter::setup_code;

const LIVING: u64 = 0xD3_0001;
const BEDROOM: u64 = 0xD3_0002;
const DOOR: u64 = 0xD3_0003;
const PLUG: u64 = 0xD3_0004;

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
    let base = if t.rem_euclid(900.0) < 360.0 { 85.0 } else { 2.0 };
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
    }
}

pub fn spawn(events: EventSink) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("demo-backend".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime tokio");
            rt.block_on(run(rx, events));
        })
        .expect("thread démo");
    tx
}

async fn run(mut commands: mpsc::UnboundedReceiver<Command>, events: EventSink) {
    let mut rng = rand::thread_rng();
    let measure = |node_id, kind, value, at| events.send(Event::Measurement { node_id, kind, value, at });
    let state = |node_id, kind, value, at| events.send(Event::State { node_id, kind, value, at });
    events.send(Event::Info("Mode démo : aucune communication réseau, données simulées.".into()));

    events.send(Event::Node(sensor(LIVING, "Salon (démo)", true)));
    events.send(Event::Node(sensor(BEDROOM, "Chambre — capteur endormi (démo)", false)));
    events.send(Event::Node(NodeInfo {
        node_id: DOOR,
        name: "Porte d'entrée (démo)".into(),
        vendor: Some("IKEA of Sweden (démo)".into()),
        product: Some("MYGGBETT door/window sensor".into()),
        device_types: vec!["Capteur d'ouverture".into()],
        caps: Capabilities { contact: true, battery: true, ..Default::default() },
    }));
    events.send(Event::Node(NodeInfo {
        node_id: PLUG,
        name: "Prise du frigo (démo)".into(),
        vendor: Some("IKEA of Sweden (démo)".into()),
        product: Some("GRILLPLATS Plug".into()),
        device_types: vec!["Prise".into(), "Electrical Sensor".into()],
        caps: Capabilities { power: true, energy: true, on_off: true, ..Default::default() },
    }));

    // 90 minutes d'historique déjà « reçu », pour que les graphiques aient de quoi afficher.
    let now = now_unix();
    let mut energy_kwh = 12.4;
    let mut t = now - 90.0 * 60.0;
    let mut last_door = None;
    while t < now {
        for (id, base) in [(LIVING, 21.0), (BEDROOM, 18.5)] {
            measure(id, MeasureKind::Temperature, simulated(t, base, &mut rng), t);
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
    measure(PLUG, MeasureKind::Energy, energy_kwh, now);
    state(PLUG, StateKind::OnOff, true, now);
    for (id, pct) in [(LIVING, 87.0), (BEDROOM, 14.0), (DOOR, 62.0)] {
        measure(id, MeasureKind::Battery, pct, now);
    }
    state(BEDROOM, StateKind::BatteryLow, true, now);
    for id in [LIVING, BEDROOM, DOOR, PLUG] {
        events.send(Event::Status { node_id: id, status: NodeStatus::Online });
        events.send(Event::Seen { node_id: id, at: now });
    }
    for id in [LIVING, DOOR, PLUG] {
        events.send(Event::Mode { node_id: id, mode: ReportingMode::Subscription { max_interval_s: 300 } });
    }
    events.send(Event::Mode { node_id: BEDROOM, mode: ReportingMode::Polling { every_s: 30 } });

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
                    let p = fridge_power(t, &mut rng);
                    energy_kwh += p * 10.0 / 3600.0 / 1000.0;
                    measure(PLUG, MeasureKind::Power, p, t);
                    events.send(Event::Seen { node_id: PLUG, at: t });
                }
                if n.is_multiple_of(15) {
                    measure(PLUG, MeasureKind::Energy, energy_kwh, t);
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
                        events.send(Event::Node(sensor(id, &name, false)));
                        events.send(Event::Status { node_id: id, status: NodeStatus::Online });
                        events.send(Event::Mode { node_id: id, mode: ReportingMode::Subscription { max_interval_s: 30 } });
                        events.send(Event::PairingDone { node_id: id, name });
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

/// Plan de démonstration : deux étages, appareils déjà placés.
pub fn demo_plan() -> crate::home::HomePlan {
    use crate::home::{Floor, HomePlan, Item, ItemKind, Room, Symbol};
    let room = |id, name: &str, x, y, w, h| Room { id, name: name.into(), x, y, w, h };
    let device = |id, node_id, symbol, x, y| Item { id, kind: ItemKind::Device { node_id, symbol }, x, y };
    let decor = |id, symbol, label: &str, x, y| Item {
        id,
        kind: ItemKind::Decor { symbol, label: label.into() },
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
                    device(12, PLUG, None, 8.5, 1.0),
                    decor(13, Symbol::Window, "Baie vitrée", 3.0, 4.25),
                    decor(14, Symbol::Light, "Suspension", 4.5, 1.0),
                ],
            },
            Floor {
                id: 20,
                name: "Étage".into(),
                rooms: vec![room(21, "Chambre", 0.0, 0.0, 4.0, 3.5), room(22, "Bureau", 4.0, 0.0, 3.0, 3.5)],
                items: vec![device(23, BEDROOM, None, 2.0, 1.75), decor(24, Symbol::Window, "Velux", 5.5, 0.25)],
            },
        ],
    }
}
