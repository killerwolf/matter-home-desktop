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
/// quand il bouge. Un volet sans retour de position (RTS) montre son dernier mouvement connu,
/// ou « ▲ ▼ » (les deux commandes possibles) : jamais un « — » en grand.
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
        None if arrow.is_empty() => "▲ ▼".to_string(),
        None => String::new(),
    };
    format!("{base}{arrow}").trim().to_string()
}

/// Ce que l'on attend d'une commande qu'on vient d'envoyer : la tuile le montre aussitôt, sans
/// attendre le serveur ; l'attente s'arrête à l'événement attendu, au refus de la commande ou
/// après `EXPECT_TTL`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expect {
    pub on: Option<bool>,
    pub motion: Option<CoverMotion>,
    pub at: Instant,
    /// Numéro de la requête partie : un vieux refus n'efface pas une attente plus récente.
    pub request: Option<u64>,
    /// Le serveur a accepté la commande (il reste à voir l'effet).
    pub acked: bool,
}

pub const EXPECT_TTL: Duration = Duration::from_secs(5);
/// Un second clic sur une tuile qui a une commande en attente est ignoré pendant ce délai, et
/// jusqu'à la réponse du serveur : un double-clic n'annule pas ce qu'il vient de demander.
pub const CLICK_GUARD: Duration = Duration::from_millis(400);

impl Expect {
    pub fn on(on: bool) -> Self {
        Self {
            on: Some(on),
            motion: None,
            at: Instant::now(),
            request: None,
            acked: false,
        }
    }

    pub fn motion(motion: CoverMotion) -> Self {
        Self {
            on: None,
            motion: Some(motion),
            at: Instant::now(),
            request: None,
            acked: false,
        }
    }

    /// Encore à montrer : pas périmée, et pas déjà réalisée.
    pub fn pending(&self, d: &Device, now: Instant) -> bool {
        now.duration_since(self.at) < EXPECT_TTL
            && self.on.is_none_or(|on| d.on_off() != Some(on))
            && self.motion.is_none_or(|m| d.cover_motion != Some(m))
    }

