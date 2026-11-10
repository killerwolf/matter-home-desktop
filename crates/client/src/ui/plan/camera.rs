//! Caméras sur le plan : état, géométrie du champ filmé (cône ou rectangle), poignées de rotation
//! et de portée. Les calculs sont des fonctions pures (testées) ; le dessin les utilise.
//!
//! Le champ est dessiné en **mètres** (il suit le zoom du plan), l'orientation est indépendante du
//! zoom, et la poignée de rotation est à une distance fixe **en pixels** de l'icône pour rester
//! saisissable à tout niveau de zoom.

use eframe::egui::{Color32, Painter, Pos2, Shape, Stroke, Vec2};
use protocol::home::{rotation_vector, FovShape};
use protocol::wire::{CameraStatus, CameraView};

/// Distance (px) entre le centre de l'icône et la poignée de rotation.
pub const ROTATE_HANDLE_DIST: f32 = 40.0;
/// Rayon (px) des poignées.
pub const HANDLE_RADIUS: f32 = 7.0;
/// Pas de la rotation à la souris (degrés) ; Maj pour une rotation libre.
pub const ROTATION_STEP: f32 = 5.0;
/// Pas de la portée à la souris (m).
pub const RANGE_STEP: f32 = 0.25;

use crate::ui::camera_text::{
    offline_since_now, problem_text as problem_phrase, NEUTRAL, OFFLINE, ONLINE,
};

/// Ce que le plan sait d'une caméra posée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraState {
    Online,
    Unreachable,
    Unknown,
    /// Le serveur ne connaît plus cette caméra (supprimée, ou plan restauré d'une sauvegarde).
    Missing,
}

impl CameraState {
    pub fn of(camera: Option<&CameraView>) -> Self {
        match camera.map(|c| &c.status) {
            None => CameraState::Missing,
            Some(CameraStatus::Online) => CameraState::Online,
            Some(CameraStatus::Unreachable { .. }) => CameraState::Unreachable,
            Some(CameraStatus::Unknown) => CameraState::Unknown,
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            CameraState::Online => ONLINE,
            CameraState::Unreachable => OFFLINE,
            CameraState::Unknown | CameraState::Missing => NEUTRAL,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CameraState::Online => "En ligne",
            CameraState::Unreachable => "Injoignable",
            CameraState::Unknown => "État inconnu",
            CameraState::Missing => "Supprimée du serveur",
        }
    }

    /// Une caméra injoignable ou absente ne « voit » rien : son champ est effacé ou très pâle.
    fn fov_alpha(self) -> u8 {
        match self {
            CameraState::Online => 55,
            CameraState::Unknown => 35,
            CameraState::Unreachable => 22,
            CameraState::Missing => 0,
        }
    }
}

/// Phrase pour une caméra injoignable (sans détail technique ni adresse).
pub fn problem_text(status: &CameraStatus) -> Option<&'static str> {
    match status {
        CameraStatus::Unreachable { problem, .. } => Some(problem_phrase(*problem)),
        _ => None,
    }
}

/// Texte du survol : nom (ou mention d'une caméra supprimée) et état.
pub fn tooltip(camera: Option<&CameraView>) -> String {
    match camera {
        None => "Caméra supprimée du serveur".to_string(),
        Some(c) => {
            let state = CameraState::of(Some(c));
            let mut text = format!("{}\n{}", c.name, state.label());
            if let Some(why) = problem_text(&c.status) {
                text.push_str(&format!(" ({why})"));
            }
            if let CameraStatus::Unreachable {
                since: Some(since), ..
            } = &c.status
            {
                text.push_str(&format!("\n{}", offline_since_now(*since)));
            }
            text
        }
    }
}

/// Position écran du point situé à `dist` pixels de `center` dans la direction `rotation`.
pub fn along(center: Pos2, rotation: f32, dist: f32) -> Pos2 {
    let (dx, dy) = rotation_vector(rotation);
    center + Vec2::new(dx, dy) * dist
}

