//! Thème du client : couleurs, espacements, rayons, tailles et polices lus dans
//! `design/tokens.json` (la même source que la future app web, ticket n° 15, qui lit le
//! `design/tokens.css` généré), et petits composants réutilisables (voir
//! `docs/design-system.md`).
//!
//! Rien de coûteux : des aplats, des traits fins, ni ombre, ni flou, ni texture, ni animation.

use std::sync::OnceLock;

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Stroke, TextStyle,
    Theme, Ui, Vec2,
};
use serde::{Deserialize, Deserializer};

/// Le fichier de tokens, embarqué au build.
const TOKENS_JSON: &str = include_str!("../../../design/tokens.json");

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
    text_faint,
    accent,
    accent_hover,
    accent_soft,
    accent_text,
    on_accent,
    lueur,
    on_lueur,
    lueur_text,
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
    video_bg,
    video_text,
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
    /// Cartes et bandeaux.
    pub card: u8,
}

/// Dimensions des composants.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Size {
    /// Hauteur d'une ligne d'appareil (la web app ; le client dessine des tuiles).
    #[allow(dead_code)]
    pub row: f32,
    /// Diamètre d'un point d'état.
    pub dot: f32,
    /// Hauteur minimale d'un contrôle.
    pub control: f32,
    pub item_gap_y: f32,
    pub button_pad_x: f32,
    pub button_pad_y: f32,
    /// Interrupteur : largeur et hauteur.
    pub switch_w: f32,
    pub switch_h: f32,
    /// Hauteur de la légende sous une vignette.
    pub caption_row: f32,
    /// Panneau de détail d'un appareil (client) : largeur de départ, bornes du redimensionnement.
    pub detail_panel: f32,
    pub detail_panel_min: f32,
    pub detail_panel_max: f32,
    /// Sous cette largeur de fenêtre, le détail remplace la grille au lieu de se mettre à côté.
    pub detail_narrow: f32,
    /// Fenêtres à taille fixe : app de bureau et fenêtre du serveur.
    pub window_desktop_w: f32,
    pub window_desktop_h: f32,
    pub window_server_w: f32,
    pub window_server_h: f32,
}

/// Une tuile d'appareil (écran Appareils).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TileTokens {
    /// Largeur d'une tuile.
    pub width: f32,
    /// Largeur d'une tuile dans l'app de bureau (6 colonnes de 192 et 16 d'écart : 1232 px).
    pub desktop_width: f32,
    /// Largeur minimale (la web app, qui étire ses tuiles ; le client garde `width`).
    #[allow(dead_code)]
    pub min_width: f32,
    pub height: f32,
    pub radius: u8,
    pub icon: f32,
    /// Part de `lueur` (palette) dans le fond d'une tuile allumée ; l'icône est en `lueur`.
    pub on_tint_alpha: f32,
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
    pub tile: TileTokens,
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
    // OSTAL_THEME=light ou dark : pour les essais et les captures ; sinon, le système.
    let preference = match std::env::var("OSTAL_THEME").as_deref() {
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
    v.extreme_bg_color = if dark { p.surface_alt } else { p.surface };
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

/// Bouton principal sur toute la largeur (écran de connexion), grisé quand `enabled` est faux.
pub fn primary_button_wide(ui: &mut Ui, label: &str, enabled: bool, width: f32) -> Response {
    let p = palette(ui);
    let fill = if enabled { p.accent } else { p.surface_alt };
    let color = if enabled {
        p.on_accent
    } else {
        p.text_secondary
    };
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).color(color).strong())
            .fill(fill)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(width, tokens().size.control + 6.0))
            .corner_radius(radius(tokens().radius.md)),
    )
}

/// En-tête de pièce : son nom, l'étage en discret et, quand elle a un capteur joignable, une
/// pastille de température moyenne teintée comme sur le plan (`temperature_fill`).
pub fn room_heading(ui: &mut Ui, name: &str, floor: Option<&str>, average: Option<f64>) {
    let t = tokens();
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(name)
                .size(t.font.size.title)
                .strong()
                .color(p.text),
        );
        if let Some(floor) = floor {
            ui.label(
                RichText::new(floor)
                    .size(t.font.size.caption)
                    .color(p.text_secondary),
            );
        }
        if let Some(celsius) = average {
            temperature_pill(ui, celsius);
        }
    });
}

