//! Les routes de l'API. Tout exige un jeton de session (`Authorization: Bearer …`, jamais
//! dans l'URL) sauf `GET /api/version` et `POST /api/login`.

use std::net::SocketAddr;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use serde::de::DeserializeOwned;

use protocol::automation::RuleSet;
use protocol::home::HomePlan;
use protocol::wire::{
    self, Accepted, ApiError, CamerasResponse, CoverRequest, CustomizeRequest, ErrorBody,
    ErrorCode, HistoryQuery, HistoryResponse, ImportResponse, LoginRequest, LoginResponse,
    OnOffRequest, PairRequest, Snapshot, TextIds, VersionInfo,
};

use super::AppState;
use crate::security::account::Verdict;
use crate::security::sessions::SessionInfo;

/// Taille maximale d'un corps JSON ordinaire (plan, règles, commandes).
const JSON_LIMIT: usize = 1024 * 1024;
const LOGIN_LIMIT: usize = 4 * 1024;
/// Une sauvegarde porte jusqu'à 5 Mo d'historique par appareil.
const IMPORT_LIMIT: usize = 128 * 1024 * 1024;
/// Pause avant de répondre à un échec de connexion.
const FAILURE_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/logout", post(logout))
        .route("/api/state", get(full_state))
        .route("/api/ws", get(super::ws::upgrade))
        .route("/api/rule-runs", get(rule_runs))
        .route("/api/cameras", get(get_cameras))
        .route("/api/plan", get(get_plan).put(put_plan))
        .route("/api/rules", get(get_rules).put(put_rules))
        .route("/api/rules/{id}/test", post(test_rule))
        .route("/api/pairing", post(pair))
        .route("/api/devices/{id}", delete(forget))
        .route("/api/devices/{id}/retry", post(retry))
        .route("/api/devices/{id}/customization", put(customize))
        .route("/api/devices/{id}/on-off", post(on_off))
        .route("/api/devices/{id}/cover", post(cover))
        .route(
            "/api/devices/{id}/history",
            get(history).delete(clear_history),
        )
        .route("/api/devices/{id}/history.csv", get(history_csv))
        .route("/api/export", get(export))
        .route(
            "/api/import",
            post(import).layer(DefaultBodyLimit::max(IMPORT_LIMIT)),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/api/version", get(version))
        .route(
            "/api/login",
            post(login).layer(DefaultBodyLimit::max(LOGIN_LIMIT)),
        )
        .merge(protected)
        .fallback(|| async { ApiErr(ApiError::new(ErrorCode::NotFound)) })
        .layer(DefaultBodyLimit::max(JSON_LIMIT))
        .layer(middleware::map_response(json_errors))
        .layer(middleware::from_fn_with_state(state.clone(), deadline))
        // En dernier (donc à l'extérieur de tout) : les en-têtes communs couvrent aussi les
        // réponses refabriquées par `json_errors` et le 503 de `deadline`.
        .layer(middleware::map_response(harden))
        .with_state(state)
}

/// Durée maximale d'une requête (corps compris) : un client qui envoie au compte-gouttes ne
/// retient pas une connexion indéfiniment. Sans effet sur un WebSocket, qui vit après la réponse 101.
async fn deadline(State(state): State<AppState>, request: Request, next: Next) -> Response {
    // Le corps a aussi son délai d'inactivité (15 s) : voir `body`.
    let request =
        request.map(|body| Body::new(super::body::IdleBody::new(body, state.body_idle_timeout)));
    match tokio::time::timeout(REQUEST_DEADLINE, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ApiErr(ApiError::with_detail(
            ErrorCode::Unavailable,
            "délai dépassé",
        ))
        .into_response(),
    }
}

const REQUEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Les réponses que fabrique la bibliothèque (405 sans corps, 413 en texte) passent au format
/// du protocole : `{"error": {"code": …}}`, avec un code stable. `Allow` est conservé.
async fn json_errors(response: Response) -> Response {
    let code = match response.status() {
        StatusCode::METHOD_NOT_ALLOWED => ErrorCode::MethodNotAllowed,
        StatusCode::PAYLOAD_TOO_LARGE => ErrorCode::PayloadTooLarge,
        _ => return response,
    };
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if is_json {
        return response;
    }
    let allow = response.headers().get(header::ALLOW).cloned();
    let mut replaced = ApiErr(ApiError::new(code)).into_response();
    if let Some(allow) = allow {
        replaced.headers_mut().insert(header::ALLOW, allow);
    }
    replaced
}

/// En-têtes communs : jamais de mise en cache, pas de devinette de type.
async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

// ---------------------------------------------------------------------------
// Erreurs et extracteurs
// ---------------------------------------------------------------------------

pub struct ApiErr(pub ApiError);

