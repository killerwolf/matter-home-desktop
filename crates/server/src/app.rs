//! Assemblage du serveur : backend (Matter ou démo), agrégation des événements, réseau.
//! Le binaire (`main.rs`) s'en sert, et les tests d'intégration aussi.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cameras::{self, CameraMonitor};
use crate::events::{Event, EventSink};
use crate::history_store::{self, HistoryStore};
use crate::hub::{ExitReason, Hub, HubConfig};
use crate::net::{self, Auth, NetConfig, NetServer};
use crate::security::account::AccountStore;
use crate::security::sessions::SessionStore;
use crate::security::{self, tls};
use crate::{demo, matter, persistence, Backend};

/// Un relevé de la box toutes les 2 minutes (la box bascule l'état d'un appareil dans ce délai),
/// sans balayage du réseau. La démonstration relève plus souvent : sa « box » n'a pas de réseau.
const WIFI_POLL: Duration = Duration::from_secs(120);
const WIFI_DEMO_POLL: Duration = Duration::from_secs(20);
/// Le fil des extensions appelle `Extensions::tick` toutes les 30 s ; chaque extension décide elle-même si c'est
/// l'heure (la météo : toutes les 15 minutes). Éteintes, elles ne font rien.
const EXTENSIONS_TICK: Duration = Duration::from_secs(30);

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
    /// Qui signe les raccourcis de l'iPhone : `/usr/bin/shortcuts`, sauf en essai.
    pub shortcut_signer: crate::shortcuts::Signer,
    /// Écart entre deux relevés de la box (présence par le Wi-Fi) ; `None` : pas de relevé
    /// périodique (les essais relèvent à la main).
    pub wifi_poll_interval: Option<Duration>,
    /// Réveil du fil des extensions ; `None` : aucun relevé périodique (les essais).
    pub extensions_tick: Option<Duration>,
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
            shortcut_signer: crate::shortcuts::Signer::Apple,
            wifi_poll_interval: Some(if demo { WIFI_DEMO_POLL } else { WIFI_POLL }),
            extensions_tick: Some(EXTENSIONS_TICK),
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
    auth: Arc<Auth>,
    net: Option<NetServer>,
    /// L'annonce Bonjour (absente en démo et sur la boucle locale).
    announcement: Option<crate::announce::Announcement>,
    backend: Option<Backend>,
    pump: Option<JoinHandle<()>>,
    demo_history_writer: Option<JoinHandle<()>>,
    /// Vérifie la joignabilité des caméras (aucune connexion gardée ouverte). Absent en démo.
    camera_monitor: Option<CameraMonitor>,
    /// Le fil qui relève la box de la maison.
    wifi_watcher: Option<WifiWatcher>,
    /// Le fil des extensions (météo…).
    extensions_watcher: Option<ExtensionsWatcher>,
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

        // Le fil des notifications web push : il surveille aussi les appareils injoignables.
        hub.push().start();
        // Les volets des extensions (Somfy) reviennent depuis leur dernier état connu, sans appel
        // réseau.
        hub.sync_extension_devices();

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
                shortcut_signer: config.shortcut_signer,
            },
            hub.clone(),
            auth.clone(),
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
        // Les caméras ne doivent jamais empêcher le serveur de démarrer : sans surveillance, leur
        // état reste « inconnu », le reste fonctionne.
        let camera_monitor = if config.demo {
            None
        } else {
            match cameras::start_monitor(hub.clone()) {
                Ok(monitor) => Some(monitor),
                Err(e) => {
                    log::error!("surveillance des caméras non démarrée : {e}");
                    None
                }
            }
        };
        // Le serveur de démonstration ne s'annonce jamais : rien ne doit sortir de la boucle locale.
        let announcement = if config.demo {
            None
        } else {
            crate::announce::start(net.addr, &identity.fingerprint)
        };
        let wifi_watcher = config
            .wifi_poll_interval
            .map(|every| WifiWatcher::start(hub.clone(), auth.clone(), every));
        let extensions_watcher = config
            .extensions_tick
            .map(|every| ExtensionsWatcher::start(hub.clone(), every));
        Ok(Server {
            announcement,
            addr: net.addr,
            hub,
            auth,
            net: Some(net),
            backend: Some(backend),
            pump: Some(pump),
            demo_history_writer,
            camera_monitor,
            wifi_watcher,
            extensions_watcher,
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

    /// Comptes et sessions du serveur qui tourne : Ostal Server y lit les connexions ouvertes et
    /// les coupe (les sessions ne vivent qu'ici, en mémoire, avec leur fichier).
    pub fn auth(&self) -> &Arc<Auth> {
        &self.auth
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
        // D'abord « au revoir » sur le réseau : les apps ne proposent plus ce serveur.
        if let Some(announcement) = self.announcement.take() {
            announcement.stop();
        }
        if let Some(net) = self.net.take() {
            net.shutdown();
        }
        self.hub.close_commands();
        self.stop_pump.store(true, Ordering::SeqCst);
        let mut clean = true;
        if let Some(monitor) = self.camera_monitor.take() {
            clean &= monitor.stop();
        }
        if let Some(watcher) = self.wifi_watcher.take() {
            clean &= watcher.stop();
        }
        // Le backend Matter s'arrête avant les extensions : un appel cloud en cours (Somfy,
        // météo) ne doit jamais retarder son arrêt propre.
        if let Some(backend) = self.backend.take() {
            clean &= backend.shutdown(SHUTDOWN_TIMEOUT);
        }
        if let Some(watcher) = self.extensions_watcher.take() {
            clean &= watcher.stop();
        }
        self.hub.extensions().shutdown();
        if let Some(pump) = self.pump.take() {
            clean &= pump.join().is_ok();
        }
        if let Some(writer) = self.demo_history_writer.take() {
            clean &= writer.join().is_ok();
        }
        self.hub.push().shutdown();
        clean
    }
}

/// Le fil qui relève la box de la maison à intervalle fixe (présence par le Wi-Fi). Sans box reliée,
/// chaque tour ne fait rien. Il s'arrête dans la seconde qui suit la demande.
pub struct WifiWatcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl WifiWatcher {
    pub fn start(hub: Arc<Hub>, auth: Arc<Auth>, every: Duration) -> WifiWatcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("wifi".into())
            .spawn(move || {
                let tick = Duration::from_millis(200);
                loop {
                    hub.wifi_poll(&auth.accounts, false);
                    let mut waited = Duration::ZERO;
                    while waited < every {
                        if flag.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(tick);
                        waited += tick;
                    }
                }
            })
            .expect("fil du Wi-Fi");
        WifiWatcher { stop, thread }
    }

    /// Arrête le fil et attend sa fin. `true` : il s'est arrêté sans panique.
    pub fn stop(self) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().is_ok()
    }
}

