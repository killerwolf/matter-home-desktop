//! Onglet « Appareils » : une ligne par appareil (un nom, une seule valeur), rangée par pièce.
//! Le pont est à part. Le détail s'ouvre dans un panneau à droite (voir `MatterApp::details`).
//! On ne signale que l'anormal : pile faible, injoignable.

use std::collections::{BTreeMap, HashMap, HashSet};

use eframe::egui::{self, Ui};
use matter_home_client::model::Device;
use protocol::events::CoverMotion;
use protocol::format::{format_position, format_power};
use protocol::home::HomePlan;
use protocol::wire::WireStatus;

use super::theme::{self, DeviceRow, Severity};

/// Le mot de l'état anormal d'un appareil, le même sur la liste, le plan et le détail.
pub const UNREACHABLE: &str = "injoignable";

/// Un groupe de la liste : un titre (la pièce) et ses appareils.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    /// Étage, quand le plan en a plusieurs.
    pub note: Option<String>,
    pub ids: Vec<u64>,
}

/// La liste rangée : les pièces, puis « Sans pièce », et à part les ponts. Calculée une fois par
/// image et partagée entre la liste et le panneau de détail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub groups: Vec<Group>,
    pub bridges: Vec<u64>,
}

impl Layout {
    /// Titre du groupe qui contient cet appareil (« Pont » pour un pont).
    pub fn room_of(&self, id: u64) -> String {
        if self.bridges.contains(&id) {
            return "Pont".into();
        }
        self.groups
            .iter()
            .find(|g| g.ids.contains(&id))
            .map_or_else(|| "Sans pièce".into(), |g| g.title.clone())
    }
}

/// Un pont expose d'autres appareils, ou se déclare « Passerelle ».
fn bridge_ids(devices: &BTreeMap<u64, Device>) -> HashSet<u64> {
    let mut bridges: HashSet<u64> = devices
        .values()
        .filter_map(|d| d.info.bridge.map(|b| b.node_id))
        .filter(|id| devices.contains_key(id))
        .collect();
    bridges.extend(
        devices
            .values()
            .filter(|d| d.info.device_types.iter().any(|t| t == "Passerelle"))
            .map(|d| d.info.node_id),
    );
    bridges
}

/// Les appareils rangés par pièce (règle : `HomePlan::room_of`, la même que les Températures).
/// Une pièce sans appareil n'apparaît pas.
pub fn groups(devices: &BTreeMap<u64, Device>, plan: &HomePlan) -> Layout {
    let bridge_set = bridge_ids(devices);
    let by_name = |ids: &mut Vec<u64>| {
        ids.sort_by_cached_key(|id| (devices[id].info.name.to_lowercase(), *id));
    };
    let mut per_room: HashMap<(u64, u64), Vec<u64>> = HashMap::new();
    let mut loose: Vec<u64> = Vec::new();
    for id in devices.keys().filter(|id| !bridge_set.contains(id)) {
        match plan.room_of(*id) {
            Some((floor, room)) => per_room.entry((floor.id, room.id)).or_default().push(*id),
            None => loose.push(*id),
        }
    }
    let mut result = Vec::new();
    for floor in &plan.floors {
        for room in &floor.rooms {
            if let Some(mut ids) = per_room.remove(&(floor.id, room.id)) {
                by_name(&mut ids);
                result.push(Group {
                    title: room.name.clone(),
                    note: (plan.floors.len() > 1).then(|| floor.name.clone()),
                    ids,
                });
            }
        }
    }
    if !loose.is_empty() {
        by_name(&mut loose);
        result.push(Group {
            title: "Sans pièce".into(),
            note: None,
            ids: loose,
        });
    }
    let mut bridges: Vec<u64> = bridge_set.into_iter().collect();
    bridges.sort_unstable();
    Layout {
        groups: result,
        bridges,
    }
}

