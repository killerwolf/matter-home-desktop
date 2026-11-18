//! Le dessin de la fenêtre « Descriptions » (ticket n° 19) : activation, clé d'API (saisie une
//! fois, jamais réaffichée), modèle et coût, caméras, classes, plafond quotidien. Les textes sont
//! dans `matter_home_server_app::descriptions`.

use std::sync::Arc;

use eframe::egui::{self, RichText};
use matter_home_server::descriptions::settings::{CameraFilter, Model, Settings};
use matter_home_server::hub::Hub;
use matter_home_server_app::descriptions::{
    filter_label, model_line, parse_cap, status_line, ON_OPEN_NOTE, PRIVACY_NOTE, RULES_NOTE,
    SPEND_NOTE,
};
use matter_home_server_app::system::{self, HelpPage};
use matter_home_theme as theme;

use crate::ui::widgets::{self, StepState};
use protocol::wire::DetectedClass;

#[derive(Default)]
pub struct DescriptionsUi {
    /// Les réglages en cours de modification (lus au premier affichage).
    draft: Option<Settings>,
    /// La clé saisie : vidée dès qu'elle est enregistrée, jamais affichée en clair.
    key: String,
    cap: String,
    /// Dernier résultat d'enregistrement : (réussi, texte).
    notice: Option<(bool, String)>,
}

