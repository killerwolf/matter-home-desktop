//! Onglet « Caméras » : la mosaïque de toutes les caméras, et la vue détaillée d'une seule.
//!
//! Règles de charge (voir `docs/cameras-contrat.md` et le ticket n° 7) :
//! - la mosaïque ne lit que le **flux réduit** ; une caméra sans flux réduit n'ouvre **rien**
//!   (une Aqara G100 ne tient pas trois canaux à la fois) : sa tuile invite à ouvrir la caméra ;
//! - une tuile n'appelle `player` que si elle est à l'écran (zone de défilement comprise) et dans
//!   la limite de [`MAX_LIVE_TILES`] : ne plus appeler `player` coupe la connexion ;
//! - une caméra qui n'est pas « en ligne » n'ouvre aucun flux : on ne harcèle pas une caméra
//!   injoignable.
//!
//! L'état de la page (caméra ouverte, flux principal demandé) vit dans la mémoire d'egui, pour ne
//! rien ajouter à l'application : il disparaît avec la caméra (supprimée) ou la session.

use eframe::egui::{self, RichText};
use protocol::home::HomePlan;
use protocol::wire::{CameraStatus, CameraView, StreamKind};

use super::camera_text::{offline_since_now, problem_text};
use super::player::{is_active, player_compact};
use super::theme::{self, Severity, Thumb};
use matter_home_client::model::Model;

/// Nombre maximal de tuiles qui lisent un flux en même temps (le décodage se fait sur le client,
/// et chaque flux ouvre une connexion côté serveur). Les suivantes montrent une image d'attente.
/// Le serveur ne garde qu'une session par (caméra, flux) ; la limite borne donc le nombre de caméras
/// distinctes lues à la fois (une Aqara G100 tolère environ deux canaux RTSP).
pub const MAX_LIVE_TILES: usize = 4;

/// Largeur minimale d'une tuile.
const TILE_MIN_WIDTH: f32 = 260.0;
const TILE_GAP: f32 = 10.0;
/// Nom de la pièce quand la caméra n'est posée dans aucune (tests : la vignette n'écrit rien).
#[cfg(test)]
pub const NO_ROOM: &str = "Sans pièce";

/// État de la page : la caméra ouverte en grand et le flux demandé.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct PageState {
    open: Option<u64>,
    main: bool,
}

/// Ouvre une caméra en grand (option `--camera`).
pub fn open(ctx: &egui::Context, id: u64) {
    ctx.data_mut(|d| {
        d.insert_temp(
            page_id(),
            PageState {
                open: Some(id),
                main: false,
            },
        )
    });
}

fn page_id() -> egui::Id {
    egui::Id::unique("cameras-page-state")
}

/// Ce que montre la zone vidéo d'une tuile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileVideo {
    /// Lire ce flux (toujours le réduit dans la mosaïque).
    Live(StreamKind),
    /// Pas de flux réduit : on n'ouvre pas le principal pour une mosaïque.
    NoSubStream,
    /// Caméra injoignable ou pas encore vérifiée : aucune connexion.
    NotOnline,
}

/// Choix du flux d'une tuile, selon l'état de la caméra.
pub fn tile_video(camera: &CameraView) -> TileVideo {
    if !matches!(camera.status, CameraStatus::Online) {
        TileVideo::NotOnline
    } else if camera.has_sub_stream {
        TileVideo::Live(StreamKind::Sub)
    } else {
        TileVideo::NoSubStream
    }
}

/// Le flux qu'une tuile lit : **toujours le réduit** (jamais le principal, trop lourd pour une
/// mosaïque), et rien si la caméra n'en a pas.
pub fn tile_stream(camera: &CameraView) -> Option<StreamKind> {
    camera.has_sub_stream.then_some(StreamKind::Sub)
}

/// Une pièce et ses caméras, triées par nom.
#[derive(Debug, PartialEq)]
pub struct Group<'a> {
    /// `None` : « Sans pièce ».
    pub room: Option<String>,
    pub cameras: Vec<&'a CameraView>,
}

impl Group<'_> {
    #[cfg(test)]
    pub fn title(&self) -> &str {
        self.room.as_deref().unwrap_or(NO_ROOM)
    }
}