/// Pastille « 21,5 °C » : le fond est la teinte du palier de l'échelle de température.
pub fn temperature_pill(ui: &mut Ui, celsius: f64) -> Response {
    let t = tokens();
    let p = palette(ui);
    let text = format!("{celsius:.1} °C").replace('.', ",");
    let galley = ui.painter().layout_no_wrap(
        text,
        FontId::proportional(t.font.size.caption + 1.0),
        p.text,
    );
    let size = galley.size() + Vec2::new(t.space.md, t.space.xs);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let r = radius(t.radius.lg);
    ui.painter().rect_filled(rect, r, p.surface);
    ui.painter()
        .rect_filled(rect, r, temperature_fill(p, Some(celsius)));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, p.text);
    response
}

/// Pastille d'état anormal : un point de couleur et un mot (« pile faible », « injoignable »).
pub fn status_pill(ui: &mut Ui, severity: Severity, text: &str) -> Response {
    let t = tokens();
    let color = severity.color(palette(ui));
    // De gauche à droite, même dans une mise en page de droite à gauche : le point, puis le mot.
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = t.space.xs;
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(t.size.dot), Sense::hover());
        ui.painter()
            .circle_filled(dot.center(), t.size.dot / 2.0, color);
        ui.label(RichText::new(text).size(t.font.size.caption).color(color));
    })
    .response
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

/// Ce que montre une vignette.
pub struct Thumb<'a> {
    /// Fond de l'image (un fond vidéo sombre, voulu dans les deux thèmes).
    pub fill: Color32,
    /// Texte centré dans l'image quand elle n'a rien à montrer (« Hors ligne depuis 14:02 »).
    pub inside: &'a str,
    pub inside_color: Color32,
    pub name: &'a str,
    /// La pièce, en légende à droite.
    pub caption: &'a str,
    /// Anomalie à signaler (injoignable), jamais l'état normal.
    pub badge: Option<(Severity, &'a str)>,
    /// Nom grisé (caméra hors ligne).
    pub dim: bool,
}

/// Vignette : une image 16/9 et, dessous, le nom, la pièce en légende et l'éventuelle anomalie.
/// Rend le rectangle de l'image : le lecteur vidéo s'y dessine.
pub fn thumbnail(ui: &mut Ui, width: f32, thumb: &Thumb<'_>) -> egui::Rect {
    let t = tokens();
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width * 9.0 / 16.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, radius(t.radius.md), thumb.fill);
    if !thumb.inside.is_empty() {
        // Le texte passe à la ligne dans la largeur de l'image.
        let galley = ui.painter().layout(
            thumb.inside.to_owned(),
            FontId::proportional(t.font.size.body),
            thumb.inside_color,
            rect.width() - 2.0 * t.space.md,
        );
        ui.painter().galley(
            rect.center() - galley.size() / 2.0,
            galley,
            thumb.inside_color,
        );
    }
    ui.allocate_ui_with_layout(
        Vec2::new(width, t.size.caption_row),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let name_color = if thumb.dim { p.text_secondary } else { p.text };
            ui.label(RichText::new(thumb.name).color(name_color));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(thumb.caption)
                        .size(t.font.size.caption)
                        .color(p.text_secondary),
                );
                if let Some((severity, text)) = thumb.badge {
                    status_pill(ui, severity, text);
                }
            });
        },
    );
    rect
}

