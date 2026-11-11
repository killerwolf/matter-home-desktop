//! Les icônes d'état des tuiles de l'écran Appareils et de la fenêtre de détail : dessinées avec
//! des formes d'egui (cercles, rectangles, traits), jamais avec un glyphe de police (un glyphe
//! absent s'affiche en carré vide). Aplats et traits fins, sans ombre ni animation.

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

use super::plan::opening::{self, OpeningIcon};
use super::theme::{self, Palette};

/// Ce que dit l'icône d'une tuile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IconKind {
    LightOn,
    LightOff,
    Plug {
        on: bool,
    },
    /// Porte ou fenêtre, ouverte ou fermée : les icônes du plan.
    Opening(OpeningIcon),
    /// Volet : `open_percent` (0 = fermé) quand sa position est connue.
    Shutter {
        open_percent: Option<f64>,
    },
    Thermometer,
    /// Télécommande ou bouton : une icône neutre.
    Button,
    Generic,
}

/// Dessine l'icône, centrée en `center`, dans un carré de `size` pixels. `fade` (0 à 1) estompe
/// tout (appareil injoignable).
pub fn paint(painter: &Painter, center: Pos2, size: f32, icon: IconKind, pal: &Palette, fade: f32) {
    let tile = theme::tokens().tile;
    let ink = pal.text_secondary.gamma_multiply(fade);
    let stroke = Stroke::new(size / 15.0, ink);
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * size;
    match icon {
        IconKind::LightOn | IconKind::LightOff => {
            let on = icon == IconKind::LightOn;
            let glass = at(0.0, -0.08);
            let r = 0.34 * size;
            if on {
                let warm = tile.on_tint.gamma_multiply(fade);
                painter.circle_filled(glass, r, warm);
                // Six rayons sur le haut de l'ampoule.
                for k in 0..7 {
                    let a = std::f32::consts::PI * (0.15 + 0.7 * k as f32 / 6.0);
                    let (dx, dy) = (-a.cos(), -a.sin());
                    let d = Vec2::new(dx, dy);
                    painter.line_segment(
                        [glass + d * (r + 0.05 * size), glass + d * (r + 0.14 * size)],
                        Stroke::new(size / 18.0, warm),
                    );
                }
            } else {
                painter.circle_stroke(glass, r, stroke);
            }
            // Le culot.
            let base = Rect::from_center_size(at(0.0, 0.32), Vec2::new(0.3 * size, 0.12 * size));
            painter.rect_filled(base, 2.0, ink);
        }
        IconKind::Plug { on } => {
            let body_color = if on { pal.accent_text } else { ink };
            let body_color = body_color.gamma_multiply(if on { fade } else { 1.0 });
            let body = Rect::from_center_size(at(0.0, 0.1), Vec2::new(0.52 * size, 0.38 * size));
            painter.rect_filled(body, 0.08 * size, body_color);
            for x in [-0.12, 0.12] {
                painter.line_segment(
                    [at(x, -0.1), at(x, -0.32)],
                    Stroke::new(size / 11.0, body_color),
                );
            }
            painter.line_segment(
                [at(0.0, 0.29), at(0.0, 0.42)],
                Stroke::new(size / 11.0, ink),
            );
        }
        IconKind::Opening(icon) => {
            let r = 0.5 * size;
            painter.circle_filled(center, r, icon.disc(pal).gamma_multiply(fade));
            opening::paint(painter, center, r, icon);
        }
        IconKind::Shutter { open_percent } => {
            let frame = Rect::from_center_size(center, Vec2::splat(0.7 * size));
            // La partie fermée (en haut) est pleine ; le reste, des lames.
            let closed = open_percent.map_or(0.0, |p| 1.0 - (p / 100.0).clamp(0.0, 1.0)) as f32;
            if closed > 0.0 {
                let shut = Rect::from_min_size(
                    frame.min,
                    Vec2::new(frame.width(), frame.height() * closed),
                );
                painter.rect_filled(shut, 0.0, ink.gamma_multiply(0.55));
            }
            for k in 1..5 {
                let y = frame.top() + frame.height() * k as f32 / 5.0;
                painter.line_segment(
                    [Pos2::new(frame.left(), y), Pos2::new(frame.right(), y)],
                    Stroke::new(size / 22.0, ink),
                );
            }
            painter.rect_stroke(frame, 1.0, stroke, eframe::egui::StrokeKind::Inside);
        }
        IconKind::Thermometer => {
            let mercury = pal.series_temperature.gamma_multiply(fade);
            let tube = Rect::from_center_size(at(0.0, -0.1), Vec2::new(0.17 * size, 0.55 * size));
            painter.rect_stroke(tube, 0.085 * size, stroke, eframe::egui::StrokeKind::Inside);
            let fill = Rect::from_min_max(
                Pos2::new(tube.left() + size / 15.0, tube.center().y),
                Pos2::new(tube.right() - size / 15.0, tube.bottom()),
            );
            painter.rect_filled(fill, 0.0, mercury);
            painter.circle_filled(at(0.0, 0.26), 0.15 * size, mercury);
            painter.circle_stroke(at(0.0, 0.26), 0.15 * size, stroke);
        }
        IconKind::Button => {
            painter.circle_stroke(center, 0.34 * size, stroke);
            painter.circle_filled(center, 0.13 * size, ink);
        }
        IconKind::Generic => {
            painter.circle_stroke(center, 0.3 * size, stroke);
        }
    }
}

/// Trois points : le bouton « détail » d'une tuile (dessiné, pas un glyphe).
pub fn paint_more(painter: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    for dx in [-5.0, 0.0, 5.0] {
        painter.circle_filled(Pos2::new(c.x + dx, c.y), 1.6, color);
    }
}