impl From<ApiError> for ApiErr {
    fn from(e: ApiError) -> Self {
        ApiErr(e)
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_after = self.0.retry_after_s;
        let unauthorized = self.0.code == ErrorCode::Unauthorized;
        let mut response = (status, Json(ErrorBody { error: self.0 })).into_response();
        if let Some(s) = retry_after {
            if let Ok(v) = HeaderValue::from_str(&s.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        if unauthorized {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

type ApiResult<T> = Result<T, ApiErr>;

/// Corps JSON dont les erreurs sont des codes du protocole, pas du texte d'axum.
struct ApiJson<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiErr;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiErr> {
        let ApiBytes(bytes) = ApiBytes::from_request(req, state).await?;
        serde_json::from_slice(&bytes).map(ApiJson).map_err(|_| {
            ApiErr(ApiError::with_detail(
                ErrorCode::BadRequest,
                "corps JSON invalide",
            ))
        })
    }
}

/// Corps brut dont les erreurs (trop gros, interrompu) sont des codes du protocole.
struct ApiBytes(Bytes);

impl<S: Send + Sync> FromRequest<S> for ApiBytes {
    type Rejection = ApiErr;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiErr> {
        Bytes::from_request(req, state)
            .await
            .map(ApiBytes)
            .map_err(|rejection| {
                if super::body::is_idle_timeout(&rejection) {
                    ApiErr(ApiError::new(ErrorCode::RequestTimeout))
                } else if rejection.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiErr(ApiError::new(ErrorCode::PayloadTooLarge))
                } else {
                    ApiErr(ApiError::new(ErrorCode::BadRequest))
                }
            })
    }
}

fn parse_id(text: &str) -> ApiResult<u64> {
    wire::parse_id(text)
        .map_err(|_| ApiError::with_detail(ErrorCode::BadRequest, "identifiant invalide").into())
}

fn accepted() -> Response {
    (StatusCode::ACCEPTED, Json(Accepted { accepted: true })).into_response()
}

/// Les opérations bloquantes (disque, argon2) ne tournent jamais sur les threads du réseau.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> ApiResult<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(ApiErr),
        Err(_) => Err(ApiError::new(ErrorCode::Internal).into()),
    }
}

// ---------------------------------------------------------------------------
// Authentification
// ---------------------------------------------------------------------------

async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        protocol_version: wire::PROTOCOL_VERSION,
        server_version: SERVER_VERSION.to_string(),
    })
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ApiJson(request): ApiJson<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    if request.protocol_version != wire::PROTOCOL_VERSION {
        return Err(ApiError::with_detail(
            ErrorCode::VersionIncompatible,
            format!(
                "le serveur parle la version {} du protocole, le client la version {}",
                wire::PROTOCOL_VERSION,
                request.protocol_version
            ),
        )
        .into());
    }
    if request.username.len() > 256 || request.password.len() > 1024 {
        return Err(ApiError::new(ErrorCode::InvalidCredentials).into());
    }
    let ip = peer.ip();
    check_not_locked(&state, ip)?;
    // Argon2 coûte du temps et de la mémoire : une vérification à la fois, pour tout le
    // serveur. C'est ce qui limite le débit d'une attaque répartie sur plusieurs adresses,
    // sans jamais refuser un mot de passe correct : les demandes font la queue (dans l'ordre
    // d'arrivée) et le propriétaire passe à son tour.
    let verdict = {
        let _slot = state
            .auth
            .hash_slots
            .acquire()
            .await
            .map_err(|_| ApiError::new(ErrorCode::Unavailable))?;
        // Deuxième contrôle, une fois la place obtenue : des tentatives lancées en parallèle
        // passent toutes le premier, mais celles qui attendaient voient l'attente décidée par
        // les échecs des précédentes. Sans lui, la limite serait contournable en simultané.
        check_not_locked(&state, ip)?;
        let auth = state.auth.clone();
        tokio::task::spawn_blocking(move || {
            auth.accounts.verify(&request.username, &request.password)
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?
    };
    match verdict {
        Verdict::Valid(account_id) => {
            state.auth.limiter.record_success(ip);
            let session = state.auth.sessions.create(&account_id);
            log::info!("connexion acceptée pour {ip}");
            Ok(Json(LoginResponse {
                token: session.token,
                expires_at: session.expires_at,
                max_expires_at: session.max_expires_at,
                protocol_version: wire::PROTOCOL_VERSION,
                server_version: SERVER_VERSION.to_string(),
            }))
        }
        Verdict::Invalid => {
            let wait = state.auth.limiter.record_failure(ip);
            log::warn!("connexion refusée pour {ip} : identifiants incorrects");
            // Petit délai avant de répondre (hors de la file des vérifications : il ne retarde
            // personne d'autre) : un échec coûte un peu de temps à celui qui le provoque.
            tokio::time::sleep(FAILURE_PAUSE).await;
            let mut error = ApiError::new(ErrorCode::InvalidCredentials);
            error.retry_after_s = wait.map(|w| w.as_secs().max(1));
            Err(error.into())
        }
    }
}

fn check_not_locked(state: &AppState, ip: std::net::IpAddr) -> ApiResult<()> {
    if let Err(wait) = state.auth.limiter.check(ip) {
        log::warn!("connexion refusée pour {ip} : trop de tentatives");
        return Err(too_many(wait.as_secs().max(1)).into());
    }
    Ok(())
}

fn too_many(retry_after_s: u64) -> ApiError {
    let mut error = ApiError::new(ErrorCode::TooManyAttempts);
    error.retry_after_s = Some(retry_after_s);
    error
}

/// Extrait le jeton d'un en-tête `Authorization: Bearer <jeton>`.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && token.len() <= 256)
        .then_some(token.trim())
}

