//! Vue « Plan de la maison » : étages, pièces rectangulaires, objets placés,
//! avec l'état des appareils en direct. Deux modes : consultation et modification.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::customize::{editor, DeviceEdit, EditResult};
use super::Device;
use crate::events::NodeStatus;
use crate::home::{
    self, snap, HomePlan, Item, ItemKind, Room, Symbol, ITEM_SNAP, MIN_ROOM_SIZE, ROOM_SNAP,
};
use crate::matter::convert::{format_power, format_temperature};
use crate::persistence;

const ITEM_RADIUS: f32 = 15.0;
const HANDLE_SIZE: f32 = 10.0;
const MIN_SCALE: f32 = 10.0;
const MAX_SCALE: f32 = 250.0;

const GREEN: Color32 = Color32::from_rgb(40, 160, 70);
const ORANGE: Color32 = Color32::from_rgb(230, 140, 30);
const RED: Color32 = Color32::from_rgb(210, 60, 50);
const GRAY: Color32 = Color32::from_rgb(110, 110, 110);
const BLUE: Color32 = Color32::from_rgb(60, 140, 220);
const TEAL: Color32 = Color32::from_rgb(40, 150, 160);
const YELLOW: Color32 = Color32::from_rgb(220, 180, 40);

/// Ce que l'on fait glisser depuis la palette vers le plan.
#[derive(Debug, Clone, Copy)]
enum Payload {
    Device(u64),
    Decor(Symbol),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Room(u64),
    Item(u64),
}

enum Drag {
    Pan,
    /// `grab` : décalage pointeur → coin de la pièce ; `carried` : objets posés dans la pièce
    /// (décalage par rapport au coin), déplacés avec elle.
    MoveRoom {
        id: u64,
        grab: Vec2,
        carried: Vec<(u64, Vec2)>,
    },
    ResizeRoom {
        id: u64,
    },
    MoveItem {
        id: u64,
        grab: Vec2,
    },
}

/// Demande remontée à l'interface principale.
pub enum PlanAction {
    None,
    OpenDevice(u64),
    /// Nom et icône modifiés depuis l'inspecteur du plan.
    Customize {
        node_id: u64,
        name: Option<String>,
        icon: Option<Symbol>,
    },
}

pub struct PlanView {
    plan: HomePlan,
    path: PathBuf,
    floor: usize,
    edit: bool,
    /// Pixels par mètre.
    scale: f32,
    /// Coordonnée (m) affichée au coin haut-gauche du canevas.
    offset: Vec2,
    fit_pending: bool,
    drag: Option<Drag>,
    selection: Option<Selection>,
    dirty: bool,
    save_error: Option<String>,
    confirm_delete_floor: bool,
    /// Brouillon du nom et de l'icône de l'appareil sélectionné.
    device_edit: Option<DeviceEdit>,
}

/// Ce que montre un appareil placé : pictogramme, couleur, lignes de texte.
struct Look {
    symbol: Option<Symbol>,
    fill: Color32,
    name: String,
    value: Option<String>,
    battery_low: bool,
}

pub fn glyph(symbol: Option<Symbol>) -> &'static str {
    match symbol {
        Some(Symbol::Door) => "🚪",
        Some(Symbol::Window) => "▦",
        Some(Symbol::Light) => "💡",
        Some(Symbol::Plug) => "🔌",
        Some(Symbol::Thermometer) => "🌡",
        Some(Symbol::Button) => "🔘",
        None => "●",
    }
}

fn short(name: &str) -> String {
    if name.chars().count() > 22 {
        format!("{}…", name.chars().take(21).collect::<String>())
    } else {
        name.to_string()
    }
}

fn temperature_color(c: f64) -> Color32 {
    if c < 18.0 {
        BLUE
    } else if c <= 24.0 {
        GREEN
    } else {
        ORANGE
    }
}

/// Pictogramme par défaut d'un appareil, d'après ce qu'il mesure.
/// Icône d'un appareil : celle choisie par l'utilisateur, sinon déduite.
pub(super) fn icon_of(d: &Device) -> Option<Symbol> {
    d.info.icon.or_else(|| default_symbol(d))
}