/// Contour du champ filmé, en pixels. `range_px` est la portée à l'écran.
/// Cône : le centre puis l'arc (un point tous les 5 degrés au plus). Rectangle : profondeur
/// `range_px`, largeur égale à la corde de l'arc (`2·portée·sin(ouverture/2)`).
pub fn fov_points(
    center: Pos2,
    rotation: f32,
    fov: f32,
    range_px: f32,
    shape: FovShape,
) -> Vec<Pos2> {
    match shape {
        FovShape::Cone => {
            let steps = (fov / 5.0).ceil().max(2.0) as usize;
            let mut points = Vec::with_capacity(steps + 2);
            points.push(center);
            for i in 0..=steps {
                let a = rotation - fov / 2.0 + fov * i as f32 / steps as f32;
                points.push(along(center, a, range_px));
            }
            points
        }
        FovShape::Rectangle => {
            let (dx, dy) = rotation_vector(rotation);
            let dir = Vec2::new(dx, dy);
            let side = Vec2::new(-dy, dx) * range_px * (fov.to_radians() / 2.0).sin();
            let far = dir * range_px;
            vec![
                center - side,
                center + side,
                center + side + far,
                center - side + far,
            ]
        }
    }
}

/// Poignée de rotation : à distance fixe en pixels, dans la direction filmée.
pub fn rotate_handle(center: Pos2, rotation: f32) -> Pos2 {
    along(center, rotation, ROTATE_HANDLE_DIST)
}

/// Poignée de portée : au bout du champ, seulement s'il est assez long pour ne pas recouvrir
/// la poignée de rotation.
pub fn range_handle(center: Pos2, rotation: f32, range_px: f32) -> Option<Pos2> {
    (range_px >= ROTATE_HANDLE_DIST + 2.0 * HANDLE_RADIUS)
        .then(|| along(center, rotation, range_px))
}

/// Orientation (degrés, pas de 5 sauf Maj) donnée par un pointeur déplacé à `(dx, dy)` mètres du
/// centre de la caméra. `None` si le pointeur est sur le centre.
pub fn rotation_from_pointer(dx: f32, dy: f32, free: bool) -> Option<f32> {
    let r = protocol::home::rotation_of(dx, dy)?;
    Some(if free {
        protocol::home::normalize_rotation(r)
    } else {
        protocol::home::normalize_rotation((r / ROTATION_STEP).round() * ROTATION_STEP)
    })
}

/// Portée (m) donnée par un pointeur à `(dx, dy)` mètres du centre, au pas de 25 cm, bornée.
pub fn range_from_pointer(dx: f32, dy: f32) -> f32 {
    let d = dx.hypot(dy);
    ((d / RANGE_STEP).round() * RANGE_STEP).clamp(
        protocol::home::MIN_CAMERA_RANGE,
        protocol::home::MAX_CAMERA_RANGE,
    )
}

/// Champ filmé d'une caméra, à l'échelle de l'écran.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub rotation: f32,
    pub fov: f32,
    pub range_px: f32,
    pub shape: FovShape,
}

/// Dessine le champ filmé (translucide) d'une caméra.
pub fn paint_fov(
    painter: &Painter,
    center: Pos2,
    field: Field,
    state: CameraState,
    selected: bool,
) {
    let Field {
        rotation,
        fov,
        range_px,
        shape,
    } = field;
    let alpha = state.fov_alpha();
    if alpha == 0 {
        return;
    }
    let c = state.color();
    let fill = Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        if selected { alpha + 35 } else { alpha },
    );
    let edge = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 150);
    painter.add(Shape::convex_polygon(
        fov_points(center, rotation, fov, range_px, shape),
        fill,
        Stroke::new(1.0, edge),
    ));
}

