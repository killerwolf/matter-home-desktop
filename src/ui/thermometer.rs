//! Vue thermomètre : valeur actuelle, fenêtre de temps, statistiques, graphique, export CSV.

use eframe::egui::{self, Color32, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints};

use super::{ago, Device};
use crate::events::{now_unix, ReportingMode};
use crate::history::{compute_stats, to_csv, TimeWindow};
use crate::matter::convert::{format_humidity, format_temperature};

const TEMP_COLOR: Color32 = Color32::from_rgb(230, 90, 50);
const HUM_COLOR: Color32 = Color32::from_rgb(60, 140, 220);

pub(super) fn local_time(t: f64, with_seconds: bool) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| {
            let fmt = if with_seconds { "%H:%M:%S" } else { "%H:%M" };
            d.with_timezone(&chrono::Local).format(fmt).to_string()
        })
        .unwrap_or_default()
}

pub fn show(
    ui: &mut egui::Ui,
    device: &Device,
    window: &mut TimeWindow,
    export_message: &mut Option<(bool, String)>,
) {
    let now = now_unix();

    // Valeur actuelle.
    ui.horizontal(|ui| {
        match device.temperature.last() {
            Some(s) => {
                ui.label(RichText::new(format_temperature(s.value)).size(56.0).strong().color(TEMP_COLOR));
            }
            None => {
                ui.label(RichText::new("— °C").size(56.0).strong().color(Color32::GRAY));
            }
        }
        if device.info.caps.humidity || !device.humidity.is_empty() {
            ui.add_space(24.0);
            let text = device
                .humidity
                .last()
                .map(|s| format_humidity(s.value))
                .unwrap_or_else(|| "— %".into());
            ui.label(RichText::new(format!("💧 {text}")).size(30.0).color(HUM_COLOR));
        }
    });
    let mut line = match device.temperature.last() {
        Some(s) => format!("Mesure reçue {}", ago(now - s.t)),
        None => "En attente de la première mesure…".into(),
    };
    match &device.mode {
        Some(ReportingMode::Subscription { max_interval_s }) => {
            line.push_str(&format!(" · souscription active (rapport au moins toutes les {max_interval_s} s)"));
        }
        Some(ReportingMode::Polling { every_s }) => {
            line.push_str(&format!(" · interrogation toutes les {every_s} s (souscription impossible)"));
        }
        None => {}
    }
    ui.label(line);
    ui.small(format!(
        "Historique en mémoire : {} / {} points",
        device.temperature.len(),
        device.temperature.capacity()
    ));
    ui.add_space(8.0);

    // Fenêtre, statistiques et export.
    let start = window.start(now);
    ui.horizontal(|ui| {
        ui.label("Fenêtre :");
        for w in TimeWindow::ALL {
            ui.selectable_value(window, w, w.label());
        }
        ui.separator();
        if ui
            .add_enabled(!device.temperature.is_empty(), egui::Button::new("Exporter en CSV"))
            .clicked()
        {
            *export_message = Some(export_csv(device));
        }
    });
    if let Some((ok, msg)) = export_message {
        let color = if *ok { Color32::from_rgb(40, 160, 70) } else { Color32::from_rgb(210, 60, 50) };
        ui.colored_label(color, msg.as_str());
    }

    let temp_stats = compute_stats(device.temperature.since(start));
    let hum_stats = compute_stats(device.humidity.since(start));
    egui::Grid::new("stats").num_columns(5).spacing([24.0, 4.0]).show(ui, |ui| {
        ui.label("");
        ui.strong("Min");
        ui.strong("Max");
        ui.strong("Moyenne");
        ui.strong("Points");
        ui.end_row();
        match temp_stats {
            Some(st) => {
                ui.label("Température");
                ui.label(format_temperature(st.min));
                ui.label(format_temperature(st.max));
                ui.label(format_temperature(st.mean));
                ui.label(st.count.to_string());
            }
            None => {
                ui.label("Température");
                ui.label("—");
                ui.label("—");
                ui.label("—");
                ui.label("0");
            }
        }
        ui.end_row();
        if let Some(st) = hum_stats {
            ui.label("Humidité");
            ui.label(format_humidity(st.min));
            ui.label(format_humidity(st.max));
            ui.label(format_humidity(st.mean));
            ui.label(st.count.to_string());
            ui.end_row();
        }
    });
    ui.add_space(8.0);

    // Graphique température (axe gauche) ; l'humidité n'a pas la même échelle, elle
    // n'est pas tracée pour garder un graphique lisible.
    let points: PlotPoints = device.temperature.since(start).map(|s| [s.t, s.value]).collect();
    let x_min = if *window == TimeWindow::Session {
        device.temperature.iter().next().map_or(now - 900.0, |s| s.t)
    } else {
        start
    };
    Plot::new(("temperature_plot", device.info.node_id))
        .legend(Legend::default())
        .height(260.0)
        .x_axis_formatter(|mark, _| local_time(mark.value, false))
        .y_axis_formatter(|mark, _| format!("{:.1} °C", mark.value))
        .label_formatter(|pos| {
            let p = match pos {
                HoverPosition::NearDataPoint { position, .. } => position,
                HoverPosition::Elsewhere { position } => position,
            };
            Some(format!("{}\n{}", local_time(p.x, true), format_temperature(p.y)))
        })
        .include_x(x_min)
        .include_x(now)
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            plot_ui.line(Line::new("Température (°C)", points).color(TEMP_COLOR).width(2.0));
        });
}

/// Écrit le CSV dans le dossier Téléchargements (ou le dossier de données à défaut).
fn export_csv(device: &Device) -> (bool, String) {
    let dir = directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_else(crate::persistence::data_dir);
    let safe_name: String = device
        .info
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let file = dir.join(format!(
        "matter-{}-{}.csv",
        safe_name,
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let humidity = (!device.humidity.is_empty()).then_some(&device.humidity);
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, to_csv(&device.temperature, humidity))) {
        Ok(()) => (
            true,
            format!("{} points exportés dans {}", device.temperature.len(), file.display()),
        ),
        Err(e) => (false, format!("Export impossible : {e}")),
    }
}
