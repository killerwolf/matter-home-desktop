//! Vue « Plan de la maison » : étages, pièces rectangulaires, objets placés,
//! avec l'état des appareils en direct. Deux modes : consultation et modification.

mod camera;
mod opening;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
};

use super::customize::{editor, DeviceEdit, EditResult};
use super::sync::{Failure, Incoming, SyncState};
use super::theme::{self, Palette};
use super::Device;
use matter_home_client::model::Model;
use protocol::format::{format_position, format_power};
use protocol::home::{
    self, snap, FovShape, HomePlan, Item, ItemKind, OpeningKind, Room, Symbol, ITEM_SNAP,
    MAX_CAMERA_RANGE, MAX_FOV, MIN_CAMERA_RANGE, MIN_FOV, MIN_ROOM_SIZE, ROOM_SNAP,
};
use protocol::wire::{CameraView, StreamKind, WireStatus};

const ITEM_RADIUS: f32 = 15.0;
const HANDLE_SIZE: f32 = 10.0;
const MIN_SCALE: f32 = 10.0;
const MAX_SCALE: f32 = 250.0;

/// Ce que l'on fait glisser depuis la palette vers le plan.
#[derive(Debug, Clone, Copy)]
enum Payload {
    Device(u64),
    Decor(Symbol),
    Camera(u64),
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
    /// Poignée de rotation d'une caméra.
    RotateCamera {
        id: u64,
    },
    /// Poignée de portée d'une caméra.
    CameraRange {
        id: u64,
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
    floor: usize,
    edit: bool,
    /// Pixels par mètre.
    scale: f32,
    /// Coordonnée (m) affichée au coin haut-gauche du canevas.
    offset: Vec2,
    fit_pending: bool,
    /// Largeur du canevas à la dernière image : un changement (panneau de détail, fenêtre
    /// redimensionnée) recadre le plan.
    canvas_width: f32,
    drag: Option<Drag>,
    selection: Option<Selection>,
    /// Modifié pendant cette image : à signaler au suivi de synchronisation.
    dirty: bool,
    /// Suivi de la synchronisation avec le serveur (dernière modification gagnante).
    sync: SyncState<HomePlan>,
    confirm_delete_floor: bool,
    /// Brouillon du nom et de l'icône de l'appareil sélectionné.
    device_edit: Option<DeviceEdit>,
    /// Caméra dont le direct est ouvert dans une fenêtre (clic en consultation).
    live_camera: Option<u64>,
}

/// Ce que montre un appareil placé : pictogramme, couleur, lignes de texte.
struct Look {
    symbol: Option<Symbol>,
    fill: Color32,
    name: String,
    value: Option<String>,
    battery_low: bool,
    /// Couleur de la valeur écrite sous le nom : discrète, sauf l'anormal (ouvert, injoignable).
    value_color: Color32,
    /// Caméras et volets : petit disque, pas de couleur vive.
    discreet: bool,
    /// Capteur d'ouverture : porte ou fenêtre, ouverte ou fermée (dessinée à part du pictogramme).
    opening: Option<opening::OpeningIcon>,
}

pub fn glyph(symbol: Option<Symbol>) -> &'static str {
    match symbol {
        Some(Symbol::Door) => "🚪",
        Some(Symbol::Window) => "▦",
        Some(Symbol::Shutter) => "▤",
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
    } else if caps.cover {
        Some(Symbol::Shutter)
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

fn look(item: &Item, devices: &BTreeMap<u64, Device>, p: &Palette) -> Look {
    match &item.kind {
        // Une caméra se dessine à part (`camera_look`, il lui faut la liste des caméras).
        ItemKind::Camera { .. } => Look {
            symbol: None,
            fill: p.text_secondary,
            name: "Caméra".into(),
            value: None,
            battery_low: false,
            value_color: p.text_secondary,
            discreet: true,
            opening: None,
        },
        ItemKind::Decor { symbol, label } => Look {
            symbol: Some(*symbol),
            fill: p.text_secondary,
            name: if label.is_empty() {
                symbol.label().into()
            } else {
                short(label)
            },
            value: None,
            battery_low: false,
            value_color: p.text_secondary,
            discreet: false,
            opening: None,
        },
        ItemKind::Device {
            node_id,
            symbol,
            opening,
        } => {
            let Some(d) = devices.get(node_id) else {
                return Look {
                    symbol: *symbol,
                    fill: p.text_secondary,
                    name: format!("Nœud {node_id:016X}"),
                    value: Some("inconnu".into()),
                    battery_low: false,
                    value_color: p.text_secondary,
                    discreet: false,
                    opening: None,
                };
            };
            // L'icône choisie pour l'appareil prime sur un ancien réglage propre au plan.
            let symbol = d.info.icon.or(*symbol).or_else(|| default_symbol(d));
            let (mut fill, mut value) = (p.accent, None);
            let mut value_color = p.text_secondary;
            if let Some(open) = d.open() {
                fill = if open { p.warn } else { p.text_secondary };
                if open {
                    value_color = p.warn;
                }
                value = Some(if open { "Ouvert" } else { "Fermé" }.to_string());
            } else if let Some(t) = d.temperature() {
                fill = theme::temperature_color(p, Some(t.value));
                let mut v = format!("{:.1} °C", t.value).replace('.', ",");
                if let Some(h) = d.humidity() {
                    v.push_str(&format!(" · {:.0} %", h.value));
                }
                value = Some(v);
            } else if d.info.caps.cover {
                let mut text = match d.position() {
                    Some(pos) => {
                        // Discret : un gris, la position est écrite en toutes lettres.
                        fill = p.text_secondary;
                        format_position(pos.value)
                    }
                    None => String::new(),
                };
                match d.cover_motion {
                    Some(protocol::events::CoverMotion::Opening) => text.push_str(" ▲"),
                    Some(protocol::events::CoverMotion::Closing) => text.push_str(" ▼"),
                    _ => {}
                }
                let text = text.trim().to_string();
                value = (!text.is_empty()).then_some(text);
            } else if d.info.caps.power || d.info.caps.on_off {
                fill = if d.on_off() == Some(true) {
                    p.accent
                } else {
                    p.text_secondary
                };
                value = d.power().map(|p| format_power(p.value)).or_else(|| {
                    d.on_off()
                        .map(|on| if on { "Allumée" } else { "Éteinte" }.to_string())
                });
            }
            // Sans nouvelles de l'appareil, la dernière mesure ne vaut plus rien : icône neutre.
            let stale = matches!(&d.status, WireStatus::Unreachable { .. })
                || (matches!(&d.status, WireStatus::Connecting) && d.last_seen.is_none());
            match &d.status {
                WireStatus::Unreachable { .. } => {
                    fill = p.text_secondary;
                    value = Some(super::devices::UNREACHABLE.into());
                    value_color = p.error;
                }
                WireStatus::Connecting if d.last_seen.is_none() => fill = p.text_secondary,
                _ => {}
            }
            Look {
                symbol,
                fill,
                name: short(&d.info.name),
                value,
                battery_low: d.battery_low(),
                value_color,
                discreet: d.info.caps.cover,
                opening: d.info.caps.contact.then(|| {
                    // Porte par défaut ; fenêtre si le plan ou l'appareil le dit.
                    let kind = opening.unwrap_or(if symbol == Some(Symbol::Window) {
                        OpeningKind::Window
                    } else {
                        OpeningKind::Door
                    });
                    opening::opening_icon(kind, if stale { None } else { d.open() })
                }),
            }
        }
    }
}

/// Ce que montre une caméra posée : nom du serveur, état en couleur ; une caméra que le serveur
/// ne connaît plus est grisée et nommée comme telle.
fn camera_look(item: &Item, cameras: &[CameraView], p: &Palette) -> Look {
    let ItemKind::Camera { camera_id, .. } = &item.kind else {
        unreachable!("camera_look ne reçoit que des caméras");
    };
    let camera = cameras.iter().find(|c| c.id == *camera_id);
    let state = camera::CameraState::of(camera);
    Look {
        symbol: None,
        fill: state.color(p),
        name: camera.map_or_else(|| "Caméra supprimée".to_string(), |c| short(&c.name)),
        value: (!matches!(
            state,
            camera::CameraState::Online | camera::CameraState::Missing
        ))
        .then(|| state.label().to_lowercase()),
        battery_low: false,
        value_color: state.color(p),
        discreet: true,
        opening: None,
    }
}

/// Le canevas a changé de largeur (panneau de détail ouvert ou fermé, fenêtre redimensionnée) :
/// le plan se recadre.
fn width_changed(before: f32, now: f32) -> bool {
    (now - before).abs() > 1.0
}

/// Texte sur une petite pastille `surface` : lisible quelle que soit la teinte de la pièce.
fn backed_text(
    painter: &Painter,
    pos: Pos2,
    text: &str,
    size: f32,
    color: Color32,
    backing: Color32,
) -> Rect {
    backed_text_at(painter, pos, Align2::CENTER_TOP, text, size, color, backing)
}

fn backed_text_at(
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    size: f32,
    color: Color32,
    backing: Color32,
) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), color);
    let rect = align.anchor_size(pos, galley.size());
    painter.rect_filled(rect.expand2(Vec2::new(3.0, 1.0)), 3.0, backing);
    painter.galley(rect.min, galley, color);
    rect
}

