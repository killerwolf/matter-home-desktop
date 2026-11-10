//! Page Températures : un seul graphique, une courbe par thermomètre, période au choix.
//! Les points viennent de l'historique lu sur le serveur (voir `matter_home_client::temperatures`).

use std::collections::HashSet;

use eframe::egui::{self, RichText};
use egui_plot::{GridMark, Line, Plot, PlotPoints, Points, Polygon, VLine};

use super::theme;
use super::thermometer::local_time;
use matter_home_client::series::TimeWindow;
use matter_home_client::temperatures::{
    axis_label, local_offset_s, period_range, round_marks, value_at, y_label, y_marks,
};
use protocol::format::format_temperature;

/// Une courbe prête à dessiner.
pub struct Curve {
    pub node_id: u64,
    /// Nom de l'appareil, avec sa pièce quand elle est connue.
    pub label: String,
    /// Rang dans la palette de courbes des tokens (la couleur suit le thème, clair ou sombre).
    pub color_index: usize,
    /// Points réduits à la largeur du graphique, du plus ancien au plus récent.
    pub points: Vec<(f64, f64)>,
    /// Points de la ligne : une densité unique sur toute la période (voir `line_points`).
    pub line: Vec<matter_home_client::temperatures::LineSegment>,
    /// Trou de données (s) au-delà duquel la ligne est coupée.
    pub gap_s: f64,
    /// Dernière température connue (état complet, tenu à jour par les événements).
    pub current: Option<f64>,
    /// Capteur injoignable : sa dernière valeur ne vaut plus rien (affichée « — » en gris).
    pub stale: bool,
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

/// Couleur préférée d'un appareil : tirée de son identifiant, donc la même d'une session à
/// l'autre. Deux appareils peuvent tomber sur la même ; l'application en choisit alors une libre
/// (voir `MatterApp::temperature_curves`).
pub fn preferred_color_index(node_id: u64) -> usize {
    (node_id.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as usize % PALETTE_LEN
}

pub const PALETTE_LEN: usize = theme::SERIES_LEN;

pub fn show(
    ui: &mut egui::Ui,
    curves: &[Curve],
    hidden: &mut HashSet<u64>,
    window: &mut TimeWindow,
    status: &Status,
    now: f64,
) -> Outcome {
    let mut outcome = Outcome::default();
    let p = theme::palette(ui);
    let dark = ui.visuals().dark_mode;
    let color_of = |c: &Curve| p.series(c.color_index, dark);

    for error in &status.errors {
        ui.colored_label(p.error, format!("Historique indisponible : {error}"));
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

    // En-tête : une colonne par thermomètre (pièce, valeur actuelle en grand, minimum et
    // maximum de la période), les périodes à droite. Un clic sur le nom masque la courbe.
    let sizes = theme::tokens().font.size;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme::tokens().space.xl;
        for c in curves {
            let off = hidden.contains(&c.node_id);
            let color = if off { p.text_secondary } else { color_of(c) };
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut name = RichText::new(&c.label).color(color);
                if off {
                    name = name.strikethrough();
                }
                let clicked = ui
                    .add(egui::Button::new(name).frame(false))
                    .on_hover_text("Cliquer pour masquer ou afficher cette courbe.")
                    .clicked();
                if clicked && !hidden.remove(&c.node_id) {
                    hidden.insert(c.node_id);
                }
                let value = match c.current {
                    Some(v) if !c.stale => one_decimal(v),
                    _ => "—".to_string(),
                };
                let grey = off || c.stale || c.current.is_none();
                ui.label(RichText::new(value).size(sizes.display).color(if grey {
                    p.text_secondary
                } else {
                    p.text
                }));
                if let Some((lo, hi)) = period_range(&c.points) {
                    ui.label(
                        RichText::new(format!("min {} · max {}", one_decimal(lo), one_decimal(hi)))
                            .size(sizes.caption)
                            .color(p.text_secondary),
                    );
                }
            });
        }
        // Les périodes suivent les thermomètres ; elles passent à la ligne si la place manque.
        for w in TimeWindow::OVERVIEW {
            ui.selectable_value(window, w, w.overview_label());
        }
        if status.loading > 0 {
            ui.spinner();
        }
    });
    if curves.is_empty() {
        ui.add_space(theme::tokens().space.md);
        ui.label("Aucun thermomètre sur ce serveur.");
        return outcome;
    }
    ui.add_space(theme::tokens().space.sm);

    let start = window.start(now);
    let visible: Vec<&Curve> = curves
        .iter()
        .filter(|c| !hidden.contains(&c.node_id))
        .collect();
    if visible.iter().all(|c| c.points.is_empty()) && status.loading == 0 {
        ui.label("Aucune mesure sur cette période.");
    }

    let mut hover_x: Option<f64> = None;
    let response = Plot::new(("temperatures_plot", *window as u8))
        .height((ui.available_height() - 8.0).max(260.0))
        // Peu de repères : des heures rondes sur l'axe du temps, quelques lignes sur l'autre.
        .x_grid_spacer(|input| {
            round_marks(input.bounds.0, input.bounds.1, &local_offset_s)
                .into_iter()
                .map(|(value, step_size)| GridMark { value, step_size })
                .collect()
        })
        .y_grid_spacer(|input| {
            y_marks(input.bounds.0, input.bounds.1)
                .into_iter()
                .map(|(value, step_size)| GridMark { value, step_size })
                .collect()
        })
        .grid_color(p.line)
        .x_axis_formatter(|mark, _| {
            axis_label(mark.value, mark.step_size, local_offset_s(mark.value))
        })
        .y_axis_formatter(|mark, _| y_label(mark.value, mark.step_size))
        .show_x(false)
        .show_y(false)
        .label_formatter(|_| None)
        // Axe du temps fixe : la période choisie, jamais celle de l'image précédente.
        .allow_scroll(false)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .show(ui, |plot_ui| {
            plot_ui.set_plot_bounds_x(start..=now);
            // Le curseur n'a de valeur que sur le graphique.
            if plot_ui.response().hovered() {
                hover_x = plot_ui.pointer_coordinate().map(|p| p.x);
            }
            for c in &visible {
                let color = color_of(c);
                for segment in &c.line {
                    let mean = &segment.mean;
                    if mean.len() == 1 {
                        plot_ui.points(
                            Points::new(c.label.as_str(), vec![[mean[0].0, mean[0].1]])
                                .color(color)
                                .radius(2.5),
                        );
                    } else {
                        // Une bande pâle du minimum au maximum de chaque tranche, sous la
                        // ligne de moyenne : les pics restent visibles.
                        // Un quadrilatère par paire de tranches (le remplissage d'egui ne sait
                        // dessiner que des formes convexes).
                        for pair in segment.band.windows(2) {
                            let (a, b) = (pair[0], pair[1]);
                            let quad = vec![[a.0, a.1], [b.0, b.1], [b.0, b.2], [a.0, a.2]];
                            plot_ui.polygon(
                                Polygon::new("", PlotPoints::from(quad))
                                    .fill_color(color.gamma_multiply(0.16))
                                    .stroke(egui::Stroke::NONE),
                            );
                        }
                        let pts: PlotPoints = mean.iter().map(|&(t, v)| [t, v]).collect();
                        plot_ui.line(Line::new(c.label.as_str(), pts).color(color).width(2.0));
                    }
                }
            }
            if let Some(x) = hover_x {
                plot_ui.vline(VLine::new("", x).color(p.text_secondary).width(1.0));
            }
        });

    // Au survol : l'heure, puis la valeur de chaque courbe affichée à cet instant.
    if let Some(x) = hover_x {
        response.response.on_hover_ui_at_pointer(|ui| {
            ui.strong(full_time(x));
            if *window == TimeWindow::All {
                ui.small(
                    "Sur 1 semaine, une valeur peut être le minimum ou le maximum de sa tranche.",
                );
            }
            let mut rows: Vec<(&Curve, Option<f64>)> = visible
                .iter()
                .map(|c| (*c, value_at(&c.points, x, c.gap_s)))
                .collect();
            rows.sort_by(|a, b| {
                b.1.unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.1.unwrap_or(f64::NEG_INFINITY))
            });
            egui::Grid::new("temperatures_tooltip")
                .num_columns(3)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (c, value) in rows {
                        ui.colored_label(color_of(c), "●");
                        ui.label(&c.label);
                        match value {
                            Some(v) => ui.strong(format_temperature(v)),
                            None => ui.weak("—"),
                        };
                        ui.end_row();
                    }
                });
        });
    }
    outcome
}

/// « 19,8 °C ».
fn one_decimal(v: f64) -> String {
    format!("{v:.1} °C").replace('.', ",")
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
