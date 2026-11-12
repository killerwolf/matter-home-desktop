//! Image fixe d'une vignette de caméra (ticket n° 12) : quand la mosaïque n'ouvre pas de flux pour
//! une caméra (pas de flux réduit, ou au-delà de `MAX_LIVE_TILES`), la tuile montre la dernière
//! image fixe du serveur (`GET /api/cameras/{id}/snapshot`), rafraîchie toutes les 20 s tant que
//! la tuile est à l'écran. Rien ne tourne quand la tuile n'est plus dessinée.
//!
//! Deux temps, pour que la tuile sache avant de se dessiner si elle a une image : [`poll`] (ouvre
//! ou entretient le fil, met la texture à jour) puis [`paint`].

use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui;

use matter_home_client::net::snapshot::{SnapshotHandle, SnapshotState};

/// La tuile se redessine à cette cadence au plus : elle entretient le fil et prend les images.
const REPAINT_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

struct State {
    handle: Option<SnapshotHandle>,
    texture: Option<egui::TextureHandle>,
    size: [usize; 2],
}

type Shared = Arc<Mutex<State>>;

fn shared(ctx: &egui::Context, camera_id: u64) -> Shared {
    let id = egui::Id::unique(("camera-snapshot", camera_id));
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with::<Shared>(id, || {
            Arc::new(Mutex::new(State {
                handle: None,
                texture: None,
                size: [0, 0],
            }))
        })
        .clone()
    })
}

/// Entretient le fil de l'image de cette caméra (à appeler à chaque image de l'interface tant que
/// la tuile est affichée) et rend `true` si on a une image à montrer.
pub fn poll(ctx: &egui::Context, camera_id: u64) -> bool {
    let shared = shared(ctx, camera_id);
    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let state = &mut *state;
    if state.handle.as_ref().is_some_and(SnapshotHandle::is_stale) {
        // Autre session : l'ancienne image n'est plus forcément la bonne caméra.
        state.handle = None;
        state.texture = None;
    }
    let reopen = match &state.handle {
        None => true,
        Some(handle) => handle.is_finished() && !handle.state().is_final(),
    };
    if reopen {
        let ctx = ctx.clone();
        state.handle = Some(SnapshotHandle::open(camera_id, move || {
            ctx.request_repaint()
        }));
    }
    if let Some(handle) = &state.handle {
        handle.touch();
        if let Some(image) = handle.take_image() {
            let size = [image.width as usize, image.height as usize];
            let color = egui::ColorImage::from_rgb(size, &image.rgb);
            match &mut state.texture {
                Some(texture) => texture.set(color, egui::TextureOptions::LINEAR),
                None => {
                    state.texture = Some(ctx.load_texture(
                        format!("camera-snapshot-{camera_id}"),
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            state.size = size;
        }
    }
    ctx.request_repaint_after(REPAINT_EVERY);
    state.texture.is_some()
}

/// Dessine l'image dans `rect` (remplie, rognée au besoin : une vignette 16/9). Rien si on n'en a
/// pas.
pub fn paint(ui: &egui::Ui, camera_id: u64, rect: egui::Rect, radius: egui::CornerRadius) {
    let shared = shared(ui.ctx(), camera_id);
    let state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(texture) = &state.texture else {
        return;
    };
    let uv = cover_uv(state.size, rect.size());
    egui::Image::new((texture.id(), rect.size()))
        .uv(uv)
        .corner_radius(radius)
        .paint_at(ui, rect);
}

/// Texte d'état à montrer tant qu'il n'y a pas d'image. `None` : une image est là, ou rien de
/// mieux que le texte habituel de la tuile (pas de session, serveur trop ancien : « ouvrez la
/// caméra pour voir le direct »).
pub fn notice(ctx: &egui::Context, camera_id: u64) -> Option<String> {
    let shared = shared(ctx, camera_id);
    let state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if state.texture.is_some() {
        return None;
    }
    match state.handle.as_ref().map(SnapshotHandle::state)? {
        SnapshotState::Loading | SnapshotState::Ready => {
            Some("Image en cours de chargement…".into())
        }
        SnapshotState::Failed(reason) => Some(format!("Pas d'image : {reason}")),
        SnapshotState::ServerTooOld | SnapshotState::CameraGone | SnapshotState::NoSession => None,
    }
}

/// La partie de la texture à montrer pour remplir `area` sans déformer (rognage centré).
fn cover_uv(image: [usize; 2], area: egui::Vec2) -> egui::Rect {
    let (iw, ih) = (image[0].max(1) as f32, image[1].max(1) as f32);
    let (aw, ah) = (area.x.max(1.0), area.y.max(1.0));
    let (image_ratio, area_ratio) = (iw / ih, aw / ah);
    let (w, h) = if image_ratio > area_ratio {
        (area_ratio / image_ratio, 1.0)
    } else {
        (1.0, image_ratio / area_ratio)
    };
    egui::Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wider_picture_is_cropped_on_the_sides() {
        let uv = cover_uv([400, 100], egui::vec2(160.0, 90.0));
        assert!((uv.height() - 1.0).abs() < 1e-6);
        assert!(uv.width() < 1.0);
    }

    #[test]
    fn the_same_ratio_shows_everything() {
        let uv = cover_uv([640, 360], egui::vec2(320.0, 180.0));
        assert!((uv.width() - 1.0).abs() < 1e-6 && (uv.height() - 1.0).abs() < 1e-6);
    }
}