/// Interrupteur : une piste et un rond, sans animation. Annoncé comme une case à cocher
/// (`label`), avec un contour au focus clavier et au survol. La piste éteinte et la piste
/// allumée contrastent au moins 3:1 avec la surface (testé).
pub fn toggle_switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = tokens();
    let p = palette(ui);
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(t.size.switch_w, t.size.switch_h), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let checked = *on;
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, checked, label));
    let track = if *on { p.ok } else { p.text_secondary };
    let h = t.size.switch_h;
    ui.painter().rect_filled(rect, h / 2.0, track);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            h / 2.0 + 2.0,
            Stroke::new(2.0, p.accent_text),
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            h / 2.0 + 1.0,
            Stroke::new(1.0, p.text_secondary),
            egui::StrokeKind::Outside,
        );
    }
    let knob = h / 2.0 - 2.0;
    let x = if *on {
        rect.right() - h / 2.0
    } else {
        rect.left() + h / 2.0
    };
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), knob, p.surface);
    response
}

// ---------------------------------------------------------------------------
// Composants de la charte « Nuit et Lueur » (refonte)
// ---------------------------------------------------------------------------

/// Couleurs du glyphe Seuil : une marque, identique en clair et en sombre (kit.md §3).
const LOGO_NIGHT: Color32 = Color32::from_rgb(0x10, 0x23, 0x2d);
const LOGO_ARCH: Color32 = Color32::from_rgb(0xf3, 0xef, 0xe6);
const LOGO_LUEUR: Color32 = Color32::from_rgb(0xf5, 0xb3, 0x3f);

/// Le glyphe Seuil, dessiné au peintre d'après le viewBox 1024 du kit : un carré arrondi, une
/// arche et un point Lueur. Pas de SVG : aucune crate de rendu n'est embarquée.
pub fn logo(ui: &mut Ui, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Ostal"));
    let k = size / 1024.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    let painter = ui.painter();
    let square = egui::Rect::from_min_max(at(100.0, 100.0), at(924.0, 924.0));
    painter.rect_filled(
        square,
        CornerRadius::same((185.0 * k).round().clamp(0.0, 255.0) as u8),
        LOGO_NIGHT,
    );
    // Arche : montant gauche, demi-cercle de rayon 180 centré en (512, 470), montant droit.
    let mut points = vec![at(332.0, 760.0)];
    for i in 0..=24 {
        let a = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
        points.push(at(512.0 + 180.0 * a.cos(), 470.0 + 180.0 * a.sin()));
    }
    points.push(at(692.0, 760.0));
    let stroke = Stroke::new(64.0 * k, LOGO_ARCH);
    painter.add(egui::Shape::line(points.clone(), stroke));
    // Bouts ronds.
    for end in [points[0], points[points.len() - 1]] {
        painter.circle_filled(end, 32.0 * k, LOGO_ARCH);
    }
    painter.circle_filled(at(512.0, 668.0), 58.0 * k, LOGO_LUEUR);
    response
}

/// Logotype : le glyphe de 28 px suivi de « Ostal » en gras (Outfit n'est pas embarquée : la
/// police d'egui).
pub fn logotype(ui: &mut Ui) {
    let t = tokens();
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.sm;
        logo(ui, 28.0);
        ui.label(
            RichText::new("Ostal")
                .size(t.font.size.heading)
                .strong()
                .color(p.text),
        );
    });
}

/// Bouton secondaire : fond `surface_alt`, trait fin (« Copier », « Annuler »).
pub fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    let p = palette(ui);
    ui.add(
        egui::Button::new(RichText::new(label).color(p.text).strong())
            .fill(p.surface_alt)
            .stroke(Stroke::new(1.0, p.line))
            .corner_radius(radius(tokens().radius.md)),
    )
}

/// Bouton destructif : bordure et texte `error` sur fond de surface (« Supprimer »).
pub fn destructive_button(ui: &mut Ui, label: &str) -> Response {
    let p = palette(ui);
    ui.add(
        egui::Button::new(RichText::new(label).color(p.error).strong())
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.error))
            .corner_radius(radius(tokens().radius.md)),
    )
}

/// Cadre d'une carte : surface, trait `line` de 1 px, rayon de `radius.card`.
pub fn card_frame(ui: &Ui) -> egui::Frame {
    let t = tokens();
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(radius(t.radius.card))
        .inner_margin(Margin::same(t.space.md as i8))
}

