//! « Mes agents » : une personne connectée crée, voit et révoque **ses** agents d'IA (Claude Code,
//! Cursor…) depuis l'app web. Sous le jeton de **session** comme le reste de l'API : un jeton
//! d'agent, de mode ou de lecture n'ouvre aucune de ces routes (un agent ne se crée pas d'agent
//! et ne se donne pas de droits).
//!
//! | route | réponse |
//! |---|---|
//! | `GET /api/agents` | `{enabled, default_days, max_days, agents: [AgentView], never, sensitive}` : les agents **de cette personne** |
//! | `POST /api/agents` | `201 {agent, token}` : le jeton en clair, **cette seule fois** (`Cache-Control: no-store`) |
//! | `DELETE /api/agents/{id}` | `202` : révoqué ; `404` pour un agent inconnu **ou à quelqu'un d'autre** (même réponse) |
//!
//! L'interrupteur général, les listes « jamais » et « sensible » et « Tout couper » ne se règlent
//! **pas** d'ici : seulement dans la fenêtre d'Ostal Server (Réglages › Agents IA) ou en ligne de
//! commande, c'est-à-dire par quelqu'un qui est devant le Mac du serveur. L'app web les montre.
//!
//! Chaque droit demandé doit être **un droit du compte lui-même** (on ne donne pas à un agent ce
//! qu'on n'a pas) : « commandes » demande de pouvoir commander, « mode » de pouvoir changer le mode.
//! Les appareils permis existent, sont des prises, des lampes ou des volets, et ne sont pas dans la
//! liste « jamais ». Jamais le jeton ni son empreinte dans une réponse de lecture ni au journal.

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{ApiErr, ApiJson, Ctx};
use super::AppState;
use crate::audit::{Action, Entry};
use crate::rights::Capability;
use crate::security::agent_tokens::{
    self, AgentInfo, Scope, DEFAULT_DAYS, MAX_DAYS, MAX_DEVICES_PER_AGENT,
};
use crate::security::sessions::SessionInfo;

/// Le libellé d'un agent : 48 caractères, comme celui d'un jeton de raccourci (le fichier garde au
/// plus ces 48 : plus long, il serait coupé en silence, donc on le refuse).
const MAX_LABEL: usize = 48;

/// Un corps de création tient dans quelques centaines d'octets (20 appareils au plus).
const BODY_LIMIT: usize = 4 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/agents",
            get(list)
                .post(create)
                .layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route("/api/agents/{id}", delete(revoke))
}

/// Ce que l'app web montre d'un agent : jamais le jeton ni son empreinte.
fn view(agent: &AgentInfo, state: &AppState) -> Value {
    let scopes: Vec<&str> = agent
        .scopes
        .iter()
        .filter(|s| **s != Scope::Unknown)
        .map(|s| s.word())
        .collect();
    let usage = state.auth.agents.usage(&agent.id);
    json!({
        "id": agent.id,
        "label": agent.label,
        "scopes": scopes,
        "devices": agent.devices,
        "created": agent.created,
        "expires": agent.expires,
        "expired": agent.is_expired(),
        // Depuis le démarrage du serveur seulement (compteurs en mémoire).
        "last_used": usage.map(|u| u.last),
        "calls_today": usage.map_or(0, |u| u.today),
    })
}

