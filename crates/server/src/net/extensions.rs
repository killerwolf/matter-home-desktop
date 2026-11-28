//! Les routes des extensions (voir `extensions/`), sous le même jeton de session que le reste.
//!
//! - `GET /api/extensions` : la liste (`{"extensions": [View…]}`), pour toute personne connectée ;
//! - `GET /api/extensions/{id}` : une extension ;
//! - `PUT /api/extensions/{id}` : `{"enabled"?, "config"?, "secrets"?}` (droit « extensions ») ;
//!   les secrets ne reviennent jamais : la réponse dit seulement « défini » ou non ;
//! - `GET /api/extensions/weather/search?q=…` : les lieux qui correspondent (droit « extensions »),
//!   refusé tant que la météo est éteinte (aucun appel ne part) ;
//! - `GET /api/weather` : ce que montrent le plan et l'accueil, pour toute personne connectée ;
//! - `GET /api/extensions/somfy/devices` : les appareils du compte Somfy (droit « extensions ») ;
//! - `POST /api/extensions/somfy/refresh` : relire cette liste chez Somfy (droit « extensions »,
//!   au plus une fois toutes les 20 s, refusé tant que l'extension est éteinte).

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{audit, require, ApiErr, ApiJson, Ctx};
use super::AppState;
use crate::audit::{Action, Entry};
use crate::extensions::somfy::{Choices, RefreshError};
use crate::extensions::weather::{Place, Status};
use crate::extensions::{Changes, SearchError, Update, UpdateError, View};
use crate::rights::Capability;
use crate::security::sessions::SessionInfo;

/// La phrase renvoyée quand un compte tente de régler la box par l'API.
pub const BOX_ON_THE_MAC: &str = "La box se règle dans Ostal Server, sur le Mac";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/extensions", get(list))
        .route("/api/extensions/{id}", get(one).put(change))
        .route("/api/extensions/weather/search", get(search))
        .route("/api/extensions/somfy/devices", get(somfy_devices))
        .route("/api/extensions/somfy/refresh", post(somfy_refresh))
        .route("/api/weather", get(weather))
}

#[derive(Serialize)]
struct List {
    extensions: Vec<View>,
}

/// La personne peut-elle régler les extensions ? Sinon, les champs de configuration privés (un
/// identifiant e-mail, par exemple) ne lui sont pas montrés.
fn may_manage(state: &AppState, session: &SessionInfo) -> bool {
    state
        .hub
        .rights()
        .can(&session.user_id, Capability::ManageExtensions)
}

async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<List> {
    Json(List {
        extensions: state.hub.extensions().list(may_manage(&state, &session)),
    })
}

