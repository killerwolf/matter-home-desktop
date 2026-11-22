//! Les événements de détection (ticket n° 18), sous le même jeton que le reste de l'API :
//!
//! | route | réponse |
//! |---|---|
//! | `GET /api/events?since=&camera=&limit=` | `EventsResponse` : les plus récents d'abord, et l'état de la détection |
//! | `GET /api/events/{id}` | `DetectionEventView` |
//! | `DELETE /api/events/{id}` | `202` ; `404` s'il n'existe pas |
//! | `GET /api/events/{id}/snapshot` | la capture, `image/jpeg` |
//! | `GET /api/events/{id}/clip` | le clip, `video/mp4`, avec `Range` (`206`) pour la lecture dans un `<video>` |
//!
//! L'identifiant est vérifié (`wire::is_event_id`) avant de toucher au disque : il ne sert que de
//! nom de fichier dans `<données>/events/`.

use std::io::{Read, Seek, SeekFrom};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;

use protocol::wire::{
    self, ApiError, ErrorCode, EventsResponse, EVENTS_DEFAULT_LIMIT, EVENTS_MAX_LIMIT,
};

use super::routes::{require, ApiErr};
use super::AppState;
use crate::detection::Part;
use crate::rights::Capability;
use crate::security::sessions::SessionInfo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(wire::EVENTS_PATH, get(list))
        .route(wire::EVENT_PATH, get(one).delete(remove))
        .route(wire::EVENT_SNAPSHOT_PATH, get(snapshot))
        .route(wire::EVENT_CLIP_PATH, get(clip))
}

#[derive(Deserialize)]
struct ListQuery {
    since: Option<f64>,
    camera: Option<String>,
    limit: Option<usize>,
}

fn bad(detail: &str) -> ApiErr {
    ApiErr(ApiError::with_detail(ErrorCode::BadRequest, detail))
}

fn not_found() -> ApiErr {
    ApiErr(ApiError::with_detail(
        ErrorCode::NotFound,
        "événement inconnu",
    ))
}

fn checked(id: &str) -> Result<&str, ApiErr> {
    if wire::is_event_id(id) {
        Ok(id)
    } else {
        Err(bad("identifiant d'événement invalide"))
    }
}

async fn list(
    State(state): State<AppState>,
    q: Result<Query<ListQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<EventsResponse>, ApiErr> {
    let Query(q) = q.map_err(|_| bad("paramètres invalides"))?;
    let camera = match q.camera.as_deref() {
        None | Some("") => None,
        Some(text) => Some(wire::parse_id(text).map_err(|_| bad("caméra invalide"))?),
    };
    let since = q.since.unwrap_or(0.0);
    if !since.is_finite() {
        return Err(bad("since invalide"));
    }
    let limit = q
        .limit
        .unwrap_or(EVENTS_DEFAULT_LIMIT)
        .clamp(1, EVENTS_MAX_LIMIT);
    let detection = state.hub.detection().clone();
    let events = tokio::task::spawn_blocking(move || detection.store().list(since, camera, limit))
        .await
        .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    Ok(Json(EventsResponse {
        events,
        detection: state.hub.detection().status(),
    }))
}

async fn one(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response, ApiErr> {
    let id = checked(&id)?.to_string();
    let detection = state.hub.detection().clone();
    let event = tokio::task::spawn_blocking(move || detection.store().get(&id))
        .await
        .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    event.map(|e| Json(e).into_response()).ok_or_else(not_found)
}

async fn remove(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<Response, ApiErr> {
    require(&state, &session.user_id, Capability::ControlDevices)?;
    let id = checked(&id)?.to_string();
    let detection = state.hub.detection().clone();
    let removed = tokio::task::spawn_blocking(move || detection.store().delete(&id))
        .await
        .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    if removed {
        Ok((
            StatusCode::ACCEPTED,
            Json(wire::Accepted { accepted: true }),
        )
            .into_response())
    } else {
        Err(not_found())
    }
}

async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiErr> {
    serve(&state, &id, Part::Snapshot, "image/jpeg", &headers).await
}

async fn clip(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiErr> {
    serve(&state, &id, Part::Clip, "video/mp4", &headers).await
}

/// Un intervalle `Range: bytes=a-b` (un seul) sur un fichier de `len` octets. `Ok(None)` : pas
/// d'en-tête (tout le fichier) ; `Err(())` : intervalle impossible (416).
pub fn parse_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(text) = header else {
        return Ok(None);
    };
    let spec = text.trim().strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || len == 0 {
        // Plusieurs intervalles : on rend tout le fichier, comme le permet la norme.
        return if len == 0 { Err(()) } else { Ok(None) };
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", "") => return Err(()),
        ("", suffix) => {
            let n: u64 = suffix.parse().map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len - 1)
        }
        (a, "") => (a.parse().map_err(|_| ())?, len - 1),
        (a, b) => (
            a.parse().map_err(|_| ())?,
            b.parse::<u64>().map_err(|_| ())?.min(len - 1),
        ),
    };
    if start > end || start >= len {
        return Err(());
    }
    Ok(Some((start, end)))
}

/// Morceaux lus à la fois : un clip de 48 Mo ne passe jamais tout entier en mémoire.
const CHUNK: usize = 256 * 1024;

/// Lit `[start, start + len)` de `file` par morceaux, dans un fil bloquant, vers un corps de
/// réponse en flux.
fn stream_file(mut file: std::fs::File, start: u64, len: u64) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(4);
    tokio::task::spawn_blocking(move || {
        if let Err(e) = file.seek(SeekFrom::Start(start)) {
            let _ = tx.blocking_send(Err(e));
            return;
        }
        let mut left = len;
        while left > 0 {
            let mut buf = vec![0u8; CHUNK.min(left as usize)];
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.truncate(n);
                    left -= n as u64;
                    // Le client est parti : on arrête de lire.
                    if tx.blocking_send(Ok(buf.into())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    });
    axum::body::Body::from_stream(stream)
}

async fn serve(
    state: &AppState,
    id: &str,
    part: Part,
    content_type: &'static str,
    headers: &HeaderMap,
) -> Result<Response, ApiErr> {
    let id = checked(id)?.to_string();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let detection = state.hub.detection().clone();
    let opened = tokio::task::spawn_blocking(move || {
        let path = detection.store().part(&id, part)?;
        let file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some((file, len))
    })
    .await
    .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    let Some((file, len)) = opened else {
        return Err(not_found());
    };
    let header_value = |text: String| HeaderValue::from_str(&text).ok();
    let (status, start, end) = match parse_range(range.as_deref(), len) {
        Err(()) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Some(v) = header_value(format!("bytes */{len}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return Ok(response);
        }
        Ok(None) => (StatusCode::OK, 0, len.saturating_sub(1)),
        Ok(Some((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b),
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mut response = (status, stream_file(file, start, count)).into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(v) = header_value(count.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if status == StatusCode::PARTIAL_CONTENT {
        if let Some(v) = header_value(format!("bytes {start}-{end}/{len}")) {
            h.insert(header::CONTENT_RANGE, v);
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-"), 100), Ok(Some((0, 99))));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Ok(Some((10, 19))));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=0-1"), 1), Ok(Some((0, 0))));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Err(()));
        assert_eq!(parse_range(Some("bytes=5-2"), 100), Err(()));
        assert_eq!(parse_range(Some("items=0-1"), 100), Err(()));
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Ok(None));
    }
}