async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<Value> {
    let agents: Vec<Value> = state
        .auth
        .agents
        .agents_of(&session.user_id)
        .iter()
        .map(|a| view(a, &state))
        .collect();
    let (never, sensitive) = state.auth.agents.lists();
    Json(json!({
        "enabled": state.auth.agents.enabled(),
        "default_days": DEFAULT_DAYS,
        "max_days": MAX_DAYS,
        "max_devices": MAX_DEVICES_PER_AGENT,
        "agents": agents,
        // Les appareils que l'app ne propose pas (« jamais ») ou qui demanderont toujours l'accord
        // (« sensible ») : des identifiants, rien d'autre.
        "never": never.iter().map(u64::to_string).collect::<Vec<_>>(),
        "sensitive": sensitive.iter().map(u64::to_string).collect::<Vec<_>>(),
        // Ce que ce compte peut donner : jamais plus que ce qu'il a lui-même.
        "can": {
            "control": state.hub.rights().can(&session.user_id, Capability::ControlDevices),
            "mode": state.hub.rights().can(&session.user_id, Capability::ChangeMode),
        },
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    label: String,
    #[serde(default = "default_days")]
    days: u32,
    /// Les droits cochés. Un mot inconnu est refusé.
    scopes: Vec<Scope>,
    /// Les appareils permis (identifiants décimaux), avec le droit « commandes » seulement.
    #[serde(default)]
    devices: Vec<String>,
}

fn default_days() -> u32 {
    DEFAULT_DAYS
}

fn bad(text: &str) -> ApiErr {
    ApiErr(ApiError::with_detail(ErrorCode::BadRequest, text))
}

fn forbidden(text: &str) -> ApiErr {
    ApiErr(ApiError::with_detail(ErrorCode::Forbidden, text))
}

async fn create(
    State(state): State<AppState>,
    Ctx(who): Ctx,
    Extension(session): Extension<SessionInfo>,
    ApiJson(request): ApiJson<CreateRequest>,
) -> Result<Response, ApiErr> {
    let label = request.label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL {
        return Err(bad(&format!(
            "donnez un nom de {MAX_LABEL} caractères au plus à cet agent"
        )));
    }
    if request.days == 0 || request.days > MAX_DAYS {
        return Err(bad(&format!("la durée va de 1 à {MAX_DAYS} jours")));
    }
    let scopes =
        agent_tokens::normalize_scopes(&request.scopes).map_err(|_| bad("droit inconnu"))?;
    // Le compte ne donne que ce qu'il a lui-même.
    let can = |c| state.hub.rights().can(&session.user_id, c);
    if scopes.contains(&Scope::Control) && !can(Capability::ControlDevices) {
        return Err(forbidden(
            "ce compte n'a pas le droit de commander la maison",
        ));
    }
    if scopes.contains(&Scope::Mode) && !can(Capability::ChangeMode) {
        return Err(forbidden("ce compte n'a pas le droit de changer le mode"));
    }
    if !can(Capability::View) {
        return Err(forbidden("ce compte n'a pas le droit de voir la maison"));
    }
    // Les appareils : identifiants canoniques, existants, prises, lampes ou volets.
    let mut devices: Vec<u64> = Vec::new();
    for text in &request.devices {
        let id =
            protocol::wire::parse_id(text).map_err(|_| bad("identifiant d'appareil invalide"))?;
        if !devices.contains(&id) {
            devices.push(id);
        }
    }
    if scopes.contains(&Scope::Control) {
        if devices.is_empty() {
            return Err(bad(
                "choisissez au moins un appareil que cet agent peut commander",
            ));
        }
    } else if !devices.is_empty() {
        return Err(bad(
            "des appareils permis n'ont de sens qu'avec le droit de commander",
        ));
    }
    if devices.len() > MAX_DEVICES_PER_AGENT {
        return Err(bad(&format!(
            "{MAX_DEVICES_PER_AGENT} appareils permis au plus par agent"
        )));
    }
    let snapshot = state.hub.current_snapshot();
    for id in &devices {
        let ok = snapshot
            .devices
            .iter()
            .find(|v| v.device.node_id == *id)
            .is_some_and(|v| v.device.caps.on_off || v.device.caps.cover);
        if !ok {
            return Err(bad(
                "un des appareils n'existe pas ou ne se commande pas (prises, lampes et volets seulement)",
            ));
        }
        if state.auth.agents.never_commanded(*id) {
            return Err(bad(
                "un des appareils est dans la liste « jamais par un agent » d'Ostal Server",
            ));
        }
    }
    let (user_id, user_name) = (
        session.user_id.clone(),
        state.auth.accounts.display_name(&session.user_id),
    );
    let dir = state.auth.accounts.dir().to_path_buf();
    let label = label.to_string();
    let days = request.days;
    let created = tokio::task::spawn_blocking(move || {
        agent_tokens::add_with(&dir, &user_id, &label, days, &scopes, &devices)
    })
    .await
    .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?;
    let (agent, token) = match created {
        Ok(done) => done,
        Err(e) => {
            // Les refus du fichier (16 agents, libellé déjà pris, appareil « jamais ») sont des
            // phrases françaises sans secret ; une panne de disque ne se détaille pas.
            let text = format!("{e:#}");
            log::warn!("agent d'IA non créé : {text}");
            return Err(if is_user_error(&text) {
                bad(&text)
            } else {
                ApiErr(ApiError::new(ErrorCode::Internal))
            });
        }
    };
    let log = state.hub.audit().clone();
    let entry = Entry::new(Action::AgentCreated, &who)
        .target(&agent.label)
        .detail(user_name.as_deref().unwrap_or(""));
    let _ = tokio::task::spawn_blocking(move || log.record(entry)).await;
    let mut response = (
        StatusCode::CREATED,
        Json(json!({"agent": view(&agent, &state), "token": token})),
    )
        .into_response();
    // Un jeton ne se garde dans aucun cache.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Les refus de `add_with` qui se disent à la personne.
fn is_user_error(text: &str) -> bool {
    text.contains("agents au plus par personne")
        || text.contains("a déjà un agent")
        || text.contains("liste « jamais")
        || text.contains("la durée va de")
        || text.contains("appareils permis")
}

async fn revoke(
    State(state): State<AppState>,
    Ctx(who): Ctx,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<Response, ApiErr> {
    let unknown = || ApiErr(ApiError::with_detail(ErrorCode::NotFound, "agent inconnu"));
    // Un identifiant d'agent : 32 chiffres hexadécimaux. Tout le reste n'existe pas.
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(unknown());
    }
    let dir = state.auth.accounts.dir().to_path_buf();
    let user_id = session.user_id.clone();
    let gone = tokio::task::spawn_blocking(move || agent_tokens::revoke_owned(&dir, &user_id, &id))
        .await
        .map_err(|_| ApiErr(ApiError::new(ErrorCode::Internal)))?
        .map_err(|e| {
            log::warn!("agent d'IA non révoqué : {e:#}");
            ApiErr(ApiError::new(ErrorCode::Internal))
        })?;
    let Some(gone) = gone else {
        return Err(unknown());
    };
    let log = state.hub.audit().clone();
    let entry = Entry::new(Action::AgentRevoked, &who).target(&gone.label);
    let _ = tokio::task::spawn_blocking(move || log.record(entry)).await;
    Ok((StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response())
}
