//! L'app web mobile (PWA, ticket n° 15) : des fichiers statiques **intégrés au binaire**, servis
//! sous `/app/`. Aucune donnée de la maison ne passe ici : c'est la coquille (HTML, CSS, JS,
//! icônes), accessible sans jeton. Tout le reste de l'app passe par l'API, qui exige le jeton.
//!
//! Sources : `crates/server/web/` et `design/tokens.css` (la même feuille que le client lit
//! par `tokens.json`, jamais recopiée).

use axum::extract::Path;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;

struct Asset {
    name: &'static str,
    content_type: &'static str,
    body: &'static [u8],
}

macro_rules! text {
    ($name:literal, $ct:literal, $path:literal) => {
        Asset {
            name: $name,
            content_type: $ct,
            body: include_str!($path).as_bytes(),
        }
    };
}

/// Les fichiers de la coquille. `index.html` est servi aussi sur `/app/`.
static ASSETS: &[Asset] = &[
    text!(
        "index.html",
        "text/html; charset=utf-8",
        "../../web/index.html"
    ),
    text!("app.css", "text/css; charset=utf-8", "../../web/app.css"),
    text!(
        "tokens.css",
        "text/css; charset=utf-8",
        "../../../../design/tokens.css"
    ),
    text!(
        "app.js",
        "text/javascript; charset=utf-8",
        "../../web/app.js"
    ),
    text!(
        "api.js",
        "text/javascript; charset=utf-8",
        "../../web/api.js"
    ),
    text!(
        "tiles.js",
        "text/javascript; charset=utf-8",
        "../../web/tiles.js"
    ),
    text!(
        "icons.js",
        "text/javascript; charset=utf-8",
        "../../web/icons.js"
    ),
    text!(
        "charts.js",
        "text/javascript; charset=utf-8",
        "../../web/charts.js"
    ),
    text!(
        "cameras.js",
        "text/javascript; charset=utf-8",
        "../../web/cameras.js"
    ),
    text!("sw.js", "text/javascript; charset=utf-8", "../../web/sw.js"),
    text!(
        "manifest.webmanifest",
        "application/manifest+json",
        "../../web/manifest.webmanifest"
    ),
    Asset {
        name: "icon-192.png",
        content_type: "image/png",
        body: include_bytes!("../../web/icon-192.png"),
    },
    Asset {
        name: "icon-512.png",
        content_type: "image/png",
        body: include_bytes!("../../web/icon-512.png"),
    },
];

/// Politique de sécurité : tout vient de la même origine, aucun script ni style en ligne, aucune
/// ressource externe. `blob:` pour les vignettes de caméra dessinées dans un canvas.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self' data: blob:; connect-src 'self'; media-src 'self' blob:; manifest-src 'self'; \
worker-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub fn router() -> Router<super::AppState> {
    Router::new()
        .route("/app", get(|| async { Redirect::permanent("/app/") }))
        .route("/app/", get(|| async { serve("index.html") }))
        .route(
            "/app/{name}",
            get(|Path(name): Path<String>| async move { serve(&name) }),
        )
}

fn serve(name: &str) -> Response {
    let Some(asset) = ASSETS.iter().find(|a| a.name == name) else {
        return super::routes::ApiErr(protocol::wire::ApiError::new(
            protocol::wire::ErrorCode::NotFound,
        ))
        .into_response();
    };
    let mut response = (StatusCode::OK, asset.body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if asset.name == "sw.js" {
        // Le service worker contrôle tout `/app/`, jamais plus large.
        headers.insert(
            HeaderName::from_static("service-worker-allowed"),
            HeaderValue::from_static("/app/"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tokens_match_the_design_system() {
        let on_disk = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../design/tokens.css"
        ))
        .unwrap();
        let embedded = ASSETS.iter().find(|a| a.name == "tokens.css").unwrap();
        assert_eq!(embedded.body, on_disk.as_bytes());
    }

    #[test]
    fn shell_has_no_inline_script_or_style_and_no_external_url() {
        let html = std::str::from_utf8(ASSETS[0].body).unwrap();
        assert!(!html.contains("style="), "style en ligne");
        for (i, _) in html.match_indices("<script") {
            let tag = &html[i..html[i..].find('>').unwrap() + i];
            assert!(tag.contains("src="), "script en ligne : {tag}");
        }
        assert!(!html.contains("http://") && !html.contains("https://"));
        for a in ASSETS.iter().filter(|a| a.name.ends_with(".js")) {
            let js = std::str::from_utf8(a.body).unwrap();
            assert!(!js.contains("eval("), "{} utilise eval", a.name);
            assert!(!js.contains("localStorage.setItem('password"), "{}", a.name);
        }
    }

    #[test]
    fn asset_names_are_plain_files() {
        for name in ASSETS.iter().map(|a| a.name) {
            assert!(!name.contains('/') && !name.contains(".."));
        }
    }
}
