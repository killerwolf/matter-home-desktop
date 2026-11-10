//! Page Températures : un seul graphique, une courbe par thermomètre, période au choix.
//! Les points viennent de l'historique lu sur le serveur (voir `matter_home_client::temperatures`).

use std::collections::HashSet;

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Line, Plot, PlotPoints, Points, VLine};

use super::thermometer::local_time;
use matter_home_client::series::TimeWindow;
use matter_home_client::temperatures::{split_gaps, value_at};
use protocol::format::format_temperature;

/// Une courbe prête à dessiner.
pub struct Curve {
    pub node_id: u64,
    /// Nom de l'appareil, avec sa pièce quand elle est connue.
    pub label: String,
    pub color: Color32,
    /// Points réduits à la largeur du graphique, du plus ancien au plus récent.
    pub points: Vec<(f64, f64)>,
    /// Trou de données (s) au-delà duquel la ligne est coupée.
    pub gap_s: f64,
    /// Dernière température connue (état complet, tenu à jour par les événements).
    pub current: Option<f64>,
}

/// Ce que la page rend à l'application.
#[derive(Default)]
pub struct Outcome {
    pub retry: bool,
}

/// Où en est la lecture de l'historique.
#[derive(Default)]
pub struct Status {
    pub loading: usize,
    pub errors: Vec<String>,
    pub truncated: bool,
}

/// Couleurs bien distinctes, lisibles sur fond clair comme sur fond sombre ; au-delà, des
/// teintes réparties par l'angle d'or.
pub fn color_for(index: usize) -> Color32 {
    const PALETTE: [(u8, u8, u8); 12] = [
        (230, 90, 50),
        (50, 130, 220),
        (40, 165, 90),
        (170, 90, 200),
        (235, 170, 30),
        (30, 175, 190),
        (215, 70, 130),
        (120, 130, 40),
        (110, 110, 230),
        (170, 110, 60),
        (90, 160, 150),
        (200, 100, 100),
    ];
    match PALETTE.get(index) {
        Some(&(r, g, b)) => Color32::from_rgb(r, g, b),
        None => {
            let hue = (index as f32 * 0.618_034).fract();
            egui::ecolor::Hsva::new(hue, 0.6, 0.85, 1.0).into()
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    curves: &[Curve],
    hidden: &mut HashSet<u64>,
    window: &mut TimeWindow,
    status: &Status,
    now: f64,
) -> Outcome {
    let mut outcome = Outcome::default();
    ui.horizontal(|ui| {
        ui.heading("Températures");
        ui.add_space(16.0);
        for w in TimeWindow::OVERVIEW {
            ui.selectable_value(window, w, w.overview_label());
        }
        if status.loading > 0 {
            ui.spinner();
        }
    });
    for error in &status.errors {
        ui.horizontal(|ui| {
            ui.colored_label(
                Color32::from_rgb(210, 60, 50),
                format!("Historique indisponible : {error}"),
            );
        });
    }
    if !status.errors.is_empty() && ui.small_button("Réessayer").clicked() {
        outcome.retry = true;
    }
    if status.truncated {
        ui.small(
            "Le serveur n'a renvoyé que les mesures les plus récentes de certains thermomètres \
             (serveur plus ancien) : la période affichée peut être incomplète.",
        );
    }
    if curves.is_empty() {
        ui.add_space(12.0);
        ui.label("Aucun thermomètre sur ce serveur.");
        return outcome;
    }

    // Légende cliquable : un clic masque ou affiche la courbe.
    ui.horizontal_wrapped(|ui| {
        for c in curves {
            let off = hidden.contains(&c.node_id);
            let mut text = c.label.clone();
            if let Some(v) = c.current {
                text.push_str(&format!("  {}", format_temperature(v)));
            }
            let mut rich = RichText::new(text);
            rich = if off {
                rich.color(Color32::GRAY).strikethrough()
            } else {
                rich.color(c.color).strong()
            };
            let swatch = RichText::new(if off { "○" } else { "●" }).color(c.color);
            ui.label(swatch);
            let clicked = ui
                .add(egui::Button::new(rich).frame(false))
                .on_hover_text("Cliquer pour masquer ou afficher cette courbe.")
                .clicked();
            if clicked && !hidden.remove(&c.node_id) {
                hidden.insert(c.node_id);
            }
            ui.add_space(10.0);
        }
    });
    ui.add_space(4.0);

    let start = window.start(now);
    let visible: Vec<&Curve> = curves
        .iter()
        .filter(|c| !hidden.contains(&c.node_id))
        .collect();
    if visible.iter().all(|c| c.points.is_empty()) && status.loading == 0 {
        ui.label("Aucune mesure sur cette période.");
    }

    let mut hover_x: Option<f64> = None;
    let long_span = window.length_s() > 36.0 * 3600.0;
    let response = Plot::new(("temperatures_plot", *window as u8))
        .height((ui.available_height() - 8.0).max(260.0))
        .x_axis_formatter(move |mark, _| axis_time(mark.value, long_span))
        .y_axis_formatter(|mark, _| format!("{:.1} °C", mark.value))
        .show_x(false)
        .show_y(false)
        .label_formatter(|_| None)
        .include_x(start)
        .include_x(now)
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            hover_x = plot_ui.pointer_coordinate().map(|p| p.x);
            for c in &visible {
                for segment in split_gaps(&c.points, c.gap_s) {
                    if segment.len() == 1 {
                        plot_ui.points(
                            Points::new(c.label.as_str(), vec![[segment[0].0, segment[0].1]])
                                .color(c.color)
                                .radius(2.5),
                        );
                    } else {
                        let pts: PlotPoints = segment.iter().map(|&(t, v)| [t, v]).collect();
                        plot_ui.line(Line::new(c.label.as_str(), pts).color(c.color).width(2.0));
                    }
                }
            }
            if let Some(x) = hover_x {
                plot_ui.vline(VLine::new("", x).color(Color32::from_gray(140)).width(1.0));
            }
        });

    // Au survol : l'heure, puis la valeur de chaque courbe affichée à cet instant.
    if let Some(x) = hover_x {
        response.response.on_hover_ui_at_pointer(|ui| {
            ui.strong(full_time(x));
            let mut rows: Vec<(&Curve, Option<f64>)> = visible
                .iter()
                .map(|c| (*c, value_at(&c.points, x, c.gap_s)))
                .collect();
            rows.sort_by(|a, b| {
                b.1.unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.1.unwrap_or(f64::NEG_INFINITY))
            });
            for (c, value) in rows {
                ui.horizontal(|ui| {
                    ui.colored_label(c.color, "●");
                    ui.label(&c.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match value {
                            Some(v) => ui.strong(format_temperature(v)),
                            None => ui.weak("—"),
                        };
                    });
                });
            }
        });
    }
    outcome
}

fn axis_time(t: f64, with_date: bool) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| {
            let fmt = if with_date { "%a %d %H:%M" } else { "%H:%M" };
            d.with_timezone(&chrono::Local).format(fmt).to_string()
        })
        .unwrap_or_default()
}

fn full_time(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| local_time(t, true))
}
