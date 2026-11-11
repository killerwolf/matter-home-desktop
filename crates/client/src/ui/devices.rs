//! Onglet « Appareils » : des tuiles rangées par pièce. Chaque tuile a une grande icône d'état, le
//! nom (deux lignes au plus) et une valeur ; le clic agit directement sur une lumière, une prise
//! ou un volet, et ouvre le détail pour le reste. Un bouton « ⋯ » (dessiné) et le clic droit
//! ouvrent le détail de toutes les tuiles. Les ponts sont à la fin, en tuiles plus discrètes.
//! On ne signale que l'anormal : pile faible, injoignable.
//!
//! La partie « quoi afficher » (`tile_of`, `groups`, `columns_for`) est pure et testée ; le
//! dessin (`show`) n'en dépend que par ces résultats.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, Rect, Sense, Stroke, Ui, Vec2};
use matter_home_client::model::Device;
use protocol::automation::CoverCommand;
use protocol::events::CoverMotion;
use protocol::home::{HomePlan, OpeningKind, Symbol};
use protocol::wire::WireStatus;

use super::plan::{self, opening};
use super::theme::{self, Severity};
use super::tile_icons::{self, IconKind};

/// Le mot de l'état anormal d'un appareil, le même sur la liste, le plan et le détail.
pub const UNREACHABLE: &str = "injoignable";

/// Un groupe de tuiles : un titre (la pièce) et ses appareils.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    /// Étage, quand le plan en a plusieurs.
    pub note: Option<String>,
    pub ids: Vec<u64>,
    /// (étage, pièce) du plan ; `None` pour « Sans pièce ».
    pub key: Option<(u64, u64)>,
}

/// Les tuiles rangées : les pièces, puis « Sans pièce », et à part les ponts. Calculée une fois
/// par image et partagée entre la grille et la fenêtre de détail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub groups: Vec<Group>,
    pub bridges: Vec<u64>,
    /// Température moyenne de chaque pièce qui a un capteur joignable (voir
    /// `plan::room_temperatures`).
    pub averages: HashMap<(u64, u64), f64>,
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
                    key: Some((floor.id, room.id)),
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
            key: None,
        });
    }
    let mut bridges: Vec<u64> = bridge_set.into_iter().collect();
    bridges.sort_unstable();
    Layout {
        groups: result,
        bridges,
        averages: plan::room_temperatures(plan, devices),
    }
}

/// Sans nouvelles de l'appareil, sa dernière mesure ne vaut plus rien (comme sur le plan).
pub fn is_stale(d: &Device) -> bool {
    matches!(d.status, WireStatus::Unreachable { .. })
        || (matches!(d.status, WireStatus::Connecting) && d.last_seen.is_none())
}

/// « Ouverte » ou « Fermée » : une porte ou une fenêtre (féminin). Un seul libellé pour les
/// tuiles, la liste et le plan.
pub fn open_label(open: bool) -> &'static str {
    if open {
        "Ouverte"
    } else {
        "Fermée"
    }
}

/// Valeur d'une ligne quand l'appareil n'en a aucune à donner.
pub const NO_VALUE: &str = "—";

/// « 19,8 °C ».
fn one_decimal(celsius: f64) -> String {
    format!("{celsius:.1} °C").replace('.', ",")
}

/// « 86 W » (une décimale sous 10 W).
fn watts(w: f64) -> String {
    if w.abs() >= 1000.0 {
        format!("{:.2} kW", w / 1000.0).replace('.', ",")
    } else if w.abs() >= 10.0 {
        format!("{w:.0} W")
    } else {
        format!("{w:.1} W").replace('.', ",")
    }
}