fn default_symbol(d: &Device) -> Option<Symbol> {
    let caps = d.info.caps;
    let types = d.info.device_types.join(" ");
    if caps.contact {
        Some(Symbol::Door)
    } else if caps.temperature {
        Some(Symbol::Thermometer)
    } else if caps.power || caps.on_off && types.contains("Prise") {
        Some(Symbol::Plug)
    } else if types.contains("Lampe") {
        Some(Symbol::Light)
    } else if types.contains("Interrupteur") {
        Some(Symbol::Button)
    } else {
        None
    }
}

fn look(item: &Item, devices: &BTreeMap<u64, Device>) -> Look {
    match &item.kind {
        ItemKind::Decor { symbol, label } => Look {
            symbol: Some(*symbol),
            fill: Color32::from_gray(70),
            name: if label.is_empty() {
                symbol.label().into()
            } else {
                short(label)
            },
            value: None,
            battery_low: false,
        },
        ItemKind::Device { node_id, symbol } => {
            let Some(d) = devices.get(node_id) else {
                return Look {
                    symbol: *symbol,
                    fill: GRAY,
                    name: format!("Nœud {node_id:016X}"),
                    value: Some("inconnu".into()),
                    battery_low: false,
                };
            };
            // L'icône choisie pour l'appareil prime sur un ancien réglage propre au plan.
            let symbol = d.info.icon.or(*symbol).or_else(|| default_symbol(d));
            let (mut fill, mut value) = (TEAL, None);
            if let Some(open) = d.open {
                fill = if open { ORANGE } else { GREEN };
                value = Some(if open { "Ouvert" } else { "Fermé" }.to_string());
            } else if let Some(t) = d.temperature.last() {
                fill = temperature_color(t.value);
                let mut v = format_temperature(t.value);
                if let Some(h) = d.humidity.last() {
                    v.push_str(&format!(" · {:.0} %", h.value));
                }
                value = Some(v);
            } else if d.info.caps.power || d.info.caps.on_off {
                fill = if d.on_off == Some(true) {
                    YELLOW
                } else {
                    Color32::from_gray(80)
                };
                value = d.power.last().map(|p| format_power(p.value)).or_else(|| {
                    d.on_off
                        .map(|on| if on { "Allumée" } else { "Éteinte" }.to_string())
                });
            }
            match &d.status {
                NodeStatus::Unreachable { .. } => {
                    fill = GRAY;
                    value = Some("injoignable".into());
                }
                NodeStatus::Connecting if d.last_seen.is_none() => fill = GRAY,
                _ => {}
            }
            Look {
                symbol,
                fill,
                name: short(&d.info.name),
                value,
                battery_low: d.battery_low,
            }
        }
    }
}