impl DescriptionsUi {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Dessine la page. Rend `true` si l'utilisateur veut revenir aux caméras.
    pub fn show(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>) -> bool {
        let descriptions = hub.detection().descriptions();
        let p = theme::palette(ui);
        let has_key = descriptions.has_key();
        if self.draft.is_none() {
            let settings = descriptions.settings();
            self.cap = settings.daily_cap.to_string();
            self.draft = Some(settings);
        }
        let mut back = false;
        if ui.button("‹ Retour aux caméras").clicked() {
            back = true;
        }
        ui.add_space(4.0);
        ui.heading("Décrire les événements par IA");
        ui.label(
            "Quand une caméra en Surveillance détecte quelqu'un, le serveur peut demander à une \
             IA (Claude, d'Anthropic) une phrase qui décrit la scène : « Un livreur dépose un \
             colis devant la porte ». C'est facultatif et payant à l'usage, avec votre propre clé.",
        );
        ui.add_space(6.0);
        let saved = descriptions.settings();
        let (good, line) = status_line(&saved, has_key);
        ui.horizontal(|ui| {
            widgets::dot(
                ui,
                if good { p.ok } else { p.text_secondary },
                theme::tokens().size.dot + 2.0,
            );
            ui.label(RichText::new(line).strong());
        });
        ui.add_space(4.0);
        widgets::hint(ui, PRIVACY_NOTE);
        ui.add_space(10.0);

        // Étape 1, la clé : saisie une fois, jamais réaffichée.
        let key_state = if has_key {
            StepState::Done
        } else {
            StepState::Current
        };
        widgets::step(ui, 1, key_state, |ui| {
            ui.label(RichText::new("Créez une clé chez Anthropic et collez-la ici").strong());
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Sur {} : créez un compte, ajoutez un moyen de paiement, puis « API keys » → « Create key ».",
                    HelpPage::AnthropicConsole.shown()
                ));
                if ui.button("Ouvrir la console Anthropic").clicked() {
                    system::open_help(HelpPage::AnthropicConsole);
                }
            });
            if has_key {
                ui.colored_label(
                    p.ok,
                    "Une clé est enregistrée (elle n'est jamais réaffichée).",
                );
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.key)
                        .password(true)
                        .desired_width(260.0)
                        .hint_text(if has_key {
                            "Nouvelle clé (remplace l'actuelle)"
                        } else {
                            "sk-ant-…"
                        }),
                );
                if ui
                    .add_enabled(
                        !self.key.trim().is_empty(),
                        egui::Button::new("Enregistrer la clé"),
                    )
                    .clicked()
                {
                    self.notice = Some(match descriptions.set_api_key(Some(&self.key)) {
                        Ok(()) => (true, "Clé enregistrée.".to_string()),
                        Err(e) => (false, format!("{e}")),
                    });
                    self.key.clear();
                }
                if has_key && ui.button("Retirer la clé").clicked() {
                    self.notice = Some(match descriptions.set_api_key(None) {
                        Ok(()) => (true, "Clé retirée.".to_string()),
                        Err(e) => (false, format!("{e}")),
                    });
                }
            });
        });

        let settings_state = match (has_key, saved.enabled) {
            (true, true) => StepState::Done,
            (true, false) => StepState::Current,
            (false, _) => StepState::Todo,
        };
        let mut save = false;
        // Lu après le dessin du champ : le bouton suit la saisie dans la même image.
        let mut cap = None;
        let cameras = hub.camera_admin_list();
        let (draft_slot, cap_text) = (&mut self.draft, &mut self.cap);
        widgets::step(ui, 2, settings_state, |ui| {
            ui.label(RichText::new("Activez, choisissez, puis enregistrez").strong());
            let Some(draft) = draft_slot.as_mut() else {
                return;
            };
            ui.checkbox(&mut draft.enabled, "Décrire les événements de détection");
            ui.checkbox(&mut draft.on_open, "Décrire dès l'ouverture du passage");
            widgets::hint(ui, ON_OPEN_NOTE);
            ui.add_space(6.0);

            ui.label(RichText::new("Modèle").strong());
            for model in Model::ALL {
                ui.radio_value(&mut draft.model, model, model_line(model));
            }
            ui.add_space(6.0);

            ui.label(RichText::new("Caméras").strong());
            let kinds = [
                CameraFilter::All,
                CameraFilter::Exterior,
                CameraFilter::List(Vec::new()),
            ];
            for kind in kinds {
                let selected =
                    std::mem::discriminant(&draft.cameras) == std::mem::discriminant(&kind);
                if ui.radio(selected, filter_label(&kind)).clicked() && !selected {
                    draft.cameras = kind;
                }
            }
            if let CameraFilter::List(ids) = &mut draft.cameras {
                for camera in &cameras {
                    let mut on = ids.contains(&camera.id);
                    if ui.checkbox(&mut on, &camera.name).changed() {
                        if on {
                            ids.push(camera.id);
                        } else {
                            ids.retain(|id| *id != camera.id);
                        }
                    }
                }
            }
            ui.add_space(6.0);

            ui.label(RichText::new("Ce qui est décrit").strong());
            ui.horizontal(|ui| {
                for class in DetectedClass::ALL {
                    let mut on = draft.classes.contains(&class);
                    if ui.checkbox(&mut on, class.label()).changed() {
                        if on {
                            draft.classes.push(class);
                        } else {
                            draft.classes.retain(|c| *c != class);
                        }
                    }
                }
            });
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Pas plus de").strong());
                ui.add(egui::TextEdit::singleline(cap_text).desired_width(70.0));
                ui.label("descriptions par jour (au-delà, l'événement reste sans description)");
            });
            cap = parse_cap(cap_text);
            widgets::hint(ui, SPEND_NOTE);
            widgets::hint(ui, RULES_NOTE);
            ui.add_space(8.0);

            if cap.is_none() {
                ui.colored_label(
                    p.error,
                    "Le plafond doit être un nombre entre 1 et 100 000.",
                );
            }
            save = ui
                .add_enabled(cap.is_some(), egui::Button::new("Enregistrer les réglages"))
                .clicked();
        });
        if save {
            if let Some(draft) = self.draft.as_mut() {
                if let Some(cap) = cap {
                    draft.daily_cap = cap;
                }
                self.notice = Some(match descriptions.update(draft.clone()) {
                    Ok(()) => (true, "Réglages enregistrés.".to_string()),
                    Err(e) => (false, format!("{e}")),
                });
            }
        }
        if let Some((ok, text)) = &self.notice {
            ui.colored_label(if *ok { p.ok } else { p.error }, text);
        }
        back
    }
}