/// Valeur d'un volet : son pourcentage (« 35 % »), « Ouvert » ou « Fermé », avec une flèche
/// quand il bouge. Sans position et immobile : « — ».
fn cover_value(d: &Device, expected: Option<CoverMotion>) -> String {
    let motion = expected.or(d.cover_motion);
    let arrow = match motion {
        Some(CoverMotion::Opening) => " ▲",
        Some(CoverMotion::Closing) => " ▼",
        _ => "",
    };
    let base = match d.position() {
        Some(p) if p.value <= 0.0 => "Fermé".to_string(),
        Some(p) if p.value >= 100.0 => "Ouvert".to_string(),
        Some(p) => format!("{:.0} %", p.value),
        None if d.info.caps.cover_position => NO_VALUE.to_string(),
        None => String::new(),
    };
    let text = format!("{base}{arrow}");
    let text = text.trim();
    if text.is_empty() {
        NO_VALUE.into()
    } else {
        text.to_string()
    }
}

/// Ce que l'on attend d'une commande qu'on vient d'envoyer : la tuile le montre aussitôt, sans
/// attendre le serveur ; l'attente s'arrête à l'événement attendu, au refus de la commande ou
/// après `EXPECT_TTL`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expect {
    pub on: Option<bool>,
    pub motion: Option<CoverMotion>,
    pub at: Instant,
}

pub const EXPECT_TTL: Duration = Duration::from_secs(5);

impl Expect {
    pub fn on(on: bool) -> Self {
        Self {
            on: Some(on),
            motion: None,
            at: Instant::now(),
        }
    }

    pub fn motion(motion: CoverMotion) -> Self {
        Self {
            on: None,
            motion: Some(motion),
            at: Instant::now(),
        }
    }

    /// Encore à montrer : pas périmée, et pas déjà réalisée.
    pub fn pending(&self, d: &Device, now: Instant) -> bool {
        now.duration_since(self.at) < EXPECT_TTL
            && self.on.is_none_or(|on| d.on_off() != Some(on))
            && self.motion.is_none_or(|m| d.cover_motion != Some(m))
    }
}

/// Ce que fait le clic sur une tuile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TileAction {
    /// Allumer ou éteindre (lumière, prise).
    Toggle,
    /// Ouvrir ou fermer un volet.
    Cover(CoverCommand),
    /// Ouvrir le détail (capteurs, portes, télécommandes, appareils injoignables).
    Detail,
}

/// Une tuile : tout ce qu'il faut pour la dessiner.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub icon: IconKind,
    /// La valeur, en grand. `None` : rien en grand (une télécommande).
    pub value: Option<String>,
    /// Orange : une porte ou une fenêtre ouverte.
    pub value_warn: bool,
    /// Le petit texte : l'humidité, « Bouton », « injoignable », « pile faible ».
    pub small: Option<(Option<Severity>, String)>,
    /// Lumière allumée : le fond de la carte prend un peu de jaune.
    pub lit: bool,
    /// Estompée : appareil injoignable.
    pub dim: bool,
    /// Petit badge orange.
    pub low_battery: bool,
    pub action: TileAction,
}

/// L'icône d'un appareil : celle choisie par l'utilisateur, sinon déduite de ce qu'il fait.
pub fn icon_of(d: &Device, on: Option<bool>) -> IconKind {
    let stale = is_stale(d);
    let symbol = plan::icon_of(d);
    match symbol {
        Some(Symbol::Light) => {
            if on == Some(true) {
                IconKind::LightOn
            } else {
                IconKind::LightOff
            }
        }
        Some(Symbol::Plug) => IconKind::Plug {
            on: on == Some(true),
        },
        Some(Symbol::Door | Symbol::Window) => {
            let kind = if symbol == Some(Symbol::Window) {
                OpeningKind::Window
            } else {
                OpeningKind::Door
            };
            IconKind::Opening(opening::opening_icon(
                kind,
                if stale { None } else { d.open() },
            ))
        }
        Some(Symbol::Shutter) => IconKind::Shutter {
            open_percent: if stale {
                None
            } else {
                d.position().map(|p| p.value)
            },
        },
        Some(Symbol::Thermometer) => IconKind::Thermometer,
        Some(Symbol::Button) => IconKind::Button,
        None if d.info.caps.temperature => IconKind::Thermometer,
        None => IconKind::Generic,
    }
}