/// Regroupe par pièce (ordre alphabétique, sans accent ni casse), puis par nom ; « Sans pièce »
/// en dernier. La pièce vient du plan : l'objet « caméra » posé dans une pièce.
pub fn group_cameras<'a>(cameras: &'a [CameraView], plan: &HomePlan) -> Vec<Group<'a>> {
    let mut groups: Vec<Group<'a>> = Vec::new();
    for camera in cameras {
        let room = plan.camera_room(camera.id).map(|r| r.name.clone());
        match groups.iter_mut().find(|g| g.room == room) {
            Some(group) => group.cameras.push(camera),
            None => groups.push(Group {
                room,
                cameras: vec![camera],
            }),
        }
    }
    for group in &mut groups {
        group
            .cameras
            .sort_by_cached_key(|c| (sort_key(&c.name), c.id));
    }
    groups.sort_by_cached_key(|g| (g.room.is_none(), g.room.as_deref().map(sort_key)));
    groups
}

fn sort_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Nombre de colonnes pour une largeur donnée (au moins une).
pub fn columns_for(width: f32) -> usize {
    (((width + TILE_GAP) / (TILE_MIN_WIDTH + TILE_GAP)).floor() as usize).max(1)
}

/// Vrai si la tuile est (au moins en partie) dans la zone visible.
pub fn tile_visible(clip: egui::Rect, tile: egui::Rect) -> bool {
    clip.intersects(tile)
}

/// Une tuile veut un flux si sa caméra est en ligne et en a un réduit, ou si elle en lisait déjà
/// un (la caméra devient injoignable : le serveur reprend tout seul, la tuile continue de
/// l'afficher et le message du lecteur dit ce qui se passe).
fn tile_wants_stream(ctx: &egui::Context, camera: &CameraView) -> bool {
    matches!(tile_video(camera), TileVideo::Live(_))
        || tile_stream(camera).is_some_and(|kind| is_active(ctx, camera.id, kind))
}

/// Décide, parmi les tuiles de l'image, lesquelles lisent vraiment un flux : celles qui en ont
/// un à lire (`wants`), sont visibles, dans la limite de `max`, dans l'ordre d'affichage.
pub fn live_slots(tiles: &[(bool, bool)], max: usize) -> Vec<bool> {
    let mut used = 0;
    tiles
        .iter()
        .map(|&(wants, visible)| {
            let live = wants && visible && used < max;
            if live {
                used += 1;
            }
            live
        })
        .collect()
}

/// `detail` redit le motif `problem` : ses trois premiers mots s'y trouvent déjà.
fn repeats(detail: &str, problem: &str) -> bool {
    let lead: Vec<&str> = problem.split_whitespace().take(3).collect();
    !lead.is_empty()
        && detail
            .to_lowercase()
            .contains(&lead.join(" ").to_lowercase())
}

/// Phrase d'état d'une caméra : (état, détail éventuel, gravité si anormal).
pub fn status_text(status: &CameraStatus) -> (&'static str, Option<String>, Option<Severity>) {
    match status {
        CameraStatus::Online => ("En ligne", None, None),
        CameraStatus::Unreachable {
            problem,
            detail,
            since,
        } => {
            let mut text = format!("Injoignable : {}", problem_text(*problem));
            // Le détail du serveur répète souvent le motif : on ne le dit qu'une fois.
            if let Some(detail) = detail
                .as_deref()
                .filter(|d| !d.is_empty() && !repeats(d, problem_text(*problem)))
            {
                text.push_str(&format!(" ({detail})"));
            }
            if let Some(since) = since {
                text.push_str(&format!(" · {}", offline_since_now(*since)));
            }
            ("Injoignable", Some(text), Some(Severity::Warn))
        }
        CameraStatus::Unknown => ("Inconnu", Some("Pas encore vérifiée.".to_string()), None),
    }
}

/// « h264 · 1920×1080 », ou « h264 », ou rien.
pub fn stream_summary(camera: &CameraView) -> Option<String> {
    let info = camera.stream.as_ref()?;
    let codec = info.codec.to_uppercase();
    Some(match (info.width, info.height) {
        (Some(w), Some(h)) => format!("{codec} · {w}×{h}"),
        _ => codec,
    })
}