/// Résumé d'une pièce : température moyenne et ouvertures des capteurs placés dedans.
fn room_summary(room: &Room, items: &[Item], devices: &BTreeMap<u64, Device>) -> Option<String> {
    let mut temps = Vec::new();
    let mut open = 0;
    for item in items.iter().filter(|i| room.contains(i.x, i.y)) {
        if let ItemKind::Device { node_id, .. } = item.kind {
            if let Some(d) = devices.get(&node_id) {
                if let Some(t) = d.temperature.last() {
                    temps.push(t.value);
                }
                if d.open == Some(true) {
                    open += 1;
                }
            }
        }
    }
    let mut parts = Vec::new();
    if !temps.is_empty() {
        parts.push(format_temperature(
            temps.iter().sum::<f64>() / temps.len() as f64,
        ));
    }
    if open > 0 {
        parts.push(format!("{open} ouvert{}", if open > 1 { "s" } else { "" }));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

impl PlanView {
    /// Charge le plan sauvegardé ; à défaut `default_plan`.
    pub fn load(demo: bool, default_plan: impl FnOnce() -> HomePlan) -> Self {
        let path = home::path(demo);
        let (plan, save_error) = match persistence::load_json::<HomePlan>(&path) {
            Ok(Some(p)) if !p.floors.is_empty() => (p, None),
            Ok(_) => (default_plan(), None),
            Err(e) => (
                default_plan(),
                Some(format!("Plan illisible, plan vide utilisé : {e:#}")),
            ),
        };
        Self {
            plan,
            path,
            floor: 0,
            edit: false,
            scale: 50.0,
            offset: Vec2::new(-1.0, -1.0),
            fit_pending: true,
            drag: None,
            selection: None,
            dirty: false,
            save_error,
            confirm_delete_floor: false,
            device_edit: None,
        }
    }

    pub fn remove_device(&mut self, node_id: u64) {
        if self.plan.remove_device(node_id) {
            self.dirty = true;
            self.selection = None;
        }
    }

    fn floor_mut(&mut self) -> &mut home::Floor {
        &mut self.plan.floors[self.floor]
    }

    fn save_if_needed(&mut self, ui: &egui::Ui) {
        if self.dirty && !ui.input(|i| i.pointer.any_down()) {
            match persistence::save_json(&self.path, &self.plan) {
                Ok(()) => self.save_error = None,
                Err(e) => self.save_error = Some(format!("Sauvegarde du plan impossible : {e:#}")),
            }
            self.dirty = false;
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, devices: &BTreeMap<u64, Device>) -> PlanAction {
        self.floor = self.floor.min(self.plan.floors.len() - 1);
        self.toolbar(ui);
        if let Some(e) = &self.save_error {
            ui.colored_label(RED, e);
        }
        ui.separator();

        let mut palette_action = None;
        if self.edit {
            egui::Panel::right("plan_palette")
                .resizable(false)
                .exact_size(250.0)
                .show(ui, |ui| {
                    palette_action = egui::ScrollArea::vertical()
                        .show(ui, |ui| self.palette(ui, devices))
                        .inner;
                });
        }
        let action = self.canvas(ui, devices);
        self.save_if_needed(ui);
        match (action, palette_action) {
            (PlanAction::None, Some(a)) => a,
            (a, _) => a,
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (i, f) in self.plan.floors.iter().enumerate() {
                if ui.selectable_label(self.floor == i, &f.name).clicked() && self.floor != i {
                    self.floor = i;
                    self.selection = None;
                    self.fit_pending = true;
                }
            }
            if self.edit && ui.button("+ Étage").clicked() {
                let id = self.plan.next_id();
                let name = format!("Étage {}", self.plan.floors.len());
                self.plan.floors.push(home::Floor {
                    id,
                    name,
                    rooms: Vec::new(),
                    items: Vec::new(),
                });
                self.floor = self.plan.floors.len() - 1;
                self.dirty = true;
            }
            ui.separator();
            let label = if self.edit {
                "✔ Terminer"
            } else {
                "✏ Modifier le plan"
            };
            if ui.selectable_label(self.edit, label).clicked() {
                self.edit = !self.edit;
                self.selection = None;
                self.confirm_delete_floor = false;
            }
            if ui.button("Recentrer").clicked() {
                self.fit_pending = true;
            }
        });
        if !self.edit {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Pièce").clicked() {
                let center = self.offset + Vec2::new(4.0, 3.0);
                let id = self.plan.next_id();
                let n = self.plan.floors[self.floor].rooms.len() + 1;
                self.floor_mut().rooms.push(Room {
                    id,
                    name: format!("Pièce {n}"),
                    x: snap(center.x, ROOM_SNAP),
                    y: snap(center.y, ROOM_SNAP),
                    w: 4.0,
                    h: 3.0,
                });
                self.selection = Some(Selection::Room(id));
                self.dirty = true;
            }
            ui.label("Nom de l'étage :");
            let floor = self.floor;
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.plan.floors[floor].name)
                        .desired_width(140.0),
                )
                .changed()
            {
                self.dirty = true;
            }
            if self.plan.floors.len() > 1 {
                if !self.confirm_delete_floor {
                    if ui.button("Supprimer l'étage").clicked() {
                        self.confirm_delete_floor = true;
                    }
                } else {
                    ui.colored_label(RED, "Supprimer l'étage et son contenu ?");
                    if ui.button("Oui, supprimer").clicked() {
                        self.plan.floors.remove(floor);
                        self.floor = floor.saturating_sub(1);
                        self.selection = None;
                        self.confirm_delete_floor = false;
                        self.dirty = true;
                    }
                    if ui.button("Annuler").clicked() {
                        self.confirm_delete_floor = false;
                    }
                }
            }
        });
    }

    fn palette(
        &mut self,
        ui: &mut egui::Ui,
        devices: &BTreeMap<u64, Device>,
    ) -> Option<PlanAction> {
        let mut action = None;
        // Inspecteur de la sélection.
        let floor = self.floor;
        match self.selection {
            Some(Selection::Room(id)) => {
                if let Some(idx) = self.plan.floors[floor]
                    .rooms
                    .iter()
                    .position(|r| r.id == id)
                {
                    ui.strong("Pièce sélectionnée");
                    let room = &mut self.plan.floors[floor].rooms[idx];
                    if ui.text_edit_singleline(&mut room.name).changed() {
                        self.dirty = true;
                    }
                    ui.label(format!("{:.1} × {:.1} m", room.w, room.h).replace('.', ","));
                    if ui.button("Supprimer la pièce").clicked() {
                        self.plan.floors[floor].rooms.remove(idx);
                        self.selection = None;
                        self.dirty = true;
                    }
                    ui.separator();
                }
            }
            Some(Selection::Item(id)) => {
                if let Some(idx) = self.plan.floors[floor]
                    .items
                    .iter()
                    .position(|i| i.id == id)
                {
                    ui.strong("Objet sélectionné");
                    let item = &mut self.plan.floors[floor].items[idx];
                    let mut changed = false;
                    match &mut item.kind {
                        ItemKind::Device { node_id, symbol } => match devices.get(node_id) {
                            Some(d) => {
                                if self.device_edit.as_ref().map(|e| e.node_id) != Some(*node_id) {
                                    self.device_edit = Some(DeviceEdit::from_info(&d.info));
                                }
                                ui.label(format!("{} {}", glyph(icon_of(d)), d.info.name));
                                let edit = self.device_edit.as_mut().expect("brouillon");
                                match editor(ui, edit, &d.info.device_name) {
                                    Some(EditResult::Save) => {
                                        // L'icône devient celle de l'appareil, partout.
                                        if symbol.take().is_some() {
                                            changed = true;
                                        }
                                        action = Some(PlanAction::Customize {
                                            node_id: *node_id,
                                            name: Some(edit.name.clone()),
                                            icon: edit.icon,
                                        });
                                    }
                                    Some(EditResult::Cancel) => {
                                        self.device_edit = Some(DeviceEdit::from_info(&d.info))
                                    }
                                    None => {}
                                }
                            }
                            None => {
                                ui.label(format!("Nœud {node_id:016X} (inconnu)"));
                            }
                        },
                        ItemKind::Decor { symbol, label } => {
                            changed |= ui.text_edit_singleline(label).changed();
                            egui::ComboBox::from_label("Pictogramme")
                                .selected_text(symbol.label())
                                .show_ui(ui, |ui| {
                                    for s in Symbol::ALL {
                                        changed |=
                                            ui.selectable_value(symbol, s, s.label()).changed();
                                    }
                                });
                        }
                    }
                    self.dirty |= changed;
                    if ui.button("Retirer du plan").clicked() {
                        self.plan.floors[floor].items.remove(idx);
                        self.selection = None;
                        self.dirty = true;
                    }
                    ui.separator();
                }
            }
            None => {}
        }

        ui.strong("Appareils à placer");
        let placed = self.plan.placed_devices();
        let mut any = false;
        for (id, d) in devices.iter().filter(|(id, _)| !placed.contains(id)) {
            any = true;
            ui.dnd_drag_source(
                egui::Id::unique(("plan_dev", *id)),
                Payload::Device(*id),
                |ui| {
                    ui.label(format!(
                        "⠿ {} {}",
                        glyph(default_symbol(d)),
                        short(&d.info.name)
                    ));
                },
            );
        }
        if !any {
            ui.small("Tous les appareils sont placés.");
        }
        ui.add_space(6.0);
        ui.strong("Objets non connectés");
        for s in Symbol::ALL {
            ui.dnd_drag_source(
                egui::Id::unique(("plan_decor", s.label())),
                Payload::Decor(s),
                |ui| {
                    ui.label(format!("⠿ {} {}", glyph(Some(s)), s.label()));
                },
            );
        }
        ui.add_space(8.0);
        ui.separator();
        ui.small(
            "Glissez un élément sur le plan. Déplacez une pièce en la faisant glisser ; \
             redimensionnez-la par son coin bas-droit. Les objets posés dans une pièce \
             la suivent. Suppr retire la sélection. Molette ou pincement : zoom ; \
             glisser dans le vide : déplacer la vue.",
        );
        action
    }

    fn fit(&mut self, rect: Rect) {
        let f = &self.plan.floors[self.floor];
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for r in &f.rooms {
            min = min.min(Vec2::new(r.x, r.y));
            max = max.max(Vec2::new(r.x + r.w, r.y + r.h));
        }
        for i in &f.items {
            min = min.min(Vec2::new(i.x, i.y));
            max = max.max(Vec2::new(i.x, i.y));
        }
        if !min.x.is_finite() {
            min = Vec2::ZERO;
            max = Vec2::new(10.0, 8.0);
        }
        let margin = Vec2::splat(1.5);
        let world = (max - min) + margin * 2.0;
        let scale = (rect.width() / world.x).min(rect.height() / world.y);
        self.scale = scale.clamp(MIN_SCALE, MAX_SCALE);
        // Centre le contenu dans le canevas.
        let visible = rect.size() / self.scale;
        self.offset = min - margin - (visible - world) / 2.0;
    }

    fn canvas(&mut self, ui: &mut egui::Ui, devices: &BTreeMap<u64, Device>) -> PlanAction {
        let size = ui.available_size().max(Vec2::new(200.0, 200.0));
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        if self.fit_pending {
            self.fit(rect);
            self.fit_pending = false;
        }

        // Zoom autour du pointeur (molette ou pincement).
        if let Some(hover) = resp.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.002).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let world = (hover - rect.min) / self.scale + self.offset;
                self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
                self.offset = world - (hover - rect.min) / self.scale;
            }
        }

        let action = self.interact(ui, &resp, devices);
        self.paint(ui, &painter, rect, &resp, devices);
        action
    }

    fn to_world(&self, rect: Rect, p: Pos2) -> Vec2 {
        (p - rect.min) / self.scale + self.offset
    }

    fn to_screen(&self, rect: Rect, x: f32, y: f32) -> Pos2 {
        rect.min + (Vec2::new(x, y) - self.offset) * self.scale
    }

    fn item_at(&self, rect: Rect, p: Pos2) -> Option<u64> {
        let f = &self.plan.floors[self.floor];
        f.items
            .iter()
            .rev()
            .find(|i| self.to_screen(rect, i.x, i.y).distance(p) <= ITEM_RADIUS + 3.0)
            .map(|i| i.id)
    }

    fn handle_at(&self, rect: Rect, p: Pos2) -> Option<u64> {
        let f = &self.plan.floors[self.floor];
        f.rooms
            .iter()
            .rev()
            .find(|r| self.to_screen(rect, r.x + r.w, r.y + r.h).distance(p) <= HANDLE_SIZE)
            .map(|r| r.id)
    }

    fn room_at(&self, rect: Rect, p: Pos2) -> Option<u64> {
        let w = self.to_world(rect, p);
        self.plan.floors[self.floor].room_at(w.x, w.y).map(|r| r.id)
    }

    fn interact(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        devices: &BTreeMap<u64, Device>,
    ) -> PlanAction {
        let rect = resp.rect;
        let mut action = PlanAction::None;

        // Dépôt depuis la palette.
        if let Some(payload) = resp.dnd_release_payload::<Payload>() {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let w = self.to_world(rect, p);
                let id = self.plan.next_id();
                let kind = match *payload {
                    Payload::Device(node_id) => ItemKind::Device {
                        node_id,
                        symbol: None,
                    },
                    Payload::Decor(symbol) => ItemKind::Decor {
                        symbol,
                        label: String::new(),
                    },
                };
                self.floor_mut().items.push(Item {
                    id,
                    kind,
                    x: snap(w.x, ITEM_SNAP),
                    y: snap(w.y, ITEM_SNAP),
                });
                self.selection = Some(Selection::Item(id));
                self.dirty = true;
            }
        }

        if resp.drag_started() {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            self.drag = Some(Drag::Pan);
            if let (true, Some(o)) = (self.edit, origin) {
                let w = self.to_world(rect, o);
                let floor = &self.plan.floors[self.floor];
                if let Some(id) = self.item_at(rect, o) {
                    let item = floor.items.iter().find(|i| i.id == id).expect("objet");
                    self.drag = Some(Drag::MoveItem {
                        id,
                        grab: Vec2::new(item.x, item.y) - w,
                    });
                    self.selection = Some(Selection::Item(id));
                } else if let Some(id) = self.handle_at(rect, o) {
                    self.drag = Some(Drag::ResizeRoom { id });
                    self.selection = Some(Selection::Room(id));
                } else if let Some(id) = self.room_at(rect, o) {
                    let room = floor
                        .rooms
                        .iter()
                        .find(|r| r.id == id)
                        .expect("pièce")
                        .clone();
                    let carried = floor
                        .items
                        .iter()
                        .filter(|i| floor.room_at(i.x, i.y).map(|r| r.id) == Some(id))
                        .map(|i| (i.id, Vec2::new(i.x - room.x, i.y - room.y)))
                        .collect();
                    self.drag = Some(Drag::MoveRoom {
                        id,
                        grab: Vec2::new(room.x, room.y) - w,
                        carried,
                    });
                    self.selection = Some(Selection::Room(id));
                }
            }
        }

        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = self.to_world(rect, p);
                let scale = self.scale;
                let floor = self.floor;
                let f = &mut self.plan.floors[floor];
                match &self.drag {
                    Some(Drag::Pan) => self.offset -= resp.drag_delta() / scale,
                    Some(Drag::MoveItem { id, grab }) => {
                        if let Some(item) = f.items.iter_mut().find(|i| i.id == *id) {
                            item.x = snap(w.x + grab.x, ITEM_SNAP);
                            item.y = snap(w.y + grab.y, ITEM_SNAP);
                            self.dirty = true;
                        }
                    }
                    Some(Drag::MoveRoom { id, grab, carried }) => {
                        if let Some(room) = f.rooms.iter_mut().find(|r| r.id == *id) {
                            room.x = snap(w.x + grab.x, ROOM_SNAP);
                            room.y = snap(w.y + grab.y, ROOM_SNAP);
                            let (rx, ry) = (room.x, room.y);
                            for (item_id, off) in carried {
                                if let Some(item) = f.items.iter_mut().find(|i| i.id == *item_id) {
                                    item.x = rx + off.x;
                                    item.y = ry + off.y;
                                }
                            }
                            self.dirty = true;
                        }
                    }
                    Some(Drag::ResizeRoom { id }) => {
                        if let Some(room) = f.rooms.iter_mut().find(|r| r.id == *id) {
                            room.w = snap(w.x - room.x, ROOM_SNAP).max(MIN_ROOM_SIZE);
                            room.h = snap(w.y - room.y, ROOM_SNAP).max(MIN_ROOM_SIZE);
                            self.dirty = true;
                        }
                    }
                    None => {}
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }

        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let item = self.item_at(rect, p);
                if self.edit {
                    self.selection = item
                        .map(Selection::Item)
                        .or_else(|| self.room_at(rect, p).map(Selection::Room));
                } else if let Some(id) = item {
                    let f = &self.plan.floors[self.floor];
                    if let Some(ItemKind::Device { node_id, .. }) =
                        f.items.iter().find(|i| i.id == id).map(|i| &i.kind)
                    {
                        if devices.contains_key(node_id) {
                            action = PlanAction::OpenDevice(*node_id);
                        }
                    }
                }
            }
        }

        // Suppr / retour arrière : retire la sélection (sauf pendant la saisie d'un texte).
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if self.edit
            && !typing
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            let floor = self.floor;
            match self.selection.take() {
                Some(Selection::Room(id)) => self.plan.floors[floor].rooms.retain(|r| r.id != id),
                Some(Selection::Item(id)) => self.plan.floors[floor].items.retain(|i| i.id != id),
                None => {}
            }
            self.dirty = true;
        }
        action
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        resp: &egui::Response,
        devices: &BTreeMap<u64, Device>,
    ) {
        let visuals = ui.visuals();
        let dark = visuals.dark_mode;
        painter.rect_filled(
            rect,
            0.0,
            if dark {
                Color32::from_gray(24)
            } else {
                Color32::from_gray(245)
            },
        );
        let painter = painter.with_clip_rect(rect);
        let text_color = visuals.text_color();

        // Grille d'un mètre en mode modification.
        if self.edit {
            let grid = if dark {
                Color32::from_gray(38)
            } else {
                Color32::from_gray(225)
            };
            let tl = self.to_world(rect, rect.min);
            let br = self.to_world(rect, rect.max);
            let mut x = tl.x.floor();
            while x <= br.x {
                let sx = self.to_screen(rect, x, 0.0).x;
                painter.line_segment(
                    [Pos2::new(sx, rect.top()), Pos2::new(sx, rect.bottom())],
                    Stroke::new(1.0, grid),
                );
                x += 1.0;
            }
            let mut y = tl.y.floor();
            while y <= br.y {
                let sy = self.to_screen(rect, 0.0, y).y;
                painter.line_segment(
                    [Pos2::new(rect.left(), sy), Pos2::new(rect.right(), sy)],
                    Stroke::new(1.0, grid),
                );
                y += 1.0;
            }
        }

        let floor = &self.plan.floors[self.floor];
        if floor.rooms.is_empty() && floor.items.is_empty() {
            let msg = if self.edit {
                "Cliquez sur « + Pièce » pour dessiner une première pièce,\npuis glissez vos appareils depuis la colonne de droite."
            } else {
                "Ce plan est vide. Cliquez sur « ✏ Modifier le plan » pour le dessiner."
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(16.0),
                GRAY,
            );
        }

        // Pièces.
        for room in &floor.rooms {
            let r = Rect::from_min_max(
                self.to_screen(rect, room.x, room.y),
                self.to_screen(rect, room.x + room.w, room.y + room.h),
            );
            let selected = self.selection == Some(Selection::Room(room.id));
            let fill = if dark {
                Color32::from_gray(42)
            } else {
                Color32::WHITE
            };
            let fill = if selected {
                visuals.selection.bg_fill.linear_multiply(0.35)
            } else {
                fill
            };
            painter.rect_filled(r, 2.0, fill);
            let wall = if dark {
                Color32::from_gray(160)
            } else {
                Color32::from_gray(60)
            };
            painter.rect_stroke(r, 2.0, Stroke::new(3.0, wall), StrokeKind::Inside);
            painter.text(
                r.min + Vec2::new(8.0, 6.0),
                Align2::LEFT_TOP,
                &room.name,
                FontId::proportional(15.0),
                text_color,
            );
            if let Some(summary) = room_summary(room, &floor.items, devices) {
                painter.text(
                    r.min + Vec2::new(8.0, 24.0),
                    Align2::LEFT_TOP,
                    summary,
                    FontId::proportional(12.0),
                    GRAY,
                );
            }
            if self.edit {
                let h = Rect::from_center_size(r.max, Vec2::splat(HANDLE_SIZE));
                painter.rect_filled(
                    h,
                    2.0,
                    if selected {
                        visuals.selection.stroke.color
                    } else {
                        wall
                    },
                );
            }
        }

        // Objets.
        let hover = resp.hover_pos();
        let mut hovered: Option<(&Item, Look)> = None;
        for item in &floor.items {
            let c = self.to_screen(rect, item.x, item.y);
            let lk = look(item, devices);
            let decor = matches!(item.kind, ItemKind::Decor { .. });
            painter.circle_filled(c, ITEM_RADIUS, lk.fill);
            let ring = if self.selection == Some(Selection::Item(item.id)) {
                Stroke::new(3.0, visuals.selection.stroke.color)
            } else {
                Stroke::new(1.5, if decor { GRAY } else { Color32::from_gray(230) })
            };
            painter.circle_stroke(c, ITEM_RADIUS, ring);
            painter.text(
                c,
                Align2::CENTER_CENTER,
                glyph(lk.symbol),
                FontId::proportional(15.0),
                Color32::WHITE,
            );
            if lk.battery_low {
                painter.circle_filled(
                    c + Vec2::new(ITEM_RADIUS * 0.75, -ITEM_RADIUS * 0.75),
                    5.0,
                    RED,
                );
            }
            let mut y = c.y + ITEM_RADIUS + 2.0;
            let name_rect = painter.text(
                Pos2::new(c.x, y),
                Align2::CENTER_TOP,
                &lk.name,
                FontId::proportional(11.0),
                text_color,
            );
            y = name_rect.bottom();
            if let Some(v) = &lk.value {
                painter.text(
                    Pos2::new(c.x, y),
                    Align2::CENTER_TOP,
                    v,
                    FontId::proportional(12.0),
                    lk.fill,
                );
            }
            if hover.is_some_and(|h| h.distance(c) <= ITEM_RADIUS + 3.0) {
                hovered = Some((item, lk));
            }
        }

        // Survol en consultation : bulle avec le nom et un rappel du clic.
        if let (false, Some((item, lk)), Some(h)) = (self.edit, hovered, hover) {
            let mut text = lk.name.clone();
            if let Some(v) = &lk.value {
                text.push_str(&format!("\n{v}"));
            }
            if matches!(item.kind, ItemKind::Device { .. }) {
                text.push_str("\nCliquer pour le détail");
            }
            let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), text_color);
            let bubble =
                Rect::from_min_size(h + Vec2::new(14.0, 14.0), galley.size() + Vec2::splat(12.0));
            painter.rect_filled(bubble, 4.0, visuals.window_fill);
            painter.rect_stroke(bubble, 4.0, visuals.window_stroke, StrokeKind::Inside);
            painter.galley(bubble.min + Vec2::splat(6.0), galley, text_color);
        }

        // Cadre accentué pendant un glisser-déposer depuis la palette.
        if resp.dnd_hover_payload::<Payload>().is_some() {
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(3.0, visuals.selection.stroke.color),
                StrokeKind::Inside,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Capabilities, NodeInfo};
    use crate::history::Sample;

    fn device(caps: Capabilities) -> Device {
        Device::new(NodeInfo {
            node_id: 1,
            name: "Capteur".into(),
            caps,
            ..Default::default()
        })
    }

    #[test]
    fn default_symbols() {
        assert_eq!(
            default_symbol(&device(Capabilities {
                contact: true,
                battery: true,
                ..Default::default()
            })),
            Some(Symbol::Door)
        );
        assert_eq!(
            default_symbol(&device(Capabilities {
                temperature: true,
                ..Default::default()
            })),
            Some(Symbol::Thermometer)
        );
        assert_eq!(
            default_symbol(&device(Capabilities {
                power: true,
                ..Default::default()
            })),
            Some(Symbol::Plug)
        );
    }

    #[test]
    fn device_look_follows_live_state() {
        let mut devices = BTreeMap::new();
        let mut door = device(Capabilities {
            contact: true,
            battery: true,
            ..Default::default()
        });
        door.status = NodeStatus::Online;
        door.open = Some(true);
        door.battery_low = true;
        devices.insert(1, door);
        let item = Item {
            id: 5,
            kind: ItemKind::Device {
                node_id: 1,
                symbol: None,
            },
            x: 0.0,
            y: 0.0,
        };
        let lk = look(&item, &devices);
        assert_eq!(
            (lk.symbol, lk.fill, lk.value.as_deref(), lk.battery_low),
            (Some(Symbol::Door), ORANGE, Some("Ouvert"), true)
        );

        devices.get_mut(&1).unwrap().open = Some(false);
        assert_eq!(look(&item, &devices).fill, GREEN);

        devices.get_mut(&1).unwrap().status = NodeStatus::Unreachable {
            error: "x".into(),
            retry_in_s: 5,
        };
        let lk = look(&item, &devices);
        assert_eq!((lk.fill, lk.value.as_deref()), (GRAY, Some("injoignable")));

        // L'icône de l'appareil prime sur un ancien pictogramme propre au plan.
        devices.get_mut(&1).unwrap().info.icon = Some(Symbol::Light);
        let legacy = Item {
            id: 7,
            kind: ItemKind::Device {
                node_id: 1,
                symbol: Some(Symbol::Window),
            },
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(look(&legacy, &devices).symbol, Some(Symbol::Light));

        // Pictogramme choisi par l'utilisateur, appareil inconnu.
        let window = Item {
            id: 6,
            kind: ItemKind::Device {
                node_id: 99,
                symbol: Some(Symbol::Window),
            },
            x: 0.0,
            y: 0.0,
        };
        let lk = look(&window, &devices);
        assert_eq!(
            (lk.symbol, lk.value.as_deref()),
            (Some(Symbol::Window), Some("inconnu"))
        );
    }

    #[test]
    fn room_summary_averages_and_counts() {
        let room = Room {
            id: 1,
            name: "Salon".into(),
            x: 0.0,
            y: 0.0,
            w: 5.0,
            h: 4.0,
        };
        let mut devices = BTreeMap::new();
        let mut t1 = device(Capabilities {
            temperature: true,
            ..Default::default()
        });
        t1.temperature.push(Sample {
            t: 0.0,
            value: 20.0,
        });
        let mut t2 = device(Capabilities {
            temperature: true,
            ..Default::default()
        });
        t2.temperature.push(Sample {
            t: 0.0,
            value: 22.0,
        });
        let mut door = device(Capabilities {
            contact: true,
            ..Default::default()
        });
        door.open = Some(true);
        devices.insert(1, t1);
        devices.insert(2, t2);
        devices.insert(3, door);
        let at = |id, node_id, x| Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
            },
            x,
            y: 1.0,
        };
        let items = vec![
            at(10, 1, 1.0),
            at(11, 2, 2.0),
            at(12, 3, 3.0),
            at(13, 1, 9.0),
        ];
        assert_eq!(
            room_summary(&room, &items, &devices).as_deref(),
            Some("21,00 °C · 1 ouvert")
        );
        assert_eq!(room_summary(&room, &[], &devices), None);
    }
}