async fn one(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<Json<View>, ApiErr> {
    state
        .hub
        .extensions()
        .get(&id, may_manage(&state, &session))
        .map(Json)
        .ok_or_else(unknown)
}

fn unknown() -> ApiErr {
    ApiErr(ApiError::with_detail(
        ErrorCode::NotFound,
        "extension inconnue",
    ))
}

fn failure(e: UpdateError) -> ApiErr {
    ApiErr(match e {
        UpdateError::Unknown => ApiError::with_detail(ErrorCode::NotFound, "extension inconnue"),
        UpdateError::Invalid(why) => ApiError::with_detail(ErrorCode::BadRequest, why),
        UpdateError::Storage => ApiError::new(ErrorCode::Internal),
    })
}

/// Les lignes du journal d'audit d'un changement : jamais une valeur.
fn audit_entries(id: &str, changes: &Changes, who: &crate::audit::Who) -> Vec<Entry> {
    let line = |detail: &str| {
        Entry::new(Action::ExtensionSetting, who)
            .target(id)
            .detail(detail)
    };
    let mut out = Vec::new();
    if let Some(on) = changes.switched {
        out.push(line(if on { "on" } else { "off" }));
    }
    if changes.config {
        out.push(line("config"));
    }
    if changes.secrets {
        out.push(line("secret"));
    }
    out
}

async fn change(
    State(state): State<AppState>,
    Ctx(who): Ctx,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
    ApiJson(update): ApiJson<Update>,
) -> Result<Json<View>, ApiErr> {
    require(&state, &session.user_id, Capability::ManageExtensions)?;
    // La box (adresse, mot de passe, interrupteur) se règle seulement sur le Mac, dans Ostal Server :
    // aucun compte, même avec le droit « extensions », ne la change par l'API.
    if id == crate::extensions::bbox::ID
        && (update.enabled.is_some() || update.config.is_some() || !update.secrets.is_empty())
    {
        return Err(ApiErr(ApiError::with_detail(
            ErrorCode::Forbidden,
            BOX_ON_THE_MAC,
        )));
    }
    let hub = state.hub.clone();
    let people = state.auth.clone();
    let done = tokio::task::spawn_blocking(move || {
        let (view, changes) = hub.extensions().update(&id, update)?;
        // Activée, lieu ou compte changé : un premier relevé tout de suite, de **cette**
        // extension seulement (aucun appel si éteinte).
        if changes.switched.is_some() || changes.config || changes.secrets {
            hub.extensions().tick_one(view.id, crate::security::now());
            // Une box (Bbox…) : la présence relève la liste des appareils aussitôt (et l'état de
            // l'extension dit si le mot de passe est bon), sans attendre le prochain tour de 2
            // minutes.
            if hub.extensions().is_box(view.id) {
                hub.wifi_poll(&people.accounts, true);
            }
        }
        // Les volets d'extension suivent (ajoutés, retirés, injoignables).
        hub.sync_extension_devices();
        let view = hub.extensions().get(view.id, true).unwrap_or(view);
        Ok::<_, UpdateError>((view, changes))
    })
    .await
    .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?
    .map_err(failure)?;
    let (view, changes) = done;
    for entry in audit_entries(view.id, &changes, &who) {
        audit(&state, entry).await;
    }

    Ok(Json(view))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
struct Places {
    places: Vec<Place>,
}

async fn search(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Places>, ApiErr> {
    require(&state, &session.user_id, Capability::ManageExtensions)?;
    let hub = state.hub.clone();
    let result = tokio::task::spawn_blocking(move || hub.extensions().weather_search(&query.q))
        .await
        .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    match result {
        Ok(places) => Ok(Json(Places { places })),
        Err(SearchError::Disabled) => Err(ApiErr(ApiError::with_detail(
            ErrorCode::BadRequest,
            "activez d'abord l'extension Météo extérieure",
        ))),
        Err(SearchError::Failed) => Err(ApiErr(ApiError::with_detail(
            ErrorCode::Unavailable,
            "Open-Meteo ne répond pas : réessayez dans un instant",
        ))),
    }
}

#[derive(Serialize)]
struct Weather {
    enabled: bool,
    #[serde(flatten)]
    status: Status,
}

async fn weather(State(state): State<AppState>) -> Json<Weather> {
    let (enabled, status) = state.hub.extensions().weather_status();
    Json(Weather { enabled, status })
}

async fn somfy_devices(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Result<Json<Choices>, ApiErr> {
    require(&state, &session.user_id, Capability::ManageExtensions)?;
    Ok(Json(state.hub.extensions().somfy().choices()))
}

async fn somfy_refresh(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Result<Json<Choices>, ApiErr> {
    require(&state, &session.user_id, Capability::ManageExtensions)?;
    let hub = state.hub.clone();
    let result = tokio::task::spawn_blocking(move || {
        let now = crate::security::now();
        hub.extensions().somfy().request_refresh(now)?;
        hub.extensions().tick_one(crate::extensions::somfy::ID, now);
        hub.sync_extension_devices();
        Ok::<_, RefreshError>(hub.extensions().somfy().choices())
    })
    .await
    .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    result.map(Json).map_err(|e| {
        ApiErr(match e {
            RefreshError::Disabled => ApiError::with_detail(
                ErrorCode::BadRequest,
                "activez l'extension Volets Somfy et saisissez le compte d'abord",
            ),
            RefreshError::Rejected => ApiError::with_detail(
                ErrorCode::BadRequest,
                "Somfy a refusé l'identifiant ou le mot de passe : corrigez-les d'abord",
            ),
            RefreshError::TooSoon => ApiError::with_detail(
                ErrorCode::Busy,
                "patientez quelques secondes avant d'actualiser",
            ),
        })
    })
}
