//! Thème du client : couleurs, espacements, rayons et tailles de texte lus dans
//! `design/tokens.json` (la même source que la future app web, ticket n° 15), et petits composants
//! réutilisables (voir `docs/design-system.md`).
//!
//! Rien de coûteux : des aplats, des traits fins, ni ombre, ni flou, ni texture, ni animation.
//!
//! Certains éléments ne servent qu'aux phases suivantes de la refonte (plan, vignettes de
//! caméras) : le module les garde disponibles.
#![allow(dead_code)]

use std::sync::OnceLock;

use eframe::egui::{
    self, text::LayoutJob, Align2, Color32, CornerRadius, FontId, Margin, Response, RichText,
    Sense, Stroke, TextFormat, TextStyle, Theme, Ui, Vec2,
};
use serde::{Deserialize, Deserializer};

/// Le fichier de tokens, embarqué au build.
const TOKENS_JSON: &str = include_str!("../../../../design/tokens.json");

fn hex<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
    let text = String::deserialize(d)?;
    parse_hex(&text).ok_or_else(|| serde::de::Error::custom(format!("couleur illisible : {text}")))
}

/// `#rrggbb`.
fn parse_hex(text: &str) -> Option<Color32> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Les couleurs d'un thème (clair ou sombre).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Palette {
    #[serde(deserialize_with = "hex")]
    pub bg: Color32,
    #[serde(deserialize_with = "hex")]
    pub surface: Color32,
    #[serde(deserialize_with = "hex")]
    pub surface_alt: Color32,
    #[serde(deserialize_with = "hex")]
    pub line: Color32,
    #[serde(deserialize_with = "hex")]
    pub text: Color32,
    #[serde(deserialize_with = "hex")]
    pub text_secondary: Color32,
    #[serde(deserialize_with = "hex")]
    pub accent: Color32,
    #[serde(deserialize_with = "hex")]
    pub accent_soft: Color32,
    #[serde(deserialize_with = "hex")]
    pub accent_text: Color32,
    #[serde(deserialize_with = "hex")]
    pub on_accent: Color32,
    #[serde(deserialize_with = "hex")]
    pub ok: Color32,
    #[serde(deserialize_with = "hex")]
    pub warn: Color32,
    #[serde(deserialize_with = "hex")]
    pub error: Color32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Colors {
    pub light: Palette,
    pub dark: Palette,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Stop {
    pub at: f64,
    #[serde(deserialize_with = "hex")]
    pub color: Color32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemperatureScale {
    pub step: f64,
    pub fill_alpha: f32,
    #[serde(deserialize_with = "hex")]
    pub no_sensor: Color32,
    pub stops: Vec<Stop>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Space {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Radius {
    pub sm: u8,
    pub md: u8,
    pub lg: u8,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TextSizes {
    pub caption: f32,
    pub body: f32,
    pub title: f32,
    pub display: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub colors: Colors,
    pub temperature: TemperatureScale,
    pub space: Space,
    pub radius: Radius,
    pub text: TextSizes,
}

impl Tokens {
    /// Lit un fichier de tokens.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn palette(&self, dark: bool) -> &Palette {
        if dark {
            &self.colors.dark
        } else {
            &self.colors.light
        }
    }
}

/// Les tokens embarqués. Le test `tokens_parse` garantit que le fichier est lisible.
pub fn tokens() -> &'static Tokens {
    static TOKENS: OnceLock<Tokens> = OnceLock::new();
    TOKENS.get_or_init(|| Tokens::parse(TOKENS_JSON).expect("design/tokens.json illisible"))
}

/// La palette du thème actuellement affiché.
pub fn palette(ui: &Ui) -> &'static Palette {
    tokens().palette(ui.visuals().dark_mode)
}

fn radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

// ---------------------------------------------------------------------------
// Application du thème à egui
// ---------------------------------------------------------------------------

/// Installe les deux thèmes et suit celui du système.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::System);
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.all_styles_mut(style);
}

/// Les `Visuals` d'un thème, tirés de la palette.
pub fn visuals(dark: bool) -> egui::Visuals {
    let t = tokens();
    let p = t.palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = radius(t.radius.lg);
    v.menu_corner_radius = radius(t.radius.md);
    // Sans ombre : un aplat et un trait fin.
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    v.extreme_bg_color = if dark { p.bg } else { p.surface };
    v.faint_bg_color = p.surface_alt;
    v.hyperlink_color = p.accent_text;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.error;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, p.on_accent);

    let r = radius(t.radius.md);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    for (state, fill) in [
        (&mut w.inactive, p.surface_alt),
        (&mut w.hovered, p.line),
        (&mut w.active, p.line),
        (&mut w.open, p.surface_alt),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = r;
    }
    // Pas de contour qui grossit au survol.
    w.hovered.expansion = 0.0;
    w.active.expansion = 0.0;
    v
}