/// Texte de la zone vidéo quand rien n'est lu.
fn placeholder_text(video: TileVideo, camera: &CameraView, capped: bool) -> String {
    match video {
        TileVideo::NoSubStream => {
            "Pas de flux réduit : ouvrez la caméra pour voir le direct".into()
        }
        TileVideo::NotOnline => match &camera.status {
            CameraStatus::Unknown => "Vérification en cours…".into(),
            CameraStatus::Unreachable {
                since: Some(since), ..
            } => offline_since_now(*since),
            _ => "Caméra injoignable".into(),
        },
        TileVideo::Live(_) if capped => {
            format!("Trop de flux en même temps (maximum {MAX_LIVE_TILES}) : ouvrez la caméra")
        }
        TileVideo::Live(_) => "Hors de l'écran".into(),
    }
}

/// Dessine la page. `plan` donne la pièce de chaque caméra.
pub fn show(ui: &mut egui::Ui, model: &Model, plan: &HomePlan) {
    let id = page_id();
    let mut state: PageState = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    // Une caméra supprimée pendant qu'on la regarde : retour à la mosaïque.
    if state.open.is_some_and(|c| model.camera(c).is_none()) {
        state = PageState::default();
    }
    match state.open.and_then(|c| model.camera(c)) {
        Some(camera) => detail(ui, model, camera, plan, &mut state),
        None => mosaic(ui, model, plan, &mut state),
    }
    ui.data_mut(|d| d.insert_temp(id, state));
}

fn empty_state(ui: &mut egui::Ui) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Aucune caméra").heading());
        ui.add_space(6.0);
        ui.label(
            "Les caméras s'ajoutent depuis l'application Matter Home Server : \
             menu « Caméras… » (adresse du flux RTSP, identifiant et mot de passe).",
        );
        ui.label("Elles apparaîtront ici dès qu'elles seront enregistrées.");
    });
}

/// Le mot d'une caméra qui ne répond pas : « hors ligne » (la maquette), alors que les appareils
/// disent « injoignable ».
pub const CAMERA_OFFLINE: &str = "hors ligne";

/// Une caméra hors ligne : « hors ligne » en orange et un nom grisé.
fn is_offline(camera: &CameraView) -> bool {
    matches!(camera.status, CameraStatus::Unreachable { .. })
}

/// Anomalie à signaler sur une vignette : seulement « hors ligne ».
fn badge_of(camera: &CameraView) -> Option<(Severity, &'static str)> {
    is_offline(camera).then_some((Severity::Warn, CAMERA_OFFLINE))
}

/// Les caméras dans l'ordre des pièces, avec leur pièce (vide si elle n'est posée nulle part).
fn flatten<'a>(groups: &'a [Group<'a>]) -> Vec<(&'a CameraView, &'a str)> {
    groups
        .iter()
        .flat_map(|g| {
            let room = g.room.as_deref().unwrap_or("");
            g.cameras.iter().map(move |c| (*c, room))
        })
        .collect()
}

fn mosaic(ui: &mut egui::Ui, model: &Model, plan: &HomePlan, state: &mut PageState) {
    if model.cameras.is_empty() {
        empty_state(ui);
        return;
    }
    let groups = group_cameras(&model.cameras, plan);
    let cameras = flatten(&groups);
    let caption_h = theme::tokens().size.caption_row;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = ui.available_width();
            let columns = columns_for(width);
            let tile_w = ((width - TILE_GAP * (columns as f32 - 1.0)) / columns as f32).floor();
            let video_h = (tile_w * 9.0 / 16.0).round();
            let tile_h = video_h + caption_h + 4.0;
            let clip = ui.clip_rect();
            let mut live_budget = MAX_LIVE_TILES;
            for row in cameras.chunks(columns) {
                let (row_rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, tile_h), egui::Sense::hover());
                // Visibilité de toute la rangée d'abord : le plafond se partage dans l'ordre.
                let rects: Vec<egui::Rect> = (0..row.len())
                    .map(|i| {
                        egui::Rect::from_min_size(
                            row_rect.min + egui::vec2(i as f32 * (tile_w + TILE_GAP), 0.0),
                            egui::vec2(tile_w, tile_h),
                        )
                    })
                    .collect();
                let wants: Vec<(bool, bool)> = row
                    .iter()
                    .zip(&rects)
                    .map(|((c, _), r)| (tile_wants_stream(ui.ctx(), c), tile_visible(clip, *r)))
                    .collect();
                let live = live_slots(&wants, live_budget);
                live_budget -= live.iter().filter(|l| **l).count();
                for (i, (camera, room)) in row.iter().enumerate() {
                    if tile(ui, model, camera, room, rects[i], live[i], wants[i].1) {
                        *state = PageState {
                            open: Some(camera.id),
                            main: false,
                        };
                    }
                }
            }
        });
}

