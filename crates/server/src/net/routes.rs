//! Les routes de l'API. Tout exige un jeton de session (`Authorization: Bearer …`, jamais
//! dans l'URL) sauf `GET /api/version` et `POST /api/login`. Deux jetons à part ouvrent chacun
//! une seule porte : le jeton de mode `PUT /api/mode`, le jeton de lecture `GET /api/state` et
//! `GET /api/events` (le widget de l'iPhone).

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
use protocol::mode::ModeSource;
use protocol::wire::{
    self, Accepted, ApiError, CamerasResponse, ConnectionView, ConnectionsResponse, CoverRequest,
    CustomizeRequest, ErrorBody, ErrorCode, HistoryQuery, HistoryResponse, ImportResponse,
    LoginRequest, LoginResponse, ModeRequest, ModeResponse, OnOffRequest, PairRequest, Snapshot,
    TextIds, VersionInfo, Via,
};

use super::AppState;
use crate::security::account::Verdict;
use crate::security::sessions::{clean_installation, SessionInfo};

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
        .route("/api/me/installations", get(my_connections))
        .route("/api/me/installations/{id}", delete(cut_connection))
        .route("/api/state", get(full_state))
        .route("/api/ws", get(super::ws::upgrade))
        .route("/api/rule-runs", get(rule_runs))
        .route("/api/cameras", get(get_cameras))
        .merge(super::camera_stream::router(state.hub.live().clone()))
        .merge(super::events::router())
        .merge(super::push::router())
        .merge(super::install::protected())
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
        .merge(super::webapp::router())
        .merge(super::install::public())
        .route("/api/version", get(version))
        .route(
            "/api/login",
            post(login).layer(DefaultBodyLimit::max(LOGIN_LIMIT)),
        )
        // Hors des routes protégées : elle accepte aussi le jeton de mode (`put_mode`).
        .route(
            "/api/mode",
            put(put_mode).layer(DefaultBodyLimit::max(LOGIN_LIMIT)),
        )
        // Pareil : « Je pars » / « Je rentre » portent le jeton de mode de la personne.
        .route(
            "/api/presence",
            put(put_presence).layer(DefaultBodyLimit::max(LOGIN_LIMIT)),
        )
        .merge(protected)
        .fallback(|| async { ApiErr(ApiError::new(ErrorCode::NotFound)) })
        .layer(DefaultBodyLimit::max(JSON_LIMIT))
        .layer(middleware::map_response(json_errors))
        .layer(middleware::from_fn_with_state(state.clone(), deadline))
        // En dernier (donc à l'extérieur de tout) : les en-têtes communs couvrent aussi les
        // réponses refabriquées par `json_errors` et le 503 de `deadline`.
        .layer(middleware::from_fn(harden))
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
async fn harden(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if is_api {
        // Une réponse d'API n'est jamais une page : rien ne doit s'y charger ni s'y encadrer.
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        );
    }
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
pub(super) struct ApiJson<T>(pub T);

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

/// Ce que demande un client à `GET /api/version` : la version qu'il parle.
#[derive(serde::Deserialize)]
struct VersionQuery {
    protocol: Option<u32>,
}

/// La version du protocole dans laquelle le serveur répond à un client qui parle `asked` (`None` :
/// il ne le dit pas, c'est un client d'avant la version 4, donc la version 3). `None` en retour :
/// aucune version commune.
pub(super) fn negotiate(asked: Option<u32>) -> Option<u32> {
    let asked = asked.unwrap_or(wire::MIN_PROTOCOL_VERSION);
    (wire::MIN_PROTOCOL_VERSION..=wire::PROTOCOL_VERSION)
        .contains(&asked)
        .then_some(asked)
}

async fn version(
    query: Result<Query<VersionQuery>, axum::extract::rejection::QueryRejection>,
) -> Json<VersionInfo> {
    let asked = query.ok().and_then(|Query(q)| q.protocol);
    Json(VersionInfo {
        // Un client de version 3 (2.0.0, 2.1.0-beta.1) compare ce nombre au sien et refuse tout
        // autre : on lui répond « 3 ». Une version que nous ne parlons pas reçoit la nôtre, pour
        // que le client dise laquelle est en cause.
        protocol_version: negotiate(asked).unwrap_or(wire::PROTOCOL_VERSION),
        server_version: SERVER_VERSION.to_string(),
    })
}