async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let session = bearer(request.headers()).and_then(|token| {
        let account_id = state.auth.accounts.account_id()?;
        state.auth.sessions.validate(token, &account_id)
    });
    match session {
        Some(info) => {
            request.extensions_mut().insert(info);
            next.run(request).await
        }
        None => ApiErr(ApiError::new(ErrorCode::Unauthorized)).into_response(),
    }
}

async fn logout(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> StatusCode {
    state.auth.sessions.revoke(&session.id);
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// État, plan, règles
// ---------------------------------------------------------------------------

async fn full_state(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.hub.current_snapshot())
}

async fn rule_runs(State(state): State<AppState>) -> Json<Vec<wire::RuleRunView>> {
    Json(state.hub.rule_runs())
}

/// Les caméras : identifiant, nom, état, codec. Jamais d'adresse ni de mot de passe. Aucune route
/// d'écriture : on les gère depuis la fenêtre du serveur.
async fn get_cameras(State(state): State<AppState>) -> Json<CamerasResponse> {
    Json(CamerasResponse {
        cameras: state.hub.cameras(),
    })
}

async fn get_plan(State(state): State<AppState>) -> Json<TextIds<HomePlan>> {
    Json(TextIds(state.hub.plan()))
}

async fn put_plan(
    State(state): State<AppState>,
    ApiJson(TextIds(plan)): ApiJson<TextIds<HomePlan>>,
) -> ApiResult<Json<TextIds<HomePlan>>> {
    let hub = state.hub.clone();
    blocking(move || hub.set_plan(plan)).await?;
    Ok(Json(TextIds(state.hub.plan())))
}

async fn get_rules(State(state): State<AppState>) -> Json<TextIds<RuleSet>> {
    Json(TextIds(state.hub.rules()))
}

async fn put_rules(
    State(state): State<AppState>,
    ApiJson(TextIds(rules)): ApiJson<TextIds<RuleSet>>,
) -> ApiResult<Json<TextIds<RuleSet>>> {
    let hub = state.hub.clone();
    blocking(move || hub.set_rules(rules)).await?;
    Ok(Json(TextIds(state.hub.rules())))
}

async fn test_rule(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    state.hub.test_rule(parse_id(&id)?)?;
    Ok(accepted())
}

// ---------------------------------------------------------------------------
// Appareils
// ---------------------------------------------------------------------------

async fn pair(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PairRequest>,
) -> ApiResult<Response> {
    state.hub.pair(&request.code)?;
    Ok(accepted())
}

async fn retry(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    state.hub.retry(parse_id(&id)?)?;
    Ok(accepted())
}

async fn forget(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    state.hub.forget(parse_id(&id)?)?;
    Ok(accepted())
}

async fn customize(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CustomizeRequest>,
) -> ApiResult<Response> {
    state.hub.customize(parse_id(&id)?, request)?;
    Ok(accepted())
}

async fn on_off(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<OnOffRequest>,
) -> ApiResult<Response> {
    state.hub.set_on_off(parse_id(&id)?, request.command)?;
    Ok(accepted())
}

async fn cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CoverRequest>,
) -> ApiResult<Response> {
    state.hub.set_cover(parse_id(&id)?, request.command)?;
    Ok(accepted())
}

// ---------------------------------------------------------------------------
// Historique, export, import
// ---------------------------------------------------------------------------