/// Petite légende en bas à gauche du plan : l'échelle de température et « Sans capteur ».
fn paint_legend(painter: &Painter, rect: Rect, pal: &Palette) {
    let stops = &theme::tokens().temperature.stops;
    let (sw, sh, gap) = (22.0, 10.0, 6.0);
    let font = FontId::proportional(11.0);
    let low = format!("{:.0}", stops.first().map_or(16.0, |s| s.at));
    let high = format!("{:.0} °C", stops.last().map_or(24.0, |s| s.at));
    let galley = |t: &str| painter.layout_no_wrap(t.to_owned(), font.clone(), pal.text_secondary);
    let (g_low, g_high, g_none) = (galley(&low), galley(&high), galley("Sans capteur"));
    let scale_w = g_low.size().x + gap + sw * stops.len() as f32 + gap + g_high.size().x;
    let total = scale_w + 16.0 + sw + gap + g_none.size().x;
    let height = 22.0;
    let origin = Pos2::new(rect.left() + 12.0, rect.bottom() - 12.0 - height);
    painter.rect_filled(
        Rect::from_min_size(origin, Vec2::new(total + 16.0, height)),
        4.0,
        pal.surface,
    );
    let mid = origin.y + height / 2.0;
    let mut x = origin.x + 8.0;
    painter.galley(
        Pos2::new(x, mid - g_low.size().y / 2.0),
        g_low.clone(),
        pal.text_secondary,
    );
    x += g_low.size().x + gap;
    for stop in stops {
        let r = Rect::from_min_size(Pos2::new(x, mid - sh / 2.0), Vec2::new(sw, sh));
        painter.rect_filled(r, 0.0, pal.surface);
        painter.rect_filled(r, 0.0, theme::temperature_fill(pal, Some(stop.at)));
        x += sw;
    }
    x += gap;
    painter.galley(
        Pos2::new(x, mid - g_high.size().y / 2.0),
        g_high.clone(),
        pal.text_secondary,
    );
    x += g_high.size().x + 16.0;
    let r = Rect::from_min_size(Pos2::new(x, mid - sh / 2.0), Vec2::new(sw, sh));
    painter.rect_filled(r, 0.0, theme::temperature_fill(pal, None));
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, pal.line), StrokeKind::Inside);
    x += sw + gap;
    painter.galley(
        Pos2::new(x, mid - g_none.size().y / 2.0),
        g_none.clone(),
        pal.text_secondary,
    );
}

/// Sans nouvelles de l'appareil : sa dernière mesure ne compte plus.
fn is_stale(d: &Device) -> bool {
    super::devices::is_stale(d)
}

/// Température moyenne de chaque pièce, d'après les capteurs qui y sont posés (règle de
/// `HomePlan::room_of`). Un capteur injoignable ne compte pas ; une pièce sans capteur n'a pas
/// d'entrée. Clé : (étage, pièce).
fn room_temperatures(plan: &HomePlan, devices: &BTreeMap<u64, Device>) -> HashMap<(u64, u64), f64> {
    let mut sums: HashMap<(u64, u64), (f64, usize)> = HashMap::new();
    for (id, d) in devices {
        let Some(t) = d.temperature().filter(|_| !is_stale(d)) else {
            continue;
        };
        if let Some((floor, room)) = plan.room_of(*id) {
            let e = sums.entry((floor.id, room.id)).or_default();
            e.0 += t.value;
            e.1 += 1;
        }
    }
    // Arrondie à la décimale affichée : la teinte se calcule depuis la valeur écrite.
    sums.into_iter()
        .map(|(k, (sum, n))| (k, (sum / n as f64 * 10.0).round() / 10.0))
        .collect()
}

/// Ouvertures (portes, fenêtres) ouvertes dans une pièce : « 1 ouvert ».
fn room_openings(room: &Room, items: &[Item], devices: &BTreeMap<u64, Device>) -> Option<String> {
    let open = items
        .iter()
        .filter(|i| room.contains(i.x, i.y))
        .filter_map(|i| match i.kind {
            ItemKind::Device { node_id, .. } => devices.get(&node_id),
            _ => None,
        })
        .filter(|d| !is_stale(d) && d.open() == Some(true))
        .count();
    (open > 0).then(|| format!("{open} ouvert{}", if open > 1 { "s" } else { "" }))
}

/// Ce qui empêche d'envoyer un plan : le serveur exige au moins un étage.
fn plan_problem(plan: &HomePlan) -> Option<String> {
    if plan.floors.is_empty() {
        return Some("le plan doit avoir au moins un étage".to_string());
    }
    // Les mêmes bornes que le serveur : une valeur hors plage ne part jamais (le serveur la
    // refuserait en bloc, et tout le plan resterait en brouillon).
    for item in plan.floors.iter().flat_map(|f| &f.items) {
        if let ItemKind::Camera { fov, range, .. } = &item.kind {
            if !(MIN_FOV..=MAX_FOV).contains(fov) {
                return Some(format!(
                    "l'ouverture d'une caméra doit être entre {MIN_FOV:.0} et {MAX_FOV:.0} °"
                ));
            }
            if !(MIN_CAMERA_RANGE..=MAX_CAMERA_RANGE).contains(range) {
                return Some(format!(
                    "la portée d'une caméra doit être entre {MIN_CAMERA_RANGE} et {MAX_CAMERA_RANGE} m"
                ));
            }
        }
        if !(0.0..360.0).contains(&item.rotation) {
            return Some("l'orientation d'un objet doit être entre 0 et 360 °".to_string());
        }
    }
    None
}