/// Le fil des extensions : à chaque réveil, chaque extension active relève si c'est l'heure. Il
/// s'arrête dans la seconde qui suit la demande (sauf pendant un appel réseau, au plus 10 s).
pub struct ExtensionsWatcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl ExtensionsWatcher {
    pub fn start(hub: Arc<Hub>, every: Duration) -> ExtensionsWatcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("extensions".into())
            .spawn(move || {
                let tick = Duration::from_millis(200);
                loop {
                    hub.extensions().tick(crate::security::now());
                    hub.sync_extension_devices();
                    let mut waited = Duration::ZERO;
                    while waited < every {
                        if flag.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(tick);
                        waited += tick;
                        // Un volet a changé (ordre parti ou raté, compte refusé) : Ostal le montre
                        // sans attendre le prochain tour.
                        if hub.extensions().somfy().take_changed() {
                            hub.sync_extension_devices();
                        }
                    }
                }
            })
            .expect("fil des extensions");
        ExtensionsWatcher { stop, thread }
    }

    /// Arrête le fil et attend sa fin. `true` : il s'est arrêté sans panique.
    /// Demande l'arrêt et attend le fil quelques secondes au plus : un tour en plein appel
    /// cloud est laissé finir seul (son résultat ne sert plus).
    pub fn stop(self) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + EXTENSIONS_STOP_WAIT;
        while !self.thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.thread.is_finished() {
            self.thread.join().is_ok()
        } else {
            log::warn!("extensions : un appel en cours finira après l'arrêt");
            true
        }
    }
}

/// Attente maximale du fil des extensions à l'arrêt.
const EXTENSIONS_STOP_WAIT: Duration = Duration::from_secs(3);

/// Lit les événements du backend, les journalise et les donne au hub.
fn pump_events(rx: mpsc::Receiver<Event>, hub: &Hub, stop: &AtomicBool, fatal: &AtomicBool) {
    let handle = |event: Event| {
        log_event(&event);
        if matches!(event, Event::Fatal(_)) {
            fatal.store(true, Ordering::SeqCst);
        }
        // Une règle demande un mode (`Action::SetMode`) : même chemin qu'une demande d'API.
        if let Event::HomeModeRequested { mode, rule } = &event {
            let source = protocol::mode::ModeSource::Rule { name: rule.clone() };
            if let Err(e) = hub.set_mode(*mode, source) {
                log::warn!("mode demandé par une règle non appliqué : {e:?}");
            }
            return;
        }
        // Une règle commande un volet d'extension (Somfy) : même chemin qu'une demande d'API
        // (`Hub::set_cover` met l'ordre en file ; rien de long ici).
        if let Event::CoverRequested { node_id, command } = &event {
            if let Err(e) = hub.set_cover(*node_id, *command) {
                let name = hub
                    .device_name(*node_id)
                    .unwrap_or_else(|| format!("{node_id:#x}"));
                hub.apply(&Event::Info(format!(
                    "{name} : ordre d'une règle non transmis ({})",
                    e.detail.as_deref().unwrap_or("refusé")
                )));
            }
            return;
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
