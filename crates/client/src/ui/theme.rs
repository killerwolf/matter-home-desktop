//! Thème du client : couleurs, espacements, rayons, tailles et polices lus dans
//! `design/tokens.json` (la même source que la future app web, ticket n° 15, qui lit le
//! `design/tokens.css` généré), et petits composants réutilisables (voir
//! `docs/design-system.md`).
//!
//! Rien de coûteux : des aplats, des traits fins, ni ombre, ni flou, ni texture, ni animation.

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

/// Les couleurs d'un thème : un champ par entrée de `colors.light` et `colors.dark`.
macro_rules! palette {
    ($($field:ident),* $(,)?) => {
        #[derive(Debug, Clone, Copy, Deserialize)]
        pub struct Palette {
            $( #[serde(deserialize_with = "hex")] pub $field: Color32, )*
        }

        impl Palette {
            /// Une couleur par son nom dans le fichier de tokens.
            pub fn named(&self, name: &str) -> Option<Color32> {
                match name {
                    $( stringify!($field) => Some(self.$field), )*
                    _ => None,
                }
            }
        }
    };
}

palette!(
    bg,
    surface,
    surface_alt,
    line,
    text,
    text_secondary,
    accent,
    accent_soft,
    accent_text,
    on_accent,
    ok,
    warn,
    on_warn,
    error,
    banner_warn,
    banner_info,
    no_sensor,
    series_temperature,
    series_humidity,
    series_power,
    series_cover,
    series_1,
    series_2,
    series_3,
    series_4,
    series_5,
    series_6,
    series_7,
    series_8,
);

/// Nombre de couleurs de courbe dans les tokens (`series_1` à `series_8`).
pub const SERIES_LEN: usize = 8;

impl Palette {
    /// Couleur de courbe numéro `index` ; au-delà des tokens, des teintes réparties par l'angle
    /// d'or (luminosité adaptée au thème).
    pub fn series(&self, index: usize, dark: bool) -> Color32 {
        let fixed = [
            self.series_1,
            self.series_2,
            self.series_3,
            self.series_4,
            self.series_5,
            self.series_6,
            self.series_7,
            self.series_8,
        ];
        match fixed.get(index) {
            Some(c) => *c,
            None => {
                let hue = (index as f32 * 0.618_034).fract();
                let value = if dark { 0.85 } else { 0.65 };
                egui::ecolor::Hsva::new(hue, 0.6, value, 1.0).into()
            }
        }
    }
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
    #[allow(dead_code)] // lu par le test de continuité
    pub step: f64,
    pub fill_alpha: f32,
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
    #[allow(dead_code)] // pour la web app
    pub sm: u8,
    pub md: u8,
    pub lg: u8,
}

/// Dimensions des composants.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Size {
    /// Hauteur d'une ligne d'appareil.
    pub row: f32,
    /// Diamètre d'un point d'état.
    pub dot: f32,
    /// Hauteur minimale d'un contrôle.
    pub control: f32,
    pub item_gap_y: f32,
    pub button_pad_x: f32,
    pub button_pad_y: f32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct FontSizes {
    pub caption: f32,
    pub body: f32,
    pub title: f32,
    pub heading: f32,
    #[allow(dead_code)] // pour la web app
    pub display: f32,
}

/// Police : egui n'embarque pas de famille choisie ni de graisse variable, ces trois champs
/// servent à la web app (`design/tokens.css`).
#[derive(Debug, Clone, Deserialize)]
pub struct Font {
    #[allow(dead_code)]
    pub family: String,
    #[allow(dead_code)]
    pub weight: std::collections::BTreeMap<String, u32>,
    #[allow(dead_code)]
    pub line_height: f32,
    pub size: FontSizes,
}

/// États d'un élément interactif : le nom d'une couleur de `colors`.
#[derive(Debug, Clone, Deserialize)]
pub struct State {
    pub hover: String,
    pub selected: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub colors: Colors,
    pub temperature: TemperatureScale,
    pub space: Space,
    pub radius: Radius,
    pub size: Size,
    pub font: Font,
    pub state: State,
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

    /// Fond d'une ligne survolée.
    pub fn hover(&self, p: &Palette) -> Color32 {
        p.named(&self.state.hover).unwrap_or(p.surface_alt)
    }

    /// Fond d'une ligne ouverte.
    pub fn selected(&self, p: &Palette) -> Color32 {
        p.named(&self.state.selected).unwrap_or(p.accent_soft)
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
    // MATTER_HOME_THEME=light ou dark : pour les essais et les captures ; sinon, le système.
    let preference = match std::env::var("MATTER_HOME_THEME").as_deref() {
        Ok("light") => egui::ThemePreference::Light,
        Ok("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    };
    ctx.set_theme(preference);
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
    let size = t.font.size;
    for (style, font) in [
        (TextStyle::Small, FontId::proportional(size.caption)),
        (TextStyle::Body, FontId::proportional(size.body)),
        (TextStyle::Button, FontId::proportional(size.body)),
        (TextStyle::Heading, FontId::proportional(size.heading)),
        (TextStyle::Monospace, FontId::monospace(size.body)),
    ] {
        s.text_styles.insert(style, font);
    }
    s.spacing.item_spacing = Vec2::new(t.space.sm, t.size.item_gap_y);
    s.spacing.button_padding = Vec2::new(t.size.button_pad_x, t.size.button_pad_y);
    s.spacing.interact_size.y = t.size.control;
    s.spacing.window_margin = Margin::same(t.space.md as i8);
    s.animation_time = 0.0;
}

// ---------------------------------------------------------------------------
// Échelle de température
// ---------------------------------------------------------------------------

/// Teinte du palier le plus proche de `celsius` (à égalité, le plus froid). Sans mesure :
/// le gris « pas de capteur » du thème.
#[allow(dead_code)] // le plan s'en servira (phase 2)
pub fn temperature_color(p: &Palette, celsius: Option<f64>) -> Color32 {
    let Some(c) = celsius.filter(|c| c.is_finite()) else {
        return p.no_sensor;
    };
    tokens()
        .temperature
        .stops
        .iter()
        .min_by(|a, b| (a.at - c).abs().total_cmp(&(b.at - c).abs()))
        .map_or(p.no_sensor, |s| s.color)
}

/// Aplat transparent d'une pièce : la teinte du palier posée sous le texte. Une pièce sans
/// capteur reste en gris opaque.
#[allow(dead_code)] // le plan s'en servira (phase 2)
pub fn temperature_fill(p: &Palette, celsius: Option<f64>) -> Color32 {
    let color = temperature_color(p, celsius);
    if celsius.is_some_and(f64::is_finite) {
        color.gamma_multiply(tokens().temperature.fill_alpha)
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
    Warn,
    Error,
}

impl Severity {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
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
    let t = tokens();
    let p = palette(ui);
    ui.add_space(t.space.md);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(name.to_uppercase())
                .size(t.font.size.caption)
                .color(p.text_secondary),
        );
        if let Some(note) = note {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(note)
                        .size(t.font.size.caption)
                        .color(p.text_secondary),
                );
            });
        }
    });
    ui.add_space(t.space.xs);
}

