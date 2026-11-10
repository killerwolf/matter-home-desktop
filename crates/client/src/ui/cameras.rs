//! Onglet « Caméras » : la mosaïque. **À écrire par le ticket n° 7** (propriétaire exclusif de ce
//! fichier ; voir `docs/cameras-contrat.md`). Pour l'instant : un texte d'attente.
//!
//! Ce qu'on a déjà : `Model::cameras` (liste triée par nom, sans adresse ni mot de passe,
//! remplacée par `cameras_changed`), `Model::camera(id)`, `CameraView::{status, has_sub_stream,
//! stream}`, et `ui::player::player` pour afficher un flux.

use eframe::egui;

use matter_home_client::model::Model;

/// Dessine la page. Rend `()` aujourd'hui ; #7 y mettra la mosaïque et son action (ouvrir un flux).
pub fn show(ui: &mut egui::Ui, model: &Model) {
    ui.heading("Caméras");
    ui.add_space(6.0);
    match model.cameras.len() {
        0 => ui.label("Aucune caméra : ajoutez-en depuis l'application Matter Home Server."),
        n => ui.label(format!(
            "{n} caméra(s). La mosaïque arrive avec le ticket n° 7."
        )),
    };
}
