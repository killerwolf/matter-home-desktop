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
    let inner = theme::card_frame(ui)
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(18, 14))
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

/// L'icône d'un état, dessinée (aucune police ne garantit les symboles) : coche, cercle barré,
/// croix ou anneau, de la couleur de l'état.
pub fn state_icon(ui: &mut egui::Ui, level: Level, size: f32) {
    let color = level_color(ui, level);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let (c, k) = (rect.center(), size / 24.0);
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    let stroke = Stroke::new(2.0, color);
    let painter = ui.painter();
    painter.circle_stroke(c, 9.0 * k, stroke);
    match level {
        Level::Ok => {
            painter.add(egui::Shape::line(
                vec![at(7.5, 12.5), at(10.8, 15.8), at(16.5, 9.0)],
                stroke,
            ));
        }
        Level::Warn => {
            painter.line_segment([at(5.6, 18.4), at(18.4, 5.6)], stroke);
        }
        Level::Error => {
            painter.line_segment([at(9.0, 9.0), at(15.0, 15.0)], stroke);
            painter.line_segment([at(15.0, 9.0), at(9.0, 15.0)], stroke);
        }
        Level::Info => {}
    }
}

/// La phrase d'état du haut de la vue d'ensemble : une icône, le titre de 18 px (en `error`
/// quand le serveur est arrêté sur une erreur) et une ligne en dessous. Pas de carte : la
/// phrase est la page.
pub fn headline(ui: &mut egui::Ui, level: Level, text: &str, detail: Option<&str>) {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let color = if level == Level::Error {
        p.error
    } else {
        p.text
    };
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.add_space(2.0);
        ui.vertical(|ui| {
            ui.add_space(3.0);
            state_icon(ui, level, 20.0);
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(text)
                    .size(t.font.size.heading)
                    .strong()
                    .color(color),
            );
            if let Some(detail) = detail {
                ui.add(egui::Label::new(RichText::new(detail).color(p.text_secondary)).wrap());
            }
        });
    });
    ui.add_space(t.space.md);
}

/// Une ligne de la carte État : le titre, puis l'icône d'état et la phrase (elle passe à la
/// ligne). Le bouton va à droite quand son texte est court (« Voir »), sinon sous la phrase,
/// suivi de `hint`. Rend `true` si le bouton est cliqué.
pub fn status_line(
    ui: &mut egui::Ui,
    level: Level,
    title: &str,
    text: &str,
    action: Option<&str>,
    hint: Option<&str>,
) -> bool {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let color = level_color(ui, level);
    let short = action.is_some_and(|a| a.chars().count() <= 12);
    let right = if short { 72.0 } else { 0.0 };
    let mut clicked = false;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.sm;
        ui.add_sized(
            [112.0, 18.0],
            egui::Label::new(RichText::new(title).strong()),
        );
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - right - 8.0).max(120.0));
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    state_icon(ui, level, 14.0);
                });
                let mut word = RichText::new(text).color(if level == Level::Info {
                    p.text_secondary
                } else {
                    color
                });
                if level.needs_attention() {
                    word = word.strong();
                }
                ui.add(egui::Label::new(word).wrap());
            });
            if let (Some(action), false) = (action, short) {
                clicked = theme::secondary_button(ui, action).clicked();
            }
            if let Some(hint) = hint {
                self::hint(ui, hint);
            }
        });
        if let (Some(action), true) = (action, short) {
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                clicked = theme::secondary_button(ui, action).clicked();
            });
        }
    });
    ui.add_space(t.space.xs);
    clicked
}

/// Une ligne cliquable pleine largeur, avec un « › » à droite (« Que voulez-vous faire ? »).
pub fn link_row(ui: &mut egui::Ui, text: &str) -> bool {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, p.surface_alt);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    ui.painter().text(
        rect.left_center() + Vec2::new(6.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        p.text,
    );
    chevron(
        ui,
        rect.right_center() - Vec2::new(10.0, 0.0),
        false,
        p.text_secondary,
    );
    response.clicked()
}

