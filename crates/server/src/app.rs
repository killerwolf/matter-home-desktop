//! Assemblage du serveur : backend (Matter ou démo), agrégation des événements, réseau.
//! Le binaire (`main.rs`) s'en sert, et les tests d'intégration aussi.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::events::{Event, EventSink};
use crate::history_store::{self, HistoryStore};
use crate::hub::{ExitReason, Hub, HubConfig};
use crate::net::{self, Auth, NetConfig, NetServer};
use crate::security::account::AccountStore;
use crate::security::sessions::SessionStore;
use crate::security::{self, tls};
use crate::{demo, matter, persistence, Backend};

/// Délai laissé au backend et à l'écriture de l'historique pour s'arrêter.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Dossier de données (en mode réel, c'est `persistence::data_dir()`).
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub demo: bool,
    /// Messages gardés pour un client lent avant de le resynchroniser.
    pub broadcast_capacity: usize,
    /// Délai d'écriture vers un client avant de le déconnecter.
    pub client_write_timeout: Duration,
    /// Délai sans octet reçu sur le corps d'une requête avant de l'abandonner.
    pub body_idle_timeout: Duration,
}

impl ServerConfig {
    pub fn new(data_dir: PathBuf, listen: SocketAddr, demo: bool) -> Self {
        Self {
            data_dir,
            listen,
            demo,
            broadcast_capacity: 256,
            client_write_timeout: Duration::from_secs(10),
            body_idle_timeout: crate::net::BODY_IDLE_TIMEOUT,
        }
    }
}

/// Pourquoi `Server::wait` a rendu la main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Arrêt demandé (signal).
    Stopped,
    /// Erreur fatale du backend : le serveur doit quitter en erreur pour être relancé.
    Fatal,
    /// Une sauvegarde importée attend d'être appliquée au prochain lancement.
    ImportStaged,
}

pub struct Server {
    hub: Arc<Hub>,
    net: Option<NetServer>,
    backend: Option<Backend>,
    pump: Option<JoinHandle<()>>,
    demo_history_writer: Option<JoinHandle<()>>,
    stop_pump: Arc<AtomicBool>,
    fatal: Arc<AtomicBool>,
    pub addr: SocketAddr,
    /// Empreinte SHA-256 du certificat, `AA:BB:…`.
    pub fingerprint: String,
    /// Le certificat vient d'être créé.
    pub certificate_created: bool,
    pub has_account: bool,
}

