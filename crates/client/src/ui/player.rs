//! Lecteur d'une caméra (ticket n° 5). #6 (clic sur la caméra du plan) et #7 (clic dans la mosaïque)
//! l'appellent : la signature de [`player`] est le contrat, elle ne change pas.
//!
//! Le lecteur montre, dans la zone disponible :
//! - un état de chargement, puis la vidéo en direct **dans son rapport largeur/hauteur** ;
//! - un choix entre le flux réduit (par défaut, quand la caméra en a un) et le flux principal ;
//! - des messages d'erreur clairs : caméra injoignable, flux interrompu (la reprise est automatique),
//!   caméra au format H.265, serveur trop ancien, caméra supprimée, session terminée.
//!
//! # Il ne lit que ce qui est affiché
//!
//! Le lecteur n'a pas de fonction « fermer » : il ouvre la liaison (`net::stream`) au premier appel
//! et la renouvelle à chaque image de l'interface (`touch`). Dès qu'il n'est plus appelé (page
//! quittée, fenêtre réduite qui ne se redessine plus), la liaison se ferme toute seule après
//! quelques secondes, et le serveur coupe la caméra quand plus personne ne la regarde.
//!
//! L'état de chaque lecteur vit dans la mémoire d'`egui`, par caméra : changer de page et revenir
//! reprend le même lecteur (et la même texture) sans rien recréer tant que la liaison tient.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui;
use protocol::wire::{CameraStatus, StreamKind};

use super::camera_text::problem_text;
use matter_home_client::model::Model;
use matter_home_client::net::stream::{StreamHandle, StreamState};

/// Délai minimal entre deux ouvertures de liaison pour une même caméra : un lecteur qui reprend
/// après une panne ne boucle pas.
const REOPEN_AFTER: Duration = Duration::from_secs(1);
/// Le lecteur se redessine au moins à cette cadence tant qu'il est affiché (il maintient ainsi la
/// liaison, et fait avancer les messages).
const REPAINT_EVERY: Duration = Duration::from_millis(100);

/// L'état d'un lecteur, gardé dans la mémoire d'`egui` **par caméra et par flux** : deux endroits
/// qui montrent la même caméra sur des flux différents n'ont jamais à se disputer une liaison.
struct State {
    handle: Option<StreamHandle>,
    opened_at: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    /// Taille de la dernière image montrée (pixels) et rapport d'aspect d'un pixel.
    shown: Option<([usize; 2], (u32, u32))>,
}

type Shared = Arc<Mutex<State>>;

/// Affiche le flux `kind` de la caméra `camera_id` dans `ui` (la zone disponible), avec un choix
/// entre flux réduit et flux principal (propre à cet `ui`, il ne se répercute nulle part ailleurs).
/// Les flux ne tournent que tant que cette fonction est appelée à chaque image : ne plus l'appeler
/// (page quittée, fenêtre fermée) coupe la connexion (aucun spectateur côté serveur).
///
/// `kind` est le flux voulu au départ. Une caméra sans flux réduit n'a que le flux principal.
pub fn player(ui: &mut egui::Ui, model: &Model, camera_id: u64, kind: StreamKind) {
    show(ui, model, camera_id, kind, true);
}

/// Comme [`player`], sans aucun contrôle : le flux est **toujours** `kind`, choisi par l'appelant
/// (mosaïque : le réduit ; page de détail : son propre bouton). Une caméra sans le flux demandé
/// l'annonce, elle ne bascule jamais sur l'autre.
pub fn player_compact(ui: &mut egui::Ui, model: &Model, camera_id: u64, kind: StreamKind) {
    show(ui, model, camera_id, kind, false);
}

/// Le flux réellement lu : celui de l'appelant, ou, avec le choix affiché, celui qu'on a cliqué
/// dans cet `ui` (le choix est oublié quand l'appelant change de demande).
fn effective_kind(
    ui: &egui::Ui,
    camera_id: u64,
    requested: StreamKind,
    has_sub: bool,
    toggle: bool,
) -> StreamKind {
    let kind = if toggle {
        let id = ui.scope_id().with(("camera-player-choice", camera_id));
        let (asked, chosen): (StreamKind, StreamKind) = ui
            .data(|d| d.get_temp(id))
            .filter(|(asked, _)| *asked == requested)
            .unwrap_or((requested, requested));
        ui.data_mut(|d| d.insert_temp(id, (asked, chosen)));
        chosen
    } else {
        requested
    };
    if toggle && kind == StreamKind::Sub && !has_sub {
        StreamKind::Main
    } else {
        kind
    }
}

