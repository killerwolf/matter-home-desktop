//! Lecteur d'une caméra : **à écrire par le ticket n° 5** (propriétaire exclusif de ce fichier,
//! avec `net/stream.rs` et `video/*` ; voir `docs/cameras-contrat.md`). #6 (clic sur la caméra du
//! plan) et #7 (clic dans la mosaïque) l'appellent : la signature ci-dessous est le contrat, elle
//! ne change pas.

use eframe::egui;
use protocol::wire::StreamKind;

use matter_home_client::model::Model;

/// Affiche le flux `kind` de la caméra `camera_id` dans `ui` (la zone disponible). Les flux ne
/// tournent que tant que cette fonction est appelée à chaque image : ne plus l'appeler (page
/// quittée, fenêtre réduite) doit couper la connexion (aucun spectateur côté serveur).
///
/// Aujourd'hui : un texte d'attente qui nomme la caméra.
#[allow(dead_code)]
pub fn player(ui: &mut egui::Ui, model: &Model, camera_id: u64, kind: StreamKind) {
    match model.camera(camera_id) {
        Some(camera) => ui.label(format!(
            "{} ({}) : la lecture arrive avec le ticket n° 5.",
            camera.name,
            kind.label().to_lowercase()
        )),
        None => ui.label("Cette caméra n'existe plus."),
    };
}
