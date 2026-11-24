//! « Demandes à valider » : l'accord d'une personne pour ce qu'un agent d'IA demande par le serveur
//! MCP (`docs/agents-ia.md`). Sous le jeton de **session** comme le reste de l'API : un jeton
//! d'agent, de mode ou de lecture n'ouvre aucune de ces routes (il n'y en a pas de plus sûre que
//! « l'agent ne peut pas s'autoriser lui-même »).
//!
//! | route | réponse |
//! |---|---|
//! | `GET /api/agent-requests` | `{"requests": [CardView]}` : les demandes en attente **de cette personne** |
//! | `POST /api/agent-requests/{id}/approve` | `202` : autorisée, une seule fois, 5 minutes au plus |
//! | `POST /api/agent-requests/{id}/refuse` | `202` |
//!
//! Seule la personne à qui appartient l'agent voit et valide sa demande : pour toute autre, ou pour
//! un identifiant inconnu ou mal formé, c'est le même `404`. Autoriser demande que le compte puisse
//! faire la chose lui-même (commander, ou changer le mode).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::json;

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{ApiErr, Ctx};
use super::AppState;
use crate::audit::{Action, Entry};
use crate::confirmations::{self, DecideError};
use crate::events::now_unix;
use crate::security::sessions::SessionInfo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agent-requests", get(list))
        .route("/api/agent-requests/{id}/approve", post(approve))
        .route("/api/agent-requests/{id}/refuse", post(refuse))
}

/// Interrupteur éteint (même en ligne de commande, serveur en marche) : plus aucune demande ne
/// vit. Rend `true` tant qu'il est éteint.
fn drop_if_switched_off(state: &AppState) -> bool {
    if state.auth.agents.switched_off_since_last_check() {
        state.auth.confirmations.clear();
    }
    !state.auth.agents.enabled()
}

async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<serde_json::Value> {
    if drop_if_switched_off(&state) {
        return Json(json!({ "requests": [] }));
    }
    let live = |agent_id: &str| state.auth.agents.live_agent(agent_id).is_some();
    let requests = state
        .auth
        .confirmations
        .list_for_at(&session.user_id, now_unix(), &live);
    Json(json!({ "requests": requests }))
}

async fn approve(
    state: State<AppState>,
    who: Ctx,
    session: Extension<SessionInfo>,
    id: Path<String>,
) -> Result<Response, ApiErr> {
    decide(state, who, session, id, true).await
}

async fn refuse(
    state: State<AppState>,
    who: Ctx,
    session: Extension<SessionInfo>,
    id: Path<String>,
) -> Result<Response, ApiErr> {
    decide(state, who, session, id, false).await
}

async fn decide(
    State(state): State<AppState>,
    Ctx(who): Ctx,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
    approve: bool,
) -> Result<Response, ApiErr> {
    let unknown = || {
        ApiErr(ApiError::with_detail(
            ErrorCode::NotFound,
            "demande inconnue",
        ))
    };
    if drop_if_switched_off(&state) || !confirmations::is_valid_id(&id) {
        return Err(unknown());
    }
    let live = |agent_id: &str| state.auth.agents.live_agent(agent_id).is_some();
    let can = |capability| state.hub.rights().can(&session.user_id, capability);
    let decided = state
        .auth
        .confirmations
        .decide_at(&session.user_id, &id, approve, now_unix(), &live, &can)
        .map_err(|e| match e {
            DecideError::Unknown => unknown(),
            DecideError::NotPending => ApiErr(ApiError::with_detail(
                ErrorCode::BadRequest,
                "cette demande n'est plus à valider (déjà traitée ou expirée)",
            )),
            DecideError::NotAllowed => ApiErr(ApiError::with_detail(
                ErrorCode::Forbidden,
                "ce compte n'a pas le droit de faire cela",
            )),
        })?;
    let action = if approve {
        Action::ConfirmationApproved
    } else {
        Action::ConfirmationRefused
    };
    let log = state.hub.audit().clone();
    let entry = Entry::new(action, &who)
        .target(&decided.label)
        .detail(decided.tool);
    let _ = tokio::task::spawn_blocking(move || log.record(entry)).await;
    Ok((StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response())
}
