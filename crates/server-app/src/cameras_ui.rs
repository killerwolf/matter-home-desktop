//! Le dessin de la fenêtre « Caméras » : liste, ajout, modification, suppression et bouton
//! « Tester ». Les textes et le formulaire sont dans `matter_home_server_app::cameras`.
//!
//! Tout passe par le hub du serveur qui tourne (`Hub::add_camera` et les autres) : la liste est
//! enregistrée en 0600, diffusée aux clients, et surveillée. Le test d'une saisie fait un
//! `DESCRIBE` dans un fil à part (jusqu'à 8 s par flux) : la fenêtre ne se fige jamais.

use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use matter_home_server::cameras::{CameraAdmin, CameraTest};
use matter_home_server::hub::Hub;
use matter_home_server_app::cameras::{
    status_line, test_lines, Form, Tone, AQARA_HINT, BACKUP_NOTE,
};

#[derive(Default)]
pub struct CamerasUi {
    form: Option<Form>,
    /// Refus de l'enregistrement (texte du serveur, sans adresse ni mot de passe).
    error: Option<String>,
    /// Un test est en cours dans un fil.
    testing: Option<CameraTest>,
    test_lines: Vec<(Tone, String)>,
    confirm_delete: Option<u64>,
}

fn tone_color(tone: Tone) -> Color32 {
    match tone {
        Tone::Good => Color32::from_rgb(40, 150, 70),
        Tone::Bad => Color32::from_rgb(210, 60, 50),
        Tone::Neutral => Color32::GRAY,
    }
}

/// Un champ de saisie sur toute la largeur.
fn wide(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).desired_width(f32::INFINITY)
}

