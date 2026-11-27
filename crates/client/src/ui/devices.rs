//! Onglet « Appareils » : des tuiles de 192 × 112 rangées par pièce, sur 6 colonnes fixes (4 quand
//! la fiche de 360 px est ouverte). Chaque tuile a une icône, le nom et une valeur ; le clic
//! bascule une lumière ou un volet, et ouvre la fiche pour tout le reste, **prises comprises** (une
//! prise peut alimenter un frigo : pas de bouton d'état sur la tuile, on allume depuis la fiche).
//! Le « ⋯ », toujours visible en `text_faint`, et le clic droit ouvrent la fiche de toutes les
//! tuiles. Les ponts sont à la fin, en tuiles discrètes de 192 × 56. On ne signale que l'anormal :
//! pile faible, injoignable (à contraste plein, avec l'heure depuis laquelle).
//!
//! La partie « quoi afficher » (`tile_of`, `groups`, `columns_for`) est pure et testée ; le
//! dessin (`show`) n'en dépend que par ces résultats.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, Rect, RichText, Sense, Stroke, Ui, Vec2};
use ostal_client::model::Device;
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

/// « 18:02 » : l'heure locale d'un instant Unix.
pub fn clock(unix: f64) -> String {
    chrono::DateTime::from_timestamp(unix as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
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
    if is_assumed_cover(d) {
        // Le dernier ordre d'Ostal, pas un mouvement : ni flèche ni pourcentage.
        return match d.position() {
            Some(p) if p.value > 0.0 => "Ouvert",
            Some(_) => "Fermé",
            None => "Inconnu",
        }
        .to_string();
    }
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

/// Un volet apporté par une extension (Somfy RTS) : sans retour d'état, Ostal montre le **dernier
/// ordre qu'il lui a donné** (ouvrir ou fermer), pas son état réel ; il ne sait ni s'arrêter ni
/// se mettre à mi-course.
pub fn is_assumed_cover(d: &Device) -> bool {
    d.info.caps.cover
        && !d.info.caps.cover_position
        && protocol::events::is_extension_id(d.info.node_id)
}

/// La phrase qui accompagne un état déduit du dernier ordre.
pub const ASSUMED_NOTE: &str = "d'après le dernier ordre d'Ostal";

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
    /// « depuis 18:02 » : depuis quand un appareil est injoignable (d'après `last_seen`).
    pub since: Option<String>,
    /// Vraiment injoignable (et pas seulement « connexion… ») : bordure orange, contraste plein.
    pub unreachable: bool,
    /// Lumière allumée : le fond de la carte prend un peu de jaune.
    pub lit: bool,
    /// Sans nouvelles de l'appareil : sa dernière mesure ne vaut plus rien. Rien n'est estompé,
    /// l'injoignable se lit à contraste plein.
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
            (Some(Severity::Warn), UNREACHABLE.into())
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
        if is_assumed_cover(d) && d.position().is_some() {
            small = Some((None, ASSUMED_NOTE.into()));
        }
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
        if !is_assumed_cover(d) && cover_is_moving(d, expect) {
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
    let unreachable = matches!(d.status, WireStatus::Unreachable { .. });
    let since = d
        .last_seen
        .filter(|_| unreachable)
        .map(|t| format!("depuis {}", clock(t)));
    Tile {
        icon,
        value,
        value_warn,
        small,
        since,
        unreachable,
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
const GAP: f32 = 16.0;
/// Colonnes de la grille sans la fiche : 6 tuiles de 192 px et 16 d'écart tiennent dans 1 232 px.
pub const GRID_COLUMNS: usize = 6;
/// Hauteur de l'en-tête d'une pièce (titre de 15 px, puis 8 px d'écart).
const HEAD_H: f32 = 30.0;
/// Écarts entre deux pièces qui se suivent, de gauche à droite puis à la ligne : ceux de la grille.
const BLOCK_GAP_X: f32 = GAP;
const BLOCK_GAP_Y: f32 = GAP;
/// Hauteur d'une tuile de pont (plus basse).
const BRIDGE_H: f32 = 56.0;

/// Nombre de colonnes de tuiles qui tiennent dans `width` (au moins une) : 6 dans 1 232 px, 4
/// quand la fiche de 360 px prend sa place.
pub fn columns_for(width: f32) -> usize {
    let tile = theme::tokens().tile.desktop_width;
    // Une demi-pixel de tolérance : la largeur d'egui est arrondie au pixel physique.
    (((width + GAP + 0.5) / (tile + GAP)).floor() as usize).max(1)
}

/// Colonnes d'une pièce de `count` appareils : au plus celles de la grille, et ce qui tient.
pub fn block_columns(count: usize, width: f32) -> usize {
    count.clamp(1, GRID_COLUMNS).min(columns_for(width))
}

/// Taille d'une pièce : son en-tête (absent pour les ponts, `head` vaut 0), puis ses tuiles sur
/// `columns` colonnes.
pub fn block_size(count: usize, columns: usize, head: f32, tile_h: f32) -> Vec2 {
    let tile_w = theme::tokens().tile.desktop_width;
    let rows = count.div_ceil(columns.max(1)).max(1);
    Vec2::new(
        columns as f32 * (tile_w + GAP) - GAP,
        head + rows as f32 * (tile_h + GAP) - GAP,
    )
}

/// Pose des pièces de gauche à droite, à la ligne quand la suivante ne tient plus, avec un
/// espacement régulier. Rend la position de chacune et la hauteur totale.
pub fn place_blocks(sizes: &[Vec2], width: f32) -> (Vec<egui::Pos2>, f32) {
    let (mut x, mut y, mut line_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut positions = Vec::with_capacity(sizes.len());
    for size in sizes {
        if x > 0.0 && x + size.x > width + 0.5 {
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
    selected: Option<u64>,
) -> Outcome {
    let mut out = Outcome::default();
    if devices.is_empty() {
        ui.add_space(theme::tokens().space.xl);
        ui.vertical_centered(|ui| {
            let secondary = theme::palette(ui).text_secondary;
            ui.label(RichText::new("Aucun appareil appairé pour l'instant.").color(secondary));
            ui.label(
                RichText::new("Le bouton « + Ajouter » en haut à droite ouvre l'appairage.")
                    .color(secondary),
            );
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
    // Les ponts n'ont pas de titre : leurs tuiles discrètes se rangent à la suite des pièces.
    if !layout.bridges.is_empty() {
        blocks.push(Block {
            title: "",
            floor: None,
            average: None,
            ids: &layout.bridges,
            bridges: true,
        });
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            let width = ui.available_width();
            let sizes: Vec<(usize, Vec2)> = blocks
                .iter()
                .map(|b| {
                    let columns = block_columns(b.ids.len(), width);
                    let (head, h) = if b.bridges {
                        (0.0, BRIDGE_H)
                    } else {
                        (HEAD_H, tokens.height)
                    };
                    (columns, block_size(b.ids.len(), columns, head, h))
                })
                .collect();
            let sizes_only: Vec<Vec2> = sizes.iter().map(|s| s.1).collect();
            let (positions, total) = place_blocks(&sizes_only, width);
            // De la marge en bas : la dernière pièce n'est pas collée au journal.
            let (area, _) = ui.allocate_exact_size(
                Vec2::new(width, total + theme::tokens().space.lg),
                Sense::hover(),
            );
            for ((block, (columns, size)), pos) in blocks.iter().zip(&sizes).zip(positions) {
                let rect = Rect::from_min_size(area.min + pos.to_vec2(), *size);
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let (head, tile_h) = if block.bridges {
                    (0.0, BRIDGE_H)
                } else {
                    (HEAD_H, tokens.height)
                };
                if !block.bridges {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        theme::room_heading(ui, block.title, block.floor, block.average);
                    });
                }
                for (i, id) in block.ids.iter().enumerate() {
                    let (col, row) = (i % columns, i / columns);
                    let tile_rect = Rect::from_min_size(
                        rect.min
                            + Vec2::new(
                                col as f32 * (tokens.desktop_width + GAP),
                                head + row as f32 * (tile_h + GAP),
                            ),
                        Vec2::new(tokens.desktop_width, tile_h),
                    );
                    if !ui.is_rect_visible(tile_rect) {
                        continue;
                    }
                    let d = &devices[id];
                    if block.bridges {
                        bridge_card(
                            ui,
                            *id,
                            &d.info.name,
                            &bridge_note(devices, *id),
                            tile_rect,
                            selected == Some(*id),
                            &mut out,
                        );
                        continue;
                    }
                    let pending = expect.get(id).filter(|e| e.pending(d, now));
                    let busy = pending.is_some_and(|e| e.blocks_click(now));
                    let tile = tile_of(d, pending);
                    card(
                        ui,
                        *id,
                        &d.info.name,
                        &bridge_via(devices, d),
                        &tile,
                        tile_rect,
                        pending.is_some(),
                        busy,
                        selected == Some(*id),
                        &mut out,
                    );
                }
            }
        });
    out
}

/// « via Passerelle TaHoma » : la troisième ligne d'un appareil exposé par un pont.
fn bridge_via(devices: &BTreeMap<u64, Device>, d: &Device) -> String {
    d.info
        .bridge
        .and_then(|link| devices.get(&link.node_id))
        .filter(|_| d.info.caps.cover)
        .map(|b| format!("via {}", b.info.name))
        .unwrap_or_default()
}

/// Pont : une tuile discrète de 192 × 56, sans bordure, qui ouvre sa fiche.
fn bridge_card(
    ui: &mut Ui,
    id: u64,
    name: &str,
    note: &str,
    rect: Rect,
    selected: bool,
    out: &mut Outcome,
) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let response = ui
        .interact(rect, ui.make_persistent_id(("tile", id)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name}, pont, {note}"))
    });
    let painter = ui.painter();
    let fill = if selected {
        p.accent_soft
    } else if response.hovered() {
        t.hover(p)
    } else {
        p.surface_alt
    };
    painter.rect_filled(rect, t.tile.radius, fill);
    if selected {
        painter.rect_stroke(
            rect,
            t.tile.radius,
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(1.0),
            t.tile.radius,
            Stroke::new(2.0, p.accent_text),
            egui::StrokeKind::Outside,
        );
    }
    let icon_center = rect.left_center() + Vec2::new(14.0 + 10.0, 0.0);
    tile_icons::paint(painter, icon_center, 20.0, IconKind::Generic, p, 1.0);
    let x = rect.left() + 14.0 + 20.0 + 10.0;
    let name_galley = painter.layout_no_wrap(
        name.to_owned(),
        FontId::proportional(t.font.size.body),
        p.text,
    );
    let note_galley = painter.layout_no_wrap(
        note.to_owned(),
        FontId::proportional(t.font.size.caption + 1.0),
        p.text_secondary,
    );
    let total = name_galley.size().y + note_galley.size().y;
    let top = rect.center().y - total / 2.0;
    let note_y = top + name_galley.size().y;
    painter.galley(egui::pos2(x, top), name_galley, p.text);
    painter.galley(egui::pos2(x, note_y), note_galley, p.text_secondary);
    if response.clicked() || response.secondary_clicked() {
        out.detail = Some(id);
    }
}

/// Pose une ligne de texte dont le bas est à `y`, puis remonte `y` de sa hauteur.
fn put_line(painter: &egui::Painter, x: f32, y: &mut f32, text: &str, size: f32, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), color);
    *y -= galley.size().y;
    painter.galley(egui::pos2(x, *y), galley, color);
}

/// Une carte de 192 × 112. `via` : la troisième ligne d'un volet exposé par un pont (vide sinon).
/// `busy` : une commande vient de partir, un clic de plus est ignoré.
#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    id: u64,
    name: &str,
    via: &str,
    tile: &Tile,
    rect: Rect,
    expecting: bool,
    busy: bool,
    selected: bool,
    out: &mut Outcome,
) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let base = ui.make_persistent_id(("tile", id));
    let response = ui
        .interact(rect, base, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    // Annoncée comme un bouton : le nom et la valeur.
    response.widget_info(|| {
        let value = tile.value.as_deref().unwrap_or("");
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name}, {value}"))
    });
    // Le « ⋯ » : 28 × 28, dans le coin haut droit de la marge, toujours visible.
    let more_rect = Rect::from_min_size(
        rect.right_top() + Vec2::new(-14.0 - 28.0, 12.0),
        Vec2::splat(28.0),
    );
    let more = ui.interact(more_rect, base.with("more"), Sense::click());
    more.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            true,
            format!("Plus d'options pour {name}"),
        )
    });
    let radius = t.tile.radius;

    let painter = ui.painter();
    let hovered = response.hovered() || more.hovered();
    let focused = response.has_focus() || more.has_focus();
    painter.rect_filled(
        rect,
        radius,
        if selected && !tile.lit {
            p.accent_soft
        } else if hovered && !tile.lit {
            t.hover(p)
        } else {
            p.surface
        },
    );
    if tile.lit {
        painter.rect_filled(rect, radius, p.lueur.gamma_multiply(t.tile.on_tint_alpha));
    }
    let border = if selected {
        Stroke::new(2.0, p.accent)
    } else if expecting {
        Stroke::new(1.5, p.accent_text)
    } else if tile.unreachable {
        Stroke::new(1.5, p.warn)
    } else if tile.lit {
        Stroke::new(1.0, p.lueur)
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

    let (pad_x, pad_y) = (14.0, 12.0);
    let icon_size = t.tile.icon;
    let icon_center = rect.left_top() + Vec2::new(pad_x + icon_size / 2.0, pad_y + icon_size / 2.0);
    tile_icons::paint(painter, icon_center, icon_size, tile.icon, p, 1.0);
    if tile.low_battery {
        painter.circle_filled(
            icon_center + Vec2::new(icon_size * 0.5, -icon_size * 0.5),
            4.0,
            p.warn,
        );
    }
    if more.hovered() {
        painter.rect_filled(more_rect, 6.0, t.hover(p));
    }
    tile_icons::paint_more(
        painter,
        more_rect,
        if more.hovered() { p.text } else { p.text_faint },
    );

    // Du bas vers le haut : la troisième ligne éventuelle, la valeur, puis le nom au-dessus.
    let width = rect.width() - 2.0 * pad_x;
    let mut y = rect.bottom() - pad_y;
    let left = rect.left() + pad_x;
    if let Some(since) = tile.since.as_deref().filter(|_| tile.unreachable) {
        put_line(
            painter,
            left,
            &mut y,
            since,
            t.font.size.body,
            p.text_secondary,
        );
    } else if !via.is_empty() {
        put_line(
            painter,
            left,
            &mut y,
            via,
            t.font.size.caption + 1.0,
            p.text_secondary,
        );
    }
    // La valeur : en Lueur pour « Allumée », en orange pour une porte ouverte, sinon secondaire ;
    // l'humidité suit la température (« 20,1 °C · 48 % »).
    let mut value_x = 0.0;
    if tile.unreachable {
        // Cercle barré et le mot : la couleur ne porte jamais seule l'information.
        let height = t.font.size.body + 2.0;
        let c = egui::pos2(rect.left() + pad_x + 7.0, y - height / 2.0 + 1.0);
        painter.circle_stroke(c, 5.2, Stroke::new(1.5, p.warn));
        painter.line_segment(
            [c + Vec2::new(-3.7, 3.7), c + Vec2::new(3.7, -3.7)],
            Stroke::new(1.5, p.warn),
        );
        value_x = 19.0;
        put_line(
            painter,
            left + value_x,
            &mut y,
            UNREACHABLE,
            t.font.size.body,
            p.warn,
        );
    } else {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(tile.value.clone());
        if let Some((severity, text)) = &tile.small {
            let _ = severity;
            parts.push(text.clone());
        }
        let text = parts.join(" · ");
        if !text.is_empty() {
            let color = if tile.value_warn {
                p.warn
            } else if tile.lit {
                p.lueur_text
            } else if tile.small.as_ref().is_some_and(|(s, _)| s.is_some()) {
                p.warn
            } else {
                p.text_secondary
            };
            if tile.lit {
                painter.circle_filled(
                    egui::pos2(rect.left() + pad_x + 3.5, y - 8.0),
                    t.size.dot / 2.0,
                    p.lueur,
                );
                value_x = 12.0;
            }
            let mut job = egui::text::LayoutJob::single_section(
                text,
                egui::TextFormat::simple(FontId::proportional(t.font.size.body), color),
            );
            job.wrap.max_width = (width - value_x).max(20.0);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = painter.layout_job(job);
            y -= galley.size().y;
            painter.galley(egui::pos2(rect.left() + pad_x + value_x, y), galley, color);
        }
    }
    // Le nom : une ligne, « … » s'il est trop long.
    let mut job = egui::text::LayoutJob::single_section(
        name.to_owned(),
        egui::TextFormat::simple(FontId::proportional(t.font.size.title), p.text),
    );
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    y -= galley.size().y + 1.0;
    painter.galley(egui::pos2(rect.left() + pad_x, y), galley, p.text);

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
            shutters: None,
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
            shutters: None,
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
    fn an_extension_shutter_shows_the_last_order_and_only_opens_or_closes() {
        let caps = Capabilities {
            cover: true,
            cover_position: false,
            ..Default::default()
        };
        let mut s = online(device(
            protocol::events::EXTENSION_ID_MIN + 1,
            "Porte",
            caps,
        ));
        assert!(is_assumed_cover(&s));
        // Jamais commandé : inconnu, le clic ouvre, rien ne prétend savoir.
        let unknown = tile_of(&s, None);
        assert_eq!(unknown.value.as_deref(), Some("Inconnu"));
        assert_eq!(unknown.action, TileAction::Cover(CoverCommand::Open));
        assert_eq!(unknown.small, None);
        // Dernier ordre : fermer. L'état est dit tel qu'il est : déduit.
        s.apply_reading(Metric::Position, 0.0, 1.0);
        let closed = tile_of(&s, None);
        assert_eq!(closed.value.as_deref(), Some("Fermé"));
        assert_eq!(closed.action, TileAction::Cover(CoverCommand::Open));
        assert_eq!(
            closed.small,
            Some((None, "d'après le dernier ordre d'Ostal".to_string()))
        );
        // Ouvrir : le clic ferme ; jamais d'« arrêt », même avec un mouvement attendu.
        s.apply_reading(Metric::Position, 100.0, 2.0);
        let open = tile_of(&s, Some(&Expect::motion(CoverMotion::Closing)));
        assert_eq!(open.value.as_deref(), Some("Ouvert"));
        assert_eq!(open.action, TileAction::Cover(CoverCommand::Close));
        // Un volet Matter à l'identifiant ordinaire garde son comportement.
        let rts = Capabilities {
            cover: true,
            ..Default::default()
        };
        assert!(!is_assumed_cover(&online(device(5, "Volet RTS", rts))));
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
    fn an_unreachable_tile_says_so_at_full_contrast_and_only_opens_the_detail() {
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
            Some((Some(Severity::Warn), "injoignable".to_string()))
        );
        // Contraste plein, bordure orange, et depuis quand (d'après `last_seen`).
        assert!(tile.unreachable);
        assert!(tile
            .since
            .as_deref()
            .is_some_and(|s| s.starts_with("depuis ")));
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
        let three = block_size(3, 3, HEAD_H, tile.height);
        let two = block_size(2, 2, HEAD_H, tile.height);
        // Largeur d'une pièce de trois colonnes : trois tuiles de 192 px et deux écarts de 16.
        assert_eq!(three.x, 3.0 * tile.desktop_width + 2.0 * GAP);
        assert_eq!(three.y, HEAD_H + tile.height);
        // 1 232 px (6 colonnes) : deux pièces de trois colonnes se suivent, la troisième passe à
        // la ligne, et la quatrième (deux colonnes) la suit avec le même écart.
        let sizes = [three, three, three, two];
        let (pos, total) = place_blocks(&sizes, 1232.0);
        assert_eq!(pos[0], egui::pos2(0.0, 0.0));
        assert_eq!(
            pos[1],
            egui::pos2(three.x + BLOCK_GAP_X, 0.0),
            "écart régulier"
        );
        assert_eq!(pos[2], egui::pos2(0.0, three.y + BLOCK_GAP_Y));
        assert_eq!(pos[3], egui::pos2(three.x + BLOCK_GAP_X, pos[2].y));
        assert_eq!(total, pos[2].y + three.y);
        // Avec la fiche de 360 px (872 px, 4 colonnes) : une pièce de trois colonnes par ligne.
        let (pos, _) = place_blocks(&sizes, 872.0);
        assert_eq!(pos[1].y, three.y + BLOCK_GAP_Y);
        // Plus étroit qu'une pièce : une par ligne, sans boucle ni position négative.
        let (pos, total) = place_blocks(&sizes, 100.0);
        assert!(pos.windows(2).all(|w| w[1].y > w[0].y));
        assert!(total > 0.0);
    }

    #[test]
    fn the_grid_has_six_fixed_columns_and_four_beside_the_detail_panel() {
        let size = theme::tokens().size;
        // 1 280 px de fenêtre, 24 px de marge de chaque côté : 6 colonnes.
        let full = size.window_desktop_w - 48.0;
        assert_eq!(columns_for(full), 6);
        // La fiche de 360 px prend sa place : 4 colonnes.
        assert_eq!(columns_for(full - size.detail_panel), 4);
        assert_eq!(columns_for(10.0), 1);
        // Une pièce a au plus 6 colonnes, et jamais plus que ce qui tient.
        assert_eq!(block_columns(1, full), 1);
        assert_eq!(block_columns(3, full), 3);
        assert_eq!(block_columns(9, full), GRID_COLUMNS);
        assert_eq!(block_columns(9, full - size.detail_panel), 4);
        assert_eq!(block_columns(9, 100.0), 1);
        // Une pièce de 7 appareils sur 4 colonnes : 2 rangées.
        let tile = theme::tokens().tile;
        assert_eq!(
            block_size(7, 4, HEAD_H, tile.height).y,
            HEAD_H + 2.0 * tile.height + GAP
        );
        // Un pont : tuile de 192 × 56, sans titre.
        assert_eq!(
            block_size(1, 1, 0.0, BRIDGE_H),
            Vec2::new(tile.desktop_width, BRIDGE_H)
        );
    }
}
