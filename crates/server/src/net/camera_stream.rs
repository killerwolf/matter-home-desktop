//! `GET /api/cameras/{id}/stream?kind=main|sub` : le direct d'une caméra, en WebSocket.
//!
//! Sous le même jeton que le reste de l'API (en-tête `Authorization`, jamais dans l'URL). Le
//! serveur relaie le H.264 de la caméra sans le modifier (voir `cameras/live.rs` pour la session
//! partagée et `protocol::wire::stream` pour les messages) :
//!
//! - texte : un `StreamStatus` à chaque changement d'état de la liaison avec la caméra ;
//! - binaire : une configuration, puis les images.
//!
//! Refus avant l'ouverture : `404` avec un `detail` si la caméra n'existe pas, `400` si le flux
//! demandé n'existe pas pour elle (pas de flux réduit) ou si `kind` est faux, `503` s'il y a trop
//! de WebSockets ouverts. Un serveur sans cette route répond `404` **sans** `detail` : c'est ainsi
//! qu'un client reconnaît un serveur trop ancien.
//!
//! Le délai d'écriture des autres WebSockets (`client_write_timeout`, 10 s) ne s'applique pas : un
//! client qui lit lentement perd des images jusqu'à la prochaine image clé, il n'est coupé que
//! s'il n'avance plus du tout pendant `LiveConfig::write_stall`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use protocol::wire::{self, ApiError, ErrorCode, StreamKind, StreamStatus};

use super::routes::ApiErr;
use super::AppState;
use crate::cameras::live::{Event, LiveConfig, LiveRegistry, SubscribeError, Subscription};
use crate::cameras::snapshot::{SnapshotConfig, SnapshotService};
use crate::security::sessions::SessionInfo;

const PING_EVERY: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const SESSION_CHECK_EVERY: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct StreamQuery {
    kind: Option<String>,
}

/// La route du flux, avec son registre de sessions. À fusionner dans les routes protégées.
pub fn router() -> Router<AppState> {
    router_with(LiveConfig::default())
}

pub fn router_with(config: LiveConfig) -> Router<AppState> {
    router_with_snapshots(config, SnapshotConfig::default())
}

/// Le flux et l'image fixe (ticket n° 12) partagent le même registre de sessions : une vignette
/// se joint au direct déjà ouvert au lieu d'ouvrir une connexion de plus.
pub fn router_with_snapshots(config: LiveConfig, snapshots: SnapshotConfig) -> Router<AppState> {
    Router::new()
        .route(wire::CAMERA_STREAM_PATH, get(upgrade))
        .route(
            wire::CAMERA_SNAPSHOT_PATH,
            get(super::camera_snapshot::snapshot),
        )
        .layer(Extension(LiveRegistry::new(config)))
        .layer(Extension(SnapshotService::new(snapshots)))
}

async fn upgrade(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Extension(live): Extension<Arc<LiveRegistry>>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let bad = |detail: &str| ApiErr(ApiError::with_detail(ErrorCode::BadRequest, detail));
    let Some(id) = id.parse::<u64>().ok().filter(|id| wire::is_camera_id(*id)) else {
        return bad("identifiant de caméra invalide").into_response();
    };
    let kind = match query.kind.as_deref() {
        None => StreamKind::Sub,
        Some(key) => match StreamKind::from_key(key) {
            Some(kind) => kind,
            None => return bad("kind attend main ou sub").into_response(),
        },
    };
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
    let subscription = match live.subscribe(&state.hub, id, kind) {
        Ok(subscription) => subscription,
        Err(SubscribeError::UnknownCamera) => {
            // Un `detail` : c'est ce qui distingue une caméra inconnue d'une route inconnue.
            return ApiErr(ApiError::with_detail(
                ErrorCode::NotFound,
                "caméra inconnue",
            ))
            .into_response();
        }
        Err(SubscribeError::NoSuchStream) => {
            return bad("cette caméra n'a pas de flux réduit").into_response()
        }
    };
    let stall = live.config().write_stall;
    ws.protocols([super::routes::WS_SUBPROTOCOL])
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            viewer_loop(socket, state, session, account_id, subscription, stall).await;
        })
}

async fn send(socket: &mut WebSocket, message: Message, stall: Duration) -> bool {
    match tokio::time::timeout(stall, socket.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            log::warn!(
                "spectateur vidéo déconnecté : écriture bloquée depuis plus de {} s",
                stall.as_secs()
            );
            false
        }
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str, stall: Duration) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = send(
        socket,
        Message::Close(Some(frame)),
        stall.min(Duration::from_secs(5)),
    )
    .await;
}

async fn viewer_loop(
    mut socket: WebSocket,
    state: AppState,
    session: SessionInfo,
    account_id: String,
    mut subscription: Subscription,
    stall: Duration,
) {
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut session_check = tokio::time::interval(SESSION_CHECK_EVERY);
    let mut shutdown = state.shutdown.clone();
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            event = subscription.next() => match event {
                Event::Video(bytes) => {
                    if !send(&mut socket, Message::Binary(bytes), stall).await {
                        return;
                    }
                }
                Event::Status(status) => {
                    let Ok(json) = serde_json::to_string(&status) else { return };
                    if !send(&mut socket, Message::text(json), stall).await {
                        return;
                    }
                    if matches!(status, StreamStatus::Unsupported { .. }) {
                        close(&mut socket, 1000, "codec non pris en charge", stall).await;
                        return;
                    }
                }
                Event::End(_) => {
                    close(&mut socket, 1001, "caméra modifiée ou lecture terminée", stall).await;
                    return;
                }
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => last_heard = Instant::now(),
            },
            _ = ping.tick() => {
                if last_heard.elapsed() > IDLE_LIMIT {
                    return;
                }
                if !send(&mut socket, Message::Ping(Vec::new().into()), stall).await {
                    return;
                }
            }
            _ = session_check.tick() => {
                if !state.auth.sessions.is_valid(&session.id, &account_id) {
                    close(&mut socket, 1008, "session terminée", stall).await;
                    return;
                }
            }
            _ = shutdown.changed() => {
                close(&mut socket, 1001, "arrêt du serveur", stall).await;
                return;
            }
        }
    }
}
