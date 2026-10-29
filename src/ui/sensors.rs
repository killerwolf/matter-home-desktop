//! Vues « détecteur d'ouverture » et « prise / consommation électrique ».

use eframe::egui::{self, Color32, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints};

use super::thermometer::local_time;
use super::{ago, Device};
use crate::events::now_unix;
use crate::history::{compute_stats, Sample, TimeWindow};
use crate::matter::convert::{format_energy, format_power};

const OPEN_COLOR: Color32 = Color32::from_rgb(230, 140, 30);
const CLOSED_COLOR: Color32 = Color32::from_rgb(40, 160, 70);
const POWER_COLOR: Color32 = Color32::from_rgb(150, 100, 230);

fn window_selector(ui: &mut egui::Ui, window: &mut TimeWindow) {
    ui.horizontal(|ui| {
        ui.label("Fenêtre :");
        for w in TimeWindow::ALL {
            ui.selectable_value(window, w, w.label());
        }
    });
}

/// Détecteur d'ouverture : état actuel, nombre d'ouvertures, chronologie.
pub fn door(ui: &mut egui::Ui, device: &Device, window: &mut TimeWindow) {
    let now = now_unix();
    ui.heading("Ouverture");
    match device.open {
        Some(open) => {
            let (text, color) = if open {
                ("Ouvert", OPEN_COLOR)
            } else {
                ("Fermé", CLOSED_COLOR)
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).size(48.0).strong().color(color));
                if let Some(since) = device.open_since {
                    ui.label(format!(
                        "depuis {} ({})",
                        local_time(since, false),
                        ago(now - since)
                    ));
                }
            });
        }
        None => {
            ui.label(
                RichText::new("État inconnu")
                    .size(32.0)
                    .color(Color32::GRAY),
            );
        }
    }

    window_selector(ui, window);
    let start = window.start(now);
    let openings = device
        .door_events
        .iter()
        .filter(|(t, open)| *open && *t >= start)
        .count();
    ui.label(format!("Ouvertures sur la fenêtre : {openings}"));

    ui.add_space(4.0);
    ui.strong("Derniers changements");
    let recent: Vec<_> = device.door_events.iter().rev().take(12).collect();
    if recent.is_empty() {
        ui.label("Aucun changement observé depuis le lancement.");
    }
    egui::Grid::new(("door_events", device.info.node_id))
        .num_columns(2)
        .spacing([24.0, 2.0])
        .show(ui, |ui| {
            for (t, open) in recent {
                ui.label(local_time(*t, true));
                if *open {
                    ui.colored_label(OPEN_COLOR, "ouverture");
                } else {
                    ui.colored_label(CLOSED_COLOR, "fermeture");
                }
                ui.end_row();
            }
        });
}

/// Tracé en escalier : la puissance garde sa valeur jusqu'au rapport suivant, puis
/// jusqu'à maintenant. Aucun point n'est répété : deux points identiques à la suite
/// (valeur inchangée) font dessiner à egui un triangle parasite.
fn step_points<'a>(samples: impl Iterator<Item = &'a Sample>, now: f64) -> Vec<[f64; 2]> {
    let mut points: Vec<[f64; 2]> = Vec::new();
    for s in samples {
        if let Some(&[_, y]) = points.last() {
            if s.value == y {
                continue; // même valeur : le palier continue
            }
            let corner = [s.t, y];
            if points.last() != Some(&corner) {
                points.push(corner);
            }
        }
        points.push([s.t, s.value]);
    }
    if let Some(&[x, y]) = points.last() {
        if now > x {
            points.push([now, y]);
        }
    }
    points
}