/// Largeur du trait d'erreur à gauche d'un bandeau d'erreur.
const ERROR_BAR: f32 = 4.0;

/// Cadre d'un bandeau d'erreur : surface et trait fin ; la bordure gauche de 4 px en `error`
/// se peint avec [`error_banner`] (un `Frame` n'a pas de bordure d'un seul côté). La marge
/// gauche laisse la place du trait.
pub fn error_banner_frame(ui: &Ui) -> egui::Frame {
    let t = tokens();
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(radius(t.radius.lg))
        .inner_margin(Margin {
            left: (t.space.md + ERROR_BAR) as i8,
            right: t.space.md as i8,
            top: t.space.sm as i8,
            bottom: t.space.sm as i8,
        })
}

/// Un bandeau d'erreur complet : le cadre, puis la bordure gauche de 4 px en `error`.
pub fn error_banner<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let t = tokens();
    let error = palette(ui).error;
    let out = error_banner_frame(ui).show(ui, add_contents);
    let r = out.response.rect;
    let bar = egui::Rect::from_min_size(r.min, Vec2::new(ERROR_BAR, r.height()));
    let corner = t.radius.lg;
    ui.painter().rect_filled(
        bar,
        CornerRadius {
            nw: corner,
            sw: corner,
            ne: 0,
            se: 0,
        },
        error,
    );
    out
}

/// Ce que dit un mot d'état (kit.md §10) : la couleur ne porte jamais seule l'information, le
/// mot et l'icône la doublent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateWord {
    /// Joignable, réussi, activé : coche verte.
    Ok,
    /// Allumé : point Lueur, texte en `lueur_text`.
    Lit,
    /// Neutre, désactivé : anneau gris.
    Neutral,
    /// À regarder (injoignable) : cercle barré, `warn`.
    Watch,
    /// Erreur, échec : croix dans un cercle, `error`.
    Error,
}

impl StateWord {
    /// Couleur du mot et de l'icône.
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            StateWord::Ok => p.ok,
            StateWord::Lit => p.lueur_text,
            StateWord::Neutral => p.text_secondary,
            StateWord::Watch => p.warn,
            StateWord::Error => p.error,
        }
    }
}

/// Mot d'état : une icône de 14 px puis le mot, de gauche à droite. `status_pill` reste pour
/// l'existant.
pub fn state_word(ui: &mut Ui, state: StateWord, text: &str) -> Response {
    let t = tokens();
    let p = palette(ui);
    let color = state.color(p);
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
        let c = rect.center();
        let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * (14.0 / 24.0);
        let painter = ui.painter();
        match state {
            StateWord::Ok => {
                painter.add(egui::Shape::line(
                    vec![at(5.0, 12.5), at(9.5, 17.0), at(19.0, 7.5)],
                    Stroke::new(1.8, color),
                ));
            }
            StateWord::Lit => {
                painter.circle_filled(c, t.size.dot / 2.0, p.lueur);
            }
            StateWord::Neutral => {
                painter.circle_stroke(c, t.size.dot / 2.0 - 0.75, Stroke::new(1.5, color));
            }
            StateWord::Watch => {
                painter.circle_stroke(c, 5.2, Stroke::new(1.5, color));
                painter.line_segment([at(5.6, 18.4), at(18.4, 5.6)], Stroke::new(1.5, color));
            }
            StateWord::Error => {
                painter.circle_stroke(c, 5.2, Stroke::new(1.5, color));
                painter.line_segment([at(9.0, 9.0), at(15.0, 15.0)], Stroke::new(1.5, color));
                painter.line_segment([at(15.0, 9.0), at(9.0, 15.0)], Stroke::new(1.5, color));
            }
        }
        let mut word = RichText::new(text).size(t.font.size.body).color(color);
        if state != StateWord::Neutral {
            word = word.strong();
        }
        ui.label(word);
    })
    .response
}

