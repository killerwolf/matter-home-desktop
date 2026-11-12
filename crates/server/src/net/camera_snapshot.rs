//! `GET /api/cameras/{id}/snapshot` : une image fixe récente d'une caméra, en JPEG (ticket n° 12).
//!
//! Sous le même jeton que le reste de l'API. La capture, le cache et les limites sont dans
//! `cameras/snapshot.rs`. Erreurs (corps `{"error": {"code": …}}`, jamais d'adresse de caméra) :
//!
//! | statut | code | quand |
//! |---|---|---|
//! | 400 | `bad_request` | identifiant invalide |
//! | 404 | `not_found` (avec `detail`) | caméra inconnue. Sans `detail` : serveur trop ancien |
//! | 502 | `device_unreachable` | pas d'image clé à temps, caméra éteinte ou muette |
//! | 503 | `unavailable` (+ `retry_after_s`) | test ou vérification en cours, codec non décodé |

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use protocol::wire::{self, ApiError, ErrorCode};

use super::routes::ApiErr;
use super::AppState;
use crate::cameras::live::LiveRegistry;
use crate::cameras::snapshot::{SnapshotError, SnapshotService};

pub async fn snapshot(
    State(state): State<AppState>,
    Extension(live): Extension<Arc<LiveRegistry>>,
    Extension(service): Extension<Arc<SnapshotService>>,
    Path(id): Path<String>,
) -> Response {
    let Some(id) = id.parse::<u64>().ok().filter(|id| wire::is_camera_id(*id)) else {
        return ApiErr(ApiError::with_detail(
            ErrorCode::BadRequest,
            "identifiant de caméra invalide",
        ))
        .into_response();
    };
    match service.get(&live, &state.hub, id).await {
        Ok(snapshot) => {
            let mut response = (StatusCode::OK, snapshot.jpeg).into_response();
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
            if let Ok(age) = HeaderValue::from_str(&snapshot.age.as_secs().to_string()) {
                headers.insert(header::AGE, age);
            }
            response
        }
        Err(error) => ApiErr(api_error(error)).into_response(),
    }
}

fn api_error(error: SnapshotError) -> ApiError {
    let retry = |mut e: ApiError, secs: u64| {
        e.retry_after_s = Some(secs);
        e
    };
    match error {
        // Un `detail` : c'est ce qui distingue une caméra inconnue d'une route inconnue.
        SnapshotError::UnknownCamera => {
            ApiError::with_detail(ErrorCode::NotFound, "caméra inconnue")
        }
        SnapshotError::Busy => retry(
            ApiError::with_detail(
                ErrorCode::Unavailable,
                "la caméra est en cours de test ou de vérification",
            ),
            5,
        ),
        SnapshotError::NoImage => retry(
            ApiError::with_detail(
                ErrorCode::DeviceUnreachable,
                "la caméra n'a pas envoyé d'image à temps",
            ),
            10,
        ),
        SnapshotError::Changed => retry(
            ApiError::with_detail(ErrorCode::Unavailable, "la caméra vient d'être modifiée"),
            2,
        ),
        SnapshotError::Unsupported => ApiError::with_detail(
            ErrorCode::Unavailable,
            "format vidéo de la caméra non pris en charge",
        ),
        SnapshotError::DecoderUnavailable => ApiError::with_detail(
            ErrorCode::Unavailable,
            "décodage vidéo indisponible sur ce serveur",
        ),
    }
}
