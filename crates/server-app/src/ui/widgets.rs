//! Les briques de la fenêtre, dessinées avec les tokens partagés avec le client (crate
//! `ostal-theme`) : carte, ligne d'état, étape numérotée, texte d'aide. Des aplats et des
//! traits fins, comme dans le client : ni ombre, ni animation.

use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};
use ostal_server_app::overview::Level;
use ostal_theme as theme;

pub fn level_color(ui: &egui::Ui, level: Level) -> Color32 {
    let p = theme::palette(ui);
    match level {
        Level::Ok => p.ok,
        Level::Info => p.text_secondary,
        Level::Warn => p.warn,
        Level::Error => p.error,
    }
}

/// Un point de couleur (état d'une ligne).
pub fn dot(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

fn framed<R>(ui: &mut egui::Ui, fill: Color32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let inner = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(egui::CornerRadius::same(t.radius.lg))
        .inner_margin(egui::Margin::same(t.space.md as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(t.space.md);
    inner
}

/// Une carte : fond `surface`, trait fin, sur toute la largeur.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let fill = theme::palette(ui).surface;
    framed(ui, fill, add)
}

/// Une carte avec son titre.
pub fn section<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card(ui, |ui| {
        section_title(ui, title);
        add(ui)
    })
}

pub fn section_title(ui: &mut egui::Ui, title: &str) {
    let t = theme::tokens();
    ui.label(RichText::new(title).size(t.font.size.title).strong());
    ui.add_space(t.space.xs);
}

/// Un texte d'aide, discret.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui);
    ui.add(egui::Label::new(RichText::new(text).small().color(p.text_secondary)).wrap());
}

/// Le bandeau du haut de la vue d'ensemble : un gros point et la phrase « Tout va bien ».
pub fn headline(ui: &mut egui::Ui, level: Level, text: &str, detail: Option<&str>) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let fill = match level {
        Level::Ok | Level::Info => p.surface,
        Level::Warn | Level::Error => p.banner_warn,
    };
    let color = level_color(ui, level);
    framed(ui, fill, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = t.space.md;
            dot(ui, color, 14.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(text)
                        .size(t.font.size.heading)
                        .strong()
                        .color(p.text),
                );
                if let Some(detail) = detail {
                    ui.add(egui::Label::new(RichText::new(detail).color(p.text)).wrap());
                }
            });
        });
    });
}

/// Une ligne d'état : point, titre, phrase, et un bouton à droite (rend `true` s'il est cliqué).
pub fn status_row(
    ui: &mut egui::Ui,
    level: Level,
    title: &str,
    text: &str,
    action: Option<&str>,
) -> bool {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let color = level_color(ui, level);
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.sm;
        dot(ui, color, t.size.dot + 2.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(action) = action {
                clicked = ui.button(action).clicked();
            }
            ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(title).strong());
                let text_color = if level.needs_attention() {
                    p.text
                } else {
                    p.text_secondary
                };
                ui.add(egui::Label::new(RichText::new(text).color(text_color)).wrap());
            });
        });
    });
    ui.add_space(t.space.xs);
    clicked
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Pas encore.
    Todo,
    /// C'est l'étape à faire maintenant.
    Current,
    Done,
}

/// Une étape numérotée d'une marche à suivre : une pastille (numéro, ou coche quand c'est fait)
/// et son contenu à droite.
pub fn step(ui: &mut egui::Ui, number: usize, state: StepState, add: impl FnOnce(&mut egui::Ui)) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.sm;
        let size = 20.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        let (c, r) = (rect.center(), size / 2.0);
        let painter = ui.painter();
        let number_text = |color: Color32| {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                number.to_string(),
                FontId::proportional(t.font.size.caption + 1.0),
                color,
            );
        };
        match state {
            StepState::Done => {
                painter.circle_filled(c, r, p.ok);
                // La coche est dessinée (aucune police ne la garantit).
                let stroke = Stroke::new(1.8, p.on_accent);
                painter.line_segment([c + Vec2::new(-4.5, 0.5), c + Vec2::new(-1.5, 3.5)], stroke);
                painter.line_segment([c + Vec2::new(-1.5, 3.5), c + Vec2::new(4.5, -3.5)], stroke);
            }
            StepState::Current => {
                painter.circle_filled(c, r, p.accent);
                number_text(p.on_accent);
            }
            StepState::Todo => {
                painter.circle_stroke(c, r - 0.5, Stroke::new(1.0, p.text_secondary));
                number_text(p.text_secondary);
            }
        }
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    });
    ui.add_space(t.space.sm);
}

/// Une valeur à recopier (adresse, code) : en chasse fixe sur un aplat, qui passe à la ligne.
pub fn value_box(ui: &mut egui::Ui, value: &str) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.surface_alt)
        .corner_radius(egui::CornerRadius::same(t.radius.sm))
        .inner_margin(egui::Margin::symmetric(t.space.sm as i8, t.space.xs as i8))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(value).monospace().color(p.text)).wrap());
        });
}