/// Prise : état marche/arrêt, puissance instantanée, énergie cumulée et graphique.
pub fn power(ui: &mut egui::Ui, device: &Device, window: &mut TimeWindow) {
    let now = now_unix();
    ui.heading("Consommation électrique");
    ui.horizontal(|ui| {
        match device.power.last() {
            Some(s) => {
                ui.label(
                    RichText::new(format_power(s.value))
                        .size(48.0)
                        .strong()
                        .color(POWER_COLOR),
                );
            }
            None if device.info.caps.power => {
                ui.label(
                    RichText::new("— W")
                        .size(48.0)
                        .strong()
                        .color(Color32::GRAY),
                );
            }
            None => {}
        }
        if let Some(on) = device.on_off {
            ui.add_space(16.0);
            let (text, color) = if on {
                ("Allumée", CLOSED_COLOR)
            } else {
                ("Éteinte", Color32::GRAY)
            };
            ui.label(RichText::new(text).size(24.0).color(color));
        }
    });
    if let Some(e) = device.energy {
        ui.label(format!(
            "Énergie consommée (compteur de la prise) : {} — relevé {}",
            format_energy(e.value),
            ago(now - e.t)
        ));
    }
    if !device.info.caps.power {
        return;
    }

    window_selector(ui, window);
    let start = window.start(now);
    if let Some(st) = compute_stats(device.power.since(start)) {
        ui.label(format!(
            "Min {} · Max {} · Moyenne {} · {} points",
            format_power(st.min),
            format_power(st.max),
            format_power(st.mean),
            st.count
        ));
        // Énergie estimée sur la fenêtre par intégration (méthode des trapèzes).
        let pts: Vec<_> = device.power.since(start).collect();
        if pts.len() >= 2 {
            // `+ 0.0` : une somme f64 vide vaut -0.0, qui s'afficherait « -0,0 ».
            let wh: f64 = pts
                .windows(2)
                .map(|w| (w[0].value + w[1].value) / 2.0 * (w[1].t - w[0].t) / 3600.0)
                .sum::<f64>()
                + 0.0;
            ui.label(format!("Énergie estimée sur la fenêtre : {:.1} Wh", wh).replace('.', ","));
        }
    }

    let points = step_points(device.power.since(start), now);
    let x_min = if *window == TimeWindow::Session {
        device.power.iter().next().map_or(now - 900.0, |s| s.t)
    } else {
        start
    };
    Plot::new(("power_plot", device.info.node_id))
        .legend(Legend::default())
        .height(240.0)
        .x_axis_formatter(|mark, _| local_time(mark.value, false))
        .y_axis_formatter(|mark, _| format_power(mark.value))
        .label_formatter(|pos| {
            let p = match pos {
                HoverPosition::NearDataPoint { position, .. } => position,
                HoverPosition::Elsewhere { position } => position,
            };
            Some(format!("{}\n{}", local_time(p.x, true), format_power(p.y)))
        })
        .include_x(x_min)
        .include_x(now)
        // 0 à au moins 10 W : évite un axe symétrique ±400 W quand la prise ne consomme rien.
        .include_y(0.0)
        .include_y(10.0)
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            // `allow_band(false)` : sinon egui_plot remplace les marches verticales (deux points
            // à la même abscisse) par une bande remplie qui déforme le tracé.
            plot_ui.line(
                Line::new("Puissance (W)", PlotPoints::from(points))
                    .color(POWER_COLOR)
                    .width(2.0)
                    .allow_band(false),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, value: f64) -> Sample {
        Sample { t, value }
    }

    #[test]
    fn step_points_without_duplicates() {
        // Cas réel : 0 W, puis 7,1 W répété trois fois.
        let data = [
            s(10.0, 0.0),
            s(20.0, 7.1),
            s(30.0, 7.1),
            s(30.0, 7.1),
            s(50.0, 7.1),
        ];
        let pts = step_points(data.iter(), 60.0);
        assert_eq!(
            pts,
            vec![[10.0, 0.0], [20.0, 0.0], [20.0, 7.1], [60.0, 7.1]]
        );
        assert!(pts.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn step_points_edge_cases() {
        assert!(step_points([].iter(), 10.0).is_empty());
        assert_eq!(step_points([s(10.0, 3.0)].iter(), 10.0), vec![[10.0, 3.0]]);
        // Deux valeurs au même instant : coin non dupliqué.
        let pts = step_points([s(10.0, 1.0), s(10.0, 2.0)].iter(), 20.0);
        assert_eq!(pts, vec![[10.0, 1.0], [10.0, 2.0], [20.0, 2.0]]);
    }
}
