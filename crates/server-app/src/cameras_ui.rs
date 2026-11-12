//! Le dessin de la fenêtre « Caméras » : liste, ajout, modification, suppression et bouton
//! « Tester ». Les textes et le formulaire sont dans `matter_home_server_app::cameras`.
//!
//! Tout passe par le hub du serveur qui tourne (`Hub::add_camera` et les autres) : la liste est
//! enregistrée en 0600, diffusée aux clients, et surveillée. Le test d'une saisie fait un
//! `DESCRIBE` dans un fil à part (jusqu'à 8 s par flux) : la fenêtre ne se fige jamais.

use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use matter_home_server::cameras::{CameraAdmin, CameraTest, StreamKind};
use matter_home_server::hub::Hub;
use matter_home_server_app::cameras::{
    dot_tone, problem_text, status_line, test_lines, test_lines_by_stream, Form, Tone, AQARA_HINT,
    AQARA_TITLE, BACKUP_NOTE, NO_SUB_NOTE,
};
use matter_home_theme as theme;

#[derive(Default)]
pub struct CamerasUi {
    form: Option<Form>,
    /// Refus de l'enregistrement (texte du serveur, sans adresse ni mot de passe).
    error: Option<String>,
    /// Un test est en cours dans un fil.
    testing: Option<CameraTest>,
    /// Résultat du dernier test, placé sous l'adresse de chaque flux.
    test_streams: Vec<(StreamKind, Tone, String)>,
    /// Message de test qui ne concerne aucun flux en particulier (erreur, rien à tester).
    test_general: Vec<(Tone, String)>,
    /// La liste du hub, relue à chaque image.
    cameras: Vec<CameraAdmin>,
    /// « Supprimer » a été cliqué dans le formulaire : on attend la confirmation.
    confirm_delete: bool,
}

fn tone_color(ui: &egui::Ui, tone: Tone) -> Color32 {
    let p = theme::palette(ui);
    match tone {
        Tone::Good => p.ok,
        Tone::Bad => p.error,
        Tone::Neutral => p.text_secondary,
    }
}

/// Un point de couleur de l'état d'une caméra.
fn dot(ui: &mut egui::Ui, color: Color32) {
    let size = theme::tokens().size.dot;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

/// Un champ de saisie sur toute la largeur.
fn wide(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).desired_width(f32::INFINITY)
}

/// Le pied de fenêtre : où vont les adresses et les mots de passe des caméras.
pub fn footer(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    ui.add_space(4.0);
    ui.label(RichText::new(BACKUP_NOTE).small().color(p.text_secondary));
    ui.add_space(4.0);
}