/// Un volet est ouvert (au moins en partie) : le clic le fermera ; sinon il l'ouvrira. Sans
/// retour de position (RTS), le dernier mouvement connu décide.
fn cover_is_open(d: &Device) -> bool {
    match d.position() {
        Some(p) => p.value > 0.0,
        None => matches!(d.cover_motion, Some(CoverMotion::Opening)),
    }
}

/// La tuile d'un appareil. `expect` : une commande qui vient de partir (retour immédiat).
pub fn tile_of(d: &Device, expect: Option<&Expect>) -> Tile {
    let caps = d.info.caps;
    let stale = is_stale(d);
    let on = expect.and_then(|e| e.on).or_else(|| d.on_off());
    let icon = icon_of(d, on);
    let mut value = None;
    let mut small: Option<(Option<Severity>, String)> = None;
    let mut value_warn = false;
    if stale {
        small = Some((Some(Severity::Error), UNREACHABLE.into()));
    } else if caps.temperature {
        value = d.temperature().map(|t| one_decimal(t.value));
        small = d.humidity().map(|h| (None, format!("{:.0} %", h.value)));
    } else if caps.contact {
        if let Some(open) = d.open() {
            value = Some(open_label(open).to_string());
            value_warn = open;
        }
    } else if caps.cover {
        value = Some(cover_value(d, expect.and_then(|e| e.motion)));
    } else if caps.on_off {
        value = Some(match on {
            Some(true) => d
                .power()
                .map_or_else(|| "Allumée".to_string(), |p| watts(p.value)),
            Some(false) => "Éteinte".to_string(),
            None => NO_VALUE.to_string(),
        });
    } else if caps.power {
        value = d.power().map(|p| watts(p.value));
    } else if plan::icon_of(d) == Some(Symbol::Button) {
        small = Some((None, "Bouton".into()));
    }
    let low_battery = d.battery_low() && !stale;
    if low_battery && small.is_none() {
        small = Some((Some(Severity::Warn), "pile faible".into()));
    }
    let action = if stale {
        TileAction::Detail
    } else if caps.cover {
        TileAction::Cover(if cover_is_open(d) {
            CoverCommand::Close
        } else {
            CoverCommand::Open
        })
    } else if caps.on_off {
        TileAction::Toggle
    } else {
        TileAction::Detail
    };
    Tile {
        icon,
        value,
        value_warn,
        small,
        lit: icon == IconKind::LightOn,
        dim: stale,
        low_battery,
        action,
    }
}

/// Une section de la fenêtre de détail est-elle à montrer ? Une courbe, seulement si l'appareil
/// en a une : température, ouverture, consommation ou volet. Une lumière ou une télécommande n'a
/// pas de section vide.
pub fn has_curve(d: &Device) -> bool {
    let c = d.info.caps;
    c.temperature || c.contact || c.power || c.energy || c.cover
}

/// Écart entre deux tuiles.
const GAP: f32 = 8.0;

/// Nombre de colonnes pour une largeur donnée : des tuiles d'au moins `tile.min_width`, au moins
/// une colonne.
pub fn columns_for(width: f32) -> usize {
    let min = theme::tokens().tile.min_width;
    (((width + GAP) / (min + GAP)).floor() as usize).max(1)
}

/// Largeur d'une tuile pour `columns` colonnes qui remplissent `width`.
pub fn tile_width(width: f32, columns: usize) -> f32 {
    ((width - GAP * (columns as f32 - 1.0)) / columns as f32).floor()
}

/// Ce que le clic sur une tuile demande à l'application.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Allumer ou éteindre cet appareil.
    pub toggle: Option<u64>,
    pub cover: Option<(u64, CoverCommand)>,
    /// Ouvrir le détail.
    pub detail: Option<u64>,
}

/// Texte de la valeur d'un pont : combien d'appareils il expose.
fn bridge_note(devices: &BTreeMap<u64, Device>, id: u64) -> String {
    let n = devices
        .values()
        .filter(|d| d.info.bridge.is_some_and(|b| b.node_id == id))
        .count();
    format!("{n} appareil{}", if n > 1 { "s" } else { "" })
}