/// Sans nouvelles de l'appareil, sa dernière mesure ne vaut plus rien (comme sur le plan).
pub fn is_stale(d: &Device) -> bool {
    matches!(d.status, WireStatus::Unreachable { .. })
        || (matches!(d.status, WireStatus::Connecting) && d.last_seen.is_none())
}

/// La seule valeur de la ligne : température, sinon ouverture, volet, puissance, état. « — »
/// quand l'appareil est injoignable.
pub fn headline(d: &Device) -> Option<String> {
    if is_stale(d) {
        return Some("—".into());
    }
    if let Some(t) = d.temperature() {
        return Some(format!("{:.1} °C", t.value).replace('.', ","));
    }
    if let Some(open) = d.open() {
        return Some(if open { "Ouverte" } else { "Fermée" }.into());
    }
    if d.info.caps.cover {
        let mut text = match d.position() {
            Some(p) => format_position(p.value),
            None if d.info.caps.cover_position => "Position inconnue".into(),
            None => String::new(),
        };
        match d.cover_motion {
            Some(CoverMotion::Opening) => text.push_str(" ▲"),
            Some(CoverMotion::Closing) => text.push_str(" ▼"),
            _ => {}
        }
        let text = text.trim().to_string();
        return (!text.is_empty()).then_some(text);
    }
    if d.on_off() == Some(false) {
        return Some("Éteinte".into());
    }
    if let Some(p) = d.power() {
        return Some(format_power(p.value));
    }
    d.on_off().map(|_| "Allumée".into())
}

/// L'anomalie à signaler sur la ligne, s'il y en a une.
pub fn anomaly(d: &Device) -> Option<(Severity, &'static str)> {
    if matches!(d.status, WireStatus::Unreachable { .. }) {
        Some((Severity::Error, UNREACHABLE))
    } else if d.battery_low() {
        Some((Severity::Warn, "pile faible"))
    } else {
        None
    }
}

/// Texte de la valeur d'un pont : combien d'appareils il expose.
fn bridge_note(devices: &BTreeMap<u64, Device>, id: u64) -> String {
    let n = devices
        .values()
        .filter(|d| d.info.bridge.is_some_and(|b| b.node_id == id))
        .count();
    format!("{n} appareil{}", if n > 1 { "s" } else { "" })
}

