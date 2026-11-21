//! Les événements de détection (ticket n° 18), à droite de la mosaïque des caméras : capture,
//! classe, caméra, heure ; « Lire le clip » le lit dans le panneau (ticket n° 22), « Ouvrir dans
//! QuickTime » dans le lecteur du Mac.

use std::collections::HashMap;

use eframe::egui::{self, RichText};

use super::theme;
use ostal_client::clip::{format_time, ClipPlayer};
use ostal_client::model::Model;
use ostal_client::net::events::EventsFeed;

/// L'état de la colonne : la liste (fils réseau) et les textures des captures.
pub struct EventsView {
    feed: Option<EventsFeed>,
    textures: HashMap<String, egui::TextureHandle>,
    /// Ce que le WebSocket a annoncé (voir [`signature`]), pour relire la liste quand un événement
    /// apparaît, passe à « terminé », reçoit une capture plus récente ou sa description.
    last_seen: Vec<Signature>,
    /// Le clip en cours de lecture dans le panneau (un seul à la fois).
    clip: Option<ClipView>,
}

/// Le lecteur d'un clip et la texture de son image courante.
struct ClipView {
    player: ClipPlayer,
    texture: Option<egui::TextureHandle>,
    /// Valeur de la barre pendant qu'on la déplace (le clip n'est déplacé qu'au relâchement).
    dragging: Option<f64>,
}

/// Ce que l'utilisateur a demandé dans la liste, à faire une fois la liste dessinée.
enum Action {
    Play(String),
    Close,
}

impl EventsView {
    pub fn new() -> Self {
        ostal_client::net::events::purge_old_clips();
        Self {
            feed: None,
            textures: HashMap::new(),
            last_seen: Vec::new(),
            clip: None,
        }
    }

    /// Déconnexion : tout est oublié.
    pub fn clear(&mut self) {
        if let Some(feed) = &self.feed {
            feed.clear();
        }
        self.textures.clear();
        self.last_seen.clear();
        self.clip = None;
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
        // L'événement a disparu de la liste (purge, rafraîchissement) : son clip se ferme.
        if self
            .clip
            .as_ref()
            .is_some_and(|c| !state.items.iter().any(|i| i.event.id == c.player.id))
        {
            self.clip = None;
        }
        let mut action: Option<Action> = None;
        let playing_id = self.clip.as_ref().map(|c| c.player.id.clone());
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
                            if ev.has_clip {
                                ui.horizontal(|ui| {
                                    if playing_id.as_deref() == Some(ev.id.as_str()) {
                                        if ui.small_button("Fermer le clip").clicked() {
                                            action = Some(Action::Close);
                                        }
                                    } else if ui.small_button("Lire le clip").clicked() {
                                        action = Some(Action::Play(ev.id.clone()));
                                    }
                                    if ui.small_button("Ouvrir dans QuickTime").clicked() {
                                        feed.open_clip(&ev.id);
                                    }
                                });
                            }
                        });
                    });
                    if let Some(view) = self.clip.as_mut().filter(|c| c.player.id == ev.id) {
                        clip_panel(ui, view);
                    }
                    ui.separator();
                }
            });
        match action {
            Some(Action::Play(id)) => {
                let ctx = ui.ctx().clone();
                self.clip = Some(ClipView {
                    player: ClipPlayer::open(&id, move || ctx.request_repaint()),
                    texture: None,
                    dragging: None,
                });
            }
            Some(Action::Close) => self.clip = None,
            None => {}
        }
        // Les textures des événements disparus de la liste sont lâchées.
        self.textures
            .retain(|key, _| state.items.iter().any(|i| &texture_key(&i.event) == key));
    }
}

/// Le lecteur sous l'événement : image à la largeur du panneau, puis lecture / pause, barre de
/// progression, temps écoulé et total, boucle ; un message en français si le clip ne se lit pas.
fn clip_panel(ui: &mut egui::Ui, view: &mut ClipView) {
    let ctx = ui.ctx().clone();
    if let Some(image) = view.player.update() {
        let color =
            egui::ColorImage::from_rgb([image.width as usize, image.height as usize], &image.rgb);
        match &mut view.texture {
            Some(texture) => texture.set(color, egui::TextureOptions::LINEAR),
            None => {
                view.texture =
                    Some(ctx.load_texture("clip-courant", color, egui::TextureOptions::LINEAR));
            }
        }
    }
    let p = theme::palette(ui);
    if let Some(error) = view.player.error() {
        ui.label(RichText::new(error).color(p.error));
        return;
    }
    let width = ui.available_width();
    match &view.texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let height = if size.x > 0.0 {
                width * size.y / size.x
            } else {
                width * 0.5625
            };
            ui.add(egui::Image::new((texture.id(), egui::vec2(width, height))));
        }
        None => {
            let text = if view.player.loading() {
                "Téléchargement du clip…"
            } else {
                "Décodage…"
            };
            ui.label(RichText::new(text).small().color(p.text_secondary));
        }
    }
    let duration = view.player.duration_ms();
    if duration == 0 {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    ui.horizontal(|ui| {
        let label = if view.player.playing() { "⏸" } else { "▶" };
        if ui
            .button(label)
            .on_hover_text(if view.player.playing() {
                "Pause"
            } else {
                "Lecture"
            })
            .clicked()
        {
            view.player.toggle_play();
        }
        let mut value = view.dragging.unwrap_or(view.player.position_ms() as f64);
        ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(60.0);
        let response = ui.add(
            egui::Slider::new(&mut value, 0.0..=duration as f64)
                .show_value(false)
                .trailing_fill(true),
        );
        if response.dragged() {
            view.dragging = Some(value);
        }
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            view.dragging = None;
            view.player.seek(value as u64);
        }
        let shown = view
            .dragging
            .map_or_else(|| view.player.position_ms(), |v| v as u64);
        ui.label(
            RichText::new(format!(
                "{} / {}",
                format_time(shown),
                format_time(duration)
            ))
            .small()
            .monospace(),
        );
        let mut looping = view.player.looping;
        if ui.checkbox(&mut looping, "Boucle").changed() {
            view.player.looping = looping;
        }
    });
    if view.player.playing() {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
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