/// Icône de la caméra : disque d'état, objectif, et museau tourné vers la direction filmée.
pub fn paint_icon(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    rotation: f32,
    state: CameraState,
    ring: Stroke,
) {
    let (dx, dy) = rotation_vector(rotation);
    let dir = Vec2::new(dx, dy);
    let side = Vec2::new(-dy, dx);
    let color = state.color();
    // Museau : triangle sur le bord du disque, dans le sens filmé.
    let nose = vec![
        center + dir * (radius + 7.0),
        center + dir * (radius - 4.0) + side * 7.0,
        center + dir * (radius - 4.0) - side * 7.0,
    ];
    painter.add(Shape::convex_polygon(
        nose,
        color,
        Stroke::new(1.5, ring.color),
    ));
    painter.circle_filled(center, radius, color);
    painter.circle_stroke(center, radius, ring);
    // Objectif.
    painter.circle_filled(center, radius * 0.55, Color32::from_gray(25));
    painter.circle_stroke(center, radius * 0.55, Stroke::new(1.5, Color32::WHITE));
    painter.circle_filled(
        center + dir * 2.0,
        radius * 0.2,
        Color32::from_rgb(120, 170, 230),
    );
    if state == CameraState::Unreachable {
        // Barre oblique : caméra hors ligne, visible même sans la couleur.
        let off = Vec2::new(1.0, -1.0).normalized() * radius;
        painter.line_segment(
            [center - off, center + off],
            Stroke::new(2.5, Color32::WHITE),
        );
    }
}