impl CamerasUi {
    /// Repart d'une page propre (à la réouverture de la fenêtre).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Dessine la page. Rend `true` si l'utilisateur veut revenir aux réglages.
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, hub: &Arc<Hub>) -> bool {
        self.poll_test();
        let mut back = false;
        ui.horizontal(|ui| {
            ui.heading("Caméras");
            if ui.button("Retour aux réglages").clicked() {
                back = true;
            }
        });
        ui.add_space(4.0);
        ui.label(BACKUP_NOTE);
        ui.add_space(8.0);

        if self.form.is_some() {
            self.form_ui(ui, ctx, hub);
        } else {
            self.list_ui(ui, hub);
        }
        back
    }

    fn poll_test(&mut self) {
        let Some(test) = &self.testing else { return };
        if let Some(outcome) = test.poll() {
            self.test_lines = match outcome {
                Ok(results) if results.is_empty() => {
                    vec![(Tone::Bad, "Rien à tester.".to_string())]
                }
                Ok(results) => test_lines(&results),
                Err(e) => vec![(Tone::Bad, e.0)],
            };
            self.testing = None;
        }
    }

    fn open_form(&mut self, form: Form) {
        self.form = Some(form);
        self.error = None;
        self.test_lines.clear();
        self.testing = None;
    }

    fn list_ui(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>) {
        let cameras = hub.camera_admin_list();
        if cameras.is_empty() {
            ui.label("Aucune caméra pour l'instant.");
        }
        for camera in &cameras {
            self.row(ui, hub, camera);
            ui.add_space(4.0);
        }
        if let Some(e) = &self.error {
            ui.colored_label(tone_color(Tone::Bad), e);
        }
        ui.add_space(6.0);
        if ui.button("Ajouter une caméra…").clicked() {
            self.open_form(Form::new());
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>, camera: &CameraAdmin) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&camera.name).strong());
            let (tone, text) = status_line(camera);
            ui.colored_label(tone_color(tone), text);
            if camera.sub_url.is_none() {
                ui.label(
                    RichText::new(
                        "Pas de flux réduit : la mosaïque affichera une image fixe rafraîchie.",
                    )
                    .weak(),
                );
            }
            if self.confirm_delete == Some(camera.id) {
                ui.colored_label(
                    tone_color(Tone::Bad),
                    format!(
                        "Supprimer « {} » ? Elle disparaît aussi des clients.",
                        camera.name
                    ),
                );
                ui.horizontal(|ui| {
                    if ui.button("Oui, supprimer").clicked() {
                        self.error = hub.delete_camera(camera.id).err().map(|e| e.0);
                        self.confirm_delete = None;
                    }
                    if ui.button("Annuler").clicked() {
                        self.confirm_delete = None;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if ui.button("Modifier…").clicked() {
                        self.open_form(Form::edit(camera));
                    }
                    if ui.button("Supprimer…").clicked() {
                        self.confirm_delete = Some(camera.id);
                        self.error = None;
                    }
                });
            }
        });
    }

    fn form_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, hub: &Arc<Hub>) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        ui.label(RichText::new(form.title()).strong());
        ui.add_space(4.0);
        let label = ui.label("Nom");
        ui.add(wide(&mut form.name).hint_text("Entrée"))
            .labelled_by(label.id);
        let label = ui.label("Adresse du flux principal");
        let url_changed = ui
            .add(wide(&mut form.url).hint_text("rtsp://192.168.1.20:554/live/ch00_0"))
            .labelled_by(label.id)
            .changed();
        let label = ui.label("Adresse du flux réduit (facultative, utile pour la mosaïque)");
        let sub_changed = ui
            .add(wide(&mut form.sub_url).hint_text("rtsp://192.168.1.20:554/live/ch00_1"))
            .labelled_by(label.id)
            .changed();
        // Un mot de passe collé dans l'adresse n'y reste pas : il passe dans son champ.
        if url_changed || sub_changed {
            form.absorb_credentials();
        }
        let label = ui.label("Identifiant");
        ui.add(wide(&mut form.username)).labelled_by(label.id);
        let label = ui.label("Mot de passe");
        let hint = form.password_hint();
        ui.add(wide(&mut form.password).password(true).hint_text(hint))
            .labelled_by(label.id);
        if form.editing.is_some() && form.had_password {
            ui.checkbox(
                &mut form.clear_password,
                "Supprimer le mot de passe enregistré",
            );
        }
        ui.add_space(2.0);
        ui.label(RichText::new(AQARA_HINT).weak());
        ui.add_space(8.0);

        let busy = self.testing.is_some();
        let fillable = form.is_fillable();
        let (mut test, mut save, mut cancel) = (false, false, false);
        ui.horizontal(|ui| {
            test = ui
                .add_enabled(fillable && !busy, egui::Button::new("Tester"))
                .clicked();
            save = ui
                .add_enabled(fillable && !busy, egui::Button::new("Enregistrer"))
                .clicked();
            cancel = ui.button("Annuler").clicked();
            if busy {
                ui.spinner();
                ui.label("Connexion à la caméra…");
            }
        });
        for (tone, line) in &self.test_lines {
            ui.colored_label(tone_color(*tone), line);
        }
        if let Some(e) = &self.error {
            ui.colored_label(tone_color(Tone::Bad), e);
        }

        let (draft, editing) = (form.draft(), form.editing);
        if test {
            self.test_lines.clear();
            self.error = None;
            match hub.start_camera_test(&draft, editing) {
                Ok(started) => {
                    self.testing = Some(started);
                    ctx.request_repaint();
                }
                Err(e) => self.test_lines = vec![(Tone::Bad, e.0)],
            }
        } else if save {
            let result = match editing {
                Some(id) => hub.update_camera(id, &draft),
                None => hub.add_camera(&draft).map(|_| ()),
            };
            match result {
                Ok(()) => {
                    self.form = None;
                    self.error = None;
                    self.test_lines.clear();
                }
                Err(e) => self.error = Some(e.0),
            }
        } else if cancel {
            // Lâcher le test l'annule : le fil s'arrête et rend le verrou de la caméra.
            self.form = None;
            self.error = None;
            self.test_lines.clear();
            self.testing = None;
        }
    }
}
