//! Les notifications web push (ticket n° 17), sous le même jeton que le reste de l'API. Les routes
//! sont décrites dans `protocol::wire::push`. L'identifiant d'un abonnement est vérifié
//! (`wire::is_push_id`) avant tout.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};

use protocol::wire::{
    self, ApiError, ErrorCode, PushSettingsRequest, PushStatus, PushSubscribeRequest,
    PushSubscriptionView, PushUpdateRequest,
};

use super::routes::{ApiErr, ApiJson};
use super::AppState;
use crate::push::PushError;
use crate::security::sessions::SessionInfo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(wire::PUSH_PATH, get(status).put(settings))
        .route(wire::PUSH_SUBSCRIPTIONS_PATH, post(subscribe))
        .route(wire::PUSH_SUBSCRIPTION_PATH, put(update).delete(remove))
        .route(wire::PUSH_TEST_PATH, post(test))
}

fn error(e: PushError) -> ApiErr {
    ApiErr(match e {
        PushError::Invalid(why) => ApiError::with_detail(ErrorCode::BadRequest, why),
        PushError::TooMany => ApiError::with_detail(
            ErrorCode::BadRequest,
            "trop d'appareils abonnés : révoquez-en un d'abord",
        ),
        PushError::NotFound => ApiError::with_detail(ErrorCode::NotFound, "abonnement inconnu"),
        PushError::Disabled => ApiError::with_detail(
            ErrorCode::Unavailable,
            "notifications coupées sur le serveur",
        ),
        PushError::Storage => ApiError::new(ErrorCode::Internal),
    })
}

fn checked(id: &str) -> Result<&str, ApiErr> {
    if wire::is_push_id(id) {
        Ok(id)
    } else {
        Err(ApiErr(ApiError::with_detail(
            ErrorCode::BadRequest,
            "identifiant d'abonnement invalide",
        )))
    }
}

/// Les écritures sur disque ne tournent pas sur les fils du réseau.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, PushError> + Send + 'static,
) -> Result<T, ApiErr> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(error),
        Err(_) => Err(ApiErr(ApiError::new(ErrorCode::Internal))),
    }
}

/// Chaque personne ne voit, n'abonne, ne règle et ne révoque que ses propres appareils.
async fn status(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<PushStatus> {
    Json(state.hub.push().status_for(&session.user_id))
}

/// `PUT /api/push` : l'interrupteur général des notifications. C'est un **réglage commun à toute
/// la maison**, pas un réglage personnel : il vaut pour tous les comptes (les abonnements, eux,
/// restent personnels). N'importe quelle personne connectée peut le changer ; la réponse ne montre
/// que ses propres appareils.
async fn settings(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    ApiJson(request): ApiJson<PushSettingsRequest>,
) -> Result<Json<PushStatus>, ApiErr> {
    let push = state.hub.push().clone();
    blocking(move || push.set_enabled(request.enabled)).await?;
    Ok(Json(state.hub.push().status_for(&session.user_id)))
}

async fn subscribe(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<PushSubscribeRequest>,
) -> Result<Json<PushSubscriptionView>, ApiErr> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let push = state.hub.push().clone();
    Ok(Json(
        blocking(move || push.subscribe_for(&session.user_id, request, &user_agent)).await?,
    ))
}

async fn update(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PushUpdateRequest>,
) -> Result<Json<PushSubscriptionView>, ApiErr> {
    let id = checked(&id)?.to_string();
    let push = state.hub.push().clone();
    Ok(Json(
        blocking(move || push.update_for(&session.user_id, &id, request)).await?,
    ))
}

async fn remove(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<Response, ApiErr> {
    let id = checked(&id)?.to_string();
    let push = state.hub.push().clone();
    blocking(move || push.remove_for(&session.user_id, &id)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(wire::Accepted { accepted: true }),
    )
        .into_response())
}

async fn test(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<Response, ApiErr> {
    let id = checked(&id)?.to_string();
    state
        .hub
        .push()
        .send_test_for(&session.user_id, &id)
        .map_err(error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(wire::Accepted { accepted: true }),
    )
        .into_response())
}