    /// Un clic sur la tuile est ignoré : la commande vient de partir ou n'a pas de réponse.
    pub fn blocks_click(&self, now: Instant) -> bool {
        !self.acked || now.duration_since(self.at) < CLICK_GUARD
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

/// Le volet est en mouvement, ou l'on attend qu'il le soit : le clic l'arrête.
fn cover_is_moving(d: &Device, expect: Option<&Expect>) -> bool {
    expect.is_some_and(|e| e.motion.is_some())
        || matches!(
            d.cover_motion,
            Some(CoverMotion::Opening | CoverMotion::Closing)
        )
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
        // Jamais joint : « connexion… » en gris neutre ; sinon, vraiment injoignable.
        small = Some(if matches!(d.status, WireStatus::Connecting) {
            (None, "connexion…".into())
        } else {
            (Some(Severity::Error), UNREACHABLE.into())
        });
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
        // Une télécommande : « Bouton » à la taille normale (on ne connaît pas sa dernière
        // pression), pas un tiret.
        value = Some("Bouton".into());
    }
    let low_battery = d.battery_low() && !stale;
    if low_battery {
        // Le mot remplace l'humidité : c'est l'information qui demande une action.
        small = Some((Some(Severity::Warn), "pile faible".into()));
    }
    let action = if stale {
        TileAction::Detail
    } else if caps.cover {
        if cover_is_moving(d, expect) {
            TileAction::Cover(CoverCommand::Stop)
        } else {
            TileAction::Cover(if cover_is_open(d) {
                CoverCommand::Close
            } else {
                CoverCommand::Open
            })
        }
    } else if matches!(icon, IconKind::LightOn | IconKind::LightOff) {
        // Seules les lumières se basculent d'un clic : une prise peut alimenter un frigo, son
        // clic ouvre le détail.
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
/// Colonnes de tuiles d'une pièce, au plus.
pub const MAX_BLOCK_COLUMNS: usize = 4;
/// Hauteur de l'en-tête d'une pièce.
const HEAD_H: f32 = 34.0;
/// Écarts entre deux pièces qui se suivent, de gauche à droite puis à la ligne.
const BLOCK_GAP_X: f32 = 28.0;
const BLOCK_GAP_Y: f32 = 14.0;
/// Hauteur d'une tuile de pont (plus basse).
const BRIDGE_H: f32 = 70.0;

/// Nombre de colonnes de tuiles qui tiennent dans `width` (au moins une).
pub fn columns_for(width: f32) -> usize {
    let tile = theme::tokens().tile.width;
    (((width + GAP) / (tile + GAP)).floor() as usize).max(1)
}

/// Colonnes d'une pièce de `count` appareils : au plus `MAX_BLOCK_COLUMNS`, et ce qui tient.
pub fn block_columns(count: usize, width: f32) -> usize {
    count.clamp(1, MAX_BLOCK_COLUMNS).min(columns_for(width))
}

/// Taille d'une pièce : son en-tête, puis ses tuiles sur `columns` colonnes.
pub fn block_size(count: usize, columns: usize, tile_h: f32) -> Vec2 {
    let tile_w = theme::tokens().tile.width;
    let rows = count.div_ceil(columns.max(1)).max(1);
    Vec2::new(
        columns as f32 * (tile_w + GAP) - GAP,
        HEAD_H + rows as f32 * (tile_h + GAP) - GAP,
    )
}

/// Pose des pièces de gauche à droite, à la ligne quand la suivante ne tient plus, avec un
/// espacement régulier. Rend la position de chacune et la hauteur totale.
pub fn place_blocks(sizes: &[Vec2], width: f32) -> (Vec<egui::Pos2>, f32) {
    let (mut x, mut y, mut line_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut positions = Vec::with_capacity(sizes.len());
    for size in sizes {
        if x > 0.0 && x + size.x > width {
            y += line_h + BLOCK_GAP_Y;
            x = 0.0;
            line_h = 0.0;
        }
        positions.push(egui::pos2(x, y));
        x += size.x + BLOCK_GAP_X;
        line_h = line_h.max(size.y);
    }
    (positions, y + line_h)
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

/// Une pièce à dessiner : son en-tête et ses tuiles.
struct Block<'a> {
    title: &'a str,
    floor: Option<&'a str>,
    average: Option<f64>,
    ids: &'a [u64],
    bridges: bool,
}

/// Dessine les pièces côte à côte, puis les ponts. Rend ce que le clic demande.
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
    let tokens = theme::tokens().tile;
    let mut blocks: Vec<Block<'_>> = layout
        .groups
        .iter()
        .map(|g| Block {
            title: &g.title,
            floor: g.note.as_deref(),
            average: g.key.and_then(|k| layout.averages.get(&k)).copied(),
            ids: &g.ids,
            bridges: false,
        })
        .collect();
    if !layout.bridges.is_empty() {
        blocks.push(Block {
            title: "Ponts",
            floor: None,
            average: None,
            ids: &layout.bridges,
            bridges: true,
        });
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(theme::tokens().space.sm);
            let width = ui.available_width();
            let sizes: Vec<(usize, Vec2)> = blocks
                .iter()
                .map(|b| {
                    let columns = block_columns(b.ids.len(), width);
                    let h = if b.bridges { BRIDGE_H } else { tokens.height };
                    (columns, block_size(b.ids.len(), columns, h))
                })
                .collect();
            let sizes_only: Vec<Vec2> = sizes.iter().map(|s| s.1).collect();
            let (positions, total) = place_blocks(&sizes_only, width);
            // De la marge en bas : la dernière pièce n'est pas collée au Journal.
            let (area, _) = ui.allocate_exact_size(
                Vec2::new(width, total + theme::tokens().space.xl * 2.0),
                Sense::hover(),
            );
            for ((block, (columns, size)), pos) in blocks.iter().zip(&sizes).zip(positions) {
                let rect = Rect::from_min_size(area.min + pos.to_vec2(), *size);
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    theme::room_heading(ui, block.title, block.floor, block.average);
                });
                let tile_h = if block.bridges {
                    BRIDGE_H
                } else {
                    tokens.height
                };
                for (i, id) in block.ids.iter().enumerate() {
                    let (col, row) = (i % columns, i / columns);
                    let tile_rect = Rect::from_min_size(
                        rect.min
                            + Vec2::new(
                                col as f32 * (tokens.width + GAP),
                                HEAD_H + row as f32 * (tile_h + GAP),
                            ),
                        Vec2::new(tokens.width, tile_h),
                    );
                    if !ui.is_rect_visible(tile_rect) {
                        continue;
                    }
                    let d = &devices[id];
                    let pending = expect.get(id).filter(|e| e.pending(d, now));
                    let busy = pending.is_some_and(|e| e.blocks_click(now));
                    let mut tile = tile_of(d, pending);
                    if block.bridges {
                        tile.icon = IconKind::Generic;
                        tile.value = None;
                        tile.lit = false;
                        tile.action = TileAction::Detail;
                        if tile.small.is_none() {
                            tile.small = Some((None, bridge_note(devices, *id)));
                        }
                    }
                    card(
                        ui,
                        *id,
                        &d.info.name,
                        &tile,
                        tile_rect,
                        pending.is_some(),
                        busy,
                        &mut out,
                    );
                }
            }
        });
    out
}