fn style(s: &mut egui::Style) {
    let t = tokens();
    let size = t.text;
    for (style, font) in [
        (TextStyle::Small, FontId::proportional(size.caption)),
        (TextStyle::Body, FontId::proportional(size.body)),
        (TextStyle::Button, FontId::proportional(size.body)),
        (TextStyle::Heading, FontId::proportional(size.title + 3.0)),
        (TextStyle::Monospace, FontId::monospace(size.body)),
    ] {
        s.text_styles.insert(style, font);
    }
    s.spacing.item_spacing = Vec2::new(t.space.sm, t.space.xs + 2.0);
    s.spacing.button_padding = Vec2::new(t.space.md - 2.0, t.space.xs - 1.0);
    s.spacing.interact_size.y = 24.0;
    s.spacing.window_margin = Margin::same(t.space.md as i8);
    s.animation_time = 0.0;
}

// ---------------------------------------------------------------------------
// Échelle de température
// ---------------------------------------------------------------------------

/// Teinte du palier le plus proche de `celsius` (à égalité, le plus froid). Sans mesure :
/// le gris « pas de capteur ».
pub fn temperature_color(celsius: Option<f64>) -> Color32 {
    let scale = &tokens().temperature;
    let Some(c) = celsius.filter(|c| c.is_finite()) else {
        return scale.no_sensor;
    };
    scale
        .stops
        .iter()
        .min_by(|a, b| (a.at - c).abs().total_cmp(&(b.at - c).abs()))
        .map_or(scale.no_sensor, |s| s.color)
}

/// Aplat transparent d'une pièce : la teinte du palier posée sous le texte. Une pièce sans
/// capteur reste en gris opaque.
pub fn temperature_fill(celsius: Option<f64>) -> Color32 {
    let scale = &tokens().temperature;
    let color = temperature_color(celsius);
    if celsius.is_some_and(f64::is_finite) {
        color.gamma_multiply(scale.fill_alpha)
    } else {
        color
    }
}

// ---------------------------------------------------------------------------
// Composants
// ---------------------------------------------------------------------------

/// Gravité d'une pastille d'état.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Error,
}

impl Severity {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Severity::Ok => p.ok,
            Severity::Warn => p.warn,
            Severity::Error => p.error,
        }
    }
}

/// Onglet de la barre du haut : plein (accent) quand il est choisi, sinon seulement du texte.
pub fn tab(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let p = palette(ui);
    let (fill, color) = if selected {
        (p.accent, p.on_accent)
    } else {
        (Color32::TRANSPARENT, p.text)
    };
    ui.add(
        egui::Button::new(RichText::new(label).color(color))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(radius(tokens().radius.md)),
    )
}

/// Bouton principal : l'action la plus importante de l'écran (« + Ajouter »).
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    let p = palette(ui);
    ui.add(
        egui::Button::new(RichText::new(label).color(p.on_accent).strong())
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .corner_radius(radius(tokens().radius.md)),
    )
}

/// En-tête de pièce : petites capitales discrètes, sans cadre. `note` : à droite, en gris.
pub fn room_header(ui: &mut Ui, name: &str, note: Option<&str>) {
    let p = palette(ui);
    ui.add_space(tokens().space.md);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(name.to_uppercase())
                .size(tokens().text.caption)
                .color(p.text_secondary),
        );
        if let Some(note) = note {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(note)
                        .size(tokens().text.caption)
                        .color(p.text_secondary),
                );
            });
        }
    });
    ui.add_space(tokens().space.xs / 2.0);
}

/// Pastille d'état anormal : un point de couleur et un mot (« pile faible », « hors ligne »).
pub fn status_pill(ui: &mut Ui, severity: Severity, text: &str) -> Response {
    let p = palette(ui);
    let color = severity.color(p);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = tokens().space.xs;
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, color);
        ui.label(RichText::new(text).size(tokens().text.caption).color(color));
    })
    .response
}

/// Ce que montre une ligne d'appareil.
pub struct DeviceRow<'a> {
    pub name: &'a str,
    /// La seule valeur de la ligne (« 19,8 °C »).
    pub value: Option<&'a str>,
    /// Anomalie à signaler, jamais l'état normal.
    pub badge: Option<(Severity, &'a str)>,
    pub selected: bool,
    /// Gris : élément secondaire (un pont).
    pub dim: bool,
}