impl PlanView {
    /// Plan vide : le vrai plan vient du serveur (état complet à la connexion).
    pub fn new() -> Self {
        Self {
            plan: HomePlan::default(),
            floor: 0,
            edit: false,
            scale: 50.0,
            offset: Vec2::new(-1.0, -1.0),
            fit_pending: true,
            canvas_width: 0.0,
            drag: None,
            selection: None,
            dirty: false,
            sync: SyncState::with_check(plan_problem),
            confirm_delete_floor: false,
            device_edit: None,
            live_camera: None,
        }
    }

    /// Passe en mode Modifier.
    pub fn start_editing(&mut self) {
        self.edit = true;
        self.selection = None;
    }

    /// Le mode Modifier est actif.
    pub fn is_editing(&self) -> bool {
        self.edit
    }

    /// Le plan tel qu'il est affiché (brouillon local compris).
    pub fn plan(&self) -> &HomePlan {
        &self.plan
    }

    /// Une modification locale n'est pas encore arrivée au serveur.
    pub fn editing(&self) -> bool {
        self.dirty || self.sync.editing()
    }

    #[cfg(test)]
    pub fn show_name(&self) -> &str {
        &self.plan.floors[0].name
    }

    /// Simule une modification locale du nom du premier étage (tests).
    #[cfg(test)]
    pub fn edit_name(&mut self, name: &str) {
        self.plan.floors[0].name = name.into();
        self.sync.mark_changed(Instant::now());
    }

    /// Force un plan sans étage, comme si un état incohérent atteignait l'affichage (tests).
    #[cfg(test)]
    pub fn force_no_floor(&mut self) {
        self.plan.floors.clear();
    }

    #[cfg(test)]
    pub fn keep_mine(&mut self) {
        self.sync.keep_mine(Instant::now());
    }

    #[cfg(test)]
    pub fn has_remote_banner(&self) -> bool {
        self.sync.remote().is_some()
    }

    /// Déconnexion : le plan affiché n'est plus celui du serveur, on l'oublie.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Plan reçu du serveur (état complet ou `plan_changed`). Une édition en cours n'est
    /// jamais écrasée : voir `SyncState`. Un plan sans étage est remplacé par le plan par
    /// défaut : l'affichage suppose qu'il y en a toujours un.
    pub fn on_server(&mut self, mut incoming: HomePlan) {
        incoming.ensure_floor();
        if self.sync.on_server(incoming, &mut self.plan) == Incoming::Adopted {
            self.selection = None;
            self.device_edit = None;
            self.confirm_delete_floor = false;
        }
        self.clamp_floor();
    }

    /// L'étage affiché existe toujours (le plan a pu être remplacé par une autre version).
    fn clamp_floor(&mut self) {
        self.floor = self.floor.min(self.plan.floors.len().saturating_sub(1));
    }

    /// Modification à envoyer maintenant, s'il y en a une. `holding` : le pointeur est
    /// enfoncé (glisser en cours), on n'envoie rien avant le relâchement.
    pub fn poll_save(&self, now: Instant, holding: bool) -> Option<HomePlan> {
        self.sync.due(now, holding, &self.plan)
    }

    /// Modification à envoyer sans attendre le délai de calme (fermeture de la fenêtre).
    pub fn poll_save_now(&self) -> Option<HomePlan> {
        self.sync.due_now(&self.plan)
    }

    pub fn sent(&mut self, request_id: u64, content: HomePlan) {
        self.sync.sent(request_id, content);
    }

    pub fn saved(&mut self, request_id: u64, mut stored: Option<HomePlan>) {
        if let Some(stored) = stored.as_mut() {
            stored.ensure_floor();
        }
        self.sync.on_saved(request_id, stored, &mut self.plan);
        self.clamp_floor();
    }

    /// Envoi échoué. `true` si c'était un envoi du plan. Selon la panne, le brouillon reste
    /// (refus du contenu, panne passagère) ou le plan revient à la version du serveur.
    pub fn failed(&mut self, request_id: u64, failure: Failure, now: Instant) -> bool {
        let reverted = matches!(failure, Failure::Rejected);
        let was_ours = self
            .sync
            .on_failed(request_id, failure, now, &mut self.plan);
        if was_ours && reverted {
            self.selection = None;
        }
        self.clamp_floor();
        was_ours
    }

    /// La liaison est coupée : un brouillon est gardé pour la reprise, sinon tout est oublié.
    pub fn suspend(&mut self, now: Instant) {
        if !self.sync.suspend(now) {
            self.reset();
        }
    }

    /// Un envoi est à faire ou en vol (réveil de l'interface).
    pub fn needs_tick(&self) -> bool {
        self.dirty || self.sync.needs_tick(&self.plan)
    }

    /// Ce qu'il faut dire d'un plan qui n'est pas (encore) parti.
    pub fn notice(&self) -> Option<String> {
        self.sync.notice(&self.plan)
    }

    fn floor_mut(&mut self) -> &mut home::Floor {
        &mut self.plan.floors[self.floor]
    }

    /// Garde de l'affichage : il y a au moins un étage, et l'étage affiché existe. Un plan sans
    /// étage (jamais envoyé par ce serveur, mais l'interface ne doit pas en dépendre) est
    /// signalé avec de quoi le réparer, au lieu de planter.
    fn has_floor(&mut self, ui: &mut egui::Ui) -> bool {
        if self.plan.floors.is_empty() {
            ui.colored_label(theme::palette(ui).error, "Le plan n'a aucun étage.");
            if ui.button("Créer un étage").clicked() {
                self.plan = HomePlan::default();
                self.floor = 0;
                self.sync.mark_changed(Instant::now());
            }
            return false;
        }
        self.clamp_floor();
        true
    }

