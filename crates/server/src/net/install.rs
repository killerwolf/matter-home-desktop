//! Installation sur l'iPhone depuis l'app web (page « Widgets et raccourcis »), sans rien taper :
//!
//! | route | effet |
//! |---|---|
//! | `GET /api/tokens` | l'état des jetons de mode et de lecture (existe, date de création) |
//! | `POST /api/tokens/mode/shortcuts` `{"origin"}` | nouveau jeton de mode, deux raccourcis signés à télécharger |
//! | `POST /api/tokens/read/widget` `{"origin"}` | nouveau jeton de lecture, script du widget prérempli |
//! | `DELETE /api/tokens/mode`, `DELETE /api/tokens/read` | révoque le jeton (`204`) |
//! | `GET /app/raccourcis/perso/{id}/{fichier}` | un raccourci préparé, **sans jeton** (lien à toucher) |
//!
//! Toutes les routes `/api/tokens…` exigent une session (jamais un jeton de mode ou de lecture).
//! Une préparation à la fois par jeton : une autre pendant ce temps répond `409 busy` tout de suite,
//! sans faire la queue.
//!
//! Les raccourcis portent le jeton de mode : un lien de Safari ne peut pas poser d'en-tête
//! `Authorization`, ils sont donc servis par une adresse **imprévisible** (128 bits), gardée en
//! mémoire seulement (dans le `Hub`), valable 5 minutes, et remplacée par la préparation suivante.
//! Elle n'est pas à usage unique : l'app installée sur l'écran d'accueil peut confier le
//! téléchargement à Safari, qui redemande le fichier ; un lien mort à ce moment-là obligerait à
//! tout recommencer (et à changer encore de jeton). Le jeton ne sert qu'à changer le mode, et
//! l'adresse expire vite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{ApiErr, ApiJson};
use super::AppState;
use crate::shortcuts::{self, ModeShortcut, LINK_TTL};

/// Préfixe des liens (le service worker n'intercepte rien sous `/app/raccourcis/`).
pub const LINK_PREFIX: &str = "/app/raccourcis/perso/";

/// Routes sous session (à fusionner dans le routeur protégé).
pub fn protected() -> Router<AppState> {
    Router::new()
        .route("/api/tokens", get(status))
        .route("/api/tokens/mode", delete(revoke_mode))
        .route("/api/tokens/read", delete(revoke_read))
        .route("/api/tokens/mode/shortcuts", post(mode_shortcuts))
        .route("/api/tokens/read/widget", post(widget))
}

/// Route publique des téléchargements.
pub fn public() -> Router<AppState> {
    Router::new().route("/app/raccourcis/perso/{id}/{file}", get(download))
}

#[derive(Serialize)]
struct TokenState {
    exists: bool,
    /// Secondes Unix.
    created: Option<f64>,
}

#[derive(Serialize)]
struct TokensResponse {
    mode: TokenState,
    read: TokenState,
}

async fn status(State(state): State<AppState>) -> Json<TokensResponse> {
    let hub = &state.hub;
    Json(TokensResponse {
        mode: TokenState {
            exists: hub.has_mode_token(),
            created: hub.mode_token_created(),
        },
        read: TokenState {
            exists: hub.has_read_token(),
            created: hub.read_token_created(),
        },
    })
}

async fn revoke_mode(State(state): State<AppState>) -> Result<StatusCode, ApiErr> {
    let hub = state.hub.clone();
    blocking(move || hub.revoke_mode_token()).await?;
    log::info!("jeton de mode révoqué depuis l'app web");
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_read(State(state): State<AppState>) -> Result<StatusCode, ApiErr> {
    let hub = state.hub.clone();
    blocking(move || hub.revoke_read_token()).await?;
    log::info!("jeton de lecture révoqué depuis l'app web");
    Ok(StatusCode::NO_CONTENT)
}

/// L'adresse de la page, `location.origin` : celle que l'iPhone sait joindre.
#[derive(Deserialize)]
struct OriginRequest {
    origin: String,
}

/// L'adresse publique à écrire dans les fichiers : celle de la page (`origin`), en .ts.net
/// (`shortcuts::public_address`), et **celle de ce Mac** :
/// - le navigateur envoie `Origin` avec un `POST` : elle doit être la même que `origin` ;
/// - `tailscale serve` transmet l'en-tête `Host` d'origine (le nom en .ts.net), comme le prouve le
///   contrôle `Origin`/`Host` des WebSocket, qui passe derrière lui. `X-Forwarded-Host` est lu
///   aussi, au cas où. Si l'un des deux est un nom en .ts.net, l'adresse doit être ce nom-là ;
///   sinon (relais qui réécrit `Host`, par exemple en `localhost:8443`), on garde l'adresse validée.
fn this_mac(headers: &HeaderMap, origin: &str) -> Result<String, ApiError> {
    let address = shortcuts::public_address(origin)?;
    let refuse = || {
        ApiError::with_detail(
            ErrorCode::BadRequest,
            "Cette adresse n'est pas celle de ce Mac : ouvre l'app par son adresse en .ts.net.",
        )
    };
    if let Some(sent) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if shortcuts::public_address(sent).ok().as_deref() != Some(address.as_str()) {
            return Err(refuse());
        }
    }
    let host = address.trim_start_matches("https://");
    let ts_names: Vec<String> = [headers.get(header::HOST), headers.get("x-forwarded-host")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(',').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .map(|v| v.strip_suffix(":443").map(str::to_string).unwrap_or(v))
        .filter(|v| v.ends_with(".ts.net"))
        .collect();
    if !ts_names.is_empty() && !ts_names.iter().any(|name| name == host) {
        return Err(refuse());
    }
    Ok(address)
}

/// Tant que le gestionnaire de la requête vit, la préparation est voulue. Si la requête est
/// abandonnée (client parti, délai de 120 s), axum lâche le gestionnaire : le drapeau tombe, et la
/// préparation, qui continue dans son thread, ne remplace pas le jeton (personne n'aurait les
/// fichiers).
struct StillWanted(Arc<AtomicBool>);

impl StillWanted {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    fn probe(&self) -> impl Fn() -> bool + Send + 'static {
        let flag = self.0.clone();
        move || flag.load(Ordering::SeqCst)
    }
}

