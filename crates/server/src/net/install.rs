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
//!
//! Les raccourcis portent le jeton de mode : un lien de Safari ne peut pas poser d'en-tête
//! `Authorization`, ils sont donc servis par une adresse **imprévisible** (128 bits), gardée en
//! mémoire seulement, valable 5 minutes, et remplacée par la préparation suivante. Elle n'est pas à
//! usage unique : l'app installée sur l'écran d'accueil peut confier le téléchargement à Safari,
//! qui redemande le fichier ; un lien mort à ce moment-là obligerait à tout recommencer (et à
//! changer encore de jeton). Le jeton ne sert qu'à changer le mode, et l'adresse expire vite.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{ApiErr, ApiJson};
use super::AppState;
use crate::shortcuts::{self, ModeShortcut};

/// Durée de vie des liens de téléchargement.
pub const LINK_TTL: Duration = Duration::from_secs(5 * 60);
/// Préfixe des liens (le service worker n'intercepte rien sous `/app/raccourcis/`).
pub const LINK_PREFIX: &str = "/app/raccourcis/perso/";

/// Les raccourcis préparés, en mémoire seulement.
#[derive(Default)]
pub struct Downloads {
    files: Mutex<HashMap<String, Prepared>>,
}

struct Prepared {
    file_name: String,
    body: Vec<u8>,
    expires: Instant,
}

impl Downloads {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Prepared>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remplace les liens d'avant (leur jeton vient d'être révoqué) par ceux-ci ; rend leurs ids.
    fn replace(&self, files: Vec<(String, Vec<u8>)>) -> Vec<String> {
        let mut map = self.lock();
        map.clear();
        let expires = Instant::now() + LINK_TTL;
        files
            .into_iter()
            .map(|(file_name, body)| {
                let mut raw = [0u8; 16];
                rand::thread_rng().fill_bytes(&mut raw);
                let id = hex::encode(raw);
                map.insert(
                    id.clone(),
                    Prepared {
                        file_name,
                        body,
                        expires,
                    },
                );
                id
            })
            .collect()
    }

    /// Oublie tous les liens (jeton de mode révoqué).
    fn clear(&self) {
        self.lock().clear();
    }

    fn get(&self, id: &str, file_name: &str) -> Option<Vec<u8>> {
        let mut map = self.lock();
        let now = Instant::now();
        map.retain(|_, p| p.expires > now);
        map.get(id)
            .filter(|p| p.file_name == file_name)
            .map(|p| p.body.clone())
    }
}

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
    state.downloads.clear();
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
/// l'ancien qu'une fois les deux signatures réussies : un échec ne casse rien de ce qui marche.
async fn mode_shortcuts(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<ShortcutsResponse>, ApiErr> {
    let address = shortcuts::public_address(&request.origin)?;
    let (hub, signer) = (state.hub.clone(), state.signer);
    let (_token, files) = blocking(move || {
        hub.generate_mode_token_with(|token| {
            ModeShortcut::ALL
                .iter()
                .map(|&which| {
                    let plist = shortcuts::mode_shortcut_plist(which, &address, token);
                    Ok((which.file_name(), signer.sign(&plist)?))
                })
                .collect::<Result<Vec<_>, ApiError>>()
        })
    })
    .await?;
    let names: Vec<_> = files.iter().map(|(name, _)| name.clone()).collect();
    let ids = state.downloads.replace(files);
    log::info!("nouveau jeton de mode et raccourcis préparés depuis l'app web");
    let shortcuts = ModeShortcut::ALL
        .iter()
        .zip(ids.iter().zip(names.iter()))
        .map(|(which, (id, file))| ShortcutLink {
            name: which.name(),
            url: format!("{LINK_PREFIX}{id}/{}", file.replace(' ', "%20")),
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
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<WidgetResponse>, ApiErr> {
    let address = shortcuts::public_address(&request.origin)?;
    let hub = state.hub.clone();
    let (_token, script) = blocking(move || {
        hub.generate_read_token_with(|token| Ok(shortcuts::widget_script(&address, token)))
    })
    .await?;
    log::info!("nouveau jeton de lecture et script du widget préparés depuis l'app web");
    Ok(Json(WidgetResponse { script }))
}

async fn download(
    State(state): State<AppState>,
    method: Method,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let Some(body) = state.downloads.get(&id, &file) else {
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

    #[test]
    fn links_are_unguessable_replaced_and_checked_by_name() {
        let downloads = Downloads::default();
        let first = downloads.replace(vec![("Mode absent.shortcut".into(), b"a".to_vec())]);
        assert_eq!(first[0].len(), 32);
        assert_eq!(
            downloads.get(&first[0], "Mode absent.shortcut").unwrap(),
            b"a"
        );
        // Réutilisable pendant sa durée de vie, mais seulement sous son nom.
        assert!(downloads.get(&first[0], "Mode absent.shortcut").is_some());
        assert!(downloads.get(&first[0], "Mode present.shortcut").is_none());
        let second = downloads.replace(vec![
            ("Mode absent.shortcut".into(), b"b".to_vec()),
            ("Mode present.shortcut".into(), b"c".to_vec()),
        ]);
        assert_ne!(first[0], second[0]);
        assert!(downloads.get(&first[0], "Mode absent.shortcut").is_none());
        assert_eq!(
            downloads.get(&second[1], "Mode present.shortcut").unwrap(),
            b"c"
        );
        downloads.clear();
        assert!(downloads.get(&second[1], "Mode present.shortcut").is_none());
    }

    #[test]
    fn expired_links_are_gone() {
        let downloads = Downloads::default();
        let ids = downloads.replace(vec![("x".into(), b"x".to_vec())]);
        downloads.lock().get_mut(&ids[0]).unwrap().expires = Instant::now();
        assert!(downloads.get(&ids[0], "x").is_none());
        assert!(downloads.lock().is_empty());
    }
}
