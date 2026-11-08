//! WebSocket du temps réel : l'état complet à l'ouverture, puis chaque événement.
//! Le client n'écrit rien (ses commandes passent par l'API HTTPS) ; un client trop lent est
//! resynchronisé avec l'état complet, ou déconnecté s'il ne suit toujours pas.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use protocol::wire::{self, ApiError, ErrorCode, ServerMessage};

use super::routes::ApiErr;
use super::AppState;
use crate::security::sessions::SessionInfo;

const PING_EVERY: Duration = Duration::from_secs(20);
/// Sans aucun signe de vie du client pendant ce temps, on ferme.
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const SESSION_CHECK_EVERY: Duration = Duration::from_secs(2);
/// Un client resynchronisé plus de `MAX_LAGS` fois dans `LAG_WINDOW` est déconnecté.
const MAX_LAGS: usize = 3;
const LAG_WINDOW: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct WsQuery {
    /// Version du protocole parlée par le client.
    protocol: Option<u32>,
}

pub async fn upgrade(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    query: Result<Query<WsQuery>, axum::extract::rejection::QueryRejection>,
    ws: WebSocketUpgrade,
) -> Response {
    let protocol = query.ok().and_then(|Query(q)| q.protocol);
    if protocol != Some(wire::PROTOCOL_VERSION) {
        return ApiErr(ApiError::with_detail(
            ErrorCode::VersionIncompatible,
            format!(
                "indiquez protocol={} dans l'URL du WebSocket",
                wire::PROTOCOL_VERSION
            ),
        ))
        .into_response();
    }
    let Ok(slot) = state.ws_slots.clone().try_acquire_owned() else {
        return ApiErr(ApiError::with_detail(
            ErrorCode::Unavailable,
            "trop de clients connectés",
        ))
        .into_response();
    };
    let Some(account_id) = state.auth.accounts.account_id() else {
        return ApiErr(ApiError::new(ErrorCode::Unauthorized)).into_response();
    };
    ws.max_message_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            client_loop(socket, state, session, account_id).await;
        })
}

/// Envoie un message avec un délai : un client qui n'avance plus ne retient personne.
async fn send(socket: &mut WebSocket, message: Message, timeout: Duration) -> bool {
    match tokio::time::timeout(timeout, socket.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            log::warn!(
                "client WebSocket déconnecté : écriture bloquée depuis plus de {} ms",
                timeout.as_millis()
            );
            false
        }
    }
}

fn text(json: &str) -> Message {
    Message::text(json.to_string())
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str, timeout: Duration) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = send(socket, Message::Close(Some(frame)), timeout).await;
}

async fn client_loop(
    mut socket: WebSocket,
    state: AppState,
    session: SessionInfo,
    account_id: String,
) {
    let timeout = state.client_write_timeout;
    let hello = ServerMessage::Hello {
        protocol_version: wire::PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let Ok(hello) = serde_json::to_string(&hello) else {
        return;
    };
    if !send(&mut socket, text(&hello), timeout).await {
        return;
    }
    let subscription = state.hub.subscribe();
    let mut rx = subscription.rx;
    if !send(&mut socket, text(&subscription.snapshot), timeout).await {
        return;
    }

    let mut ping = tokio::time::interval(PING_EVERY);
    let mut session_check = tokio::time::interval(SESSION_CHECK_EVERY);
    let mut shutdown = state.shutdown.clone();
    let mut last_heard = Instant::now();
    let mut lags: VecDeque<Instant> = VecDeque::new();
    loop {
        tokio::select! {
            message = rx.recv() => match message {
                Ok(json) => {
                    if !send(&mut socket, text(&json), timeout).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    let now = Instant::now();
                    lags.push_back(now);
                    while lags.front().is_some_and(|t| now.duration_since(*t) > LAG_WINDOW) {
                        lags.pop_front();
                    }
                    if lags.len() > MAX_LAGS {
                        log::warn!("client trop lent ({missed} messages perdus) : déconnecté");
                        close(&mut socket, 1013, "client trop lent", timeout).await;
                        return;
                    }
                    // Les messages perdus ne se rattrapent pas : on repart de l'état complet.
                    let fresh = state.hub.subscribe();
                    rx = fresh.rx;
                    if !send(&mut socket, text(&fresh.snapshot), timeout).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                // Pong, ou tout autre message : le client est vivant. Le contenu est ignoré.
                Some(Ok(_)) => last_heard = Instant::now(),
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > IDLE_LIMIT {
                    return;
                }
                if !send(&mut socket, Message::Ping(Vec::new().into()), timeout).await {
                    return;
                }
            }
            _ = session_check.tick() => {
                // Déconnexion, expiration ou remplacement du compte : le flux s'arrête.
                if !state.auth.sessions.is_valid(&session.id, &account_id) {
                    close(&mut socket, 1008, "session terminée", timeout).await;
                    return;
                }
            }
            _ = shutdown.changed() => {
                close(&mut socket, 1001, "arrêt du serveur", timeout).await;
                return;
            }
        }
    }
}