impl Drop for StillWanted {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
struct ShortcutLink {
    /// Nom du raccourci, tel que le widget l'appelle.
    name: &'static str,
    /// Lien relatif à l'origine, à toucher.
    url: String,
}

#[derive(Serialize)]
struct ShortcutsResponse {
    shortcuts: Vec<ShortcutLink>,
    expires_in_s: u64,
}

/// Nouveau jeton de mode et les deux raccourcis qui le portent, signés. Le jeton ne remplace
/// l'ancien qu'une fois les deux signatures réussies, et si la requête attend toujours : un échec
/// ou un abandon ne casse rien de ce qui marche.
async fn mode_shortcuts(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<ShortcutsResponse>, ApiErr> {
    let address = this_mac(&headers, &request.origin)?;
    let (hub, signer) = (state.hub.clone(), state.signer);
    let wanted = StillWanted::new();
    let still_wanted = wanted.probe();
    let ids = blocking(move || {
        hub.prepare_mode_shortcuts(
            |token| {
                ModeShortcut::ALL
                    .iter()
                    .map(|&which| {
                        let plist = shortcuts::mode_shortcut_plist(which, &address, token);
                        Ok((which.file_name(), signer.sign(&plist)?))
                    })
                    .collect()
            },
            still_wanted,
        )
    })
    .await?;
    drop(wanted);
    log::info!("nouveau jeton de mode et raccourcis préparés depuis l'app web");
    let shortcuts = ModeShortcut::ALL
        .iter()
        .zip(ids)
        .map(|(which, id)| ShortcutLink {
            name: which.name(),
            url: format!(
                "{LINK_PREFIX}{id}/{}",
                which.file_name().replace(' ', "%20")
            ),
        })
        .collect();
    Ok(Json(ShortcutsResponse {
        shortcuts,
        expires_in_s: LINK_TTL.as_secs(),
    }))
}

#[derive(Serialize)]
struct WidgetResponse {
    script: String,
}

/// Nouveau jeton de lecture et le script du widget qui le porte (à copier dans Scriptable).
async fn widget(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<WidgetResponse>, ApiErr> {
    let address = this_mac(&headers, &request.origin)?;
    let hub = state.hub.clone();
    let wanted = StillWanted::new();
    let still_wanted = wanted.probe();
    let script = blocking(move || {
        hub.prepare_widget(
            |token| Ok(shortcuts::widget_script(&address, token)),
            still_wanted,
        )
    })
    .await?;
    drop(wanted);
    log::info!("nouveau jeton de lecture et script du widget préparés depuis l'app web");
    Ok(Json(WidgetResponse { script }))
}

async fn download(
    State(state): State<AppState>,
    method: Method,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let Some(body) = state.hub.shortcut_download(&id, &file) else {
        return ApiErr(ApiError::with_detail(
            ErrorCode::NotFound,
            "lien expiré : recommencez depuis l'app web",
        ))
        .into_response();
    };
    let mut response = if method == Method::HEAD {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::OK, body).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // Le nom est l'un des deux noms fixes, en ASCII (voir `shortcuts`).
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    super::webapp::security_headers(headers);
    response
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiErr> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(ApiErr),
        Err(_) => Err(ApiErr(ApiError::new(ErrorCode::Internal))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn the_address_must_be_this_macs_when_the_request_says_so() {
        let ok = "https://maison.tailnet-1.ts.net";
        // `tailscale serve` : Host est le nom en .ts.net, Origin la page.
        let h = headers(&[("host", "maison.tailnet-1.ts.net"), ("origin", ok)]);
        assert_eq!(this_mac(&h, ok).unwrap(), ok);
        assert_eq!(
            this_mac(&headers(&[("host", "Maison.tailnet-1.ts.net:443")]), ok).unwrap(),
            ok
        );
        // Un autre Mac du tailnet, par Host ou par X-Forwarded-Host : refusé.
        for h in [
            headers(&[("host", "autre.tailnet-1.ts.net")]),
            headers(&[
                ("host", "localhost:8443"),
                ("x-forwarded-host", "autre.tailnet-1.ts.net"),
            ]),
            headers(&[
                ("host", "maison.tailnet-1.ts.net"),
                ("origin", "https://autre.tailnet-1.ts.net"),
            ]),
        ] {
            let e = this_mac(&h, ok).unwrap_err();
            assert!(e.detail.unwrap().contains("ce Mac"));
        }
        // Un relais qui réécrit Host (pas de nom en .ts.net) : l'adresse validée est gardée.
        assert_eq!(
            this_mac(&headers(&[("host", "localhost:8443")]), ok).unwrap(),
            ok
        );
        // L'adresse elle-même reste contrôlée.
        assert!(this_mac(
            &headers(&[("host", "192.168.1.20:8443")]),
            "https://192.168.1.20:8443"
        )
        .is_err());
    }

    #[test]
    fn a_dropped_handler_means_the_preparation_is_no_longer_wanted() {
        let wanted = StillWanted::new();
        let probe = wanted.probe();
        assert!(probe());
        drop(wanted);
        assert!(!probe());
    }
}
