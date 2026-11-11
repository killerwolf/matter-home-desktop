//! `design/tokens.json` → `design/tokens.css` : les variables CSS de la future app web
//! (ticket n° 15). Le fichier CSS est commité ; le test `committed_css_is_up_to_date` vérifie
//! qu'il correspond au JSON. Pour le régénérer :
//!
//! ```text
//! cargo run -p matter-home-desktop --example tokens_css
//! ```
//!
//! Préfixes : `--color-*`, `--temp-*`, `--space-*`, `--radius-*`, `--font-*`, `--size-*`. Le
//! thème sombre est dans `@media (prefers-color-scheme: dark)`. `meta` est ignoré.

use serde_json::{Map, Value};

/// Le fichier de tokens et le CSS commité, embarqués.
pub const TOKENS_JSON: &str = include_str!("../../../design/tokens.json");
pub const TOKENS_CSS: &str = include_str!("../../../design/tokens.css");

fn kebab(name: &str) -> String {
    name.replace('_', "-")
}

fn obj<'a>(v: &'a Value, key: &str) -> Result<&'a Map<String, Value>, String> {
    v.get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("« {key} » absent ou invalide"))
}

fn text(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(format!("valeur inattendue : {other}")),
    }
}

/// Lignes `--prefix-nom: valeur;` d'un groupe de nombres, avec une unité.
fn numbers(out: &mut Vec<String>, group: &Map<String, Value>, prefix: &str, unit: &str) {
    for (k, v) in group {
        if let Ok(t) = text(v) {
            // Une couleur ou une opacité n'a pas d'unité.
            let unit = if v.is_number() && !k.ends_with("alpha") {
                unit
            } else {
                ""
            };
            out.push(format!("--{prefix}-{}: {t}{unit};", kebab(k)));
        }
    }
}

fn colors(out: &mut Vec<String>, palette: &Map<String, Value>) -> Result<(), String> {
    for (k, v) in palette {
        out.push(format!("--color-{}: {};", kebab(k), text(v)?));
    }
    Ok(())
}

fn block(selector: &str, lines: &[String], indent: &str) -> String {
    let mut s = format!("{indent}{selector} {{\n");
    for l in lines {
        s.push_str(&format!("{indent}  {l}\n"));
    }
    s.push_str(&format!("{indent}}}\n"));
    s
}

/// Le CSS d'un fichier de tokens.
pub fn generate(json: &str) -> Result<String, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let all_colors = obj(&root, "colors")?;
    let light = all_colors
        .get("light")
        .and_then(Value::as_object)
        .ok_or("colors.light absent")?;
    let dark = all_colors
        .get("dark")
        .and_then(Value::as_object)
        .ok_or("colors.dark absent")?;

    let mut common: Vec<String> = Vec::new();
    colors(&mut common, light)?;

    let temperature = obj(&root, "temperature")?;
    for stop in temperature
        .get("stops")
        .and_then(Value::as_array)
        .ok_or("temperature.stops absent")?
    {
        let at = stop.get("at").map(text).ok_or("palier sans « at »")??;
        let color = stop
            .get("color")
            .map(text)
            .ok_or("palier sans « color »")??;
        common.push(format!("--temp-{at}: {color};"));
    }
    common.push(format!(
        "--temp-fill-alpha: {};",
        text(temperature.get("fill_alpha").ok_or("fill_alpha absent")?)?
    ));

    numbers(&mut common, obj(&root, "space")?, "space", "px");
    numbers(&mut common, obj(&root, "radius")?, "radius", "px");
    numbers(&mut common, obj(&root, "size")?, "size", "px");
    numbers(&mut common, obj(&root, "tile")?, "tile", "px");

    let font = obj(&root, "font")?;
    common.push(format!(
        "--font-family: {};",
        text(font.get("family").ok_or("font.family absent")?)?
    ));
    common.push(format!(
        "--font-line-height: {};",
        text(font.get("line_height").ok_or("font.line_height absent")?)?
    ));
    let weights = font
        .get("weight")
        .and_then(Value::as_object)
        .ok_or("font.weight absent")?;
    numbers(&mut common, weights, "font-weight", "");
    let sizes = font
        .get("size")
        .and_then(Value::as_object)
        .ok_or("font.size absent")?;
    numbers(&mut common, sizes, "font-size", "px");

    // Un état désigne une couleur : il suit le thème sans être répété.
    for (k, v) in obj(&root, "state")? {
        common.push(format!(
            "--color-state-{}: var(--color-{});",
            kebab(k),
            kebab(&text(v)?)
        ));
    }

    let mut dark_lines = Vec::new();
    colors(&mut dark_lines, dark)?;

    let mut css = String::from(
        "/* Généré depuis design/tokens.json : ne pas modifier à la main.\n   \
         Régénérer : cargo run -p matter-home-desktop --example tokens_css */\n",
    );
    css.push_str(&block(":root", &common, ""));
    css.push_str("@media (prefers-color-scheme: dark) {\n");
    css.push_str(&block(":root", &dark_lines, "  "));
    css.push_str("}\n");
    Ok(css)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_css_is_up_to_date() {
        let fresh = generate(TOKENS_JSON).expect("le JSON doit se convertir");
        assert_eq!(
            TOKENS_CSS, fresh,
            "design/tokens.css n'est plus à jour : cargo run -p matter-home-desktop --example tokens_css"
        );
    }

    #[test]
    fn css_has_the_documented_prefixes_and_a_dark_media_query() {
        let css = generate(TOKENS_JSON).unwrap();
        for prefix in [
            "--color-bg:",
            "--temp-16:",
            "--temp-24:",
            "--space-md:",
            "--radius-lg:",
            "--font-size-body:",
            "--font-family:",
            "--size-row:",
            "--tile-width: 180px;",
            "--tile-on-tint: #f2b84b;",
            "--tile-on-tint-alpha: 0.16;",
            "--color-state-hover: var(--color-surface-alt);",
        ] {
            assert!(css.contains(prefix), "{prefix} manque");
        }
        let (light, dark) = css
            .split_once("@media (prefers-color-scheme: dark)")
            .unwrap();
        assert!(light.contains("--color-bg: #f6f5f2;"));
        assert!(dark.contains("--color-bg: #1b1b1b;"));
        assert!(!css.contains("meta"));
    }
}