impl Server {
    pub fn start(config: ServerConfig) -> Result<Server> {
        let dir = security::server_dir(&config.data_dir)?;
        let identity = tls::load_or_create(&dir)?;
        let accounts = AccountStore::open(&dir)?;
        let has_account = accounts.exists();
        let sessions = SessionStore::open(&dir)?;

        let (tx, rx) = mpsc::channel();
        let sink = EventSink::new(tx);
        let (backend, demo_history, history_dir) = if config.demo {
            let history_dir = config.data_dir.join("history-demo");
            let (history_sink, writer) =
                history_store::start(HistoryStore::new(history_dir.clone()));
            (demo::spawn(sink), Some((history_sink, writer)), history_dir)
        } else {
            if persistence::data_dir() != config.data_dir {
                bail!(
                    "le dossier de données du serveur doit être celui de persistence::data_dir()"
                );
            }
            (matter::spawn(sink), None, history_store::dir())
        };
        let (demo_history_sink, demo_history_writer) = match demo_history {
            Some((sink, writer)) => (Some(sink), Some(writer)),
            None => (None, None),
        };
        let hub = Arc::new(Hub::new(HubConfig {
            data_dir: config.data_dir.clone(),
            demo: config.demo,
            history_dir,
            demo_history: demo_history_sink,
            broadcast_capacity: config.broadcast_capacity,
            commands: backend.sender(),
        }));

        let stop_pump = Arc::new(AtomicBool::new(false));
        let fatal = Arc::new(AtomicBool::new(false));
        let pump = {
            let (hub, stop, fatal) = (hub.clone(), stop_pump.clone(), fatal.clone());
            std::thread::Builder::new()
                .name("evenements".into())
                .spawn(move || pump_events(rx, &hub, &stop, &fatal))
                .context("thread des événements")?
        };

        let auth = Arc::new(Auth::new(accounts, sessions));
        let net = match net::start(
            NetConfig {
                listen: config.listen,
                client_write_timeout: config.client_write_timeout,
                body_idle_timeout: config.body_idle_timeout,
            },
            hub.clone(),
            auth,
            identity.config.clone(),
        ) {
            Ok(net) => net,
            Err(e) => {
                // Le réseau n'a pas démarré : on remet tout en ordre avant de remonter l'erreur.
                hub.close_commands();
                stop_pump.store(true, Ordering::SeqCst);
                backend.shutdown(SHUTDOWN_TIMEOUT);
                let _ = pump.join();
                return Err(e);
            }
        };
        Ok(Server {
            addr: net.addr,
            hub,
            net: Some(net),
            backend: Some(backend),
            pump: Some(pump),
            demo_history_writer,
            stop_pump,
            fatal,
            fingerprint: identity.fingerprint,
            certificate_created: identity.created,
            has_account,
        })
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Une erreur fatale ou un import à appliquer sont-ils survenus ? (Sans attendre.)
    pub fn poll(&self) -> Option<Outcome> {
        if self.fatal.load(Ordering::Relaxed) {
            return Some(Outcome::Fatal);
        }
        if self.hub.exit_reason() == Some(ExitReason::ImportStaged) {
            return Some(Outcome::ImportStaged);
        }
        None
    }

    /// Attend un arrêt demandé (`stop` devient vrai), une erreur fatale ou un import à appliquer.
    pub fn wait(&self, stop: &AtomicBool) -> Outcome {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Outcome::Stopped;
            }
            match self.poll() {
                Some(Outcome::ImportStaged) => {
                    // Laisse partir la réponse de l'API avant de couper les connexions.
                    std::thread::sleep(Duration::from_millis(500));
                    return Outcome::ImportStaged;
                }
                Some(other) => return other,
                None => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Arrêt ordonné : le réseau d'abord (plus de commandes), puis le backend, puis les
    /// écritures sur disque. `true` si tout s'est arrêté à temps.
    pub fn stop(mut self) -> bool {
        if let Some(net) = self.net.take() {
            net.shutdown();
        }
        self.hub.close_commands();
        self.stop_pump.store(true, Ordering::SeqCst);
        let mut clean = true;
        if let Some(backend) = self.backend.take() {
            clean &= backend.shutdown(SHUTDOWN_TIMEOUT);
        }
        if let Some(pump) = self.pump.take() {
            clean &= pump.join().is_ok();
        }
        if let Some(writer) = self.demo_history_writer.take() {
            clean &= writer.join().is_ok();
        }
        clean
    }
}

/// Lit les événements du backend, les journalise et les donne au hub.
fn pump_events(rx: mpsc::Receiver<Event>, hub: &Hub, stop: &AtomicBool, fatal: &AtomicBool) {
    let handle = |event: Event| {
        log_event(&event);
        if matches!(event, Event::Fatal(_)) {
            fatal.store(true, Ordering::SeqCst);
        }
        hub.apply(&event);
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => handle(event),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Ce qui restait dans le canal à l'arrêt.
    for event in rx.try_iter() {
        handle(event);
    }
}

/// Une ligne de journal par événement ; les mesures et états (très nombreux au démarrage,
/// quand l'historique est rejoué) restent au niveau debug.
fn log_event(event: &Event) {
    match event {
        Event::Node(info) => log::info!("appareil {:#x} : {}", info.node_id, info.name),
        Event::NodeRemoved(id) => log::info!("appareil {id:#x} oublié"),
        Event::Status { node_id, status } => log::info!("appareil {node_id:#x} : {status:?}"),
        Event::RuleRun { .. } => log::info!("{event:?}"),
        Event::PairingProgress(m) | Event::Info(m) => log::info!("{m}"),
        Event::PairingDone { node_id, name } => log::info!("appairage : {name} ({node_id:#x})"),
        Event::PairingFailed(m) => log::warn!("appairage échoué : {m}"),
        Event::Fatal(m) => log::error!("{m}"),
        // Le contenu d'une notification reste hors du journal : il peut citer des valeurs privées.
        Event::Notification { .. } => log::debug!("notification transmise aux clients"),
        _ => log::debug!("{event:?}"),
    }
}
