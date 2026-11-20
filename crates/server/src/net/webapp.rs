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
        "temps.js",
        "text/javascript; charset=utf-8",
        "../../web/temps.js"
    ),
    text!(
        "cameras.js",
        "text/javascript; charset=utf-8",
        "../../web/cameras.js"
    ),
    text!(
        "events.js",
        "text/javascript; charset=utf-8",
        "../../web/events.js"
    ),
    text!(
        "plan.js",
        "text/javascript; charset=utf-8",
        "../../web/plan.js"
    ),
    text!(
        "mode.js",
        "text/javascript; charset=utf-8",
        "../../web/mode.js"
    ),
    text!(
        "push.js",
        "text/javascript; charset=utf-8",
        "../../web/push.js"
    ),
    text!(
        "widgets.js",
        "text/javascript; charset=utf-8",
        "../../web/widgets.js"
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

/// Un fichier à télécharger depuis la page « Widgets et raccourcis » (sous `/app/raccourcis/`).
/// Servi sans jeton comme la coquille : aucun secret (l'adresse et le jeton sont des questions
/// posées à l'import sur l'iPhone). Jamais mis en cache par le service worker, pour qu'une
/// nouvelle version signée remplace l'ancienne.
struct Download {
    /// Nom dans l'adresse (ASCII).
    path: &'static str,
    content_type: &'static str,
    /// Nom du fichier enregistré, accents compris, déjà codé pour `filename*=UTF-8''…`.
    /// Le raccourci importé porte le nom du fichier : le widget Scriptable l'appelle par son nom.
    filename: Option<&'static str>,
    body: &'static [u8],
}

static DOWNLOADS: &[Download] = &[
    Download {
        path: "mode-absent.shortcut",
        content_type: "application/octet-stream",
        filename: Some("Mode%20absent.shortcut"),
        body: include_bytes!("../../web/raccourcis/mode-absent.shortcut"),
    },
    Download {
        path: "mode-present.shortcut",
        content_type: "application/octet-stream",
        filename: Some("Mode%20pr%C3%A9sent.shortcut"),
        body: include_bytes!("../../web/raccourcis/mode-present.shortcut"),
    },
    // Le script Scriptable : un texte que la page lit par `fetch` et copie, jamais exécuté.
    Download {
        path: "matter-home.js",
        content_type: "text/plain; charset=utf-8",
        filename: None,
        body: include_bytes!("../../web/raccourcis/matter-home.js"),
    },
];

/// Politique de sécurité : tout vient de la même origine, aucun script ni style en ligne, aucune
/// ressource externe (les images des caméras sont dessinées dans un canvas : pas de `blob:` en
/// `img-src`). Seule exception : `media-src blob:`, le clip d'un événement de détection (ticket
/// n° 18), chargé par `fetch` avec le jeton puis lu par un `<video>` depuis une URL locale.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self'; media-src blob:; connect-src 'self'; manifest-src 'self'; \
worker-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub fn router() -> Router<super::AppState> {
    Router::new()
        .route("/app", get(|| async { Redirect::permanent("/app/") }))
        .route("/app/", get(|| async { serve("index.html") }))
        .route(
            "/app/raccourcis/{name}",
            get(|Path(name): Path<String>| async move { serve_download(&name) }),
        )
        .route(
            "/app/{name}",
            get(|Path(name): Path<String>| async move { serve(&name) }),
        )
}

fn not_found() -> Response {
    super::routes::ApiErr(protocol::wire::ApiError::new(
        protocol::wire::ErrorCode::NotFound,
    ))
    .into_response()
}

fn serve_download(name: &str) -> Response {
    let Some(file) = DOWNLOADS.iter().find(|d| d.path == name) else {
        return not_found();
    };
    let mut response = (StatusCode::OK, file.body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(file.content_type),
    );
    if let Some(filename) = file.filename {
        let value = format!("attachment; filename*=UTF-8''{filename}");
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&value).expect("nom de fichier en ASCII"),
        );
    }
    security_headers(headers);
    response
}

fn serve(name: &str) -> Response {
    let Some(asset) = ASSETS.iter().find(|a| a.name == name) else {
        return not_found();
    };
    let mut response = (StatusCode::OK, asset.body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    security_headers(headers);
    if asset.name == "sw.js" {
        // Le service worker contrôle tout `/app/`, jamais plus large.
        headers.insert(
            HeaderName::from_static("service-worker-allowed"),
            HeaderValue::from_static("/app/"),
        );
    }
    response
}

fn security_headers(headers: &mut axum::http::HeaderMap) {
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
    fn service_worker_caches_every_embedded_shell_file() {
        let sw =
            std::str::from_utf8(ASSETS.iter().find(|a| a.name == "sw.js").unwrap().body).unwrap();
        for a in ASSETS.iter().filter(|a| a.name != "sw.js") {
            let listed = if a.name == "index.html" {
                "\"index.html\""
            } else {
                a.name
            };
            assert!(
                sw.contains(listed),
                "{} absent du cache du service worker",
                a.name
            );
        }
    }

    #[test]
    fn downloads_are_ascii_paths_with_the_exact_accented_file_names() {
        let names: Vec<_> = DOWNLOADS.iter().map(|d| (d.path, d.filename)).collect();
        assert!(names.contains(&(
            "mode-present.shortcut",
            Some("Mode%20pr%C3%A9sent.shortcut")
        )));
        for d in DOWNLOADS {
            assert!(d.path.is_ascii() && !d.path.contains('/') && !d.path.contains(".."));
            assert!(!d.body.is_empty());
        }
    }

    #[test]
    fn asset_names_are_plain_files() {
        for name in ASSETS.iter().map(|a| a.name) {
            assert!(!name.contains('/') && !name.contains(".."));
        }
    }
}