impl CamerasUi {
    /// Repart d'une page propre (à la réouverture de la fenêtre).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Dessine la page. Rend `true` si l'utilisateur veut revenir aux réglages.
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, hub: &Arc<Hub>) -> bool {
        self.poll_test();
        self.cameras = hub.camera_admin_list();
        let mut back = false;
        ui.horizontal(|ui| {
            ui.heading("Caméras");
            if ui.button("Retour aux réglages").clicked() {
                back = true;
            }
        });
        ui.add_space(8.0);

        if self.form.is_some() {
            self.form_ui(ui, ctx, hub);
        } else {
            self.list_ui(ui);
            self.list_actions(ui, hub);
        }
        back
    }

    fn poll_test(&mut self) {
        let Some(test) = &self.testing else { return };
        if let Some(outcome) = test.poll() {
            match outcome {
                Ok(results) if results.is_empty() => {
                    self.test_general = vec![(Tone::Bad, "Rien à tester.".to_string())];
                }
                Ok(results) => {
                    self.test_streams = test_lines_by_stream(&results);
                    // Les lignes par flux sont sous leur adresse ; on garde les mêmes mots.
                    debug_assert_eq!(self.test_streams.len(), test_lines(&results).len());
                }
                Err(e) => self.test_general = vec![(Tone::Bad, e.0)],
            }
            self.testing = None;
        }
    }

    fn clear_test(&mut self) {
        self.test_streams.clear();
        self.test_general.clear();
        self.testing = None;
    }

    fn open_form(&mut self, form: Form) {
        self.form = Some(form);
        self.error = None;
        self.confirm_delete = false;
        self.clear_test();
    }

    fn close_form(&mut self) {
        self.form = None;
        self.error = None;
        self.confirm_delete = false;
        self.clear_test();
    }

    /// La liste : une ligne par caméra, un point de couleur et le nom ; seul un problème s'écrit.
    /// Cliquer sur la ligne ouvre son formulaire.
    fn list_ui(&mut self, ui: &mut egui::Ui) {
        // La liste vient du hub à chaque image ; le formulaire s'ouvre à l'image suivante.
        let cameras = self.cameras.clone();
        if cameras.is_empty() {
            ui.label("Aucune caméra pour l'instant.");
        }
        for camera in &cameras {
            if self.row(ui, camera) {
                self.open_form(Form::edit(camera));
            }
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, camera: &CameraAdmin) -> bool {
        let color = tone_color(ui, dot_tone(camera));
        let p = theme::palette(ui);
        let inner = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme::tokens().space.sm;
            dot(ui, color);
            ui.label(RichText::new(&camera.name).strong());
            if let Some(problem) = problem_text(camera) {
                ui.label(RichText::new(problem).color(p.error));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.button("Modifier…").clicked()
            })
            .inner
        });
        ui.add_space(2.0);
        ui.separator();
        inner.inner
    }

    fn list_actions(&mut self, ui: &mut egui::Ui, _hub: &Arc<Hub>) {
        if let Some(e) = &self.error {
            let color = tone_color(ui, Tone::Bad);
            ui.colored_label(color, e);
        }
        ui.add_space(6.0);
        if theme::primary_button(ui, "Ajouter une caméra…").clicked() {
            self.open_form(Form::new());
        }
    }

    fn form_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, hub: &Arc<Hub>) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let p = theme::palette(ui);
        ui.label(RichText::new(form.title()).strong());
        // L'état de la caméra (dont le détail du flux), qui n'est plus dans la liste.
        if let Some(camera) = form
            .editing
            .and_then(|id| self.cameras.iter().find(|c| c.id == id))
        {
            let (tone, text) = status_line(camera);
            let color = tone_color(ui, tone);
            ui.horizontal(|ui| {
                dot(ui, color);
                ui.label(RichText::new(text).color(p.text_secondary));
            });
        }
        ui.add_space(4.0);
        let label = ui.label("Nom");
        ui.add(wide(&mut form.name).hint_text("Entrée"))
            .labelled_by(label.id);

        ui.add_space(4.0);
        let label = ui.label("Adresse de la caméra");
        let url_changed = ui
            .add(wide(&mut form.url).hint_text("rtsp://192.168.1.20:554/live/ch00_0"))
            .labelled_by(label.id)
            .changed();
        test_result(ui, &self.test_streams, StreamKind::Main);

        ui.add_space(4.0);
        let label = ui.label("Adresse pour la mosaïque (facultatif)");
        let sub_changed = ui
            .add(wide(&mut form.sub_url).hint_text("rtsp://192.168.1.20:554/live/ch00_1"))
            .labelled_by(label.id)
            .changed();
        test_result(ui, &self.test_streams, StreamKind::Sub);
        if form.sub_url.trim().is_empty() {
            ui.label(RichText::new(NO_SUB_NOTE).small().color(p.text_secondary));
        }
        // Un mot de passe collé dans l'adresse n'y reste pas : il passe dans son champ.
        if url_changed || sub_changed {
            form.absorb_credentials();
        }

        ui.add_space(4.0);
        let label = ui.label("Identifiant");
        ui.add(wide(&mut form.username)).labelled_by(label.id);

        ui.add_space(4.0);
        if form.shows_saved_password() {
            // Une seule ligne : le mot de passe n'est jamais montré.
            ui.horizontal(|ui| {
                ui.label("Mot de passe");
                ui.label(RichText::new("enregistré").color(p.text_secondary));
                if ui.button("Changer").clicked() {
                    form.start_password_change();
                }
                if ui.button("Effacer").clicked() {
                    form.mark_password_cleared();
                }
            });
        } else if form.clear_password {
            ui.horizontal(|ui| {
                ui.label("Mot de passe");
                ui.label(RichText::new("sera effacé à l'enregistrement").color(p.error));
                if ui.button("Annuler").clicked() {
                    form.keep_saved_password();
                }
            });
        } else {
            let label = ui.label("Mot de passe");
            let hint = form.password_hint();
            ui.add(wide(&mut form.password).password(true).hint_text(hint))
                .labelled_by(label.id);
            if form.change_password && ui.button("Garder l'ancien").clicked() {
                form.keep_saved_password();
            }
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new(AQARA_TITLE).show(ui, |ui| {
            ui.label(RichText::new(AQARA_HINT).color(p.text_secondary));
        });
        ui.add_space(8.0);

        let busy = self.testing.is_some();
        let fillable = form.is_fillable();
        let editing_now = form.editing;
        let (mut test, mut save, mut cancel) = (false, false, false);
        ui.horizontal(|ui| {
            save = ui
                .add_enabled(fillable && !busy, egui::Button::new("Enregistrer"))
                .clicked();
            test = ui
                .add_enabled(fillable && !busy, egui::Button::new("Tester"))
                .clicked();
            cancel = ui.button("Annuler").clicked();
            if busy {
                ui.spinner();
                ui.label("Connexion à la caméra…");
            }
        });
        for (tone, line) in &self.test_general {
            ui.colored_label(tone_color(ui, *tone), line);
        }
        if let Some(e) = &self.error {
            ui.colored_label(tone_color(ui, Tone::Bad), e);
        }

        // « Supprimer » n'existe que pour une caméra déjà enregistrée.
        let mut delete = None;
        if let Some(id) = editing_now {
            ui.add_space(10.0);
            ui.separator();
            let name = form.name.clone();
            if self.confirm_delete {
                ui.colored_label(
                    tone_color(ui, Tone::Bad),
                    format!("Supprimer « {name} » ? Elle disparaît aussi des clients."),
                );
                ui.horizontal(|ui| {
                    if ui.button("Oui, supprimer").clicked() {
                        delete = Some(id);
                    }
                    if ui.button("Non").clicked() {
                        self.confirm_delete = false;
                    }
                });
            } else if ui
                .button(RichText::new("Supprimer cette caméra…").color(p.error))
                .clicked()
            {
                self.confirm_delete = true;
                self.error = None;
            }
        }

        let (draft, editing) = (form.draft(), form.editing);
        if let Some(id) = delete {
            match hub.delete_camera(id) {
                Ok(()) => self.close_form(),
                Err(e) => {
                    self.error = Some(e.0);
                    self.confirm_delete = false;
                }
            }
        } else if test {
            self.clear_test();
            self.error = None;
            match hub.start_camera_test(&draft, editing) {
                Ok(started) => {
                    self.testing = Some(started);
                    ctx.request_repaint();
                }
                Err(e) => self.test_general = vec![(Tone::Bad, e.0)],
            }
        } else if save {
            let result = match editing {
                Some(id) => hub.update_camera(id, &draft),
                None => hub.add_camera(&draft).map(|_| ()),
            };
            match result {
                Ok(()) => self.close_form(),
                Err(e) => self.error = Some(e.0),
            }
        } else if cancel {
            // Lâcher le test l'annule : le fil s'arrête et rend le verrou de la caméra.
            self.close_form();
        }
    }
}

/// Le résultat du test d'un flux, sous son adresse.
fn test_result(ui: &mut egui::Ui, results: &[(StreamKind, Tone, String)], kind: StreamKind) {
    for (_, tone, line) in results.iter().filter(|(k, _, _)| *k == kind) {
        ui.colored_label(tone_color(ui, *tone), line);
    }
}