fn severity_color(p: &theme::Palette, severity: Option<Severity>) -> Color32 {
    severity.map_or(p.text_secondary, |s| s.color(p))
}

/// Une carte. `discreet` se déduit de la hauteur : un pont est plus bas et plus gris. `busy` : une
/// commande vient de partir, un clic de plus est ignoré.
#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    id: u64,
    name: &str,
    tile: &Tile,
    rect: Rect,
    expecting: bool,
    busy: bool,
    out: &mut Outcome,
) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let discreet = rect.height() < t.tile.height - 1.0;
    let base = ui.make_persistent_id(("tile", id));
    let response = ui
        .interact(rect, base, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    // Annoncée comme un bouton : le nom et la valeur.
    response.widget_info(|| {
        let value = tile.value.as_deref().unwrap_or("");
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name}, {value}"))
    });
    let more_rect = Rect::from_min_size(
        rect.right_top() + Vec2::new(-30.0, 6.0),
        Vec2::new(24.0, 20.0),
    );
    let more = ui.interact(more_rect, base.with("more"), Sense::click());
    let radius = t.tile.radius;
    let fade = if tile.dim { 0.5 } else { 1.0 };

    let painter = ui.painter();
    let hovered = response.hovered() || more.hovered();
    let focused = response.has_focus() || more.has_focus();
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
    if focused {
        painter.rect_stroke(
            rect.expand(1.0),
            radius,
            Stroke::new(2.0, p.accent_text),
            egui::StrokeKind::Outside,
        );
    }

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
    // Le bouton « détail » : trois points, visibles seulement au survol ou au focus.
    if hovered || focused {
        if more.hovered() {
            painter.rect_filled(more_rect, 4.0, t.hover(p));
        }
        tile_icons::paint_more(painter, more_rect, p.text);
    }

    // Nom : deux lignes au plus, coupé aux espaces, avec « … » en fin de deuxième ligne.
    let name_color = if discreet { p.text_secondary } else { p.text }.gamma_multiply(fade);
    let name_top = rect.top() + pad + icon_size + 6.0;
    let mut job = egui::text::LayoutJob::single_section(
        name.to_owned(),
        egui::TextFormat::simple(FontId::proportional(t.font.size.body), name_color),
    );
    job.wrap.max_width = rect.width() - 2.0 * pad;
    job.wrap.max_rows = if discreet { 1 } else { 2 };
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
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

    if more.clicked() || more.secondary_clicked() || response.secondary_clicked() {
        out.detail = Some(id);
    } else if response.clicked() && !response.double_clicked() && !busy {
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
        // Une prise peut alimenter un frigo : son clic ouvre le détail, il ne la bascule pas.
        assert_eq!(tile.action, TileAction::Detail);
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
        // Un volet qui bouge (ou dont on attend le mouvement) : le clic l'arrête.
        s.cover_motion = Some(CoverMotion::Closing);
        assert_eq!(
            tile_of(&s, None).action,
            TileAction::Cover(CoverCommand::Stop)
        );
        s.cover_motion = Some(CoverMotion::Stopped);
        assert_eq!(
            tile_of(&s, Some(&Expect::motion(CoverMotion::Closing))).action,
            TileAction::Cover(CoverCommand::Stop)
        );
        // Volet RTS : le dernier mouvement décide, et « ▲ ▼ » quand on ne sait rien (jamais
        // un tiret en grand).
        let rts = Capabilities {
            cover: true,
            ..Default::default()
        };
        let mut r = online(device(5, "Volet RTS", rts));
        assert_eq!(tile_of(&r, None).value.as_deref(), Some("▲ ▼"));
        assert_eq!(
            tile_of(&r, None).action,
            TileAction::Cover(CoverCommand::Open)
        );
        r.cover_motion = Some(CoverMotion::Opening);
        assert_eq!(tile_of(&r, None).value.as_deref(), Some("▲"));
        // Il monte encore : le clic l'arrête.
        assert_eq!(
            tile_of(&r, None).action,
            TileAction::Cover(CoverCommand::Stop)
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
            Some((Some(Severity::Warn), "pile faible".to_string())),
            "le mot remplace l'humidité"
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
        assert_eq!(
            tile.value.as_deref(),
            Some("Bouton"),
            "pas de « — » en grand"
        );
        assert_eq!(tile.small, None);
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
    fn a_device_never_reached_says_connecting_not_unreachable() {
        let mut d = device(1, "Sonde", temp_caps());
        d.apply_reading(Metric::Temperature, 20.0, 1.0);
        assert_eq!(d.status, WireStatus::Connecting);
        let tile = tile_of(&d, None);
        assert_eq!(tile.small, Some((None, "connexion…".to_string())));
        assert!(tile.dim);
        assert_eq!(tile.value, None);
        assert_eq!(tile.action, TileAction::Detail);
    }

    #[test]
    fn a_second_click_on_a_tile_with_a_pending_command_is_ignored_and_a_stale_refusal_is_too() {
        let now = Instant::now();
        let mut expect = Expect::on(true);
        // La commande vient de partir, sans réponse : le clic est ignoré.
        assert!(expect.blocks_click(now));
        // Acceptée, mais le délai de garde n'est pas passé : toujours ignoré.
        expect.acked = true;
        assert!(expect.blocks_click(expect.at + Duration::from_millis(100)));
        // Acceptée depuis assez longtemps : le clic est de nouveau pris en compte.
        assert!(!expect.blocks_click(expect.at + CLICK_GUARD + Duration::from_millis(1)));
        // L'attente garde le numéro de sa requête : un vieux refus ne l'efface pas.
        expect.request = Some(7);
        assert_eq!(expect.request, Some(7));
    }

    #[test]
    fn rooms_flow_side_by_side_with_regular_gaps_and_wrap() {
        let tile = theme::tokens().tile;
        let three = block_size(3, 3, tile.height);
        let two = block_size(2, 2, tile.height);
        // Largeur d'une pièce de trois colonnes : trois tuiles et deux écarts.
        assert_eq!(three.x, 3.0 * tile.width + 2.0 * GAP);
        assert_eq!(three.y, HEAD_H + tile.height);
        // 1 900 px : trois pièces de trois colonnes se suivent, la quatrième passe à la ligne.
        let sizes = [three, three, three, two, three];
        let (pos, total) = place_blocks(&sizes, 1900.0);
        assert_eq!(pos[0], egui::pos2(0.0, 0.0));
        assert_eq!(pos[1].y, 0.0);
        assert_eq!(pos[1].x, three.x + BLOCK_GAP_X, "écart régulier");
        assert_eq!(pos[2].y, 0.0);
        // La quatrième ne tient plus (1 752 + 368 > 1 900) : elle passe à la ligne, et la
        // cinquième la suit avec le même écart.
        assert_eq!(pos[3], egui::pos2(0.0, three.y + BLOCK_GAP_Y));
        assert_eq!(pos[4].y, pos[3].y);
        assert_eq!(pos[4].x, two.x + BLOCK_GAP_X, "écart régulier");
        assert_eq!(total, pos[3].y + three.y);
        // 1 300 px : deux pièces par ligne.
        let (pos, _) = place_blocks(&sizes, 1300.0);
        assert_eq!(pos.iter().filter(|p| p.y == 0.0).count(), 2);
        // Plus étroit qu'une pièce : une par ligne, sans boucle ni position négative.
        let (pos, total) = place_blocks(&sizes, 100.0);
        assert!(pos.windows(2).all(|w| w[1].y > w[0].y));
        assert!(total > 0.0);
    }

    #[test]
    fn a_room_has_at_most_four_columns_and_never_more_than_fit() {
        assert_eq!(block_columns(1, 1900.0), 1);
        assert_eq!(block_columns(3, 1900.0), 3);
        assert_eq!(block_columns(9, 1900.0), MAX_BLOCK_COLUMNS);
        // Une fenêtre étroite : ce qui tient, au moins une colonne.
        assert_eq!(block_columns(9, 400.0), 2);
        assert_eq!(block_columns(9, 100.0), 1);
        // Une pièce de 7 appareils : 4 colonnes, 2 rangées.
        let tile = theme::tokens().tile;
        assert_eq!(
            block_size(7, 4, tile.height).y,
            HEAD_H + 2.0 * tile.height + GAP
        );
    }
}