/// Une tuile : l'image, le nom, la pièce en légende, et « injoignable » seulement si besoin.
/// Rend `true` si on a cliqué dessus.
fn tile(
    ui: &mut egui::Ui,
    model: &Model,
    camera: &CameraView,
    room: &str,
    rect: egui::Rect,
    live: bool,
    visible: bool,
) -> bool {
    if !visible {
        // Hors de l'écran : on ne dessine rien et on n'appelle pas `player`.
        return false;
    }
    let response = ui.interact(
        rect,
        ui.scope_id().with(("camera-tile", camera.id)),
        egui::Sense::click(),
    );
    let video = tile_video(camera);
    let stream = tile_stream(camera).filter(|_| live);
    let capped = matches!(video, TileVideo::Live(_));
    let text = if stream.is_some() {
        String::new()
    } else {
        placeholder_text(video, camera, capped)
    };
    let image = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            theme::thumbnail(
                ui,
                rect.width(),
                &Thumb {
                    fill: theme::palette(ui).video_bg,
                    inside: &text,
                    inside_color: theme::palette(ui).video_text,
                    name: &camera.name,
                    caption: room,
                    badge: badge_of(camera),
                    dim: is_offline(camera),
                },
            )
        })
        .inner;
    if let Some(kind) = stream {
        ui.scope_builder(egui::UiBuilder::new().max_rect(image), |ui| {
            player_compact(ui, model, camera.id, kind);
        });
    }
    if response.hovered() {
        ui.painter().rect_stroke(
            image,
            theme::tokens().radius.md,
            egui::Stroke::new(1.5, theme::palette(ui).accent_text),
            egui::StrokeKind::Outside,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Les deux réglages de qualité du direct : « Fluide » (flux réduit) et « Haute » (principal).
pub const QUALITY_SMOOTH: &str = "Fluide";
pub const QUALITY_HIGH: &str = "Haute";

fn detail(
    ui: &mut egui::Ui,
    model: &Model,
    camera: &CameraView,
    plan: &HomePlan,
    state: &mut PageState,
) {
    let p = theme::palette(ui);
    let room = plan.camera_room(camera.id).map(|r| r.name.clone());
    // Une seule ligne d'en-tête : retour, nom, pièce, et le réglage de qualité à droite.
    ui.horizontal(|ui| {
        if ui.button("← Caméras").clicked() {
            *state = PageState::default();
        }
        ui.label(
            RichText::new(&camera.name)
                .size(theme::tokens().font.size.heading)
                .strong(),
        );
        if let Some(room) = room {
            ui.label(RichText::new(room).color(p.text_secondary));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let high = detail_stream(camera, state.main) == StreamKind::Main;
            let label_after = |ui: &mut egui::Ui| {
                ui.label(RichText::new("Qualité").color(theme::palette(ui).text_secondary));
            };
            // Ajoutés de droite à gauche : « Fluide » puis « Haute » à l'écran.
            if ui.selectable_label(high, QUALITY_HIGH).clicked() {
                state.main = true;
            }
            ui.add_enabled_ui(camera.has_sub_stream, |ui| {
                if ui
                    .selectable_label(!high, QUALITY_SMOOTH)
                    .on_disabled_hover_text("Cette caméra n'a qu'un flux : la qualité haute.")
                    .clicked()
                {
                    state.main = false;
                }
            });
            label_after(ui);
        });
    });
    let kind = detail_stream(camera, state.main);
    ui.add_space(4.0);
    let bar_h = theme::tokens().size.control;
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail.x, (avail.y - bar_h - 4.0).max(120.0)),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, 4.0, theme::palette(ui).video_bg);
    // Toujours appelé, même hors ligne : le lecteur dit l'état (injoignable, reprise…) et le
    // serveur reprend la caméra tout seul. Le seul choix du flux est le réglage de l'en-tête.
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        player_compact(ui, model, camera.id, kind);
    });
    // Barre d'état : codec et résolution, et ce qui ne va pas.
    ui.horizontal(|ui| {
        if let Some(s) = stream_summary(camera) {
            ui.label(RichText::new(s).small().color(p.text_secondary));
        }
        if let (_, Some(text), severity) = status_text(&camera.status) {
            let color = severity.map_or(p.text_secondary, |s| s.color(p));
            ui.label(RichText::new(text).small().color(color));
        }
    });
}