/// D'où vient la requête : par Tailscale (adresse du tailnet, ou `tailscale serve` qui relaie
/// depuis cette machine en laissant son nom `….ts.net` et ses en-têtes d'identité), sinon sur place.
/// Une indication pour s'y retrouver, jamais une décision de sécurité.
pub(super) fn via_of(peer: std::net::IpAddr, headers: &HeaderMap) -> Via {
    let host_is_tailnet = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| {
            h.rsplit_once(':')
                .map_or(h, |(name, port)| {
                    if port.chars().all(|c| c.is_ascii_digit()) {
                        name
                    } else {
                        h
                    }
                })
                .to_ascii_lowercase()
                .ends_with(".ts.net")
        });
    let serve_headers = headers.contains_key("tailscale-user-login");
    let peer = peer.to_canonical();
    let in_tailnet = match peer {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        std::net::IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    };
    if in_tailnet || host_is_tailnet || serve_headers {
        Via::Tailscale
    } else {
        Via::Local
    }
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    let Some(version) = negotiate(Some(request.protocol_version)) else {
        return Err(ApiError::with_detail(
            ErrorCode::VersionIncompatible,
            format!(
                "le serveur parle les versions {} à {} du protocole, le client la version {}",
                wire::MIN_PROTOCOL_VERSION,
                wire::PROTOCOL_VERSION,
                request.protocol_version
            ),
        )
        .into());
    };
    if request.username.len() > 256 || request.password.len() > 1024 {
        return Err(ApiError::new(ErrorCode::InvalidCredentials).into());
    }
    let ip = peer.ip();
    check_not_locked(&state, ip, &request.username)?;
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
        check_not_locked(&state, ip, &request.username)?;
        let auth = state.auth.clone();
        let (username, password) = (request.username.clone(), request.password);
        tokio::task::spawn_blocking(move || auth.accounts.verify(&username, &password))
            .await
            .map_err(|_| ApiError::new(ErrorCode::Internal))?
    };
    match verdict {
        Verdict::Valid(granted) => {
            state.auth.limiter.record_success(ip);
            state.auth.limiter.record_user_success(&request.username);
            // Un client d'avant la version 4 ne dit rien de lui : connexion « inconnue ».
            let installation = clean_installation(request.installation);
            let session = state.auth.sessions.create_from(
                &granted.user_id,
                granted.epoch,
                installation,
                via_of(ip, &headers),
            );
            log::info!("connexion acceptée pour {ip}");
            Ok(Json(LoginResponse {
                token: session.token,
                expires_at: session.expires_at,
                max_expires_at: session.max_expires_at,
                protocol_version: version,
                server_version: SERVER_VERSION.to_string(),
                tailnet_urls: if version >= 4 {
                    super::tailnet_urls(state.listen)
                } else {
                    Vec::new()
                },
            }))
        }
        Verdict::Invalid => {
            let by_ip = state.auth.limiter.record_failure(ip);
            let by_user = state.auth.limiter.record_user_failure(&request.username);
            let wait = by_ip.max(by_user);
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

/// Refuse si l'adresse **ou** l'identifiant demandé est en attente après trop d'échecs.
fn check_not_locked(state: &AppState, ip: std::net::IpAddr, username: &str) -> ApiResult<()> {
    let by_ip = state.auth.limiter.check(ip).err();
    let by_user = state.auth.limiter.check_user(username).err();
    if let Some(wait) = by_ip.max(by_user) {
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

/// Sous-protocole WebSocket que le serveur choisit toujours quand un client le propose : c'est
/// celui-là qui est renvoyé au navigateur, jamais l'entrée qui porte le jeton.
pub const WS_SUBPROTOCOL: &str = "ostal.v1";
/// Sous-protocoles acceptés : le nouveau, et celui de « Matter Home » (un onglet de l'app web
/// resté ouvert sur l'ancienne version se reconnecte sans être rechargé).
pub const WS_SUBPROTOCOLS: [&str; 2] = [WS_SUBPROTOCOL, crate::legacy::WS_SUBPROTOCOL];

/// Jeton d'un navigateur : un `WebSocket` de navigateur ne peut pas poser d'en-tête
/// `Authorization`. L'app web propose donc `Sec-WebSocket-Protocol: ostal.v1,
/// bearer.<jeton>` (jamais le jeton dans l'URL). Accepté **seulement** pour `GET /api/ws` et
/// `GET /api/cameras/{id}/stream` avec un vrai upgrade WebSocket (`Connection: upgrade`,
/// `Sec-WebSocket-Key`, `Sec-WebSocket-Version: 13`) : jamais pour une autre route. Le client
/// natif, qui envoie `Authorization`, n'est pas touché.
pub fn ws_subprotocol_token<'a>(
    method: &axum::http::Method,
    path: &str,
    headers: &'a HeaderMap,
) -> Option<&'a str> {
    if !is_ws_route(method, path) || !is_ws_upgrade(headers) {
        return None;
    }
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|entry| entry.trim().strip_prefix("bearer."))
        .find(|token| !token.is_empty() && token.len() <= 256)
}

/// Les deux seules routes WebSocket.
fn is_ws_route(method: &axum::http::Method, path: &str) -> bool {
    if method != axum::http::Method::GET {
        return false;
    }
    if path == "/api/ws" {
        return true;
    }
    path.strip_prefix("/api/cameras/")
        .and_then(|rest| rest.strip_suffix("/stream"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

fn is_ws_upgrade(headers: &HeaderMap) -> bool {
    let has = |name: header::HeaderName, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    };
    has(header::UPGRADE, "websocket")
        && has(header::CONNECTION, "upgrade")
        && headers.contains_key(header::SEC_WEBSOCKET_KEY)
        && headers
            .get(header::SEC_WEBSOCKET_VERSION)
            .is_some_and(|v| v.as_bytes() == b"13")
}

/// Un navigateur qui ouvre un WebSocket envoie `Origin` : s'il ne correspond pas à `Host`, la page
/// vient d'ailleurs (détournement de WebSocket entre sites) et on refuse. Le client natif n'en
/// envoie pas.
fn origin_mismatch(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return false;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let authority = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://").map(|(_, rest)| rest));
    match (authority, host) {
        (Some(a), Some(h)) => !a.eq_ignore_ascii_case(h),
        _ => true,
    }
}

async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if is_ws_route(request.method(), request.uri().path())
        && is_ws_upgrade(request.headers())
        && origin_mismatch(request.headers())
    {
        let body = ErrorBody {
            error: ApiError::with_detail(ErrorCode::Unauthorized, "origine refusée"),
        };
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }
    let via = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| via_of(c.0.ip(), request.headers()));
    let session = bearer(request.headers())
        .or_else(|| ws_subprotocol_token(request.method(), request.uri().path(), request.headers()))
        .and_then(|token| state.auth.authenticate_from(token, via));
    if let Some(info) = session {
        request.extensions_mut().insert(info);
        return next.run(request).await;
    }
    // Le jeton de lecture (widget de l'iPhone) : seulement sur les deux routes de lecture, et
    // seulement dans `Authorization` (jamais par le sous-protocole d'un WebSocket). Les deux
    // gestionnaires ne lisent pas la session : aucune extension n'est posée.
    if is_read_route(request.method(), request.uri().path()) {
        if let Some(token) = bearer(request.headers()) {
            // Le jeton d'une personne désactivée ne vaut plus rien.
            if state
                .hub
                .verify_read_token(token)
                .is_some_and(|t| state.auth.accounts.is_active(&t.user_id))
            {
                return next.run(request).await;
            }
            // Ni session ni jeton de lecture. Rien ne distingue une session expirée d'un essai :
            // la pause (250 ms, jamais de blocage par adresse : derrière `tailscale serve` tout
            // vient de 127.0.0.1) n'existe donc que s'il y a un jeton de lecture à protéger ;
            // sans widget installé, une session expirée est refusée comme avant, sans attendre.
            if state.hub.any_read_token() {
                let ip = request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .map(|c| c.0.ip().to_string())
                    .unwrap_or_default();
                log::info!(
                    "lecture refusée pour {ip} : session expirée ou jeton de lecture inconnu"
                );
                tokio::time::sleep(FAILURE_PAUSE).await;
            }
        }
    }
    ApiErr(ApiError::new(ErrorCode::Unauthorized)).into_response()
}

/// Les seules routes qu'ouvre le jeton de lecture (le chemin exact, sans la requête `?…`).
fn is_read_route(method: &axum::http::Method, path: &str) -> bool {
    method == axum::http::Method::GET && (path == "/api/state" || path == wire::EVENTS_PATH)
}

/// Qui fait une requête acceptée par un jeton de session **ou** par un jeton de mode.
struct Actor {
    user_id: String,
    /// `true` : le jeton de mode d'un raccourci ; `false` : une session.
    by_mode_token: bool,
}

/// Reconnaît l'auteur d'une requête `PUT /api/mode` ou `PUT /api/presence`. Le jeton est lu dans
/// `Authorization` seulement, jamais dans l'URL, et n'est jamais journalisé. Il est vérifié
/// **d'abord** : un bon jeton n'est jamais refusé, même derrière `tailscale serve` où toutes les
/// connexions viennent de 127.0.0.1 et où un compteur d'échecs par adresse serait partagé par tout
/// le monde. Un échec n'est que compté au journal et retardé de 250 ms (le jeton fait 256 bits :
/// l'essai en force est sans espoir). Le jeton d'une personne désactivée est refusé.
async fn authenticate_mode_request(
    state: &AppState,
    ip: std::net::IpAddr,
    what: &str,
    headers: &HeaderMap,
) -> ApiResult<Actor> {
    let Some(token) = bearer(headers).map(str::to_owned) else {
        return Err(ApiError::new(ErrorCode::Unauthorized).into());
    };
    if let Some(session) = state.auth.authenticate(&token) {
        return Ok(Actor {
            user_id: session.user_id,
            by_mode_token: false,
        });
    }
    if let Some(owner) = state
        .hub
        .verify_mode_token(&token)
        .filter(|t| state.auth.accounts.is_active(&t.user_id))
    {
        return Ok(Actor {
            user_id: owner.user_id,
            by_mode_token: true,
        });
    }
    log::warn!("{what} refusé pour {ip} : jeton invalide");
    tokio::time::sleep(FAILURE_PAUSE).await;
    Err(ApiError::new(ErrorCode::Unauthorized).into())
}

/// `PUT /api/mode` : change le mode de la maison. Accepte un jeton de session (clients) **ou** le
/// jeton de mode d'une personne (Raccourcis de l'iPhone), qui ne vaut que pour cette route et
/// `PUT /api/presence`. L'authentification passe avant la lecture du corps. Le mode reste commun à
/// toute la maison ; le journal technique dit quelle personne l'a demandé.
async fn put_mode(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> ApiResult<Json<ModeResponse>> {
    let ip = peer.ip();
    let actor =
        authenticate_mode_request(&state, ip, "changement de mode", request.headers()).await?;
    let ApiBytes(bytes) = ApiBytes::from_request(request, &()).await?;
    let ModeRequest { mode, source } = serde_json::from_slice(&bytes).map_err(|_| {
        ApiErr(ApiError::with_detail(
            ErrorCode::BadRequest,
            "mode attendu : present, absent, nuit ou vacances",
        ))
    })?;
    // La source annoncée n'est retenue que si le jeton la permet (un raccourci ne peut pas se
    // dire « automatisation », une session pas « raccourci ») ; un ancien client n'en dit rien.
    let source = ModeSource::claimed(source, actor.by_mode_token);
    let hub = state.hub.clone();
    let label = source.label();
    let changed = blocking(move || hub.set_mode(mode, source)).await?;
    let who = state
        .auth
        .accounts
        .display_name(&actor.user_id)
        .unwrap_or_else(|| "?".into());
    log::info!(
        "mode demandé par {who} depuis {ip} ({}, {label}) : {}",
        if actor.by_mode_token {
            "jeton de mode"
        } else {
            "session"
        },
        mode.label()
    );
    Ok(Json(ModeResponse { mode, changed }))
}

/// `PUT /api/presence` `{"state": "home" | "away"}` : la personne dit où elle est (raccourcis « Je
/// rentre » et « Je pars »). Mêmes jetons que `PUT /api/mode`. Le serveur retient seulement ce
/// que chacun a déclaré : le mode de la maison ne change pas pour autant.
async fn put_presence(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> ApiResult<Json<PresenceResponse>> {
    let ip = peer.ip();
    let actor = authenticate_mode_request(&state, ip, "présence", request.headers()).await?;
    let ApiBytes(bytes) = ApiBytes::from_request(request, &()).await?;
    let PresenceRequest { state: wanted } = serde_json::from_slice(&bytes).map_err(|_| {
        ApiErr(ApiError::with_detail(
            ErrorCode::BadRequest,
            "état attendu : home ou away",
        ))
    })?;
    let source = if actor.by_mode_token {
        crate::presence::Source::Shortcut
    } else {
        crate::presence::Source::Session
    };
    let hub = state.hub.clone();
    let user_id = actor.user_id.clone();
    let changed = blocking(move || hub.declare_presence(&user_id, wanted, source)).await?;
    // PROVISOIRE, jusqu'à la présence automatique (étape suivante) : un raccourci « Je pars » ou « Je
    // rentre » fait ce que faisaient les raccourcis Absent et Présent, pour que les téléphones n'aient
    // rien à réinstaller. L'étape suivante remplace ce bloc par la règle « Absent quand le dernier est
    // parti, Présent dès que quelqu'un rentre » ; une session, elle, ne fait que déclarer.
    if actor.by_mode_token {
        let mode = match wanted {
            crate::presence::Whereabouts::Away => protocol::mode::HomeMode::Absent,
            crate::presence::Whereabouts::Home => protocol::mode::HomeMode::Present,
        };
        let hub = state.hub.clone();
        blocking(move || hub.set_mode(mode, ModeSource::Shortcut)).await?;
    }
    let who = state
        .auth
        .accounts
        .display_name(&actor.user_id)
        .unwrap_or_else(|| "?".into());
    log::info!(
        "présence déclarée par {who} depuis {ip} : {}",
        match wanted {
            crate::presence::Whereabouts::Home => "à la maison",
            crate::presence::Whereabouts::Away => "partie",
        }
    );
    Ok(Json(PresenceResponse {
        state: wanted,
        changed,
    }))
}

/// Corps de `PUT /api/presence`.
#[derive(serde::Deserialize)]
struct PresenceRequest {
    state: crate::presence::Whereabouts,
}

/// Réponse de `PUT /api/presence` (200). `changed` est faux si la personne l'avait déjà dit.
#[derive(serde::Serialize)]
struct PresenceResponse {
    state: crate::presence::Whereabouts,
    changed: bool,
}

async fn logout(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> StatusCode {
    state.auth.sessions.revoke(&session.id);
    StatusCode::NO_CONTENT
}

/// `GET /api/me/installations` : les connexions ouvertes sur le compte de la personne (jamais
/// celles des autres), la plus récente d'abord.
async fn my_connections(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<ConnectionsResponse> {
    let connections = state
        .auth
        .sessions
        .connections()
        .into_iter()
        .filter(|c| c.user_id == session.user_id)
        .map(|c| ConnectionView {
            current: c.session_id == session.id,
            id: c.id,
            name: c.name,
            kind: c.kind,
            device: c.device,
            last_seen: c.last_seen,
            via: c.via,
            unknown: c.unknown,
        })
        .collect();
    Json(ConnectionsResponse { connections })
}

/// `DELETE /api/me/installations/{id}` : coupe une connexion de la personne. Le jeton de cette
/// connexion cesse de marcher tout de suite ; ses flux ouverts se ferment. Les connexions des
/// autres comptes ne se coupent pas d'ici (404, comme si elles n'existaient pas).
async fn cut_connection(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if state
        .auth
        .sessions
        .cut_connection(Some(&session.user_id), &id)
        == 0
    {
        return Err(ApiError::new(ErrorCode::NotFound).into());
    }
    log::info!("connexion coupée par son propriétaire");
    Ok(StatusCode::NO_CONTENT)
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