/// Dessine la liste pleine largeur. Rend l'appareil sur lequel on a cliqué.
pub fn show(
    ui: &mut Ui,
    devices: &BTreeMap<u64, Device>,
    layout: &Layout,
    selected: Option<u64>,
) -> Option<u64> {
    if devices.is_empty() {
        ui.add_space(theme::tokens().space.xl);
        ui.vertical_centered(|ui| {
            ui.label("Aucun appareil appairé pour l'instant.");
            ui.label("Le bouton « + Ajouter » en haut à droite ouvre l'appairage.");
        });
        return None;
    }
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for g in &layout.groups {
                theme::room_header(ui, &g.title, g.note.as_deref());
                for id in &g.ids {
                    let d = &devices[id];
                    let value = headline(d);
                    let row = DeviceRow {
                        name: &d.info.name,
                        value: value.as_deref(),
                        badge: anomaly(d),
                        selected: selected == Some(*id),
                        dim: false,
                    };
                    if theme::device_row(ui, &row).clicked() {
                        clicked = Some(*id);
                    }
                }
            }
            if !layout.bridges.is_empty() {
                theme::room_header(ui, "Ponts", None);
                for id in &layout.bridges {
                    let d = &devices[id];
                    let note = bridge_note(devices, *id);
                    let row = DeviceRow {
                        name: &d.info.name,
                        value: Some(&note),
                        badge: anomaly(d),
                        selected: selected == Some(*id),
                        dim: true,
                    };
                    if theme::device_row(ui, &row).clicked() {
                        clicked = Some(*id);
                    }
                }
            }
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::events::Capabilities;
    use protocol::home::{Floor, Item, ItemKind, OpeningKind, Room};
    use protocol::wire::{ErrorCode, Metric, WireBridge, WireNode};

    fn device(id: u64, name: &str, caps: Capabilities) -> Device {
        Device::new(WireNode {
            node_id: id,
            name: name.into(),
            device_name: name.into(),
            custom_name: None,
            icon: None,
            vendor: None,
            product: None,
            device_types: vec![],
            caps,
            bridge: None,
        })
    }

    fn on_plan(id: u64, node_id: u64, x: f32, y: f32) -> Item {
        Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
                opening: None::<OpeningKind>,
            },
            x,
            y,
            rotation: 0.0,
        }
    }

    fn room(id: u64, name: &str, x: f32) -> Room {
        Room {
            id,
            name: name.into(),
            x,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        }
    }

    fn temp_caps() -> Capabilities {
        Capabilities {
            temperature: true,
            ..Default::default()
        }
    }

    fn online(mut d: Device) -> Device {
        d.status = WireStatus::Online;
        d.last_seen = Some(1.0);
        d
    }

    #[test]
    fn devices_are_grouped_by_room_with_the_bridge_apart() {
        let mut devices = BTreeMap::new();
        devices.insert(1, device(1, "Sonde salon", temp_caps()));
        devices.insert(2, device(2, "Sonde chambre", temp_caps()));
        devices.insert(3, device(3, "Prise", Capabilities::default()));
        devices.insert(4, device(4, "Pont", Capabilities::default()));
        let mut shutter = device(5, "Volet", Capabilities::default());
        shutter.info.bridge = Some(WireBridge {
            node_id: 4,
            endpoint: 1,
        });
        devices.insert(5, shutter);
        // Un pont sans appareil exposé se reconnaît à son type.
        let mut lonely = device(6, "Passerelle seule", Capabilities::default());
        lonely.info.device_types = vec!["Passerelle".into()];
        devices.insert(6, lonely);
        let plan = HomePlan {
            version: 1,
            floors: vec![Floor {
                id: 1,
                name: "RDC".into(),
                rooms: vec![
                    room(1, "Salon", 0.0),
                    room(2, "Chambre", 200.0),
                    room(3, "Vide", 400.0),
                ],
                items: vec![
                    on_plan(10, 1, 10.0, 10.0),
                    on_plan(11, 2, 210.0, 10.0),
                    on_plan(12, 5, 20.0, 20.0),
                    // Le pont posé dans une pièce reste rangé à part.
                    on_plan(13, 4, 30.0, 30.0),
                ],
            }],
        };
        let layout = groups(&devices, &plan);
        let titles: Vec<&str> = layout.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Salon", "Chambre", "Sans pièce"]);
        assert_eq!(layout.groups[0].ids, [1, 5]); // triés par nom
        assert_eq!(layout.groups[2].ids, [3]);
        assert_eq!(layout.bridges, [4, 6]);
        assert!(layout.groups.iter().all(|g| g.note.is_none()));
        assert_eq!(layout.room_of(2), "Chambre");
        assert_eq!(layout.room_of(4), "Pont");
        assert_eq!(layout.room_of(3), "Sans pièce");
    }

    #[test]
    fn rooms_follow_the_shared_rule_on_double_pose_and_several_floors() {
        let mut devices = BTreeMap::new();
        devices.insert(1, device(1, "Double pose", temp_caps()));
        devices.insert(2, device(2, "Hors pièce", temp_caps()));
        devices.insert(3, device(3, "Étage", temp_caps()));
        let plan = HomePlan {
            version: 1,
            floors: vec![
                Floor {
                    id: 1,
                    name: "RDC".into(),
                    rooms: vec![room(1, "Salon", 0.0)],
                    // Posé hors pièce, puis dans le salon : le salon compte.
                    items: vec![on_plan(10, 1, 500.0, 10.0), on_plan(11, 1, 10.0, 10.0)],
                },
                Floor {
                    id: 2,
                    name: "Étage".into(),
                    rooms: vec![room(1, "Chambre", 0.0)],
                    items: vec![on_plan(12, 3, 10.0, 10.0), on_plan(13, 2, 500.0, 10.0)],
                },
            ],
        };
        let layout = groups(&devices, &plan);
        let rooms: Vec<(&str, Option<&str>)> = layout
            .groups
            .iter()
            .map(|g| (g.title.as_str(), g.note.as_deref()))
            .collect();
        // Deux étages : la pièce porte son étage ; même identifiant de pièce, groupes distincts.
        assert_eq!(
            rooms,
            [
                ("Salon", Some("RDC")),
                ("Chambre", Some("Étage")),
                ("Sans pièce", None)
            ]
        );
        assert_eq!(layout.groups[0].ids, [1]);
        assert_eq!(layout.groups[1].ids, [3]);
        assert_eq!(layout.groups[2].ids, [2]);
    }

    #[test]
    fn headline_is_a_single_value() {
        let mut d = online(device(1, "Sonde", temp_caps()));
        assert_eq!(headline(&d), None);
        d.apply_reading(Metric::Temperature, 19.84, 1.0);
        d.apply_reading(Metric::Battery, 87.0, 1.0);
        assert_eq!(headline(&d).as_deref(), Some("19,8 °C"));
        assert_eq!(anomaly(&d), None);
        d.apply_reading(Metric::BatteryLow, 1.0, 2.0);
        assert_eq!(anomaly(&d), Some((Severity::Warn, "pile faible")));
        d.status = WireStatus::Unreachable {
            code: ErrorCode::DeviceUnreachable,
            detail: None,
            retry_in_s: 0,
        };
        // Injoignable : le même mot que le plan, et plus de dernière mesure.
        assert_eq!(anomaly(&d), Some((Severity::Error, "injoignable")));
        assert_eq!(headline(&d).as_deref(), Some("—"));
    }

    #[test]
    fn a_device_never_reached_shows_a_dash_not_a_stale_value() {
        let mut d = device(1, "Sonde", temp_caps());
        d.apply_reading(Metric::Temperature, 20.0, 1.0);
        assert_eq!(d.status, WireStatus::Connecting);
        assert_eq!(headline(&d).as_deref(), Some("—"));
        // Joint au moins une fois : la valeur revient.
        d.last_seen = Some(5.0);
        assert_eq!(headline(&d).as_deref(), Some("20,0 °C"));
    }

    #[test]
    fn wording_of_doors_plugs_and_shutters() {
        let door = Capabilities {
            contact: true,
            ..Default::default()
        };
        let mut d = online(device(1, "Porte", door));
        d.apply_reading(Metric::Open, 0.0, 1.0);
        assert_eq!(headline(&d).as_deref(), Some("Fermée"));
        d.apply_reading(Metric::Open, 1.0, 2.0);
        assert_eq!(headline(&d).as_deref(), Some("Ouverte"));

        let plug = Capabilities {
            on_off: true,
            power: true,
            ..Default::default()
        };
        let mut p = online(device(2, "Prise", plug));
        p.apply_reading(Metric::OnOff, 1.0, 1.0);
        p.apply_reading(Metric::Power, 86.0, 1.0);
        assert_eq!(headline(&p).as_deref(), Some("86,0 W"));
        p.apply_reading(Metric::OnOff, 0.0, 2.0);
        assert_eq!(headline(&p).as_deref(), Some("Éteinte"));
        let mut q = online(device(
            3,
            "Lampe",
            Capabilities {
                on_off: true,
                ..Default::default()
            },
        ));
        q.apply_reading(Metric::OnOff, 1.0, 1.0);
        assert_eq!(headline(&q).as_deref(), Some("Allumée"));

        // Volet RTS : sans position, seule la flèche du mouvement.
        let rts = Capabilities {
            cover: true,
            ..Default::default()
        };
        let mut s = online(device(4, "Volet", rts));
        assert_eq!(headline(&s), None);
        s.cover_motion = Some(CoverMotion::Closing);
        assert_eq!(headline(&s).as_deref(), Some("▼"));
        s.cover_motion = Some(CoverMotion::Opening);
        assert_eq!(headline(&s).as_deref(), Some("▲"));
    }
}