fn show(ui: &mut egui::Ui, model: &Model, camera_id: u64, requested: StreamKind, toggle: bool) {
    let Some(camera) = model.camera(camera_id) else {
        ui.label("Cette caméra n'existe plus.");
        return;
    };
    let kind = effective_kind(ui, camera_id, requested, camera.has_sub_stream, toggle);
    if kind == StreamKind::Sub && !camera.has_sub_stream {
        ui.label("Cette caméra n'a pas de flux réduit.");
        return;
    }
    let shared = state(ui.ctx(), camera_id, kind);
    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let state = &mut *state;

    // Le choix du flux (l'appelant qui n'en veut pas utilise `player_compact`).
    if toggle {
        let choice_id = ui.scope_id().with(("camera-player-choice", camera_id));
        ui.horizontal(|ui| {
            ui.strong(&camera.name);
            if camera.has_sub_stream {
                ui.separator();
                for choice in [StreamKind::Sub, StreamKind::Main] {
                    if ui
                        .selectable_label(kind == choice, choice.label())
                        .clicked()
                        && kind != choice
                    {
                        ui.data_mut(|d| d.insert_temp(choice_id, (requested, choice)));
                        ui.request_repaint();
                    }
                }
            }
        });
        ui.add_space(4.0);
    }

    // Une liaison ouverte sous une autre session (reconnexion) est périmée, même si elle affiche
    // encore « session terminée » ou « pas de connexion » : on repart de zéro.
    if state.handle.as_ref().is_some_and(StreamHandle::is_stale) {
        state.handle = None;
    }

    // La liaison : ouverte au premier affichage, rouverte si elle s'est arrêtée (page quittée puis
    // retrouvée) tant qu'aucun état définitif ne l'interdit.
    let reopen = match &state.handle {
        None => true,
        Some(handle) => {
            handle.is_finished()
                && !handle.state().is_final()
                && state
                    .opened_at
                    .is_none_or(|at| at.elapsed() >= REOPEN_AFTER)
        }
    };
    if reopen {
        let ctx = ui.ctx().clone();
        state.handle = Some(StreamHandle::open(camera_id, kind, move || {
            ctx.request_repaint()
        }));
        state.opened_at = Some(Instant::now());
    }
    let Some(handle) = &state.handle else { return };
    handle.touch();
    ui.ctx().request_repaint_after(REPAINT_EVERY);

    // La dernière image décodée devient la texture.
    if let Some(image) = handle.take_image() {
        let size = [image.width as usize, image.height as usize];
        let color = egui::ColorImage::new(size, image.pixels);
        match &mut state.texture {
            Some(texture) => texture.set(color, egui::TextureOptions::LINEAR),
            None => {
                state.texture = Some(ui.ctx().load_texture(
                    format!("camera-{camera_id}-{}", kind.key()),
                    color,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        let aspect = handle.info().map_or((1, 1), |i| i.pixel_aspect);
        state.shown = Some((size, aspect));
    }

    let stream_state = handle.state();
    let notice = notice(&stream_state, &camera.status);

    let area = ui.available_rect_before_wrap();
    let (rect, _) = ui.allocate_exact_size(area.size(), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(18));
    if let (Some(texture), Some((size, (par_n, par_d)))) = (&state.texture, state.shown) {
        let picture = fit(rect, size, (par_n, par_d));
        painter.image(
            texture.id(),
            picture,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        // Une image figée sous un message : on voit encore la dernière image connue.
        if let Some(notice) = &notice {
            painter.rect_filled(picture, 0.0, egui::Color32::from_black_alpha(150));
            paint_notice(
                ui,
                rect,
                notice,
                handle_retry(&stream_state),
                &mut state.handle,
            );
        }
    } else if let Some(notice) = &notice {
        paint_notice(
            ui,
            rect,
            notice,
            handle_retry(&stream_state),
            &mut state.handle,
        );
    }
}

/// Le rectangle de la vidéo : la plus grande qui tient dans `area`, centrée, avec le bon rapport
/// largeur/hauteur (celui de l'image, corrigé de la forme des pixels).
fn fit(area: egui::Rect, size: [usize; 2], pixel_aspect: (u32, u32)) -> egui::Rect {
    let width = size[0].max(1) as f32 * pixel_aspect.0.max(1) as f32 / pixel_aspect.1.max(1) as f32;
    let height = size[1].max(1) as f32;
    let scale = (area.width() / width).min(area.height() / height).max(0.0);
    egui::Rect::from_center_size(area.center(), egui::vec2(width * scale, height * scale))
}

/// Ce que le lecteur dit, et ce qu'il propose.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    text: String,
    /// Un chargement : la roue tourne.
    busy: bool,
}

/// Un état définitif se réessaie à la main (le bouton « Réessayer »).
fn handle_retry(state: &StreamState) -> bool {
    state.is_final() && !matches!(state, StreamState::SessionEnded)
}

fn paint_notice(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    notice: &Notice,
    retry: bool,
    handle: &mut Option<StreamHandle>,
) {
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(16.0)).layout(
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
    ));
    inner.vertical_centered(|ui| {
        ui.add_space((rect.height() / 2.0 - 36.0).max(0.0));
        if notice.busy {
            ui.spinner();
            ui.add_space(6.0);
        }
        ui.label(
            egui::RichText::new(&notice.text)
                .color(egui::Color32::from_gray(230))
                .size(15.0),
        );
        if retry {
            ui.add_space(8.0);
            if ui.button("Réessayer").clicked() {
                *handle = None;
            }
        }
    });
}

/// Le message de chaque état (`None` : la vidéo passe, rien à dire).
fn notice(state: &StreamState, status: &CameraStatus) -> Option<Notice> {
    let say = |text: &str, busy: bool| {
        Some(Notice {
            text: text.to_string(),
            busy,
        })
    };
    match state {
        StreamState::Live => None,
        StreamState::Connecting => say("Connexion au serveur…", true),
        StreamState::Waiting => match status {
            CameraStatus::Unreachable { problem, .. } => Some(Notice {
                text: format!(
                    "Caméra injoignable : {}. Nouvelle tentative automatique…",
                    problem_text(*problem)
                ),
                busy: true,
            }),
            _ => say("Connexion à la caméra…", true),
        },
        StreamState::CameraRetrying { problem } => Some(Notice {
            text: format!(
                "Flux interrompu : {}. Reprise automatique…",
                problem_text(*problem)
            ),
            busy: true,
        }),
        StreamState::Interrupted { reason, .. } => Some(Notice {
            text: format!(
                "Flux interrompu : {}. Reprise automatique…",
                reason.trim_end_matches('.').to_lowercase()
            ),
            busy: true,
        }),
        StreamState::Unsupported { codec } => Some(Notice {
            text: format!(
                "Cette caméra envoie du {}, que l'application ne sait pas lire (seul le H.264 \
                 l'est). Choisissez un flux H.264 dans la configuration de la caméra.",
                codec_name(codec)
            ),
            busy: false,
        }),
        StreamState::ServerTooOld => say(
            "Le serveur est trop ancien pour la lecture vidéo : mettez à jour Matter Home Server.",
            false,
        ),
        StreamState::CameraGone => say("Cette caméra n'existe plus sur le serveur.", false),
        StreamState::NoSuchStream => say("Cette caméra n'a pas ce flux.", false),
        StreamState::BadRequest => say(
            "Le serveur a refusé cette demande de flux : mettez à jour l'application et le serveur.",
            false,
        ),
        StreamState::SessionEnded => say("Votre session a expiré : reconnectez-vous.", false),
        StreamState::NoConnection => say("Pas de connexion avec le serveur.", false),
        StreamState::DecoderUnavailable(why) => Some(Notice {
            text: format!("Le décodage vidéo n'est pas disponible sur ce Mac ({why})."),
            busy: false,
        }),
    }
}

fn codec_name(codec: &str) -> String {
    match codec {
        "h265" | "hevc" => "H.265".into(),
        "h264" => "H.264".into(),
        "jpeg" => "MJPEG".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// Ce lecteur a une liaison ouverte (et pas finie) : il est affiché depuis peu. Une tuile qui
/// lisait continue ainsi de l'appeler quand sa caméra devient injoignable, pour que la reprise
/// se fasse toute seule, comme dans la fenêtre du plan.
pub fn is_active(ctx: &egui::Context, camera_id: u64, kind: StreamKind) -> bool {
    let id = egui::Id::unique(("camera-player", camera_id, kind.key()));
    let Some(shared) = ctx.data(|data| data.get_temp::<Shared>(id)) else {
        return false;
    };
    let state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    state
        .handle
        .as_ref()
        .is_some_and(|h| !h.is_finished() && !h.is_stale())
}

#[cfg(test)]
fn has_handle(ctx: &egui::Context, camera_id: u64, kind: StreamKind) -> bool {
    let id = egui::Id::unique(("camera-player", camera_id, kind.key()));
    ctx.data(|d| d.get_temp::<Shared>(id)).is_some_and(|s| {
        s.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handle
            .is_some()
    })
}

/// Images décodées par le lecteur de cette caméra depuis l'ouverture de sa liaison (diagnostic ;
/// sert à l'exemple `camera_player`, qui attend la vidéo avant de faire une capture).
#[allow(dead_code)]
pub fn frames_decoded(ctx: &egui::Context, camera_id: u64, kind: StreamKind) -> u64 {
    let id = egui::Id::unique(("camera-player", camera_id, kind.key()));
    let Some(shared) = ctx.data(|data| data.get_temp::<Shared>(id)) else {
        return 0;
    };
    let state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    state.handle.as_ref().map_or(0, StreamHandle::frames)
}

fn state(ctx: &egui::Context, camera_id: u64, kind: StreamKind) -> Shared {
    let id = egui::Id::unique(("camera-player", camera_id, kind.key()));
    ctx.data_mut(|data| {
        data.get_temp_mut_or_insert_with::<Shared>(id, || {
            Arc::new(Mutex::new(State {
                handle: None,
                opened_at: None,
                texture: None,
                shown: None,
            }))
        })
        .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::CameraProblem;

    fn unreachable(problem: CameraProblem) -> CameraStatus {
        CameraStatus::Unreachable {
            problem,
            detail: None,
        }
    }

    #[test]
    fn the_picture_keeps_its_aspect_ratio_inside_any_area() {
        let area = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 800.0));
        // 16/9 dans un carré : pleine largeur, centré en hauteur.
        let r = fit(area, [1280, 720], (1, 1));
        assert!((r.width() - 800.0).abs() < 0.01 && (r.height() - 450.0).abs() < 0.01);
        assert_eq!(r.center(), area.center());
        // Dans une zone plus large que haute : pleine hauteur.
        let wide = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 450.0));
        let r = fit(wide, [640, 360], (1, 1));
        assert!((r.height() - 450.0).abs() < 0.01 && (r.width() - 800.0).abs() < 0.01);
        // Pixels anamorphiques d'un flux réduit (704 × 480 en 4/3 environ, pixel 40/33).
        let r = fit(area, [704, 480], (40, 33));
        assert!((r.width() / r.height() - 704.0 * 40.0 / 33.0 / 480.0).abs() < 0.01);
        // Une zone vide ne fait pas de NaN.
        let empty = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 0.0));
        assert_eq!(fit(empty, [640, 360], (1, 1)).width(), 0.0);
    }

    #[test]
    fn every_state_has_a_clear_french_message() {
        let online = CameraStatus::Online;
        assert_eq!(notice(&StreamState::Live, &online), None);
        let text = |s: StreamState, st: &CameraStatus| notice(&s, st).unwrap().text;
        assert!(text(StreamState::Connecting, &online).contains("Connexion"));
        assert_eq!(
            text(StreamState::Waiting, &online),
            "Connexion à la caméra…"
        );
        assert!(text(
            StreamState::Waiting,
            &unreachable(CameraProblem::Unreachable)
        )
        .starts_with("Caméra injoignable"));
        let interrupted = text(
            StreamState::CameraRetrying {
                problem: CameraProblem::Unreachable,
            },
            &online,
        );
        assert!(
            interrupted.contains("Flux interrompu") && interrupted.contains("Reprise automatique")
        );
        assert!(text(
            StreamState::Interrupted {
                reason: "Connexion coupée.".into(),
                retry_in_s: 2
            },
            &online
        )
        .contains("connexion coupée"));
        let h265 = text(
            StreamState::Unsupported {
                codec: "h265".into(),
            },
            &online,
        );
        assert!(h265.contains("H.265") && h265.contains("H.264"));
        assert!(text(StreamState::ServerTooOld, &online).contains("trop ancien"));
        assert!(text(StreamState::CameraGone, &online).contains("n'existe plus"));
        assert!(text(StreamState::SessionEnded, &online).contains("session"));
        assert!(text(StreamState::NoConnection, &online).contains("Pas de connexion"));
        assert!(text(StreamState::DecoderUnavailable("x".into()), &online).contains("décodage"));
        // Seuls les chargements ont une roue.
        assert!(notice(&StreamState::Waiting, &online).unwrap().busy);
        assert!(!notice(&StreamState::ServerTooOld, &online).unwrap().busy);
    }

    #[test]
    fn only_final_states_offer_to_retry_by_hand() {
        assert!(handle_retry(&StreamState::ServerTooOld));
        assert!(handle_retry(&StreamState::NoConnection));
        assert!(!handle_retry(&StreamState::Waiting));
        assert!(!handle_retry(&StreamState::SessionEnded));
    }

    /// Sans session avec un serveur, le lecteur le dit et ne plante pas ; et il n'ouvre rien tant
    /// qu'on ne l'appelle pas.
    #[test]
    fn without_a_session_the_player_says_so() {
        let id = protocol::wire::CAMERA_ID_MIN + 5;
        let model = Model {
            cameras: vec![protocol::wire::CameraView {
                id,
                name: "Entrée".into(),
                status: CameraStatus::Online,
                has_sub_stream: true,
                stream: None,
            }],
            ..Model::default()
        };
        let ctx = egui::Context::default();
        let run = |camera: u64| {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                player(ui, &model, camera, StreamKind::Sub);
            })
            .drop_without_applying_deltas();
        };
        // Une caméra inconnue ne plante pas non plus.
        run(id + 1);
        for _ in 0..3 {
            run(id);
            std::thread::sleep(Duration::from_millis(30));
        }
        let shared = state(&ctx, id, StreamKind::Sub);
        let state = shared.lock().unwrap();
        let handle = state
            .handle
            .as_ref()
            .expect("liaison ouverte à l'affichage");
        // Rien n'est publié par ce test : jamais d'image (états de chargement ou d'échec).
        assert_ne!(handle.state(), StreamState::Live);
    }

    fn camera(id: u64, has_sub: bool) -> protocol::wire::CameraView {
        protocol::wire::CameraView {
            id,
            name: "Entrée".into(),
            status: CameraStatus::Online,
            has_sub_stream: has_sub,
            stream: None,
        }
    }

    fn draw(ctx: &egui::Context, model: &Model, compact: bool, kind: StreamKind) {
        ctx.run_ui(egui::RawInput::default(), |ui| {
            if compact {
                player_compact(ui, model, 7, kind)
            } else {
                player(ui, model, 7, kind)
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn a_compact_player_only_ever_opens_the_stream_it_is_given() {
        let ctx = egui::Context::default();
        let mut model = Model {
            cameras: vec![camera(7, true)],
            ..Model::default()
        };
        // Une caméra avec flux réduit : la tuile lit le réduit, jamais le principal.
        draw(&ctx, &model, true, StreamKind::Sub);
        assert!(has_handle(&ctx, 7, StreamKind::Sub));
        assert!(!has_handle(&ctx, 7, StreamKind::Main));
        // Sans flux réduit : rien ne s'ouvre, le principal non plus (pas de repli silencieux).
        let ctx = egui::Context::default();
        model.cameras = vec![camera(7, false)];
        draw(&ctx, &model, true, StreamKind::Sub);
        assert!(!has_handle(&ctx, 7, StreamKind::Sub));
        assert!(!has_handle(&ctx, 7, StreamKind::Main));
        // Le lecteur avec choix, lui, se replie sur le principal.
        draw(&ctx, &model, false, StreamKind::Sub);
        assert!(has_handle(&ctx, 7, StreamKind::Main));
    }

    #[test]
    fn two_callers_on_the_same_camera_never_share_a_stream_kind() {
        let ctx = egui::Context::default();
        let model = Model {
            cameras: vec![camera(7, true)],
            ..Model::default()
        };
        draw(&ctx, &model, false, StreamKind::Main);
        draw(&ctx, &model, true, StreamKind::Sub);
        assert!(has_handle(&ctx, 7, StreamKind::Main));
        assert!(has_handle(&ctx, 7, StreamKind::Sub));
    }
}