/// Flux du détail : le principal si demandé, ou s'il n'y a que lui.
pub fn detail_stream(camera: &CameraView, main_requested: bool) -> StreamKind {
    if main_requested || !camera.has_sub_stream {
        StreamKind::Main
    } else {
        StreamKind::Sub
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::home::{Floor, Item, ItemKind, Room};
    use protocol::wire::CameraProblem;
    use protocol::wire::StreamInfo;

    const A: u64 = 0xFFFF_FFF8_0000_0001;
    const B: u64 = 0xFFFF_FFF8_0000_0002;
    const C: u64 = 0xFFFF_FFF8_0000_0003;
    const D: u64 = 0xFFFF_FFF8_0000_0004;

    fn cam(id: u64, name: &str, status: CameraStatus, sub: bool) -> CameraView {
        CameraView {
            id,
            name: name.into(),
            status,
            has_sub_stream: sub,
            stream: None,
        }
    }

    fn online(id: u64, name: &str) -> CameraView {
        cam(id, name, CameraStatus::Online, true)
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

    fn placed(camera_id: u64, x: f32) -> Item {
        Item {
            id: camera_id,
            kind: ItemKind::camera(camera_id),
            x,
            y: 50.0,
            rotation: 0.0,
        }
    }

    fn floor0() -> Floor {
        Floor {
            id: 1,
            name: "RdC".into(),
            rooms: vec![],
            items: vec![],
        }
    }

    fn plan() -> HomePlan {
        let mut plan = HomePlan::default();
        plan.floors.push(Floor {
            rooms: vec![room(1, "Salon", 0.0), room(2, "Éntrée", 200.0)],
            items: vec![placed(A, 10.0), placed(B, 210.0), placed(C, 20.0)],
            ..floor0()
        });
        plan
    }

    #[test]
    fn cameras_are_grouped_by_room_then_sorted_by_name_with_no_room_last() {
        let cameras = vec![
            online(D, "Garage"),
            online(A, "Zoom"),
            online(B, "Porte"),
            online(C, "alcôve"),
        ];
        let groups = group_cameras(&cameras, &plan());
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|g| {
                (
                    g.title(),
                    g.cameras.iter().map(|c| c.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Éntrée", vec!["Porte"]),
                ("Salon", vec!["alcôve", "Zoom"]),
                ("Sans pièce", vec!["Garage"]),
            ]
        );
    }

    #[test]
    fn a_camera_not_in_the_plan_has_no_room_and_no_plan_means_one_group() {
        let cameras = vec![online(B, "b"), online(A, "a")];
        let groups = group_cameras(&cameras, &HomePlan::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].room, None);
        assert_eq!(groups[0].title(), NO_ROOM);
        assert_eq!(groups[0].cameras[0].name, "a");
        assert!(group_cameras(&[], &plan()).is_empty());
    }

    #[test]
    fn the_mosaic_only_ever_picks_the_sub_stream_and_never_wakes_a_dead_camera() {
        assert_eq!(
            tile_video(&online(A, "a")),
            TileVideo::Live(StreamKind::Sub)
        );
        assert_eq!(
            tile_video(&cam(A, "a", CameraStatus::Online, false)),
            TileVideo::NoSubStream
        );
        let down = CameraStatus::Unreachable {
            problem: CameraProblem::Unreachable,
            detail: None,
            since: None,
        };
        assert_eq!(tile_video(&cam(A, "a", down, true)), TileVideo::NotOnline);
        assert_eq!(
            tile_video(&cam(A, "a", CameraStatus::Unknown, true)),
            TileVideo::NotOnline
        );
    }

    #[test]
    fn only_visible_tiles_play_within_the_cap() {
        // (veut un flux, visible)
        let tiles = [
            (true, true),
            (true, false),
            (false, true),
            (true, true),
            (true, true),
        ];
        assert_eq!(live_slots(&tiles, 2), vec![true, false, false, true, false]);
        assert_eq!(live_slots(&tiles, 0), vec![false; 5]);
        assert!(live_slots(&[], 3).is_empty());
    }

    #[test]
    fn visibility_follows_the_scroll_area() {
        let clip = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(500.0, 300.0));
        let tile = |y| egui::Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(200.0, 150.0));
        assert!(!tile_visible(clip, tile(-100.0)));
        assert!(tile_visible(clip, tile(0.0)));
        assert!(tile_visible(clip, tile(350.0)));
        assert!(!tile_visible(clip, tile(450.0)));
    }

    #[test]
    fn a_tile_can_never_request_the_main_stream() {
        for has_sub in [true, false] {
            for status in [
                CameraStatus::Online,
                CameraStatus::Unknown,
                CameraStatus::Unreachable {
                    problem: CameraProblem::Failed,
                    detail: None,
                    since: None,
                },
            ] {
                let c = cam(A, "a", status, has_sub);
                assert_ne!(tile_stream(&c), Some(StreamKind::Main));
                assert_eq!(tile_stream(&c).is_some(), has_sub);
                assert_ne!(tile_video(&c), TileVideo::Live(StreamKind::Main));
            }
        }
    }

    #[test]
    fn the_grid_always_has_a_column() {
        assert_eq!(columns_for(10.0), 1);
        assert_eq!(columns_for(260.0), 1);
        assert_eq!(columns_for(540.0), 2);
        assert_eq!(columns_for(1200.0), 4);
    }

    #[test]
    fn statuses_read_in_french_with_the_reason() {
        assert_eq!(status_text(&CameraStatus::Online).0, "En ligne");
        let (label, text, _) = status_text(&CameraStatus::Unreachable {
            problem: CameraProblem::Unauthorized,
            detail: Some("401".into()),
            since: None,
        });
        assert_eq!(label, "Injoignable");
        let text = text.unwrap();
        assert!(
            text.contains("mot de passe refusé") && text.contains("401"),
            "{text}"
        );
        assert_eq!(status_text(&CameraStatus::Unknown).0, "Inconnu");
        // Avec l'heure de la panne, la vignette et le détail disent depuis quand.
        let (_, text, _) = status_text(&CameraStatus::Unreachable {
            problem: CameraProblem::Unreachable,
            detail: None,
            since: Some(1_700_000_000),
        });
        assert!(text.unwrap().contains("Hors ligne depuis"));
    }

    #[test]
    fn the_stream_summary_and_the_detail_stream() {
        let mut c = online(A, "a");
        assert_eq!(stream_summary(&c), None);
        c.stream = Some(StreamInfo {
            codec: "h264".into(),
            width: Some(640),
            height: Some(360),
        });
        assert_eq!(stream_summary(&c).as_deref(), Some("H264 · 640×360"));
        c.stream = Some(StreamInfo {
            codec: "h265".into(),
            width: None,
            height: None,
        });
        assert_eq!(stream_summary(&c).as_deref(), Some("H265"));
        assert_eq!(detail_stream(&c, false), StreamKind::Sub);
        assert_eq!(detail_stream(&c, true), StreamKind::Main);
        c.has_sub_stream = false;
        assert_eq!(detail_stream(&c, false), StreamKind::Main);
    }

    #[test]
    fn the_page_draws_empty_with_cameras_and_in_detail_without_a_window() {
        let ctx = egui::Context::default();
        let mut model = Model::default();
        let run = |model: &Model| {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, model, &plan()));
            out.textures_delta.clear();
        };
        run(&model);
        model.cameras = vec![
            online(A, "Entrée"),
            cam(B, "Jardin", CameraStatus::Online, false),
            cam(C, "Cave", CameraStatus::Unknown, true),
        ];
        run(&model);
        // Ouverture en grand, puis suppression de la caméra : retour à la mosaïque.
        ctx.data_mut(|d| {
            d.insert_temp(
                page_id(),
                PageState {
                    open: Some(A),
                    main: true,
                },
            )
        });
        run(&model);
        model.cameras.retain(|c| c.id != A);
        run(&model);
        let state: PageState = ctx.data_mut(|d| d.get_temp(page_id())).unwrap();
        assert_eq!(state, PageState::default());
    }
}