/// Poignées d'une caméra sélectionnée en mode modification.
pub fn paint_handles(
    painter: &Painter,
    center: Pos2,
    rotation: f32,
    range_px: f32,
    accent: Color32,
) {
    let h = rotate_handle(center, rotation);
    painter.line_segment([center, h], Stroke::new(1.5, accent));
    painter.circle_filled(h, HANDLE_RADIUS, accent);
    painter.circle_stroke(h, HANDLE_RADIUS, Stroke::new(1.5, Color32::WHITE));
    if let Some(r) = range_handle(center, rotation, range_px) {
        let sq = eframe::egui::Rect::from_center_size(r, Vec2::splat(HANDLE_RADIUS * 1.6));
        painter.rect_filled(sq, 2.0, accent);
        painter.rect_stroke(
            sq,
            2.0,
            Stroke::new(1.5, Color32::WHITE),
            eframe::egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::{CameraProblem, StreamInfo};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-2
    }

    fn view(status: CameraStatus) -> CameraView {
        CameraView {
            id: protocol::wire::CAMERA_ID_MIN + 1,
            name: "Entrée".into(),
            status,
            has_sub_stream: true,
            stream: None::<StreamInfo>,
        }
    }

    #[test]
    fn states_and_tooltips() {
        assert_eq!(CameraState::of(None), CameraState::Missing);
        let online = view(CameraStatus::Online);
        assert_eq!(CameraState::of(Some(&online)), CameraState::Online);
        assert_eq!(tooltip(Some(&online)), "Entrée\nEn ligne");
        let down = view(CameraStatus::Unreachable {
            problem: CameraProblem::Unauthorized,
            detail: Some("secret".into()),

            since: None,
        });
        assert_eq!(CameraState::of(Some(&down)), CameraState::Unreachable);
        let text = tooltip(Some(&down));
        assert_eq!(
            text,
            "Entrée\nInjoignable (identifiant ou mot de passe refusé)"
        );
        assert!(!text.contains("secret"));
        assert_eq!(
            CameraState::of(Some(&view(CameraStatus::Unknown))),
            CameraState::Unknown
        );
        assert!(tooltip(None).contains("supprimée"));
        // Avec l'heure de la panne, le survol dit depuis quand.
        let since = view(CameraStatus::Unreachable {
            problem: CameraProblem::Unreachable,
            detail: None,
            since: Some(1_700_000_000),
        });
        assert!(tooltip(Some(&since)).contains("\nHors ligne depuis "));
    }

    #[test]
    fn cone_points_lie_on_the_arc_and_follow_rotation() {
        let c = Pos2::new(100.0, 100.0);
        for rot in [0.0, 90.0, 200.0] {
            let pts = fov_points(c, rot, 90.0, 80.0, FovShape::Cone);
            assert_eq!(pts[0], c);
            for p in &pts[1..] {
                assert!(close(p.distance(c), 80.0));
            }
            // Le milieu de l'arc est exactement dans la direction filmée.
            let mid = pts[1 + (pts.len() - 2) / 2];
            let expected = along(c, rot, 80.0);
            // 90 degrés = 18 pas : le point central existe.
            assert!(mid.distance(expected) < 0.5, "{rot} {mid:?} {expected:?}");
        }
        // Tourné vers le haut du plan : les points sont au-dessus (y plus petit).
        let up = fov_points(c, 0.0, 90.0, 80.0, FovShape::Cone);
        assert!(up[1..].iter().all(|p| p.y < c.y));
        // Tourné vers la droite : x plus grand.
        let right = fov_points(c, 90.0, 90.0, 80.0, FovShape::Cone);
        assert!(right[1..].iter().all(|p| p.x > c.x));
    }

    #[test]
    fn the_shape_scales_with_the_range_only() {
        // Même orientation, deux zooms : la forme est homothétique, la direction ne change pas.
        let c = Pos2::ZERO;
        let a = fov_points(c, 135.0, 60.0, 50.0, FovShape::Cone);
        let b = fov_points(c, 135.0, 60.0, 100.0, FovShape::Cone);
        for (p, q) in a.iter().zip(&b) {
            assert!(close(p.x * 2.0, q.x) && close(p.y * 2.0, q.y));
        }
    }

    #[test]
    fn rectangle_has_range_depth_and_chord_width() {
        let c = Pos2::new(10.0, 10.0);
        let pts = fov_points(c, 0.0, 90.0, 100.0, FovShape::Rectangle);
        assert_eq!(pts.len(), 4);
        let width = pts[0].distance(pts[1]);
        let depth = pts[1].distance(pts[2]);
        assert!(close(width, 2.0 * 100.0 * 45f32.to_radians().sin()));
        assert!(close(depth, 100.0));
        // Vers le haut : le rectangle est au-dessus de la caméra.
        assert!(pts[2].y < c.y && pts[3].y < c.y);
        // Tourné à 90 degrés : il s'étend vers la droite.
        let r = fov_points(c, 90.0, 90.0, 100.0, FovShape::Rectangle);
        assert!(close(r[2].x, c.x + 100.0));
    }

    #[test]
    fn pointer_gives_snapped_rotation_and_range() {
        assert_eq!(rotation_from_pointer(0.0, -2.0, false), Some(0.0));
        assert_eq!(rotation_from_pointer(2.0, 0.0, false), Some(90.0));
        // 47 degrés : arrondi à 45 ; libre : gardé.
        let (dx, dy) = rotation_vector(47.0);
        assert_eq!(rotation_from_pointer(dx, dy, false), Some(45.0));
        assert!(close(rotation_from_pointer(dx, dy, true).unwrap(), 47.0));
        // 359 degrés s'arrondit à 360, qui redevient 0 : jamais 360.
        let (dx, dy) = rotation_vector(359.0);
        assert_eq!(rotation_from_pointer(dx, dy, false), Some(0.0));
        assert_eq!(rotation_from_pointer(0.0, 0.0, false), None);
        assert_eq!(range_from_pointer(3.1, 0.0), 3.0);
        assert_eq!(
            range_from_pointer(0.0, 0.1),
            protocol::home::MIN_CAMERA_RANGE
        );
        assert_eq!(
            range_from_pointer(500.0, 0.0),
            protocol::home::MAX_CAMERA_RANGE
        );
    }

    #[test]
    fn handle_sits_at_a_fixed_pixel_distance_whatever_the_zoom() {
        let c = Pos2::new(0.0, 0.0);
        let h = rotate_handle(c, 90.0);
        assert!(close(h.x, ROTATE_HANDLE_DIST) && close(h.y, 0.0));
        // Portée trop courte pour une seconde poignée.
        assert!(range_handle(c, 90.0, 30.0).is_none());
        let far = range_handle(c, 90.0, 120.0).unwrap();
        assert!(close(far.x, 120.0));
    }
}
