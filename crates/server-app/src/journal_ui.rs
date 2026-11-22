//! Le dessin de l'onglet « Journal » (maquette Serveur-Journal) : deux filtres (Personne,
//! Période) et la liste chronologique en quatre colonnes (Heure, Qui, Depuis, Quoi), par jour. La
//! logique est dans `ostal_server_app::journal`. Le journal se relit toutes les deux secondes tant
//! que l'onglet est ouvert : une action faite sur un téléphone apparaît toute seule.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use ostal_server::audit::{AuditLog, Tone};
use ostal_server_app::{accounts, journal};
use ostal_theme as theme;

use crate::ui::widgets;

const RELOAD_EVERY: Duration = Duration::from_secs(2);
/// Les colonnes de la maquette : Heure, Qui, Depuis ; « Quoi » prend le reste.
const TIME_W: f32 = 52.0;
const WHO_W: f32 = 84.0;
const FROM_W: f32 = 120.0;

#[derive(Default)]
pub struct JournalUi {
    /// Identifiant du compte choisi ; `None` : toutes les personnes.
    person: Option<String>,
    period: journal::Period,
    /// (identifiant, nom affiché) de chaque compte.
    people: Vec<(String, String)>,
    log: Option<AuditLog>,
    days: Vec<journal::Day>,
    shown: usize,
    loaded_at: Option<Instant>,
}

impl JournalUi {
    /// On repart d'un onglet propre (filtres par défaut) quand on le quitte.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn reload(&mut self, data_dir: &Path) {
        let log = self.log.get_or_insert_with(|| journal::open(data_dir));
        let now = ostal_server::events::now_unix();
        let entries = journal::load(log, self.person.as_deref(), self.period, now);
        self.shown = entries.len();
        self.days = journal::days_of(&entries, journal::today());
        self.people = accounts::list(data_dir)
            .map(|users| users.into_iter().map(|u| (u.id, u.display_name)).collect())
            .unwrap_or_default();
        // Un compte qui n'existe plus ne reste pas choisi.
        if let Some(id) = &self.person {
            if !self.people.iter().any(|(p, _)| p == id) {
                self.person = None;
            }
        }
        self.loaded_at = Some(Instant::now());
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, data_dir: &Path) {
        if self.loaded_at.is_none_or(|at| at.elapsed() > RELOAD_EVERY) {
            self.reload(data_dir);
        }
        ctx.request_repaint_after(RELOAD_EVERY);
        let p = theme::palette(ui);
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new("Personne").strong().small());
            let current = self
                .person
                .as_ref()
                .and_then(|id| self.people.iter().find(|(p, _)| p == id))
                .map_or("Toutes les personnes", |(_, name)| name.as_str())
                .to_string();
            egui::ComboBox::from_id_salt("journal_person")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut self.person, None, "Toutes les personnes")
                        .changed();
                    for (id, name) in &self.people {
                        changed |= ui
                            .selectable_value(&mut self.person, Some(id.clone()), name)
                            .changed();
                    }
                });
            ui.add_space(10.0);
            ui.label(RichText::new("Période").strong().small());
            egui::ComboBox::from_id_salt("journal_period")
                .selected_text(self.period.label())
                .show_ui(ui, |ui| {
                    for period in journal::Period::ALL {
                        changed |= ui
                            .selectable_value(&mut self.period, period, period.label())
                            .changed();
                    }
                });
        });
        if changed {
            self.reload(data_dir);
        }
        ui.add_space(8.0);

        widgets::card(ui, |ui| {
            header(ui);
            ui.separator();
            if self.days.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(journal::empty_text(self.period, self.person.is_some()))
                        .color(p.text_secondary),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height((ui.available_height() - 40.0).max(120.0))
                .show(ui, |ui| {
                    for day in &self.days {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&day.title)
                                .strong()
                                .small()
                                .color(p.text_secondary),
                        );
                        ui.add_space(2.0);
                        for row in &day.rows {
                            line(ui, row);
                        }
                    }
                });
        });
        widgets::hint(
            ui,
            &format!("{} · {}", journal::count_label(self.shown), journal::FOOTER),
        );
    }
}

fn cell(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 22.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            add(ui);
        },
    );
}

fn header(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (width, title) in [
            (TIME_W, "Heure"),
            (WHO_W, "Qui"),
            (FROM_W, "Depuis"),
            (
                ui.available_width() - TIME_W - WHO_W - FROM_W - 30.0,
                "Quoi",
            ),
        ] {
            cell(ui, width.max(60.0), |ui| {
                ui.label(
                    RichText::new(title)
                        .strong()
                        .small()
                        .color(p.text_secondary),
                );
            });
        }
    });
}

fn line(ui: &mut egui::Ui, row: &journal::Row) {
    let p = theme::palette(ui);
    let color = match row.tone {
        Tone::Info => p.accent,
        Tone::Device => p.lueur_text,
        Tone::Warn => p.warn,
    };
    let what_w = (ui.available_width() - TIME_W - WHO_W - FROM_W - 30.0).max(60.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        cell(ui, TIME_W, |ui| {
            ui.label(RichText::new(&row.time).monospace().color(p.text_secondary));
        });
        cell(ui, WHO_W, |ui| {
            ui.add(egui::Label::new(RichText::new(&row.who).strong()).truncate());
        });
        cell(ui, FROM_W, |ui| {
            ui.add(egui::Label::new(RichText::new(&row.from).color(p.text_secondary)).truncate());
        });
        cell(ui, what_w, |ui| {
            widgets::dot(ui, color, 8.0);
            ui.add(egui::Label::new(&row.what).truncate())
                .on_hover_text(&row.what);
        });
    });
}
