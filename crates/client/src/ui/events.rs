//! Les événements de détection (ticket n° 18), à droite de la mosaïque des caméras : capture,
//! classe, caméra, heure ; « Lire le clip » l'ouvre dans le lecteur du Mac.

use std::collections::HashMap;

use eframe::egui::{self, RichText};

use super::theme;
use matter_home_client::model::Model;
use matter_home_client::net::events::EventsFeed;

/// L'état de la colonne : la liste (fils réseau) et les textures des captures.
pub struct EventsView {
    feed: Option<EventsFeed>,
    textures: HashMap<String, egui::TextureHandle>,
    /// Le dernier événement annoncé par le WebSocket (et s'il était en cours), pour relire la
    /// liste quand il change.
    last_seen: Option<(String, bool)>,
}

impl EventsView {
    pub fn new() -> Self {
        matter_home_client::net::events::purge_old_clips();
        Self {
            feed: None,
            textures: HashMap::new(),
            last_seen: None,
        }
    }

    /// Déconnexion : tout est oublié.
    pub fn clear(&mut self) {
        if let Some(feed) = &self.feed {
            feed.clear();
        }
        self.textures.clear();
        self.last_seen = None;
    }

    fn feed(&mut self, ctx: &egui::Context) -> &EventsFeed {
        self.feed.get_or_insert_with(|| {
            let ctx = ctx.clone();
            let feed = EventsFeed::new(move || ctx.request_repaint());
            feed.refresh();
            feed
        })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, model: &Model) {
        let newest = model
            .detections
            .front()
            .map(|e| (e.id.clone(), e.in_progress));
        let feed = self.feed(ui.ctx()).clone();
        if newest.is_some() && newest != self.last_seen {
            self.last_seen = newest;
            feed.refresh();
        }
        let state = feed.state();
        if state.server_too_old {
            return;
        }
        ui.horizontal(|ui| {
            ui.heading("Événements");
            if ui.small_button("Actualiser").clicked() {
                feed.refresh();
            }
        });
        let secondary = theme::palette(ui).text_secondary;
        if let Some(status) = &state.status {
            let line = if !status.available {
                format!("Détection {}", status.engine)
            } else if status.paused {
                format!(
                    "Détection suspendue sur {} caméra(s) : {}",
                    status.paused_cameras.len(),
                    status
                        .pause_reason
                        .as_deref()
                        .unwrap_or("serveur surchargé")
                )
            } else {
                format!("Détection : {} caméra(s) analysée(s)", status.watched.len())
            };
            ui.label(RichText::new(line).small().color(secondary));
        }
        if let Some(notice) = &state.notice {
            ui.label(RichText::new(notice).small().color(secondary));
        }
        if state.items.is_empty() {
            ui.add_space(8.0);
            ui.label(if state.loaded {
                "Aucun événement ces 7 derniers jours."
            } else {
                "Chargement…"
            });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("events-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for item in &state.items {
                    let ev = &item.event;
                    ui.horizontal(|ui| {
                        let size = egui::vec2(112.0, 63.0);
                        match &item.thumb {
                            Some(image) => {
                                let key = texture_key(ev);
                                let texture = self.textures.entry(key).or_insert_with(|| {
                                    let color = egui::ColorImage::from_rgb(
                                        [image.width as usize, image.height as usize],
                                        &image.rgb,
                                    );
                                    ui.ctx().load_texture(
                                        format!("event-{}", ev.id),
                                        color,
                                        egui::TextureOptions::LINEAR,
                                    )
                                });
                                ui.add(egui::Image::new((texture.id(), size)));
                            }
                            None => {
                                ui.allocate_exact_size(size, egui::Sense::hover());
                            }
                        }
                        ui.vertical(|ui| {
                            let classes: Vec<&str> = ev.classes.iter().map(|c| c.label()).collect();
                            let mut title = classes.join(", ");
                            if ev.in_progress {
                                title.push_str(" · en cours");
                            }
                            ui.label(RichText::new(title).strong());
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    ev.camera_name,
                                    when_text(ev.start)
                                ))
                                .small()
                                .color(secondary),
                            );
                            if ev.has_clip && ui.small_button("Lire le clip").clicked() {
                                feed.open_clip(&ev.id);
                            }
                        });
                    });
                    ui.separator();
                }
            });
        // Les textures des événements disparus de la liste sont lâchées.
        self.textures
            .retain(|key, _| state.items.iter().any(|i| &texture_key(&i.event) == key));
    }
}

/// La capture d'un passage en cours n'est pas la finale : clé différente.
fn texture_key(ev: &protocol::wire::DetectionEventView) -> String {
    format!("{}{}", ev.id, if ev.in_progress { "-encours" } else { "" })
}

/// « 14:05 » aujourd'hui, sinon « 03/11 14:05 » (heure locale).
fn when_text(at: f64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(at as i64, 0).single() else {
        return String::new();
    };
    if t.date_naive() == Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%d/%m %H:%M").to_string()
    }
}
