//! Onglet « Appareils » : une ligne par appareil (un nom, une seule valeur), rangée par pièce.
//! Le pont est à part. Le détail s'ouvre dans un panneau à droite (voir `MatterApp::details`).
//! On ne signale que l'anormal : pile faible, hors ligne.

use std::collections::BTreeMap;

use eframe::egui::{self, Ui};
use matter_home_client::model::Device;
use protocol::events::CoverMotion;
use protocol::format::{format_position, format_power};
use protocol::home::{HomePlan, ItemKind};
use protocol::wire::WireStatus;

use super::theme::{self, DeviceRow, Severity};

/// Un groupe de la liste : un titre (la pièce) et ses appareils.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    /// Étage, quand le plan en a plusieurs.
    pub note: Option<String>,
    pub ids: Vec<u64>,
}

/// Un nœud qui expose d'autres appareils : un pont.
fn is_bridge(devices: &BTreeMap<u64, Device>, id: u64) -> bool {
    devices
        .values()
        .any(|d| d.info.bridge.is_some_and(|b| b.node_id == id))
}

/// Les appareils rangés par pièce (d'après leur place sur le plan), puis « Sans pièce », et
/// à part les ponts. Une pièce sans appareil n'apparaît pas.
pub fn groups(devices: &BTreeMap<u64, Device>, plan: &HomePlan) -> (Vec<Group>, Vec<u64>) {
    let bridges: Vec<u64> = devices
        .keys()
        .copied()
        .filter(|id| is_bridge(devices, *id))
        .collect();
    let by_name = |ids: &mut Vec<u64>| {
        ids.sort_by_cached_key(|id| (devices[id].info.name.to_lowercase(), *id));
    };
    let mut placed: Vec<u64> = Vec::new();
    let mut result = Vec::new();
    for floor in &plan.floors {
        let mut per_room: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for item in &floor.items {
            let ItemKind::Device { node_id, .. } = item.kind else {
                continue;
            };
            if !devices.contains_key(&node_id)
                || bridges.contains(&node_id)
                || placed.contains(&node_id)
            {
                continue;
            }
            if let Some(room) = floor.room_at(item.x, item.y) {
                placed.push(node_id);
                per_room.entry(room.id).or_default().push(node_id);
            }
        }
        for room in &floor.rooms {
            if let Some(mut ids) = per_room.remove(&room.id) {
                by_name(&mut ids);
                result.push(Group {
                    title: room.name.clone(),
                    note: (plan.floors.len() > 1).then(|| floor.name.clone()),
                    ids,
                });
            }
        }
    }
    let mut loose: Vec<u64> = devices
        .keys()
        .copied()
        .filter(|id| !placed.contains(id) && !bridges.contains(id))
        .collect();
    if !loose.is_empty() {
        by_name(&mut loose);
        result.push(Group {
            title: "Sans pièce".into(),
            note: None,
            ids: loose,
        });
    }
    (result, bridges)
}

/// La seule valeur de la ligne : température, sinon ouverture, volet, puissance, état.
pub fn headline(d: &Device) -> Option<String> {
    if let Some(t) = d.temperature() {
        return Some(format!("{:.1} °C", t.value).replace('.', ","));
    }
    if let Some(open) = d.open() {
        return Some(if open { "Ouvert" } else { "Fermé" }.into());
    }
    if d.info.caps.cover {
        let mut text = match d.position() {
            Some(p) => format_position(p.value),
            None if d.info.caps.cover_position => "Position inconnue".into(),
            None => return None,
        };
        match d.cover_motion {
            Some(CoverMotion::Opening) => text.push_str(" ▲"),
            Some(CoverMotion::Closing) => text.push_str(" ▼"),
            _ => {}
        }
        return Some(text);
    }
    if let Some(p) = d.power() {
        return Some(format_power(p.value));
    }
    d.on_off()
        .map(|on| if on { "Allumé" } else { "Éteint" }.into())
}

/// L'anomalie à signaler sur la ligne, s'il y en a une.
pub fn anomaly(d: &Device) -> Option<(Severity, &'static str)> {
    if matches!(d.status, WireStatus::Unreachable { .. }) {
        Some((Severity::Error, "hors ligne"))
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
    plan: &HomePlan,
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
    let (groups, bridges) = groups(devices, plan);
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for g in &groups {
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
            if !bridges.is_empty() {
                theme::room_header(ui, "Ponts", None);
                for id in &bridges {
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
    use protocol::home::{Floor, Item, OpeningKind, Room};
    use protocol::wire::{Metric, WireBridge, WireNode};

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
        let (groups, bridges) = groups(&devices, &plan);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Salon", "Chambre", "Sans pièce"]);
        assert_eq!(groups[0].ids, [1, 5]); // triés par nom
        assert_eq!(groups[2].ids, [3]);
        assert_eq!(bridges, [4]);
        assert!(groups.iter().all(|g| g.note.is_none()));
    }

    #[test]
    fn headline_is_a_single_value() {
        let mut d = device(1, "Sonde", temp_caps());
        assert_eq!(headline(&d), None);
        d.apply_reading(Metric::Temperature, 19.84, 1.0);
        d.apply_reading(Metric::Battery, 87.0, 1.0);
        assert_eq!(headline(&d).as_deref(), Some("19,8 °C"));
        assert_eq!(anomaly(&d), None);
        d.apply_reading(Metric::BatteryLow, 1.0, 2.0);
        assert_eq!(anomaly(&d), Some((Severity::Warn, "pile faible")));
        d.status = WireStatus::Unreachable {
            code: protocol::wire::ErrorCode::DeviceUnreachable,
            detail: None,
            retry_in_s: 0,
        };
        assert_eq!(anomaly(&d), Some((Severity::Error, "hors ligne")));
    }
}