    /// Plan non envoyé (brouillon incomplet, refus du serveur, panne passagère), avec de quoi
    /// en sortir : revenir à la version du serveur.
    fn draft_banner(&mut self, ui: &mut egui::Ui) {
        let Some(text) = self.notice() else { return };
        let mut revert = false;
        theme::banner_frame(ui, theme::Banner::Warn).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(text);
                revert = ui
                    .add_enabled(
                        self.sync.can_revert(),
                        egui::Button::new("Revenir à la version du serveur"),
                    )
                    .on_hover_text("Abandonne vos modifications non enregistrées du plan.")
                    .on_disabled_hover_text("Un envoi est en cours : réessayez dans un instant.")
                    .clicked();
            });
        });
        if revert {
            self.revert_to_server();
        }
    }

    /// Abandonne le brouillon : le plan affiché redevient celui du serveur. `false` si c'est
    /// impossible pour l'instant (envoi en cours, liaison coupée).
    pub fn revert_to_server(&mut self) -> bool {
        if !self.sync.revert(&mut self.plan) {
            return false;
        }
        self.dirty = false;
        self.selection = None;
        self.device_edit = None;
        self.confirm_delete_floor = false;
        self.plan.ensure_floor();
        self.clamp_floor();
        true
    }

    /// Pourquoi le plan affiché ne peut pas être enregistré tel quel (rien ne partira avant
    /// que l'utilisateur agisse), s'il y a lieu.
    pub fn blocked(&self) -> Option<String> {
        self.sync.blocked(&self.plan)
    }

    /// Fenêtre du direct d'une caméra, ouverte par un clic sur le plan.
    fn live_window(&mut self, ui: &egui::Ui, model: &Model) {
        let Some(id) = self.live_camera else {
            return;
        };
        let title = model
            .camera(id)
            .map_or_else(|| "Caméra".to_string(), |c| c.name.clone());
        let mut open = true;
        egui::Window::new(title)
            .open(&mut open)
            .default_size([480.0, 300.0])
            .show(ui.ctx(), |ui| {
                super::player::player(ui, model, id, StreamKind::Sub);
            });
        if !open {
            self.live_camera = None;
        }
    }

    /// Un autre client a modifié le plan pendant l'édition : un choix attend.
    pub fn conflict_pending(&self) -> bool {
        self.sync.remote().is_some()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, model: &Model) -> PlanAction {
        let devices = &model.devices;
        let cameras = &model.cameras[..];
        if !self.has_floor(ui) {
            return PlanAction::None;
        }
        self.remote_banner(ui);
        self.draft_banner(ui);
        if !self.has_floor(ui) {
            return PlanAction::None;
        }
        self.toolbar(ui);
        ui.separator();

        let mut palette_action = None;
        if self.edit {
            egui::Panel::right("plan_palette")
                .resizable(false)
                .exact_size(250.0)
                .show(ui, |ui| {
                    palette_action = egui::ScrollArea::vertical()
                        .show(ui, |ui| self.palette(ui, devices, cameras))
                        .inner;
                });
        }
        let action = self.canvas(ui, devices, cameras);
        self.live_window(ui, model);
        if std::mem::take(&mut self.dirty) {
            self.sync.mark_changed(Instant::now());
        }
        match (action, palette_action) {
            (PlanAction::None, Some(a)) => a,
            (a, _) => a,
        }
    }

    /// Un autre client a modifié le plan pendant que nous l'éditions.
    fn remote_banner(&mut self, ui: &mut egui::Ui) {
        if self.sync.remote().is_none() {
            return;
        }
        theme::banner_frame(ui, theme::Banner::Info).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Le plan a été modifié par un autre client pendant votre édition.");
                if ui.button("Afficher sa version").clicked() {
                    self.sync.accept_remote(&mut self.plan);
                    self.selection = None;
                    self.device_edit = None;
                }
                if ui.button("Garder la mienne").clicked() {
                    self.sync.keep_mine(Instant::now());
                }
            });
        });
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
                    ui.colored_label(
                        theme::palette(ui).error,
                        "Supprimer l'étage et son contenu ?",
                    );
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
        cameras: &[CameraView],
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
                        ItemKind::Camera {
                            camera_id,
                            fov,
                            range,
                            shape,
                        } => {
                            let camera = cameras.iter().find(|c| c.id == *camera_id);
                            let state = camera::CameraState::of(camera);
                            match camera {
                                Some(c) => ui.label(format!("📹 {}", c.name)),
                                None => ui.label("📹 Caméra supprimée"),
                            };
                            ui.colored_label(state.color(theme::palette(ui)), state.label());
                            if camera.is_none() {
                                ui.small(
                                    "Le serveur ne connaît plus cette caméra. Retirez-la du plan.",
                                );
                            } else {
                                ui.small(
                                    "Orientation : tirez la poignée ronde (Maj : sans aimantation). \
                                     Portée : tirez la poignée carrée au bout du champ.",
                                );
                            }
                            // Ni plafond à 359 ni 359,x : on laisse dépasser puis on ramène dans
                            // [0, 360) (359 puis un cran de plus = 0).
                            let turned = ui
                                .add(
                                    egui::DragValue::new(&mut item.rotation)
                                        .range(-720.0..=720.0)
                                        .speed(1.0)
                                        .fixed_decimals(0)
                                        .suffix(" °"),
                                )
                                .on_hover_text("Orientation (0 ° = vers le haut du plan)")
                                .changed();
                            if turned || !(0.0..360.0).contains(&item.rotation) {
                                item.rotation = protocol::home::normalize_rotation(item.rotation);
                            }
                            changed |= turned;
                            changed |= ui
                                .add(
                                    egui::Slider::new(fov, MIN_FOV..=MAX_FOV)
                                        .text("Ouverture")
                                        .suffix(" °")
                                        .fixed_decimals(0),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    egui::Slider::new(range, MIN_CAMERA_RANGE..=MAX_CAMERA_RANGE)
                                        .logarithmic(true)
                                        .text("Portée")
                                        .suffix(" m")
                                        .step_by(0.25)
                                        .clamping(egui::SliderClamping::Always),
                                )
                                .changed();
                            ui.horizontal(|ui| {
                                for s in FovShape::ALL {
                                    changed |= ui.selectable_value(shape, s, s.label()).changed();
                                }
                            });
                        }
                        ItemKind::Device {
                            node_id,
                            symbol,
                            opening,
                        } => match devices.get(node_id) {
                            Some(d) => {
                                if self.device_edit.as_ref().map(|e| e.node_id) != Some(*node_id) {
                                    self.device_edit = Some(DeviceEdit::from_info(&d.info));
                                }
                                ui.label(format!("{} {}", glyph(icon_of(d)), d.info.name));
                                if d.info.caps.contact {
                                    // Matter ne distingue pas une porte d'une fenêtre.
                                    let current = opening.unwrap_or(
                                        if d.info.icon.or(*symbol) == Some(Symbol::Window) {
                                            OpeningKind::Window
                                        } else {
                                            OpeningKind::Door
                                        },
                                    );
                                    egui::ComboBox::from_label("Porte / Fenêtre")
                                        .selected_text(current.label())
                                        .show_ui(ui, |ui| {
                                            for k in OpeningKind::ALL {
                                                if ui
                                                    .selectable_label(current == k, k.label())
                                                    .clicked()
                                                    && current != k
                                                {
                                                    *opening = Some(k);
                                                    changed = true;
                                                }
                                            }
                                        });
                                }
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
        ui.strong("Caméras à placer");
        let placed_cameras = self.plan.placed_cameras();
        let mut any = false;
        for c in cameras.iter().filter(|c| !placed_cameras.contains(&c.id)) {
            any = true;
            ui.dnd_drag_source(
                egui::Id::unique(("plan_cam", c.id)),
                Payload::Camera(c.id),
                |ui| {
                    let state = camera::CameraState::of(Some(c));
                    ui.horizontal(|ui| {
                        ui.label(format!("⠿ 📹 {}", short(&c.name)));
                        ui.colored_label(state.color(theme::palette(ui)), "●");
                    });
                },
            );
        }
        if !any {
            ui.small(if cameras.is_empty() {
                "Aucune caméra : ajoutez-en dans l'application du serveur."
            } else {
                "Toutes les caméras sont placées."
            });
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

    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        devices: &BTreeMap<u64, Device>,
        cameras: &[CameraView],
    ) -> PlanAction {
        let size = ui.available_size().max(Vec2::new(200.0, 200.0));
        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = resp.rect;
        if width_changed(self.canvas_width, rect.width()) {
            self.canvas_width = rect.width();
            self.fit_pending = true;
        }
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

        let action = self.interact(ui, &resp, devices, cameras);
        self.paint(ui, &painter, rect, &resp, devices, cameras);
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

    /// Caméra sélectionnée (mode modification) : `(id, centre écran, rotation, portée en px)`.
    fn selected_camera(&self, rect: Rect) -> Option<(u64, Pos2, f32, f32)> {
        let Some(Selection::Item(id)) = self.selection else {
            return None;
        };
        let item = self.plan.floors[self.floor]
            .items
            .iter()
            .find(|i| i.id == id)?;
        let ItemKind::Camera { range, .. } = item.kind else {
            return None;
        };
        Some((
            id,
            self.to_screen(rect, item.x, item.y),
            item.rotation,
            range * self.scale,
        ))
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
        cameras: &[CameraView],
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
                        opening: None,
                    },
                    Payload::Decor(symbol) => ItemKind::Decor {
                        symbol,
                        label: String::new(),
                    },
                    Payload::Camera(camera_id) => ItemKind::camera(camera_id),
                };
                // Une caméra n'a qu'un objet sur le plan : déjà placée, on ignore le dépôt.
                let twice = matches!(kind, ItemKind::Camera { camera_id, .. }
                    if self.plan.placed_cameras().contains(&camera_id));
                if !twice {
                    self.floor_mut().items.push(Item {
                        id,
                        kind,
                        x: snap(w.x, ITEM_SNAP),
                        y: snap(w.y, ITEM_SNAP),
                        rotation: 0.0,
                    });
                    self.selection = Some(Selection::Item(id));
                    self.dirty = true;
                }
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
                let camera_handle =
                    self.selected_camera(rect)
                        .and_then(|(id, c, rot, range_px)| {
                            if camera::rotate_handle(c, rot).distance(o)
                                <= camera::HANDLE_RADIUS + 4.0
                            {
                                Some(Drag::RotateCamera { id })
                            } else {
                                camera::range_handle(c, rot, range_px)
                                    .filter(|h| h.distance(o) <= camera::HANDLE_RADIUS + 4.0)
                                    .map(|_| Drag::CameraRange { id })
                            }
                        });
                if let Some(handle) = camera_handle {
                    self.drag = Some(handle);
                } else if let Some(id) = self.item_at(rect, o) {
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
                    Some(Drag::RotateCamera { id }) => {
                        let free = ui.input(|i| i.modifiers.shift);
                        if let Some(item) = f.items.iter_mut().find(|i| i.id == *id) {
                            if let Some(r) =
                                camera::rotation_from_pointer(w.x - item.x, w.y - item.y, free)
                            {
                                item.rotation = r;
                                self.dirty = true;
                            }
                        }
                    }
                    Some(Drag::CameraRange { id }) => {
                        if let Some(item) = f.items.iter_mut().find(|i| i.id == *id) {
                            let new = camera::range_from_pointer(w.x - item.x, w.y - item.y);
                            if let ItemKind::Camera { range, .. } = &mut item.kind {
                                *range = new;
                                self.dirty = true;
                            }
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
                    match f.items.iter().find(|i| i.id == id).map(|i| &i.kind) {
                        Some(ItemKind::Device { node_id, .. }) if devices.contains_key(node_id) => {
                            action = PlanAction::OpenDevice(*node_id);
                        }
                        // Une caméra que le serveur ne connaît plus n'ouvre rien.
                        Some(ItemKind::Camera { camera_id, .. })
                            if cameras.iter().any(|c| c.id == *camera_id) =>
                        {
                            self.live_camera = Some(*camera_id);
                        }
                        _ => {}
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
        cameras: &[CameraView],
    ) {
        let visuals = ui.visuals();

        let pal = theme::palette(ui);
        painter.rect_filled(rect, 0.0, pal.bg);
        let painter = painter.with_clip_rect(rect);
        let text_color = pal.text;
        let hover = resp.hover_pos();

        // Grille d'un mètre en mode modification.
        if self.edit {
            let grid = pal.line;
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
                pal.text_secondary,
            );
        }

        // Pièces, teintées selon la température moyenne de leurs capteurs.
        let averages = room_temperatures(&self.plan, devices);
        for room in &floor.rooms {
            let r = Rect::from_min_max(
                self.to_screen(rect, room.x, room.y),
                self.to_screen(rect, room.x + room.w, room.y + room.h),
            );
            let selected = self.selection == Some(Selection::Room(room.id));
            let average = averages.get(&(floor.id, room.id)).copied();
            painter.rect_filled(r, 2.0, pal.surface);
            painter.rect_filled(r, 2.0, theme::temperature_fill(pal, average));
            if selected {
                painter.rect_filled(r, 2.0, visuals.selection.bg_fill.linear_multiply(0.35));
            }
            let wall = pal.text_secondary;
            painter.rect_stroke(r, 2.0, Stroke::new(3.0, wall), StrokeKind::Inside);
            painter.text(
                r.min + Vec2::new(8.0, 6.0),
                Align2::LEFT_TOP,
                &room.name,
                FontId::proportional(15.0),
                text_color,
            );
            let mut y = 26.0;
            if let Some(t) = average {
                painter.text(
                    r.min + Vec2::new(8.0, y),
                    Align2::LEFT_TOP,
                    format!("{t:.1} °C").replace('.', ","),
                    FontId::proportional(theme::tokens().font.size.heading + 4.0),
                    text_color,
                );
                y += 28.0;
            }
            if let Some(open) = room_openings(room, &floor.items, devices) {
                backed_text_at(
                    &painter,
                    r.min + Vec2::new(8.0, y),
                    Align2::LEFT_TOP,
                    &open,
                    12.0,
                    pal.warn,
                    pal.surface,
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

        // Champs filmés des caméras, sous les objets.
        for item in &floor.items {
            let ItemKind::Camera {
                camera_id,
                fov,
                range,
                shape,
            } = &item.kind
            else {
                continue;
            };
            let selected = self.selection == Some(Selection::Item(item.id));
            // Le champ filmé n'apparaît qu'au survol, à la sélection ou en mode Modifier.
            let c = self.to_screen(rect, item.x, item.y);
            let hovered = hover.is_some_and(|h| h.distance(c) <= ITEM_RADIUS + 3.0);
            if !(self.edit || selected || hovered) {
                continue;
            }
            let state = camera::CameraState::of(cameras.iter().find(|c| c.id == *camera_id));
            let field = camera::Field {
                rotation: item.rotation,
                fov: *fov,
                range_px: range * self.scale,
                shape: *shape,
            };
            camera::paint_fov(&painter, c, field, state, selected, pal);
        }

        // Objets.
        let mut hovered: Option<(&Item, Look)> = None;
        for item in &floor.items {
            let c = self.to_screen(rect, item.x, item.y);
            let is_camera = matches!(item.kind, ItemKind::Camera { .. });
            let lk = if is_camera {
                camera_look(item, cameras, pal)
            } else {
                look(item, devices, pal)
            };
            // Caméras et volets : plus petits, sans couleur vive.
            let radius = if lk.discreet {
                ITEM_RADIUS * 0.7
            } else {
                ITEM_RADIUS
            };
            let decor = matches!(item.kind, ItemKind::Decor { .. });
            let selected = self.selection == Some(Selection::Item(item.id));
            let ring = if selected {
                Stroke::new(3.0, visuals.selection.stroke.color)
            } else {
                Stroke::new(1.5, if decor { pal.text_secondary } else { pal.line })
            };
            if let ItemKind::Camera { camera_id, .. } = &item.kind {
                let state = camera::CameraState::of(cameras.iter().find(|c| c.id == *camera_id));
                camera::paint_icon(&painter, c, radius, item.rotation, state, ring, pal);
            } else {
                if let Some(icon) = lk.opening {
                    painter.circle_filled(c, radius, icon.disc(pal));
                    painter.circle_stroke(c, radius, ring);
                    opening::paint(&painter, c, radius, icon);
                } else {
                    painter.circle_filled(c, radius, lk.fill);
                    painter.circle_stroke(c, radius, ring);
                    painter.text(
                        c,
                        Align2::CENTER_CENTER,
                        glyph(lk.symbol),
                        FontId::proportional(if lk.discreet { 11.0 } else { 15.0 }),
                        pal.on_accent,
                    );
                }
            }
            if lk.battery_low {
                painter.circle_filled(c + Vec2::new(radius * 0.75, -radius * 0.75), 5.0, pal.error);
            }
            let is_hover = hover.is_some_and(|h| h.distance(c) <= ITEM_RADIUS + 3.0);
            // Caméras et volets : leur nom n'apparaît qu'au survol ou à la sélection (en mode
            // Modifier, il n'y a pas de bulle : les noms restent).
            if !lk.discreet || self.edit || selected || is_hover {
                let mut y = c.y + radius + 2.0;
                let name_rect = backed_text(
                    &painter,
                    Pos2::new(c.x, y),
                    &lk.name,
                    11.0,
                    text_color,
                    pal.surface,
                );
                y = name_rect.bottom();
                if let Some(v) = &lk.value {
                    backed_text(
                        &painter,
                        Pos2::new(c.x, y),
                        v,
                        if lk.discreet { 11.0 } else { 12.0 },
                        lk.value_color,
                        pal.surface,
                    );
                }
            }
            if is_hover {
                hovered = Some((item, lk));
            }
        }

        paint_legend(&painter, rect, pal);

        // Poignées de la caméra sélectionnée (rotation, portée).
        if self.edit {
            if let Some((_, c, rot, range_px)) = self.selected_camera(rect) {
                camera::paint_handles(
                    &painter,
                    c,
                    rot,
                    range_px,
                    visuals.selection.stroke.color,
                    pal,
                );
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
            if let ItemKind::Camera { camera_id, .. } = &item.kind {
                let camera = cameras.iter().find(|c| c.id == *camera_id);
                text = camera::tooltip(camera);
                if camera.is_some() {
                    text.push_str("\nCliquer pour le direct");
                }
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
    fn pal() -> &'static Palette {
        theme::tokens().palette(true)
    }

    use protocol::events::Capabilities;
    use protocol::wire::{Metric, WireNode};

    fn device(caps: Capabilities) -> Device {
        Device::new(WireNode {
            node_id: 1,
            name: "Capteur".into(),
            device_name: "Capteur".into(),
            custom_name: None,
            icon: None,
            vendor: None,
            product: None,
            device_types: vec![],
            caps,
            bridge: None,
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
        assert_eq!(
            default_symbol(&device(Capabilities {
                cover: true,
                ..Default::default()
            })),
            Some(Symbol::Shutter)
        );
    }

    #[test]
    fn cover_look_shows_position_and_motion() {
        let mut devices = BTreeMap::new();
        let mut shutter = device(Capabilities {
            cover: true,
            cover_position: true,
            ..Default::default()
        });
        shutter.status = WireStatus::Online;
        shutter.apply_reading(Metric::Position, 0.0, 0.0);
        devices.insert(1, shutter);
        let item = Item {
            id: 5,
            kind: ItemKind::Device {
                node_id: 1,
                symbol: None,
                opening: None,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        let lk = look(&item, &devices, pal());
        assert_eq!(
            (lk.symbol, lk.value.as_deref()),
            (Some(Symbol::Shutter), Some("Fermé"))
        );
        let d = devices.get_mut(&1).unwrap();
        d.apply_reading(Metric::Position, 65.0, 1.0);
        d.cover_motion = Some(protocol::events::CoverMotion::Opening);
        let lk = look(&item, &devices, pal());
        assert_eq!(
            (lk.fill, lk.value.as_deref()),
            (pal().text_secondary, Some("Ouvert à 65 % ▲"))
        );
    }

    #[test]
    fn a_contact_sensor_is_a_door_or_a_window_open_or_closed() {
        use opening::OpeningIcon::*;
        let mut devices = BTreeMap::new();
        let mut sensor = device(Capabilities {
            contact: true,
            ..Default::default()
        });
        sensor.status = WireStatus::Online;
        devices.insert(1, sensor);
        let item = |opening, symbol| Item {
            id: 5,
            kind: ItemKind::Device {
                node_id: 1,
                symbol,
                opening,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        let icon = |devices: &BTreeMap<u64, Device>, opening, symbol| {
            look(&item(opening, symbol), devices, pal()).opening
        };
        // Pas encore de mesure : neutre. Porte par défaut.
        assert_eq!(icon(&devices, None, None), Some(DoorUnknown));
        devices
            .get_mut(&1)
            .unwrap()
            .apply_reading(Metric::Open, 0.0, 1.0);
        assert_eq!(icon(&devices, None, None), Some(DoorClosed));
        assert_eq!(
            icon(&devices, Some(OpeningKind::Window), None),
            Some(WindowClosed)
        );
        // Un ancien réglage « fenêtre » du plan reste une fenêtre.
        assert_eq!(
            icon(&devices, None, Some(Symbol::Window)),
            Some(WindowClosed)
        );
        devices
            .get_mut(&1)
            .unwrap()
            .apply_reading(Metric::Open, 1.0, 2.0);
        assert_eq!(icon(&devices, None, None), Some(DoorOpen));
        assert_eq!(
            icon(&devices, Some(OpeningKind::Window), None),
            Some(WindowOpen)
        );
        // Injoignable (ou jamais joint) : la dernière mesure ne vaut plus rien, icône neutre,
        // jamais une porte orange à côté de « injoignable ».
        devices.get_mut(&1).unwrap().status = WireStatus::Unreachable {
            code: protocol::wire::ErrorCode::DeviceUnreachable,
            detail: None,
            retry_in_s: 5,
        };
        assert_eq!(icon(&devices, None, None), Some(DoorUnknown));
        assert_eq!(
            icon(&devices, Some(OpeningKind::Window), None),
            Some(WindowUnknown)
        );
        devices.get_mut(&1).unwrap().status = WireStatus::Connecting;
        assert!(devices[&1].last_seen.is_none());
        assert_eq!(icon(&devices, None, None), Some(DoorUnknown));
        devices.get_mut(&1).unwrap().status = WireStatus::Online;
        assert_eq!(icon(&devices, None, None), Some(DoorOpen));
        // Un appareil qui n'est pas un capteur d'ouverture garde son pictogramme.
        devices.insert(2, device(Capabilities::default()));
        let other = Item {
            kind: ItemKind::Device {
                node_id: 2,
                symbol: None,
                opening: Some(OpeningKind::Window),
            },
            ..item(None, None)
        };
        assert_eq!(look(&other, &devices, pal()).opening, None);
    }

    #[test]
    fn device_look_follows_live_state() {
        let mut devices = BTreeMap::new();
        let mut door = device(Capabilities {
            contact: true,
            battery: true,
            ..Default::default()
        });
        door.status = WireStatus::Online;
        door.apply_reading(Metric::Open, 1.0, 1.0);
        door.apply_reading(Metric::BatteryLow, 1.0, 1.0);
        devices.insert(1, door);
        let item = Item {
            id: 5,
            kind: ItemKind::Device {
                node_id: 1,
                symbol: None,
                opening: None,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        let lk = look(&item, &devices, pal());
        assert_eq!(
            (lk.symbol, lk.fill, lk.value.as_deref(), lk.battery_low),
            (Some(Symbol::Door), pal().warn, Some("Ouvert"), true)
        );

        devices
            .get_mut(&1)
            .unwrap()
            .apply_reading(Metric::Open, 0.0, 2.0);
        assert_eq!(look(&item, &devices, pal()).fill, pal().text_secondary);

        devices.get_mut(&1).unwrap().status = WireStatus::Unreachable {
            code: protocol::wire::ErrorCode::DeviceUnreachable,
            detail: Some("x".into()),
            retry_in_s: 5,
        };
        let lk = look(&item, &devices, pal());
        assert_eq!(
            (lk.fill, lk.value.as_deref()),
            (pal().text_secondary, Some("injoignable"))
        );

        // L'icône de l'appareil prime sur un ancien pictogramme propre au plan.
        devices.get_mut(&1).unwrap().info.icon = Some(Symbol::Light);
        let legacy = Item {
            id: 7,
            kind: ItemKind::Device {
                node_id: 1,
                symbol: Some(Symbol::Window),
                opening: None,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        assert_eq!(look(&legacy, &devices, pal()).symbol, Some(Symbol::Light));

        // Pictogramme choisi par l'utilisateur, appareil inconnu.
        let window = Item {
            id: 6,
            kind: ItemKind::Device {
                node_id: 99,
                symbol: Some(Symbol::Window),
                opening: None,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        let lk = look(&window, &devices, pal());
        assert_eq!(
            (lk.symbol, lk.value.as_deref()),
            (Some(Symbol::Window), Some("inconnu"))
        );
    }

    fn online(mut d: Device, id: u64) -> Device {
        d.info.node_id = id;
        d.status = WireStatus::Online;
        d.last_seen = Some(1.0);
        d
    }

    fn at(id: u64, node_id: u64, x: f32) -> Item {
        Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
                opening: None,
            },
            x,
            y: 1.0,
            rotation: 0.0,
        }
    }

    fn salon_plan(items: Vec<Item>) -> (HomePlan, Room) {
        let room = Room {
            id: 1,
            name: "Salon".into(),
            x: 0.0,
            y: 0.0,
            w: 5.0,
            h: 4.0,
        };
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room.clone());
        plan.floors[0].items = items;
        (plan, room)
    }

    fn thermometer(id: u64, celsius: f64) -> Device {
        let mut d = online(
            device(Capabilities {
                temperature: true,
                ..Default::default()
            }),
            id,
        );
        d.apply_reading(Metric::Temperature, celsius, 0.0);
        d
    }

    #[test]
    fn a_room_is_tinted_by_the_average_of_its_sensors() {
        let mut devices = BTreeMap::new();
        devices.insert(1, thermometer(1, 20.0));
        devices.insert(2, thermometer(2, 22.0));
        let mut door = online(
            device(Capabilities {
                contact: true,
                ..Default::default()
            }),
            3,
        );
        door.apply_reading(Metric::Open, 1.0, 0.0);
        devices.insert(3, door);
        // Un capteur hors de la pièce ne compte pas.
        devices.insert(4, thermometer(4, 30.0));
        let (plan, room) = salon_plan(vec![
            at(10, 1, 1.0),
            at(11, 2, 2.0),
            at(12, 3, 3.0),
            at(13, 4, 9.0),
        ]);
        let averages = room_temperatures(&plan, &devices);
        assert_eq!(averages.len(), 1);
        assert_eq!(averages[&(plan.floors[0].id, room.id)], 21.0);
        assert_eq!(
            room_openings(&room, &plan.floors[0].items, &devices).as_deref(),
            Some("1 ouvert")
        );
        assert_eq!(room_openings(&room, &[], &devices), None);
        // 21 °C tombe sur le palier de 20 ou de 22 : jamais le gris des pièces sans capteur.
        let p = pal();
        assert_ne!(theme::temperature_fill(p, Some(21.0)), p.no_sensor);
    }

    #[test]
    fn an_unreachable_sensor_does_not_count_and_a_room_without_sensor_stays_grey() {
        let mut devices = BTreeMap::new();
        devices.insert(1, thermometer(1, 20.0));
        let mut lost = thermometer(2, 30.0);
        lost.status = WireStatus::Unreachable {
            code: protocol::wire::ErrorCode::DeviceUnreachable,
            detail: None,
            retry_in_s: 5,
        };
        devices.insert(2, lost);
        let (plan, room) = salon_plan(vec![at(10, 1, 1.0), at(11, 2, 2.0)]);
        let key = (plan.floors[0].id, room.id);
        assert_eq!(room_temperatures(&plan, &devices)[&key], 20.0);

        // Le seul capteur devient injoignable : plus de moyenne, la pièce est grise.
        devices.remove(&1);
        assert!(room_temperatures(&plan, &devices).is_empty());
        let p = pal();
        assert_eq!(theme::temperature_fill(p, None), p.no_sensor);
        // Jamais joint (en connexion, sans dernière communication) : ne compte pas non plus.
        let mut fresh = thermometer(5, 25.0);
        fresh.status = WireStatus::Connecting;
        fresh.last_seen = None;
        devices.insert(5, fresh);
        let (plan, _) = salon_plan(vec![at(10, 5, 1.0)]);
        assert!(room_temperatures(&plan, &devices).is_empty());
    }

    #[test]
    fn the_room_tint_uses_the_step_colour_of_the_average() {
        let p = pal();
        let stops = &theme::tokens().temperature.stops;
        assert_eq!(theme::temperature_color(p, Some(16.0)), stops[0].color);
        assert_eq!(theme::temperature_color(p, Some(23.4)), stops[4].color);
        // Le même aplat, à peine plus froid, change de palier : paliers de 2 °C.
        assert_ne!(
            theme::temperature_color(p, Some(18.9)),
            theme::temperature_color(p, Some(19.1)),
        );
    }

    #[test]
    fn the_plan_is_fitted_again_when_the_canvas_width_changes() {
        assert!(width_changed(0.0, 1180.0), "première image");
        assert!(width_changed(1180.0, 720.0), "le panneau s'ouvre");
        assert!(width_changed(720.0, 1180.0), "le panneau se ferme");
        assert!(
            !width_changed(1180.0, 1180.4),
            "pas de recadrage pour un demi-pixel"
        );
        let mut view = PlanView::new();
        view.fit_pending = false;
        view.start_editing();
        assert!(view.is_editing());
    }

    #[test]
    fn a_shutter_is_a_discreet_grey_and_labels_follow_the_discreet_rule() {
        let mut devices = BTreeMap::new();
        let mut shutter = online(
            device(Capabilities {
                cover: true,
                cover_position: true,
                ..Default::default()
            }),
            1,
        );
        shutter.apply_reading(Metric::Position, 65.0, 1.0);
        devices.insert(1, shutter);
        let lk = look(&at(5, 1, 0.0), &devices, pal());
        assert!(lk.discreet);
        assert_eq!(lk.fill, pal().text_secondary, "plus de bleu vif");
        assert_eq!(lk.value_color, pal().text_secondary);
        // Un thermomètre n'est pas discret : son étiquette reste visible, à une décimale.
        let mut t = thermometer(2, 20.86);
        t.apply_reading(Metric::Humidity, 49.0, 1.0);
        devices.insert(2, t);
        let lk = look(&at(6, 2, 0.0), &devices, pal());
        assert!(!lk.discreet);
        assert_eq!(lk.value.as_deref(), Some("20,9 °C · 49 %"));
    }

    #[test]
    fn the_tint_follows_the_displayed_rounded_average() {
        let mut devices = BTreeMap::new();
        devices.insert(1, thermometer(1, 18.96));
        devices.insert(2, thermometer(2, 18.96));
        let (plan, room) = salon_plan(vec![at(10, 1, 1.0), at(11, 2, 2.0)]);
        let key = (plan.floors[0].id, room.id);
        // 18,96 est écrit « 19,0 » : la teinte est celle de 19,0, pas de 18,96.
        assert_eq!(room_temperatures(&plan, &devices)[&key], 19.0);
    }

    #[test]
    fn a_plan_with_a_bad_camera_value_is_never_sent() {
        let mut plan = HomePlan::default();
        assert_eq!(plan_problem(&plan), None);
        let with = |fov: f32, range: f32, rotation: f32| {
            let mut plan = HomePlan::default();
            plan.floors[0].items.push(Item {
                id: 1,
                kind: ItemKind::Camera {
                    camera_id: 5,
                    fov,
                    range,
                    shape: FovShape::Cone,
                },
                x: 0.0,
                y: 0.0,
                rotation,
            });
            plan
        };
        assert_eq!(plan_problem(&with(90.0, 3.0, 0.0)), None);
        assert_eq!(plan_problem(&with(180.0, 50.0, 359.9)), None);
        assert!(plan_problem(&with(5.0, 3.0, 0.0))
            .unwrap()
            .contains("ouverture"));
        assert!(plan_problem(&with(90.0, 51.0, 0.0))
            .unwrap()
            .contains("portée"));
        assert!(plan_problem(&with(90.0, 0.1, 0.0)).is_some());
        assert!(plan_problem(&with(f32::NAN, 3.0, 0.0)).is_some());
        assert!(plan_problem(&with(90.0, 3.0, 360.0)).is_some());
        plan.floors.clear();
        assert!(plan_problem(&plan).is_some());
    }

    fn camera_item(id: u64, camera_id: u64, x: f32, rotation: f32) -> Item {
        Item {
            id,
            kind: ItemKind::camera(camera_id),
            x,
            y: 1.0,
            rotation,
        }
    }

    fn camera_view(id: u64, name: &str, status: protocol::wire::CameraStatus) -> CameraView {
        CameraView {
            id,
            name: name.into(),
            status,
            has_sub_stream: false,
            stream: None,
        }
    }

    #[test]
    fn camera_look_follows_status_and_greys_out_orphans() {
        let cam = protocol::wire::CAMERA_ID_MIN + 1;
        let item = camera_item(5, cam, 1.0, 0.0);
        let mut cameras = vec![camera_view(
            cam,
            "Entrée",
            protocol::wire::CameraStatus::Online,
        )];
        let lk = camera_look(&item, &cameras, pal());
        assert_eq!(
            (lk.name.as_str(), lk.value.as_deref(), lk.fill),
            ("Entrée", None, pal().ok)
        );
        cameras[0].status = protocol::wire::CameraStatus::Unreachable {
            problem: protocol::wire::CameraProblem::Unreachable,
            detail: None,
            since: None,
        };
        let lk = camera_look(&item, &cameras, pal());
        assert_eq!(
            (lk.value.as_deref(), lk.fill),
            (Some("injoignable"), pal().error)
        );
        cameras[0].status = protocol::wire::CameraStatus::Unknown;
        assert_eq!(
            camera_look(&item, &cameras, pal()).value.as_deref(),
            Some("état inconnu")
        );
        // Caméra supprimée du serveur : grisée, nommée comme telle, rien à ouvrir.
        let lk = camera_look(&item, &[], pal());
        assert_eq!(lk.name, "Caméra supprimée");
        assert_eq!(lk.fill, pal().text_secondary);
    }

    #[test]
    fn the_plan_draws_and_selects_cameras_including_orphans() {
        let cam = protocol::wire::CAMERA_ID_MIN + 1;
        let gone = protocol::wire::CAMERA_ID_MIN + 2;
        let model = Model {
            cameras: vec![camera_view(
                cam,
                "Entrée",
                protocol::wire::CameraStatus::Online,
            )],
            ..Model::default()
        };
        let mut view = PlanView::new();
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(Room {
            id: 2,
            name: "Salon".into(),
            x: 0.0,
            y: 0.0,
            w: 5.0,
            h: 4.0,
        });
        plan.floors[0].items.push(camera_item(5, cam, 1.0, 45.0));
        plan.floors[0].items.push(camera_item(6, gone, 3.0, 300.0));
        view.on_server(plan);
        let ctx = egui::Context::default();
        let run = |view: &mut PlanView| {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = view.show(ui, &model);
            })
            .textures_delta
            .clear();
        };
        // Consultation : dessin sans panique, rien d'ouvert.
        run(&mut view);
        assert_eq!(view.live_camera, None);
        // Modification : la caméra sélectionnée a ses poignées, l'orpheline aussi (à retirer).
        view.edit = true;
        for id in [5, 6] {
            view.selection = Some(Selection::Item(id));
            run(&mut view);
            let (sel, _, rot, range_px) = view
                .selected_camera(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0)))
                .unwrap();
            assert_eq!(sel, id);
            assert!(rot > 0.0 && range_px > 0.0);
        }
        // Une pièce sélectionnée n'a pas de poignée de caméra.
        view.selection = Some(Selection::Room(2));
        assert!(view.selected_camera(Rect::ZERO).is_none());
        // La caméra posée est dans la pièce (même règle que pour les appareils).
        assert_eq!(
            view.plan().camera_room(cam).map(|r| r.name.as_str()),
            Some("Salon")
        );
    }
}