/// Pastille du mode de la maison : Présent en Lueur ; les autres modes en `accent_soft` avec une
/// bordure `accent`. `present` dit si c'est le mode Présent (le thème ne connaît pas `protocol`).
pub fn mode_pill(ui: &mut Ui, label: &str, present: bool) -> Response {
    let t = tokens();
    let p = palette(ui);
    let (fill, text, stroke) = if present {
        (p.lueur, p.on_lueur, Stroke::NONE)
    } else {
        (p.accent_soft, p.accent_text, Stroke::new(1.5, p.accent))
    };
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(t.font.size.body)
                .strong()
                .color(text),
        )
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(255)),
    )
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
                (p.on_lueur, p.lueur, "texte sur lueur (mode Présent)"),
                (p.lueur_text, p.surface, "lueur en texte"),
                (p.lueur_text, p.bg, "lueur en texte sur fond"),
            ] {
                let c = contrast(fg, bg);
                assert!(c >= 4.5, "{name} ({}) : contraste {c:.1}", dark_name(dark));
            }
        }
    }

    /// Couleur d'un aplat transparent posé sur `under`.
    fn over(under: Color32, tint: Color32, alpha: f32) -> Color32 {
        let mix =
            |a: u8, b: u8| (f32::from(a) * (1.0 - alpha) + f32::from(b) * alpha).round() as u8;
        Color32::from_rgb(
            mix(under.r(), tint.r()),
            mix(under.g(), tint.g()),
            mix(under.b(), tint.b()),
        )
    }

    #[test]
    fn the_switch_tracks_and_the_video_text_have_enough_contrast() {
        for dark in [false, true] {
            let p = tokens().palette(dark);
            for (track, what) in [(p.ok, "allumée"), (p.text_secondary, "éteinte")] {
                assert!(
                    contrast(track, p.surface) >= 3.0,
                    "piste {what} ({})",
                    dark_name(dark)
                );
                assert!(contrast(p.surface, track) >= 3.0, "rond sur piste {what}");
            }
            assert!(contrast(p.video_text, p.video_bg) >= 4.5, "texte vidéo");
        }
    }

    #[test]
    fn labels_on_tinted_rooms_stay_readable() {
        let t = tokens();
        for dark in [false, true] {
            let p = t.palette(dark);
            // Le texte directement sur la pièce : nom et température de la pièce.
            let mut grounds: Vec<(Color32, String)> = t
                .temperature
                .stops
                .iter()
                .map(|s| {
                    (
                        over(p.surface, s.color, t.temperature.fill_alpha),
                        format!("palier {}", s.at),
                    )
                })
                .collect();
            grounds.push((p.no_sensor, "sans capteur".into()));
            for (ground, what) in grounds {
                let c = contrast(p.text, ground);
                assert!(c >= 4.5, "texte sur {what} ({}) : {c:.1}", dark_name(dark));
            }
            // Les petites étiquettes (valeurs, « ouvert », « injoignable ») sont sur une
            // pastille `surface`.
            for (fg, what) in [
                (p.text_secondary, "valeur"),
                (p.warn, "ouvert"),
                (p.error, "injoignable"),
                (p.text, "nom"),
            ] {
                let c = contrast(fg, p.surface);
                assert!(
                    c >= 4.5,
                    "{what} sur pastille ({}) : {c:.1}",
                    dark_name(dark)
                );
            }
        }
        // Sans capteur est plus léger qu'une pièce teintée, en clair.
        let p = t.palette(false);
        let lum = |c: Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
        for s in &t.temperature.stops {
            assert!(
                lum(p.no_sensor) > lum(over(p.surface, s.color, t.temperature.fill_alpha)) - 30
            );
        }
    }

    /// Palette « Nuit et Lueur » : une tuile allumée reste lisible, son icône se voit, et Lueur
    /// (allumé) ne se confond pas avec l'alerte (`warn`).
    #[test]
    fn lit_tiles_stay_readable_and_lueur_is_not_a_warning() {
        let t = tokens();
        for dark in [false, true] {
            let p = t.palette(dark);
            let lit = over(p.surface, p.lueur, t.tile.on_tint_alpha);
            for (fg, what) in [(p.text, "nom"), (p.text_secondary, "valeur secondaire")] {
                let c = contrast(fg, lit);
                assert!(
                    c >= 4.5,
                    "{what} sur tuile allumée ({}) : {c:.1}",
                    dark_name(dark)
                );
            }
            // L'icône allumée est en lueur sur le fond teinté : en clair, une ampoule jaune sur
            // fond très pâle reste sous 3:1 (comme l'ancien jaune) ; on vérifie qu'elle se
            // distingue au moins du fond.
            let icon = contrast(p.lueur, lit);
            assert!(
                icon >= 1.5,
                "icône allumée ({}) : {icon:.1}",
                dark_name(dark)
            );
            // Deux teintes bien distinctes : l'écart de teinte entre lueur et warn.
            let hue = |c: Color32| egui::ecolor::Hsva::from(c).h * 360.0;
            let gap = (hue(p.lueur) - hue(p.warn)).abs();
            assert!(
                gap >= 12.0,
                "lueur et warn trop proches ({}) : {gap:.0}°",
                dark_name(dark)
            );
        }
    }

    /// Charte « Nuit et Lueur » : les ajouts de la refonte.
    #[test]
    fn refonte_tokens_are_there() {
        let t = tokens();
        assert_eq!(t.radius.card, 10);
        assert_eq!(t.tile.desktop_width, 192.0);
        assert_eq!(t.tile.width, 180.0);
        assert_eq!(
            (t.size.window_desktop_w, t.size.window_desktop_h),
            (1280.0, 800.0)
        );
        assert_eq!(
            (t.size.window_server_w, t.size.window_server_h),
            (640.0, 700.0)
        );
        assert_eq!(parse_hex("#9aa8ae"), Some(t.palette(false).text_faint));
        assert_eq!(parse_hex("#08506f"), Some(t.palette(false).accent_hover));
        // Six colonnes de tuiles et leur écart tiennent dans la fenêtre de bureau.
        assert!(6.0 * t.tile.desktop_width + 5.0 * t.space.lg <= t.size.window_desktop_w);
        for dark in [false, true] {
            let p = t.palette(dark);
            // text_faint : ⋯ et texte désactivé, jamais du texte à lire. En clair, #9aa8ae est
            // la valeur de la maquette (2,4:1 sur surface, un contrôle désactivé étant exempté
            // par WCAG) ; en sombre, on tient 3:1.
            let floor = if dark { 3.0 } else { 2.0 };
            assert!(
                contrast(p.text_faint, p.surface) >= floor,
                "text_faint ({}) : {:.1}",
                dark_name(dark),
                contrast(p.text_faint, p.surface)
            );
            assert!(
                contrast(p.on_accent, p.accent_hover) >= 4.5,
                "accent survolé"
            );
        }
    }

    #[test]
    fn state_words_are_readable_and_distinct() {
        for dark in [false, true] {
            let p = tokens().palette(dark);
            for state in [
                StateWord::Ok,
                StateWord::Lit,
                StateWord::Neutral,
                StateWord::Watch,
                StateWord::Error,
            ] {
                let c = contrast(state.color(p), p.surface);
                assert!(c >= 4.5, "{state:?} ({}) : {c:.1}", dark_name(dark));
            }
        }
    }

    /// Les composants de la refonte se dessinent sans plantage, dans les deux thèmes.
    #[test]
    fn refonte_components_draw() {
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals(dark));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                logo(ui, 32.0);
                logotype(ui);
                secondary_button(ui, "Copier");
                destructive_button(ui, "Supprimer");
                card_frame(ui).show(ui, |ui| ui.label("carte"));
                error_banner(ui, |ui| ui.label("erreur"));
                for s in [
                    StateWord::Ok,
                    StateWord::Lit,
                    StateWord::Neutral,
                    StateWord::Watch,
                    StateWord::Error,
                ] {
                    state_word(ui, s, "mot");
                }
                mode_pill(ui, "Présent", true);
                mode_pill(ui, "Nuit", false);
            });
            out.textures_delta.clear();
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