/// Pastille d'état anormal : un point de couleur et un mot (« pile faible », « injoignable »).
#[allow(dead_code)] // la ligne d'appareil dessine la sienne ; les vignettes s'en serviront
pub fn status_pill(ui: &mut Ui, severity: Severity, text: &str) -> Response {
    let t = tokens();
    let color = severity.color(palette(ui));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.xs;
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(t.size.dot), Sense::hover());
        ui.painter()
            .circle_filled(dot.center(), t.size.dot / 2.0, color);
        ui.label(RichText::new(text).size(t.font.size.caption).color(color));
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
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), t.size.row), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let fill = if row.selected {
        t.selected(p)
    } else if response.hovered() {
        t.hover(p)
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
        let size = if row.dim {
            t.font.size.body
        } else {
            t.font.size.title
        };
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
        let galley = painter.layout_no_wrap(
            text.to_owned(),
            FontId::proportional(t.font.size.caption),
            color,
        );
        right -= galley.size().x;
        painter.galley(
            egui::pos2(right, inner.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        right -= t.space.xs + t.size.dot;
        painter.circle_filled(
            egui::pos2(right + t.size.dot / 2.0, inner.center().y),
            t.size.dot / 2.0,
            color,
        );
        right -= t.space.md;
    }
    // Le nom prend ce qui reste, et se coupe par « … » s'il est trop long.
    let mut job = LayoutJob::single_section(
        row.name.to_owned(),
        TextFormat::simple(FontId::proportional(t.font.size.body), name_color),
    );
    job.wrap.max_width = (right - inner.left()).max(0.0);
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

/// Cadre du panneau de détail : une surface, un trait fin.
pub fn detail_frame(ui: &Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.line))
        .inner_margin(Margin::same(tokens().space.lg as i8))
}

