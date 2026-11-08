//! Réseau : HTTPS (axum derrière rustls) pour la connexion et les requêtes, WebSocket pour
//! le temps réel. Une boucle d'acceptation TLS minimale pilote hyper : pas de socket sans
//! chiffrement, délais sur la poignée de main et sur les en-têtes, nombre de connexions borné.

mod routes;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::connect_info::ConnectInfo;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};
use tower::ServiceExt;

use crate::hub::Hub;
use crate::security::account::AccountStore;
use crate::security::limiter::LoginLimiter;
use crate::security::sessions::SessionStore;

/// Connexions simultanées au plus ; au-delà, les nouvelles sont fermées aussitôt.
const MAX_CONNECTIONS: usize = 128;
/// WebSockets ouverts en même temps au plus.
const MAX_WEBSOCKETS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// Délai accordé aux connexions en cours à l'arrêt.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Réglages du réseau.
#[derive(Clone)]
pub struct NetConfig {
    pub listen: SocketAddr,
    /// Délai d'écriture d'un message vers un client WebSocket avant de le déconnecter.
    pub client_write_timeout: Duration,
}

/// Ce que partagent les gestionnaires de requêtes.
#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub auth: Arc<Auth>,
    pub shutdown: watch::Receiver<bool>,
    pub ws_slots: Arc<Semaphore>,
    pub client_write_timeout: Duration,
}

/// Compte, sessions et limitation des tentatives.
pub struct Auth {
    pub accounts: AccountStore,
    pub sessions: SessionStore,
    pub limiter: LoginLimiter,
    /// Vérifications de mot de passe simultanées (argon2 consomme de la mémoire).
    pub hash_slots: Semaphore,
}

impl Auth {
    pub fn new(accounts: AccountStore, sessions: SessionStore) -> Self {
        Self {
            accounts,
            sessions,
            limiter: LoginLimiter::new(),
            hash_slots: Semaphore::new(2),
        }
    }
}

/// Serveur réseau en marche, sur son propre runtime.
pub struct NetServer {
    pub addr: SocketAddr,
    runtime: Option<tokio::runtime::Runtime>,
    shutdown: watch::Sender<bool>,
    connections: Arc<Semaphore>,
}

pub fn start(
    config: NetConfig,
    hub: Arc<Hub>,
    auth: Arc<Auth>,
    tls: Arc<rustls::ServerConfig>,
) -> Result<NetServer> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("net")
        .enable_all()
        .build()
        .context("runtime réseau")?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = AppState {
        hub,
        auth,
        shutdown: shutdown_rx.clone(),
        ws_slots: Arc::new(Semaphore::new(MAX_WEBSOCKETS)),
        client_write_timeout: config.client_write_timeout,
    };
    let router = routes::router(state);
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let listener = runtime
        .block_on(TcpListener::bind(config.listen))
        .with_context(|| format!("écoute sur {}", config.listen))?;
    let addr = listener.local_addr().context("adresse d'écoute")?;
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    runtime.spawn(accept_loop(
        listener,
        acceptor,
        router,
        shutdown_rx,
        connections.clone(),
    ));
    Ok(NetServer {
        addr,
        runtime: Some(runtime),
        shutdown: shutdown_tx,
        connections,
    })
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    router: axum::Router,
    mut shutdown: watch::Receiver<bool>,
    connections: Arc<Semaphore>,
) {
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    log::warn!("accept : {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = shutdown.changed() => break,
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            // Trop de connexions : on ferme sans rien dire.
            drop(stream);
            continue;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        let mut shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = stream.set_nodelay(true);
            let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(tls)) => tls,
                // Poignée de main ratée : client qui n'épingle pas le bon certificat, ou
                // balayage de ports. Pas de journal à chaque fois.
                _ => return,
            };
            let service = TowerToHyperService::new(tower::service_fn(
                move |mut request: axum::extract::Request<hyper::body::Incoming>| {
                    request.extensions_mut().insert(ConnectInfo(peer));
                    router.clone().oneshot(request)
                },
            ));
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .serve_connection(TokioIo::new(tls), service)
                .with_upgrades();
            tokio::pin!(connection);
            tokio::select! {
                _ = connection.as_mut() => {}
                _ = shutdown.changed() => {
                    connection.as_mut().graceful_shutdown();
                    let _ = tokio::time::timeout(DRAIN_TIMEOUT, connection).await;
                }
            }
        });
    }
}

impl NetServer {
    /// Arrête l'écoute, laisse les requêtes en cours se terminer (au plus quelques secondes)
    /// puis libère le runtime.
    pub fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(runtime) = self.runtime.take() {
            let connections = self.connections.clone();
            runtime.block_on(async move {
                let all = MAX_CONNECTIONS as u32;
                let _ = tokio::time::timeout(
                    DRAIN_TIMEOUT + Duration::from_secs(1),
                    connections.acquire_many(all),
                )
                .await;
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for NetServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