/// Un chevron dessiné : « › » (fermé) ou « ⌄ » (ouvert).
pub fn chevron(ui: &egui::Ui, at: egui::Pos2, open: bool, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let painter = ui.painter();
    if open {
        painter.line_segment(
            [at + Vec2::new(-4.0, -2.0), at + Vec2::new(0.0, 2.0)],
            stroke,
        );
        painter.line_segment(
            [at + Vec2::new(0.0, 2.0), at + Vec2::new(4.0, -2.0)],
            stroke,
        );
    } else {
        painter.line_segment(
            [at + Vec2::new(-2.0, -4.0), at + Vec2::new(2.0, 0.0)],
            stroke,
        );
        painter.line_segment(
            [at + Vec2::new(2.0, 0.0), at + Vec2::new(-2.0, 4.0)],
            stroke,
        );
    }
}

/// Une carte repliable : le titre et, à droite, un résumé et un chevron. Rend `true` quand
/// l'en-tête est cliqué ; le contenu ne se dessine que si `open`.
pub fn fold_card(
    ui: &mut egui::Ui,
    title: &str,
    summary: Option<&str>,
    open: bool,
    add: impl FnOnce(&mut egui::Ui),
) -> bool {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let mut toggled = false;
    theme::card_frame(ui)
        .inner_margin(egui::Margin::symmetric(18, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title));
            let painter = ui.painter();
            painter.text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(t.font.size.title),
                p.text,
            );
            let mut right = rect.right() - 8.0;
            if let Some(summary) = summary {
                let galley = painter.layout_no_wrap(
                    summary.to_string(),
                    FontId::proportional(t.font.size.body),
                    p.text_secondary,
                );
                let w = galley.size().x;
                painter.galley(
                    egui::pos2(right - 12.0 - w, rect.center().y - galley.size().y / 2.0),
                    galley,
                    p.text_secondary,
                );
                right -= 12.0 + w;
            }
            let _ = right;
            chevron(
                ui,
                egui::pos2(rect.right() - 8.0, rect.center().y),
                open,
                p.text_secondary,
            );
            toggled = response.clicked();
            if open {
                ui.add_space(t.space.sm);
                add(ui);
            }
        });
    ui.add_space(t.space.sm);
    toggled
}

/// Un bandeau d'erreur : la cause, « Réessayer » si on peut reprendre, et « ✕ ». Rend
/// `(réessayer, fermer)`.
pub fn error_notice(ui: &mut egui::Ui, text: &str, retry: bool) -> (bool, bool) {
    let p = theme::palette(ui);
    let (mut again, mut close) = (false, false);
    theme::error_banner(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui
                    .add(egui::Button::new(RichText::new("✕").color(p.text_secondary)).frame(false))
                    .on_hover_text("Fermer")
                    .clicked();
                if retry {
                    again = theme::primary_button(ui, "Réessayer").clicked();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap());
                });
            });
        });
    });
    ui.add_space(theme::tokens().space.sm);
    (again, close)
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

/// Un bouton de mode (« Présent », « Absent »…) de 132 × 32 : le mode actuel est plein (Lueur
/// pour Présent, `accent_soft` pour les autres), les autres sont des boutons secondaires.
pub fn mode_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    present: bool,
) -> egui::Response {
    let t = theme::tokens();
    let p = theme::palette(ui);
    let (fill, text, stroke) = match (selected, present) {
        (true, true) => (p.lueur, p.on_lueur, Stroke::new(1.0, p.lueur)),
        (true, false) => (p.accent_soft, p.accent_text, Stroke::new(1.5, p.accent)),
        (false, _) => (p.surface_alt, p.text, Stroke::new(1.0, p.line)),
    };
    let mut word = RichText::new(label).size(t.font.size.body).color(text);
    if selected {
        word = word.strong();
    }
    ui.add(
        egui::Button::new(word)
            .fill(fill)
            .stroke(stroke)
            .min_size(Vec2::new(132.0, 32.0))
            .corner_radius(egui::CornerRadius::same(t.radius.md)),
    )
}