fn query(
    parsed: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<HistoryQuery> {
    parsed.map(|Query(q)| q).map_err(|_| {
        ApiError::with_detail(ErrorCode::BadRequest, "paramètres d'historique invalides").into()
    })
}

async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    q: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<HistoryResponse>> {
    let (id, q) = (parse_id(&id)?, query(q)?);
    let hub = state.hub.clone();
    Ok(Json(blocking(move || hub.history(id, &q)).await?))
}

async fn history_csv(
    State(state): State<AppState>,
    Path(id): Path<String>,
    q: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Response> {
    let (id, q) = (parse_id(&id)?, query(q)?);
    let hub = state.hub.clone();
    let csv = blocking(move || hub.history_csv(id, &q)).await?;
    Ok(attachment(
        "text/csv; charset=utf-8",
        &format!("matter-{id:016X}.csv"),
        csv,
    ))
}

async fn clear_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    state.hub.clear_history(parse_id(&id)?)?;
    Ok(accepted())
}

fn attachment(content_type: &'static str, file_name: &str, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // Le nom vient du serveur (date, identifiant en hexadécimal) : pas de caractère à échapper.
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    response
}

async fn export(State(state): State<AppState>) -> ApiResult<Response> {
    let hub = state.hub.clone();
    let (name, text) = blocking(move || hub.export()).await?;
    log::warn!("export de la sauvegarde complète (clés privées en clair) demandé");
    Ok(attachment(wire::EXPORT_CONTENT_TYPE, &name, text))
}

/// `POST /api/import` : `?dry_run=1` valide et résume la sauvegarde sans rien engager.
#[derive(serde::Deserialize, Default)]
struct ImportQuery {
    #[serde(default)]
    dry_run: Option<String>,
    /// `replace_identity=1` : accepte une sauvegarde d'une autre identité Matter (fabric).
    #[serde(default)]
    replace_identity: Option<String>,
}

async fn import(
    State(state): State<AppState>,
    q: Result<Query<ImportQuery>, axum::extract::rejection::QueryRejection>,
    ApiBytes(body): ApiBytes,
) -> ApiResult<Response> {
    let flag = |value: Option<String>, name: &str| match value.as_deref() {
        None | Some("0" | "false" | "") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(_) => Err(ApiError::with_detail(
            ErrorCode::BadRequest,
            format!("paramètre {name} invalide"),
        )),
    };
    let (dry_run, replace_identity) = match q {
        Ok(Query(q)) => (
            flag(q.dry_run, "dry_run")?,
            flag(q.replace_identity, "replace_identity")?,
        ),
        Err(_) => {
            return Err(
                ApiError::with_detail(ErrorCode::BadRequest, "paramètre dry_run invalide").into(),
            )
        }
    };
    let hub = state.hub.clone();
    let response = blocking(move || {
        let text = std::str::from_utf8(&body).map_err(|_| {
            ApiError::with_detail(
                ErrorCode::InvalidImport,
                "le fichier n'est pas du texte JSON",
            )
        })?;
        if dry_run {
            hub.inspect_import(text).map(|(summary, identity)| {
                (
                    StatusCode::OK,
                    ImportResponse {
                        summary,
                        restarting: false,
                        identity: Some(identity),
                    },
                )
            })
        } else {
            hub.stage_import(text, replace_identity)
                .map(|(summary, identity)| {
                    (
                        StatusCode::ACCEPTED,
                        ImportResponse {
                            summary,
                            restarting: true,
                            identity: Some(identity),
                        },
                    )
                })
        }
    })
    .await?;
    Ok((response.0, Json(response.1)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn mini_router() -> Router {
        async fn raw(ApiBytes(body): ApiBytes) -> StatusCode {
            assert!(body.len() <= 8);
            StatusCode::NO_CONTENT
        }
        Router::new()
            .route("/raw", post(raw).layer(DefaultBodyLimit::max(8)))
            .route("/only-get", get(|| async { "ok" }))
            .layer(middleware::map_response(json_errors))
    }

    async fn send(method: &str, path: &str, body: &'static [u8]) -> Response {
        mini_router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn error_code(response: Response) -> (StatusCode, String, Option<HeaderValue>) {
        let status = response.status();
        let allow = response.headers().get(header::ALLOW).cloned();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        assert_eq!(
            content_type.as_ref().map(|v| v.as_bytes()),
            Some(&b"application/json"[..])
        );
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
        (status, format!("{:?}", body.error.code), allow)
    }

    #[test]
    fn payload_too_large_and_method_not_allowed_use_the_protocol_format() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            // Corps brut trop gros (l'import) : 413 au format du protocole, plus du texte.
            let (status, code, _) = error_code(send("POST", "/raw", b"123456789").await).await;
            assert_eq!((status.as_u16(), code.as_str()), (413, "PayloadTooLarge"));
            assert_eq!(send("POST", "/raw", b"12345678").await.status(), 204);
            // Mauvaise méthode : 405 avec un corps JSON, et l'en-tête Allow est conservé.
            let (status, code, allow) = error_code(send("DELETE", "/only-get", b"").await).await;
            assert_eq!((status.as_u16(), code.as_str()), (405, "MethodNotAllowed"));
            assert!(allow.unwrap().to_str().unwrap().contains("GET"));
        });
    }
}
