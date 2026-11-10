//! Vue thermomètre : valeur actuelle, fenêtre de temps, statistiques, graphique, export CSV.
//! Les points viennent de l'historique lu sur le serveur (`series`).

use super::theme;
use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints};

use super::{ago, Device, ViewRequests};
use matter_home_client::series::{compute_stats, DeviceSeries, Series, TimeWindow};
use protocol::events::now_unix;
use protocol::format::{format_humidity, format_temperature};
use protocol::wire::{Metric, WireMode};

pub(super) fn local_time(t: f64, with_seconds: bool) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| {
            let fmt = if with_seconds { "%H:%M:%S" } else { "%H:%M" };
            d.with_timezone(&chrono::Local).format(fmt).to_string()
        })
        .unwrap_or_default()
}

/// État de la lecture de l'historique : en cours, en échec (avec « Réessayer »), tronquée.
pub(super) fn history_status(ui: &mut egui::Ui, series: &DeviceSeries, req: &mut ViewRequests) {
    let pal = theme::palette(ui);
    if series.loading.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.small("Lecture de l'historique sur le serveur…");
        });
    }
    if let Some(error) = &series.error {
        ui.horizontal(|ui| {
            ui.colored_label(pal.error, format!("Historique indisponible : {error}"));
            if ui.small_button("Réessayer").clicked() {
                req.retry = true;
            }
        });
    }
    if series.truncated {
        ui.small("Le serveur n'a renvoyé que les mesures les plus récentes de cette période.");
    }
}

pub fn show(
    ui: &mut egui::Ui,
    device: &Device,
    series: &DeviceSeries,
    window: &mut TimeWindow,
    export_message: &Option<(bool, String)>,
    req: &mut ViewRequests,
) {
    let pal = theme::palette(ui);
    let now = now_unix();
    let empty = Series::default();
    let temperature = series.get(Metric::Temperature).unwrap_or(&empty);
    let humidity = series.get(Metric::Humidity).unwrap_or(&empty);

    // Valeur actuelle (celle de l'état complet, tenue à jour par les événements).
    ui.horizontal(|ui| {
        match device.temperature() {
            Some(s) => {
                ui.label(
                    RichText::new(format_temperature(s.value))
                        .size(56.0)
                        .strong()
                        .color(pal.series_temperature),
                );
            }
            None => {
                ui.label(
                    RichText::new("— °C")
                        .size(56.0)
                        .strong()
                        .color(pal.text_secondary),
                );
            }
        }
        if device.info.caps.humidity || device.humidity().is_some() {
            ui.add_space(24.0);
            let text = device
                .humidity()
                .map(|s| format_humidity(s.value))
                .unwrap_or_else(|| "— %".into());
            ui.label(
                RichText::new(format!("💧 {text}"))
                    .size(30.0)
                    .color(pal.series_humidity),
            );
        }
    });
    let mut line = match device.temperature() {
        Some(s) => format!("Mesure reçue {}", ago(now - s.at)),
        None => "En attente de la première mesure…".into(),
    };
    match &device.mode {
        Some(WireMode::Subscription { max_interval_s }) => {
            line.push_str(&format!(
                " · souscription active (rapport au moins toutes les {max_interval_s} s)"
            ));
        }
        Some(WireMode::Polling { every_s }) => {
            line.push_str(&format!(
                " · interrogation toutes les {every_s} s (souscription impossible)"
            ));
        }
        None => {}
    }
    ui.label(line);
    history_status(ui, series, req);
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
            .add_enabled(
                !temperature.is_empty() && series.loading.is_none(),
                egui::Button::new("Exporter en CSV"),
            )
            .on_hover_text("Fichier produit par le serveur pour la fenêtre choisie.")
            .clicked()
        {
            req.export_csv = true;
        }
    });
    if let Some((ok, msg)) = export_message {
        let color = if *ok { pal.ok } else { pal.error };
        ui.colored_label(color, msg.as_str());
    }

    let temp_stats = compute_stats(temperature.since(start));
    let hum_stats = compute_stats(humidity.since(start));
    egui::Grid::new("stats")
        .num_columns(5)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
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
    let points: PlotPoints = temperature.since(start).map(|s| [s.t, s.value]).collect();
    let x_min = temperature
        .since(start)
        .next()
        .map_or(start, |s| s.t)
        .max(start);
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
            Some(format!(
                "{}\n{}",
                local_time(p.x, true),
                format_temperature(p.y)
            ))
        })
        .include_x(x_min)
        .include_x(now)
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new("Température (°C)", points)
                    .color(pal.series_temperature)
                    .width(2.0),
            );
        });
}
