//! Portes et fenêtres sur le plan : le pictogramme d'un capteur d'ouverture montre une porte ou
//! une fenêtre, ouverte ou fermée. Le choix de l'icône est une fonction pure (testée) ; le dessin
//! n'utilise que des primitives d'`egui` (lignes, rectangles, polylignes, arc), à la taille des
//! autres pictogrammes du plan. Seul l'état anormal (ouvert) ressort : disque orange.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use protocol::home::OpeningKind;

const ORANGE: Color32 = Color32::from_rgb(230, 140, 30);
const CLOSED_DISC: Color32 = Color32::from_gray(70);
const UNKNOWN_DISC: Color32 = Color32::from_rgb(110, 110, 110);

/// Ce que dessine le plan pour un capteur d'ouverture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningIcon {
    DoorClosed,
    DoorOpen,
    DoorUnknown,
    WindowClosed,
    WindowOpen,
    WindowUnknown,
}

/// L'icône d'une porte ou d'une fenêtre dans cet état (`None` : pas encore de mesure).
pub fn opening_icon(kind: OpeningKind, open: Option<bool>) -> OpeningIcon {
    match (kind, open) {
        (OpeningKind::Door | OpeningKind::Other, Some(false)) => OpeningIcon::DoorClosed,
        (OpeningKind::Door | OpeningKind::Other, Some(true)) => OpeningIcon::DoorOpen,
        (OpeningKind::Door | OpeningKind::Other, None) => OpeningIcon::DoorUnknown,
        (OpeningKind::Window, Some(false)) => OpeningIcon::WindowClosed,
        (OpeningKind::Window, Some(true)) => OpeningIcon::WindowOpen,
        (OpeningKind::Window, None) => OpeningIcon::WindowUnknown,
    }
}

impl OpeningIcon {
    /// Couleur du disque : seul l'ouvert ressort.
    pub fn disc(self) -> Color32 {
        match self {
            OpeningIcon::DoorOpen | OpeningIcon::WindowOpen => ORANGE,
            OpeningIcon::DoorClosed | OpeningIcon::WindowClosed => CLOSED_DISC,
            OpeningIcon::DoorUnknown | OpeningIcon::WindowUnknown => UNKNOWN_DISC,
        }
    }

    fn is_unknown(self) -> bool {
        matches!(self, OpeningIcon::DoorUnknown | OpeningIcon::WindowUnknown)
    }
}

/// Dessine l'icône par-dessus un disque déjà peint (`radius` = rayon du disque, 15 px au plan).
pub fn paint(painter: &Painter, center: Pos2, radius: f32, icon: OpeningIcon) {
    let u = radius / 15.0;
    let ink = if icon.is_unknown() {
        Color32::from_gray(200)
    } else {
        Color32::WHITE
    };
    let stroke = Stroke::new(1.6 * u, ink);
    let faint = Color32::from_white_alpha(70);
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * u;
    let polygon = |points: &[(f32, f32)], fill: Color32| {
        painter.add(Shape::convex_polygon(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            fill,
            stroke,
        ));
    };
    match icon {
        OpeningIcon::DoorClosed | OpeningIcon::DoorUnknown => {
            // Cadre, vantail plein dedans, poignée.
            painter.rect_stroke(
                Rect::from_two_pos(at(-5.5, -8.5), at(5.5, 8.5)),
                0.0,
                stroke,
                eframe::egui::StrokeKind::Middle,
            );
            painter.rect_filled(Rect::from_two_pos(at(-3.5, -6.5), at(3.5, 8.5)), 0.0, faint);
            painter.circle_filled(at(1.8, 1.0), 1.1 * u, ink);
        }
        OpeningIcon::DoorOpen => {
            // Cadre, vantail ouvert (en perspective, charnière à gauche) et petit arc de débattement.
            painter.rect_stroke(
                Rect::from_two_pos(at(-5.5, -8.5), at(5.5, 8.5)),
                0.0,
                stroke,
                eframe::egui::StrokeKind::Middle,
            );
            polygon(&[(-5.5, -8.5), (1.0, -6.0), (1.0, 6.0), (-5.5, 8.5)], faint);
            let hinge = (-5.5_f32, 8.5_f32);
            let arc: Vec<Pos2> = (0..=6)
                .map(|i| {
                    let a = (-i as f32 * 5.0).to_radians();
                    at(hinge.0 + 9.0 * a.cos(), hinge.1 + 9.0 * a.sin())
                })
                .collect();
            painter.add(Shape::line(arc, Stroke::new(1.0 * u, ink)));
        }
        OpeningIcon::WindowClosed | OpeningIcon::WindowUnknown => {
            // Cadre et croisillon.
            painter.rect_stroke(
                Rect::from_two_pos(at(-8.0, -8.0), at(8.0, 8.0)),
                0.0,
                stroke,
                eframe::egui::StrokeKind::Middle,
            );
            painter.line_segment([at(0.0, -8.0), at(0.0, 8.0)], stroke);
            painter.line_segment([at(-8.0, 0.0), at(8.0, 0.0)], stroke);
        }
        OpeningIcon::WindowOpen => {
            // Cadre et deux battants ouverts vers l'intérieur (trapèzes sur les côtés).
            painter.rect_stroke(
                Rect::from_two_pos(at(-8.0, -8.0), at(8.0, 8.0)),
                0.0,
                stroke,
                eframe::egui::StrokeKind::Middle,
            );
            polygon(
                &[(-8.0, -8.0), (-3.0, -5.5), (-3.0, 5.5), (-8.0, 8.0)],
                faint,
            );
            polygon(&[(8.0, -8.0), (3.0, -5.5), (3.0, 5.5), (8.0, 8.0)], faint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_follows_the_kind_and_the_state() {
        use OpeningKind::{Door, Window};
        assert_eq!(opening_icon(Door, Some(false)), OpeningIcon::DoorClosed);
        assert_eq!(opening_icon(Door, Some(true)), OpeningIcon::DoorOpen);
        assert_eq!(opening_icon(Door, None), OpeningIcon::DoorUnknown);
        assert_eq!(opening_icon(Window, Some(false)), OpeningIcon::WindowClosed);
        assert_eq!(opening_icon(Window, Some(true)), OpeningIcon::WindowOpen);
        assert_eq!(opening_icon(Window, None), OpeningIcon::WindowUnknown);
    }

    #[test]
    fn only_an_open_opening_stands_out() {
        assert_eq!(OpeningIcon::DoorOpen.disc(), OpeningIcon::WindowOpen.disc());
        assert_ne!(OpeningIcon::DoorOpen.disc(), OpeningIcon::DoorClosed.disc());
        assert_ne!(
            OpeningIcon::DoorClosed.disc(),
            OpeningIcon::DoorUnknown.disc()
        );
        assert_eq!(
            OpeningIcon::WindowClosed.disc(),
            OpeningIcon::DoorClosed.disc()
        );
    }

    #[test]
    fn every_icon_draws_without_a_window() {
        let ctx = eframe::egui::Context::default();
        for icon in [
            OpeningIcon::DoorClosed,
            OpeningIcon::DoorOpen,
            OpeningIcon::DoorUnknown,
            OpeningIcon::WindowClosed,
            OpeningIcon::WindowOpen,
            OpeningIcon::WindowUnknown,
        ] {
            let out = ctx.run_ui(eframe::egui::RawInput::default(), |ui| {
                let painter = ui.painter().clone();
                paint(&painter, Pos2::new(50.0, 50.0), 15.0, icon);
            });
            assert!(!out.shapes.is_empty(), "{icon:?} ne dessine rien");
            out.drop_without_applying_deltas();
        }
    }
}