/// Gravité d'un bandeau.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Banner {
    /// Quelque chose n'a pas pu être enregistré.
    Warn,
    /// Un choix attend l'utilisateur.
    Info,
}

/// Cadre d'un bandeau d'avertissement : aplat teinté, texte du thème (lisible en clair comme en
/// sombre).
pub fn banner_frame(ui: &Ui, kind: Banner) -> egui::Frame {
    let p = palette(ui);
    let fill = match kind {
        Banner::Warn => p.banner_warn,
        Banner::Info => p.banner_info,
    };
    egui::Frame::group(ui.style()).fill(fill)
}

/// Une ligne « clé : valeur » du panneau de détail, à mettre dans une `egui::Grid`.
pub fn detail_row(ui: &mut Ui, key: &str, value: impl Into<String>) {
    let p = palette(ui);
    ui.label(RichText::new(key).color(p.text_secondary));
    ui.label(value.into());
    ui.end_row();
}

/// Vignette : une image (ici un aplat) et, dessous, le nom et la pièce en légende.
#[allow(dead_code)] // les caméras s'en serviront (phase 2)
pub fn thumbnail(ui: &mut Ui, width: f32, fill: Color32, inside: &str, name: &str, caption: &str) {
    let t = tokens();
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * 9.0 / 16.0), Sense::hover());
    ui.painter().rect_filled(rect, radius(t.radius.md), fill);
    if !inside.is_empty() {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            inside,
            FontId::proportional(t.font.size.caption),
            p.text_secondary,
        );
    }
    ui.horizontal(|ui| {
        ui.label(name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(caption)
                    .size(t.font.size.caption)
                    .color(p.text_secondary),
            );
        });
    });
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
        let s = t.font.size;
        assert!(s.caption < s.body && s.body < s.title && s.title < s.heading);
        // Le même fichier sert au thème de l'application.
        assert_eq!(tokens().palette(true).accent, t.colors.dark.accent);
        // Les états désignent de vraies couleurs.
        for dark in [false, true] {
            let p = t.palette(dark);
            assert!(p.named(&t.state.hover).is_some());
            assert!(p.named(&t.state.selected).is_some());
        }
        assert_eq!(t.palette(true).named("nope"), None);
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
        let t = tokens();
        let stops = &t.temperature.stops;
        for dark in [false, true] {
            let p = t.palette(dark);
            assert_eq!(temperature_color(p, Some(10.0)), stops[0].color);
            assert_eq!(temperature_color(p, Some(17.5)), stops[1].color);
            assert_eq!(temperature_color(p, Some(19.8)), stops[2].color);
            assert_eq!(temperature_color(p, Some(30.0)), stops[4].color);
            // Sans capteur : le gris du thème, différent entre clair et sombre.
            assert_eq!(temperature_color(p, None), p.no_sensor);
            assert_eq!(temperature_color(p, Some(f64::NAN)), p.no_sensor);
            assert_eq!(temperature_fill(p, None), p.no_sensor);
            assert!(temperature_fill(p, Some(20.0)).a() < 255);
        }
        assert_ne!(t.colors.light.no_sensor, t.colors.dark.no_sensor);
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

    /// Contraste WCAG entre deux couleurs.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_is_readable_on_every_background_in_both_themes() {
        for dark in [false, true] {
            let p = tokens().palette(dark);
            for (fg, bg, name) in [
                (p.text, p.bg, "texte sur fond"),
                (p.text, p.surface, "texte sur surface"),
                (p.text, p.banner_warn, "texte sur bandeau d'alerte"),
                (p.text, p.banner_info, "texte sur bandeau d'information"),
                (p.on_accent, p.accent, "texte sur accent"),
                (p.on_warn, p.warn, "texte sur alerte (DÉCONNECTÉ)"),
                (p.text_secondary, p.surface, "texte secondaire"),
                (p.warn, p.surface, "pastille d'alerte"),
                (p.error, p.surface, "pastille d'erreur"),
            ] {
                let c = contrast(fg, bg);
                assert!(c >= 4.4, "{name} ({}) : contraste {c:.1}", dark_name(dark));
            }
        }
    }

    fn dark_name(dark: bool) -> &'static str {
        if dark {
            "sombre"
        } else {
            "clair"
        }
    }
}
