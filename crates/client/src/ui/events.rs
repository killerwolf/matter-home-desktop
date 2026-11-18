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
    /// Ce que le WebSocket a annoncé (voir [`signature`]), pour relire la liste quand un événement
    /// apparaît, passe à « terminé », reçoit une capture plus récente ou sa description.
    last_seen: Vec<Signature>,
}

impl EventsView {
    pub fn new() -> Self {
        matter_home_client::net::events::purge_old_clips();
        Self {
            feed: None,
            textures: HashMap::new(),
            last_seen: Vec::new(),
        }
    }

    /// Déconnexion : tout est oublié.
    pub fn clear(&mut self) {
        if let Some(feed) = &self.feed {
            feed.clear();
        }
        self.textures.clear();
        self.last_seen.clear();
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
        let seen: Vec<Signature> = model.detections.iter().map(signature).collect();
        let feed = self.feed(ui.ctx()).clone();
        if !seen.is_empty() && seen != self.last_seen {
            self.last_seen = seen;
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
                            if let Some((text, niveau)) = description_line(ev) {
                                let p = theme::palette(ui);
                                let color = match niveau {
                                    Some(2) => p.error,
                                    Some(1) => p.warn,
                                    _ => p.text,
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(text).small().color(color))
                                        .wrap(),
                                );
                            }
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

/// Ce qui change dans un événement quand il faut relire la liste : identifiant, « en cours », fin du
/// passage (une capture plus récente à chaque mise à jour d'un passage en cours), description.
type Signature = (String, bool, u64, bool);

fn signature(ev: &protocol::wire::DetectionEventView) -> Signature {
    (
        ev.id.clone(),
        ev.in_progress,
        ev.end.to_bits(),
        ev.description.is_some(),
    )
}

/// La capture d'un passage en cours n'est pas la finale, et change à chaque mise à jour du passage :
/// clé différente (sinon la vignette resterait périmée).
fn texture_key(ev: &protocol::wire::DetectionEventView) -> String {
    format!(
        "{}-{:x}{}",
        ev.id,
        ev.end.to_bits(),
        if ev.in_progress { "-encours" } else { "" }
    )
}

/// La description (ticket n° 19) et son niveau : « Un livreur dépose un colis. », avec « (menace :
/// suspect) » pour un niveau non nul.
fn description_line(ev: &protocol::wire::DetectionEventView) -> Option<(String, Option<u8>)> {
    let text = ev.description.as_deref().filter(|d| !d.is_empty())?;
    Some(match ev.niveau {
        Some(n @ 1..) => (
            format!("{text} (menace : {})", protocol::wire::niveau_label(n)),
            Some(n),
        ),
        other => (text.to_string(), other),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::{DetectedClass, DetectionEventView};

    fn event() -> DetectionEventView {
        DetectionEventView {
            id: "1000-7".into(),
            camera_id: 7,
            camera_name: "Allée".into(),
            classes: vec![DetectedClass::Personne],
            score: 0.8,
            start: 1000.0,
            end: 1005.0,
            has_clip: false,
            clip_s: 0.0,
            in_progress: true,
            description: None,
            niveau: None,
        }
    }

    #[test]
    fn an_update_of_a_passage_in_progress_changes_the_texture_and_the_signature() {
        let first = event();
        let mut update = event();
        update.end = 1012.0;
        assert_ne!(texture_key(&first), texture_key(&update));
        assert_ne!(signature(&first), signature(&update));
        // La description seule change la signature (la liste est relue) mais pas la capture.
        let mut described = update.clone();
        described.description = Some("Une personne.".into());
        assert_ne!(signature(&update), signature(&described));
        assert_eq!(texture_key(&update), texture_key(&described));
        let mut closed = described.clone();
        closed.in_progress = false;
        assert_ne!(texture_key(&described), texture_key(&closed));
    }

    #[test]
    fn the_description_line_shows_the_threat_level_when_not_normal() {
        assert_eq!(description_line(&event()), None);
        let mut ev = event();
        ev.description = Some("Un livreur dépose un colis.".into());
        ev.niveau = Some(0);
        assert_eq!(
            description_line(&ev),
            Some(("Un livreur dépose un colis.".into(), Some(0)))
        );
        ev.niveau = Some(2);
        assert_eq!(
            description_line(&ev),
            Some((
                "Un livreur dépose un colis. (menace : critique)".into(),
                Some(2)
            ))
        );
        ev.description = Some("description indisponible".into());
        ev.niveau = None;
        assert_eq!(
            description_line(&ev),
            Some(("description indisponible".into(), None))
        );
    }
}