/// Ligne d'appareil : le nom à gauche, la pastille éventuelle, puis la valeur à droite.
/// Un clic ouvre le panneau de détail.
pub fn device_row(ui: &mut Ui, row: &DeviceRow<'_>) -> Response {
    let t = tokens();
    let p = palette(ui);
    let height = 30.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let fill = if row.selected {
        p.accent_soft
    } else if response.hovered() {
        p.surface_alt
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, radius(t.radius.md), fill);

    let inner = rect.shrink2(Vec2::new(t.space.sm, 0.0));
    let (name_color, value_color) = if row.dim {
        (p.text_secondary, p.text_secondary)
    } else {
        (p.text, p.text)
    };
    let mut right = inner.right();
    if let Some(value) = row.value {
        let size = if row.dim { t.text.body } else { t.text.title };
        let galley =
            painter.layout_no_wrap(value.to_owned(), FontId::proportional(size), value_color);
        right -= galley.size().x;
        painter.galley(
            egui::pos2(right, inner.center().y - galley.size().y / 2.0),
            galley,
            value_color,
        );
        right -= t.space.md;
    }
    if let Some((severity, text)) = row.badge {
        let color = severity.color(p);
        let galley =
            painter.layout_no_wrap(text.to_owned(), FontId::proportional(t.text.caption), color);
        right -= galley.size().x;
        painter.galley(
            egui::pos2(right, inner.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        right -= 5.0 + 7.0;
        painter.circle_filled(egui::pos2(right + 3.5, inner.center().y), 3.5, color);
        right -= t.space.md;
    }
    // Le nom prend ce qui reste, et se coupe par « … » s'il est trop long.
    let mut job = LayoutJob::single_section(
        row.name.to_owned(),
        TextFormat::simple(FontId::proportional(t.text.body + 1.0), name_color),
    );
    job.wrap.max_width = (right - inner.left()).max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(inner.left(), inner.center().y - galley.size().y / 2.0),
        galley,
        name_color,
    );
    response
}

/// Cadre du panneau de détail : une surface, un trait fin à gauche.
pub fn detail_frame(ui: &Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.line))
        .inner_margin(Margin::same(tokens().space.lg as i8))
}

/// Une ligne « clé : valeur » du panneau de détail, à mettre dans une `egui::Grid`.
pub fn detail_row(ui: &mut Ui, key: &str, value: impl Into<String>) {
    let p = palette(ui);
    ui.label(RichText::new(key).color(p.text_secondary));
    ui.label(value.into());
    ui.end_row();
}

/// Vignette : une image (ici un aplat) et, dessous, le nom et la pièce en légende.
pub fn thumbnail(ui: &mut Ui, width: f32, fill: Color32, inside: &str, name: &str, caption: &str) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * 9.0 / 16.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, radius(tokens().radius.md), fill);
    if !inside.is_empty() {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            inside,
            FontId::proportional(tokens().text.caption),
            p.text_secondary,
        );
    }
    ui.horizontal(|ui| {
        ui.label(name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(caption)
                    .size(tokens().text.caption)
                    .color(p.text_secondary),
            );
        });
    });
}

/// Pour les tests et les appels qui veulent le thème par nom.
pub fn palette_of(theme: Theme) -> &'static Palette {
    tokens().palette(theme == Theme::Dark)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_parse() {
        let t = Tokens::parse(TOKENS_JSON).expect("le JSON doit se lire");
        assert_ne!(t.colors.light.bg, t.colors.dark.bg);
        assert_ne!(t.colors.light.text, t.colors.dark.text);
        assert!(t.space.xs < t.space.sm && t.space.sm < t.space.md && t.space.md < t.space.lg);
        assert!(t.text.caption < t.text.body && t.text.body < t.text.display);
        // Le même fichier sert au thème de l'application.
        assert_eq!(tokens().palette(true).accent, t.colors.dark.accent);
    }

    #[test]
    fn temperature_scale_is_continuous() {
        let scale = &Tokens::parse(TOKENS_JSON).unwrap().temperature;
        assert_eq!(scale.stops.len(), 5);
        assert_eq!(scale.stops.first().unwrap().at, 16.0);
        assert_eq!(scale.stops.last().unwrap().at, 24.0);
        for pair in scale.stops.windows(2) {
            assert_eq!(
                pair[1].at - pair[0].at,
                scale.step,
                "palier manquant entre {} et {}",
                pair[0].at,
                pair[1].at
            );
            assert_ne!(pair[0].color, pair[1].color);
        }
        assert!(scale.fill_alpha > 0.0 && scale.fill_alpha < 1.0);
    }

    #[test]
    fn temperature_color_picks_the_nearest_step() {
        let scale = &tokens().temperature;
        assert_eq!(temperature_color(Some(10.0)), scale.stops[0].color);
        assert_eq!(temperature_color(Some(17.5)), scale.stops[1].color);
        assert_eq!(temperature_color(Some(19.8)), scale.stops[2].color);
        assert_eq!(temperature_color(Some(30.0)), scale.stops[4].color);
        assert_eq!(temperature_color(None), scale.no_sensor);
        assert_eq!(temperature_color(Some(f64::NAN)), scale.no_sensor);
        assert_eq!(temperature_fill(None), scale.no_sensor);
        assert!(temperature_fill(Some(20.0)).a() < 255);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#0b6a94"), Some(Color32::from_rgb(11, 106, 148)));
        assert_eq!(parse_hex("0b6a94"), None);
        assert_eq!(parse_hex("#0b6a9"), None);
        assert_eq!(parse_hex("#zz6a94"), None);
    }

    #[test]
    fn both_themes_build_without_shadow() {
        for dark in [false, true] {
            let v = visuals(dark);
            assert_eq!(v.dark_mode, dark);
            assert_eq!(v.window_shadow, egui::Shadow::NONE);
            assert_eq!(v.panel_fill, tokens().palette(dark).bg);
        }
    }
}
