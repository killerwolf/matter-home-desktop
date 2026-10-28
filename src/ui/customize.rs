//! Éditeur du nom et de l'icône d'un appareil, partagé par la fiche et le plan.

use eframe::egui;

use super::plan::glyph;
use crate::events::NodeInfo;
use crate::home::Symbol;

/// Brouillon en cours d'édition.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceEdit {
    pub node_id: u64,
    pub name: String,
    pub icon: Option<Symbol>,
}

impl DeviceEdit {
    pub fn from_info(info: &NodeInfo) -> Self {
        Self {
            node_id: info.node_id,
            name: info.custom_name.clone().unwrap_or_default(),
            icon: info.icon,
        }
    }
}

pub enum EditResult {
    Save,
    Cancel,
}

/// Champ nom (vide = nom de l'appareil), choix d'icône, boutons Enregistrer / Annuler.
pub fn editor(ui: &mut egui::Ui, edit: &mut DeviceEdit, device_name: &str) -> Option<EditResult> {
    let mut result = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("Nom :");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut edit.name)
                .hint_text(device_name)
                .desired_width(200.0),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            result = Some(EditResult::Save);
        }
        let current = edit
            .icon
            .map_or_else(|| "Automatique".to_string(), |s| format!("{} {}", glyph(Some(s)), s.label()));
        egui::ComboBox::from_id_salt(("icon", edit.node_id))
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut edit.icon, None, "Automatique");
                for s in Symbol::ALL {
                    ui.selectable_value(&mut edit.icon, Some(s), format!("{} {}", glyph(Some(s)), s.label()));
                }
            });
        if ui.button("Enregistrer").clicked() {
            result = Some(EditResult::Save);
        }
        if ui.button("Annuler").clicked() {
            result = Some(EditResult::Cancel);
        }
    });
    ui.small("Nom vide : nom annoncé par l'appareil. Icône « Automatique » : déduite de ce qu'il mesure.");
    result
}