/// Dessine les tuiles de toutes les pièces, puis des ponts. Rend ce que le clic demande.
pub fn show(
    ui: &mut Ui,
    devices: &BTreeMap<u64, Device>,
    layout: &Layout,
    expect: &HashMap<u64, Expect>,
) -> Outcome {
    let mut out = Outcome::default();
    if devices.is_empty() {
        ui.add_space(theme::tokens().space.xl);
        ui.vertical_centered(|ui| {
            ui.label("Aucun appareil appairé pour l'instant.");
            ui.label("Le bouton « + Ajouter » en haut à droite ouvre l'appairage.");
        });
        return out;
    }
    let now = Instant::now();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for g in &layout.groups {
                let average = g.key.and_then(|k| layout.averages.get(&k)).copied();
                theme::room_heading(ui, &g.title, g.note.as_deref(), average);
                grid(ui, &g.ids, false, |ui, id, rect| {
                    let d = &devices[&id];
                    let pending = expect.get(&id).filter(|e| e.pending(d, now));
                    let tile = tile_of(d, pending);
                    card(
                        ui,
                        id,
                        &d.info.name,
                        &tile,
                        rect,
                        pending.is_some(),
                        &mut out,
                    );
                });
            }
            if !layout.bridges.is_empty() {
                theme::room_heading(ui, "Ponts", None, None);
                grid(ui, &layout.bridges, true, |ui, id, rect| {
                    let d = &devices[&id];
                    let mut tile = tile_of(d, None);
                    tile.icon = IconKind::Generic;
                    tile.value = None;
                    tile.lit = false;
                    tile.action = TileAction::Detail;
                    if tile.small.is_none() {
                        tile.small = Some((None, bridge_note(devices, id)));
                    }
                    card(ui, id, &d.info.name, &tile, rect, false, &mut out);
                });
            }
            ui.add_space(theme::tokens().space.lg);
        });
    out
}

/// Pose les tuiles de `ids` en lignes qui remplissent la largeur ; `paint` dessine chacune dans
/// son rectangle.
fn grid(ui: &mut Ui, ids: &[u64], discreet: bool, mut paint: impl FnMut(&mut Ui, u64, Rect)) {
    let tokens = theme::tokens().tile;
    let width = ui.available_width();
    let columns = columns_for(width);
    let tile_w = tile_width(width, columns);
    let tile_h = if discreet {
        (tokens.height * 0.62).round()
    } else {
        tokens.height
    };
    for row in ids.chunks(columns) {
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, tile_h), Sense::hover());
        for (i, id) in row.iter().enumerate() {
            let rect = Rect::from_min_size(
                row_rect.min + Vec2::new(i as f32 * (tile_w + GAP), 0.0),
                Vec2::new(tile_w, tile_h),
            );
            if ui.is_rect_visible(rect) {
                paint(ui, *id, rect);
            }
        }
        ui.add_space(GAP - ui.spacing().item_spacing.y);
    }
}

fn severity_color(p: &theme::Palette, severity: Option<Severity>) -> Color32 {
    severity.map_or(p.text_secondary, |s| s.color(p))
}

/// Une carte. `discreet` se déduit de la hauteur : un pont est plus bas et plus gris.
fn card(
    ui: &mut Ui,
    id: u64,
    name: &str,
    tile: &Tile,
    rect: Rect,
    expecting: bool,
    out: &mut Outcome,
) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let discreet = rect.height() < t.tile.height - 1.0;
    let base = ui.make_persistent_id(("tile", id));
    let response = ui
        .interact(rect, base, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let more_rect = Rect::from_min_size(
        rect.right_top() + Vec2::new(-30.0, 6.0),
        Vec2::new(24.0, 20.0),
    );
    let more = ui.interact(more_rect, base.with("more"), Sense::click());
    let radius = t.tile.radius;
    let fade = if tile.dim { 0.5 } else { 1.0 };

    let painter = ui.painter();
    let hovered = response.hovered() || more.hovered();
    painter.rect_filled(
        rect,
        radius,
        if hovered && !tile.lit {
            t.hover(p)
        } else {
            p.surface
        },
    );
    if tile.lit {
        painter.rect_filled(
            rect,
            radius,
            t.tile.on_tint.gamma_multiply(t.tile.on_tint_alpha),
        );
    }
    let border = if expecting {
        Stroke::new(1.5, p.accent_text)
    } else {
        Stroke::new(1.0, p.line)
    };
    painter.rect_stroke(rect, radius, border, egui::StrokeKind::Inside);

    let pad = 10.0;
    let icon_size = if discreet {
        t.tile.icon * 0.7
    } else {
        t.tile.icon
    };
    let icon_center = rect.left_top() + Vec2::splat(pad + icon_size / 2.0);
    tile_icons::paint(painter, icon_center, icon_size, tile.icon, p, fade);
    if tile.low_battery {
        painter.circle_filled(
            icon_center + Vec2::new(icon_size * 0.5, -icon_size * 0.5),
            4.0,
            p.warn,
        );
    }
    // Le bouton « détail » : trois points, plus visibles au survol.
    if more.hovered() {
        painter.rect_filled(more_rect, 4.0, t.hover(p));
    }
    tile_icons::paint_more(
        painter,
        more_rect,
        if hovered {
            p.text
        } else {
            p.text_secondary.gamma_multiply(0.6)
        },
    );

    // Nom : deux lignes au plus, coupé par « … ».
    let name_color = if discreet { p.text_secondary } else { p.text }.gamma_multiply(fade);
    let name_top = rect.top() + pad + icon_size + 6.0;
    let mut job = egui::text::LayoutJob::single_section(
        name.to_owned(),
        egui::TextFormat::simple(FontId::proportional(t.font.size.body), name_color),
    );
    job.wrap.max_width = rect.width() - 2.0 * pad;
    job.wrap.max_rows = if discreet { 1 } else { 2 };
    job.wrap.break_anywhere = false;
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(rect.left() + pad, name_top), galley, name_color);

    // Valeur en grand, petit texte à droite sur la même ligne.
    let base_y = rect.bottom() - pad;
    let small_width = if let Some((severity, text)) = &tile.small {
        let color = severity_color(p, *severity).gamma_multiply(if severity.is_some() {
            1.0
        } else {
            fade
        });
        let galley = painter.layout_no_wrap(
            text.clone(),
            FontId::proportional(t.font.size.caption),
            color,
        );
        let w = galley.size().x;
        painter.galley(
            egui::pos2(rect.right() - pad - w, base_y - galley.size().y),
            galley,
            color,
        );
        w + 6.0
    } else {
        0.0
    };
    if let Some(value) = &tile.value {
        let color = if tile.value_warn { p.warn } else { p.text }.gamma_multiply(fade);
        let mut job = egui::text::LayoutJob::single_section(
            value.clone(),
            egui::TextFormat::simple(FontId::proportional(t.font.size.heading + 2.0), color),
        );
        job.wrap.max_width = (rect.width() - 2.0 * pad - small_width).max(20.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(rect.left() + pad, base_y - galley.size().y),
            galley,
            color,
        );
    }

    if more.clicked() || response.secondary_clicked() {
        out.detail = Some(id);
    } else if response.clicked() {
        match tile.action {
            TileAction::Toggle => out.toggle = Some(id),
            TileAction::Cover(command) => out.cover = Some((id, command)),
            TileAction::Detail => out.detail = Some(id),
        }
    }
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

    fn typed(mut d: Device, types: &[&str]) -> Device {
        d.info.device_types = types.iter().map(|t| (*t).to_string()).collect();
        d
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

    fn lamp(on: bool) -> Device {
        let mut d = online(typed(
            device(
                1,
                "Lampe",
                Capabilities {
                    on_off: true,
                    ..Default::default()
                },
            ),
            &["Lampe"],
        ));
        d.apply_reading(Metric::OnOff, if on { 1.0 } else { 0.0 }, 1.0);
        d
    }

    #[test]
    fn devices_are_grouped_by_room_with_the_bridge_apart() {
        let mut devices = BTreeMap::new();
        devices.insert(1, online(device(1, "Sonde salon", temp_caps())));
        devices.insert(2, online(device(2, "Sonde chambre", temp_caps())));
        devices.insert(3, device(3, "Prise", Capabilities::default()));
        devices.insert(4, device(4, "Pont", Capabilities::default()));
        let mut shutter = device(5, "Volet", Capabilities::default());
        shutter.info.bridge = Some(WireBridge {
            node_id: 4,
            endpoint: 1,
        });
        devices.insert(5, shutter);
        // Un pont sans appareil exposé se reconnaît à son type.
        let lonely = typed(
            device(6, "Passerelle seule", Capabilities::default()),
            &["Passerelle"],
        );
        devices.insert(6, lonely);
        devices
            .get_mut(&1)
            .unwrap()
            .apply_reading(Metric::Temperature, 20.0, 1.0);
        devices
            .get_mut(&2)
            .unwrap()
            .apply_reading(Metric::Temperature, 18.0, 1.0);
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
        // « Sans pièce » juste avant les ponts, qui sont à part.
        assert_eq!(titles, ["Salon", "Chambre", "Sans pièce"]);
        assert_eq!(layout.groups[0].ids, [1, 5]); // triés par nom
        assert_eq!(layout.groups[2].ids, [3]);
        assert_eq!(layout.bridges, [4, 6]);
        assert!(layout.groups.iter().all(|g| g.note.is_none()));
        assert_eq!(layout.room_of(2), "Chambre");
        assert_eq!(layout.room_of(4), "Pont");
        assert_eq!(layout.room_of(3), "Sans pièce");
        // La pastille de température : la moyenne des capteurs de la pièce, pas de pastille
        // pour une pièce sans capteur ni pour « Sans pièce ».
        let key = |i: usize| layout.groups[i].key.unwrap();
        assert_eq!(layout.averages[&key(0)], 20.0);
        assert_eq!(layout.averages[&key(1)], 18.0);
        assert_eq!(layout.groups[2].key, None);
        assert_eq!(layout.averages.len(), 2);
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
    fn a_light_tile_shows_its_state_and_toggles_on_click() {
        let on = tile_of(&lamp(true), None);
        assert_eq!(on.icon, IconKind::LightOn);
        assert_eq!(on.value.as_deref(), Some("Allumée"));
        assert!(on.lit, "le fond de la carte prend un peu de jaune");
        assert_eq!(on.action, TileAction::Toggle);
        let off = tile_of(&lamp(false), None);
        assert_eq!(off.icon, IconKind::LightOff);
        assert_eq!(off.value.as_deref(), Some("Éteinte"));
        assert!(!off.lit);
        assert_eq!(off.action, TileAction::Toggle);
    }

    #[test]
    fn a_command_just_sent_is_shown_at_once_then_follows_the_device() {
        let d = lamp(false);
        let expect = Expect::on(true);
        // Retour immédiat : la tuile montre l'état demandé avant la réponse du serveur.
        let tile = tile_of(&d, Some(&expect));
        assert_eq!(tile.icon, IconKind::LightOn);
        assert_eq!(tile.value.as_deref(), Some("Allumée"));
        assert!(expect.pending(&d, Instant::now()));
        // L'appareil s'est allumé : l'attente est finie.
        assert!(!expect.pending(&lamp(true), Instant::now()));
        // Une attente périmée (commande perdue) n'est plus montrée.
        assert!(!expect.pending(&d, expect.at + EXPECT_TTL + Duration::from_millis(1)));
    }

    #[test]
    fn a_plug_tile_shows_the_watts_when_on() {
        let mut plug = online(typed(
            device(
                2,
                "Prise du frigo",
                Capabilities {
                    on_off: true,
                    power: true,
                    energy: true,
                    ..Default::default()
                },
            ),
            &["Prise", "Electrical Sensor"],
        ));
        plug.apply_reading(Metric::OnOff, 1.0, 1.0);
        plug.apply_reading(Metric::Power, 85.7, 1.0);
        let tile = tile_of(&plug, None);
        assert_eq!(tile.icon, IconKind::Plug { on: true });
        assert_eq!(tile.value.as_deref(), Some("86 W"));
        assert_eq!(tile.action, TileAction::Toggle);
        plug.apply_reading(Metric::OnOff, 0.0, 2.0);
        let off = tile_of(&plug, None);
        assert_eq!(off.icon, IconKind::Plug { on: false });
        assert_eq!(off.value.as_deref(), Some("Éteinte"));
    }

    #[test]
    fn a_door_tile_reuses_the_plan_icons_and_opens_the_detail() {
        let door_caps = Capabilities {
            contact: true,
            ..Default::default()
        };
        let mut door = online(device(3, "Porte", door_caps));
        door.apply_reading(Metric::Open, 0.0, 1.0);
        let closed = tile_of(&door, None);
        assert_eq!(
            closed.icon,
            IconKind::Opening(opening::opening_icon(OpeningKind::Door, Some(false)))
        );
        assert_eq!(closed.value.as_deref(), Some("Fermée"));
        assert!(!closed.value_warn);
        assert_eq!(closed.action, TileAction::Detail);
        door.apply_reading(Metric::Open, 1.0, 2.0);
        let open = tile_of(&door, None);
        assert_eq!(
            open.icon,
            IconKind::Opening(opening::opening_icon(OpeningKind::Door, Some(true)))
        );
        assert_eq!(open.value.as_deref(), Some("Ouverte"));
        assert!(open.value_warn, "ouverte = orange");
        // Une fenêtre (icône choisie) a sa propre icône.
        door.info.icon = Some(Symbol::Window);
        assert_eq!(
            tile_of(&door, None).icon,
            IconKind::Opening(opening::opening_icon(OpeningKind::Window, Some(true)))
        );
    }

    #[test]
    fn a_shutter_tile_shows_its_percentage_and_opens_or_closes_on_click() {
        let caps = Capabilities {
            cover: true,
            cover_position: true,
            ..Default::default()
        };
        let mut s = online(device(4, "Volet", caps));
        s.apply_reading(Metric::Position, 35.0, 1.0);
        let tile = tile_of(&s, None);
        assert_eq!(
            tile.icon,
            IconKind::Shutter {
                open_percent: Some(35.0)
            }
        );
        assert_eq!(tile.value.as_deref(), Some("35 %"));
        // Ouvert (au moins en partie) : le clic le ferme.
        assert_eq!(tile.action, TileAction::Cover(CoverCommand::Close));
        s.apply_reading(Metric::Position, 0.0, 2.0);
        let closed = tile_of(&s, None);
        assert_eq!(closed.value.as_deref(), Some("Fermé"));
        assert_eq!(closed.action, TileAction::Cover(CoverCommand::Open));
        // Retour immédiat de la commande : la flèche du mouvement attendu.
        let moving = tile_of(&s, Some(&Expect::motion(CoverMotion::Opening)));
        assert_eq!(moving.value.as_deref(), Some("Fermé ▲"));
        s.apply_reading(Metric::Position, 100.0, 3.0);
        assert_eq!(tile_of(&s, None).value.as_deref(), Some("Ouvert"));
        // Volet RTS : le dernier mouvement décide, et « — » quand on ne sait rien.
        let rts = Capabilities {
            cover: true,
            ..Default::default()
        };
        let mut r = online(device(5, "Volet RTS", rts));
        assert_eq!(tile_of(&r, None).value.as_deref(), Some("—"));
        assert_eq!(
            tile_of(&r, None).action,
            TileAction::Cover(CoverCommand::Open)
        );
        r.cover_motion = Some(CoverMotion::Opening);
        assert_eq!(tile_of(&r, None).value.as_deref(), Some("▲"));
        assert_eq!(
            tile_of(&r, None).action,
            TileAction::Cover(CoverCommand::Close)
        );
    }

    #[test]
    fn a_sensor_tile_shows_the_temperature_with_the_humidity_small() {
        let mut d = online(device(
            6,
            "Salon",
            Capabilities {
                temperature: true,
                humidity: true,
                battery: true,
                ..Default::default()
            },
        ));
        d.apply_reading(Metric::Temperature, 19.84, 1.0);
        d.apply_reading(Metric::Humidity, 48.2, 1.0);
        let tile = tile_of(&d, None);
        assert_eq!(tile.icon, IconKind::Thermometer);
        assert_eq!(tile.value.as_deref(), Some("19,8 °C"));
        assert_eq!(tile.small, Some((None, "48 %".to_string())));
        assert_eq!(tile.action, TileAction::Detail);
        assert!(!tile.low_battery);
        // Pile faible : un petit badge, et le mot quand rien d'autre n'occupe la place.
        d.apply_reading(Metric::BatteryLow, 1.0, 2.0);
        let low = tile_of(&d, None);
        assert!(low.low_battery);
        assert_eq!(
            low.small,
            Some((None, "48 %".to_string())),
            "l'humidité reste"
        );
    }

    #[test]
    fn a_remote_has_a_neutral_icon_a_small_label_and_no_dash() {
        let remote = online(typed(
            device(
                7,
                "Télécommande",
                Capabilities {
                    battery: true,
                    ..Default::default()
                },
            ),
            &["Interrupteur"],
        ));
        let tile = tile_of(&remote, None);
        assert_eq!(tile.icon, IconKind::Button);
        assert_eq!(tile.value, None, "pas de « — » en grand");
        assert_eq!(tile.small, Some((None, "Bouton".to_string())));
        assert_eq!(tile.action, TileAction::Detail);
    }

    #[test]
    fn an_unreachable_tile_is_dimmed_says_so_and_only_opens_the_detail() {
        let mut d = online(typed(
            device(
                1,
                "Lampe",
                Capabilities {
                    on_off: true,
                    ..Default::default()
                },
            ),
            &["Lampe"],
        ));
        d.apply_reading(Metric::OnOff, 1.0, 1.0);
        d.status = WireStatus::Unreachable {
            code: ErrorCode::DeviceUnreachable,
            detail: None,
            retry_in_s: 0,
        };
        let tile = tile_of(&d, None);
        assert!(tile.dim);
        assert_eq!(
            tile.small,
            Some((Some(Severity::Error), "injoignable".to_string()))
        );
        assert_eq!(tile.value, None);
        // Une commande à un appareil injoignable échouerait : le clic ouvre le détail.
        assert_eq!(tile.action, TileAction::Detail);
    }

    #[test]
    fn the_detail_has_a_curve_section_only_when_the_device_has_a_curve() {
        assert!(!has_curve(&lamp(true)), "une lumière n'a pas de courbe");
        assert!(!has_curve(&device(
            1,
            "Télécommande",
            Capabilities {
                battery: true,
                ..Default::default()
            }
        )));
        for caps in [
            temp_caps(),
            Capabilities {
                contact: true,
                ..Default::default()
            },
            Capabilities {
                power: true,
                ..Default::default()
            },
            Capabilities {
                cover: true,
                ..Default::default()
            },
        ] {
            assert!(has_curve(&device(1, "x", caps)));
        }
    }

    #[test]
    fn the_grid_fills_the_width_with_tiles_of_about_180_px_at_any_window_size() {
        for width in [600.0, 900.0, 1300.0, 1800.0, 2400.0, 3200.0] {
            let columns = columns_for(width);
            let tile = tile_width(width, columns);
            assert!(
                (170.0..=197.0).contains(&tile),
                "{width} px : {columns} colonnes de {tile} px"
            );
            // La grille remplit la largeur, à un pixel par colonne près.
            let used = tile * columns as f32 + GAP * (columns as f32 - 1.0);
            assert!(width - used < columns as f32, "{width} : {used}");
        }
        assert_eq!(columns_for(100.0), 1, "au moins une colonne");
    }
}
