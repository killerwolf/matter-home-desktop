//! Le chef d'orchestre : un fil d'exécution qui prend le verrou, applique les gardes, démarre le
//! serveur (dans ce même processus, avec ses propres fils et son runtime tokio) et le surveille.
//! Le fil principal (boucle d'événements de macOS) ne fait jamais rien de long : il lit
//! l'état partagé et envoie des commandes.

use std::ffi::OsString;
use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use matter_home_server::app::{Outcome, Server, ServerConfig};
use matter_home_server::backup::{self, PendingKind};
use matter_home_server::persistence::{self, LockHeld};
use matter_home_server::security::{self, account};
use matter_home_server::setup::{self, IdentityState};
use protocol::wire::WireStatus;

use crate::sleep::SleepGuard;
use crate::startup::{self, Blocker, Decision, Inputs, LockOutcome, SetupNeed};
use crate::status::{Health, Phase};
use crate::{agent, system};

pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// Appareils simulés : aucun trafic réseau, aucun LaunchAgent.
    pub demo: bool,
    /// En démo, passe quand même par la configuration (identité et compte).
    pub demo_setup: bool,
    /// Dossier jetable créé par l'application, supprimé à la sortie.
    pub temp_data_dir: bool,
    pub log_path: PathBuf,
    /// Arguments à reprendre pour se relancer à l'identique après un import.
    pub args: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Ok(String),
    Err(String),
}

#[derive(Debug, Clone)]
pub struct State {
    pub phase: Phase,
    pub url: Option<String>,
    pub fingerprint: Option<String>,
    pub health: Health,
    pub setup: Option<SetupNeed>,
    pub busy: Option<String>,
    pub notice: Option<Notice>,
    /// Pendant que le serveur tourne : un processus `matter-home-desktop` dont on ne peut pas
    /// dire si c'est l'ancienne v0.2 (le serveur n'est pas arrêté pour autant).
    pub desktop_doubt: bool,
    pub data_dir: PathBuf,
    pub log_path: PathBuf,
    pub demo: bool,
}

pub type Shared = Arc<Mutex<State>>;

pub enum Cmd {
    Retry,
    Import {
        file: PathBuf,
        replace_identity: bool,
    },
    AbandonImport,
    NewHome,
    CreateAccount {
        username: String,
        password: String,
    },
    ReplaceAccount {
        username: String,
        password: String,
    },
    Quit,
}

/// Jamais de mot de passe dans un journal : `Debug` le masque.
impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Retry => f.write_str("Retry"),
            Cmd::Import {
                file,
                replace_identity,
            } => f
                .debug_struct("Import")
                .field("file", file)
                .field("replace_identity", replace_identity)
                .finish(),
            Cmd::AbandonImport => f.write_str("AbandonImport"),
            Cmd::NewHome => f.write_str("NewHome"),
            Cmd::CreateAccount { username, .. } => f
                .debug_struct("CreateAccount")
                .field("username", username)
                .field("password", &"***")
                .finish(),
            Cmd::ReplaceAccount { username, .. } => f
                .debug_struct("ReplaceAccount")
                .field("username", username)
                .field("password", &"***")
                .finish(),
            Cmd::Quit => f.write_str("Quit"),
        }
    }
}

/// Signalé par le chef d'orchestre quand l'arrêt propre du serveur est terminé : l'interface
/// attend ce signal (borné) avant de laisser le processus finir.
#[derive(Clone, Default)]
pub struct ExitSignal(Arc<(Mutex<bool>, Condvar)>);

impl ExitSignal {
    pub fn set(&self) {
        let (flag, cv) = &*self.0;
        *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }

    /// `true` si le signal est arrivé avant `timeout`.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (flag, cv) = &*self.0;
        let guard = flag.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = cv
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

pub struct Handle {
    pub state: Shared,
    pub tx: Sender<Cmd>,
    pub exited: ExitSignal,
}

impl Handle {
    pub fn snapshot(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Que faire quand le serveur s'arrête de lui-même.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterStop {
    /// Arrêt demandé : sortie normale (code 0), launchd ne relance pas.
    Quit,
    /// Import à appliquer : s'arrêter proprement puis se relancer à l'identique (même numéro de
    /// processus, le verrou est repris, l'import appliqué avant tout chargement).
    Relaunch,
    /// Erreur fatale sous launchd : sortie en erreur (code 1), launchd relance.
    ExitFailure,
    /// Erreur fatale sans launchd : personne ne relancerait, on reste ouvert et on le dit.
    StayInProblem,
}

/// L'application ne sort jamais en code 78 (sans identité elle ouvre la configuration et
/// reste ouverte) et le code 75 de la ligne de commande devient une relance interne.
pub fn after_outcome(outcome: Outcome, supervised: bool) -> AfterStop {
    match outcome {
        Outcome::Stopped => AfterStop::Quit,
        Outcome::ImportStaged => AfterStop::Relaunch,
        Outcome::Fatal if supervised => AfterStop::ExitFailure,
        Outcome::Fatal => AfterStop::StayInProblem,
    }
}

/// Prend le verrou d'instance. En cas d'échec, dit qui le tient.
pub fn acquire_lock() -> (LockOutcome, Option<File>) {
    let path = persistence::instance_lock_path();
    match persistence::acquire_instance_lock(&path) {
        Ok(file) => (LockOutcome::Acquired, Some(file)),
        Err(e) if e.downcast_ref::<LockHeld>().is_some() => {
            let holder = startup::classify_holder(
                persistence::lock_holder_pid(&path),
                &system::processes(),
                system::service_installed(),
            );
            (LockOutcome::Held(holder), None)
        }
        Err(e) => (LockOutcome::Failed(format!("{e:#}")), None),
    }
}

/// Le verrou propre à l'application, distinct de celui du serveur : il reste tenu même quand le
/// serveur n'est pas démarré (service launchd, ancienne application), pour qu'une seconde
/// ouverture se ferme au lieu d'ajouter une seconde icône.
pub fn app_lock_path_for(instance_lock: &std::path::Path) -> PathBuf {
    instance_lock.with_extension("app.lock")
}

/// `Some(fichier)` : verrou pris ; `None` : une autre instance de l'application le tient.
pub fn acquire_app_lock_at(path: &std::path::Path) -> anyhow::Result<Option<File>> {
    match persistence::acquire_instance_lock(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.downcast_ref::<LockHeld>().is_some() => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn acquire_app_lock() -> anyhow::Result<Option<File>> {
    acquire_app_lock_at(&app_lock_path_for(&persistence::instance_lock_path()))
}

pub fn spawn(config: Config, first_lock: (LockOutcome, Option<File>)) -> Handle {
    let state = Arc::new(Mutex::new(State {
        phase: Phase::Starting,
        url: None,
        fingerprint: None,
        health: Health::default(),
        setup: None,
        busy: None,
        notice: None,
        desktop_doubt: false,
        data_dir: config.data_dir.clone(),
        log_path: config.log_path.clone(),
        demo: config.demo,
    }));
    let (tx, rx) = mpsc::channel();
    let exited = ExitSignal::default();
    let ctl = Controller {
        cfg: config,
        state: state.clone(),
        rx,
        lock: first_lock.1,
        first_outcome: Some(first_lock.0),
        server: None,
        sleep: None,
        exited: exited.clone(),
        last_desktop_check: Instant::now(),
        desktop_verdicts: startup::VerdictCache::default(),
    };
    std::thread::Builder::new()
        .name("chef-orchestre".into())
        .spawn(move || ctl.run())
        .expect("fil du chef d'orchestre");
    Handle { state, tx, exited }
}

struct Controller {
    cfg: Config,
    state: Shared,
    rx: Receiver<Cmd>,
    lock: Option<File>,
    first_outcome: Option<LockOutcome>,
    server: Option<Server>,
    sleep: Option<SleepGuard>,
    exited: ExitSignal,
    /// Dernier contrôle de l'ancienne application de bureau pendant que le serveur tourne.
    last_desktop_check: Instant,
    /// Verdicts par (processus, heure de lancement) : un processus jugé sain n'est pas réévalué.
    desktop_verdicts: startup::VerdictCache,
}

/// Pendant que le serveur tourne, l'ancienne application de bureau est recherchée toutes les 30 s.
const DESKTOP_CHECK_EVERY: Duration = Duration::from_secs(30);

enum Next {
    Reboot,
}

impl Controller {
    fn desktop_scan(&mut self) -> startup::DesktopScan {
        startup::scan_desktop(
            &system::processes(),
            SystemTime::now(),
            &mut self.desktop_verdicts,
            system::exe_path_of,
            system::bundle_info_of,
        )
    }

    fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn set_phase(&self, phase: Phase) {
        self.set(|s| {
            s.phase = phase;
            s.setup = None;
        });
    }

    fn run(mut self) {
        loop {
            self.boot();
            match self.wait() {
                Next::Reboot => continue,
            }
        }
    }

    fn supervised(&self) -> bool {
        !self.cfg.demo && std::env::var_os(agent::SUPERVISED_ENV).is_some()
    }

    /// Une passe de démarrage : verrou, gardes, puis serveur si tout est en ordre.
    fn boot(&mut self) {
        self.set(|s| {
            s.phase = Phase::Starting;
            s.setup = None;
            s.url = None;
            s.fingerprint = None;
            s.health = Health::default();
            s.desktop_doubt = false;
        });
        let demo_skips_setup = self.cfg.demo && !self.cfg.demo_setup;
        // Le service launchd installé : on ne prend même pas le verrou (il le reprendrait en vain
        // à chaque relance), on explique.
        if !self.cfg.demo && system::service_installed() {
            self.lock = None;
            self.first_outcome = None;
            log::warn!("le serveur ne démarre pas : {}", Blocker::Service.explain());
            self.set_phase(Phase::Blocked(Blocker::Service));
            return;
        }
        let outcome = if self.lock.is_some() {
            LockOutcome::Acquired
        } else if let Some(first) = self.first_outcome.take() {
            first
        } else {
            let (outcome, file) = acquire_lock();
            self.lock = file;
            outcome
        };
        let mut inputs = Inputs {
            lock: outcome,
            desktop_v02_running: false,
            service_installed: !self.cfg.demo && system::service_installed(),
            identity: IdentityState::Missing,
            account_exists: false,
        };
        if inputs.lock == LockOutcome::Acquired {
            // Au démarrage, un doute suffit à refuser (nouvel essai toutes les 10 s).
            inputs.desktop_v02_running = !self.cfg.demo && {
                let scan = self.desktop_scan();
                scan.v02 || !scan.uncertain.is_empty()
            };
            if !inputs.desktop_v02_running && !inputs.service_installed && !demo_skips_setup {
                if let Err(e) = setup::prologue(true) {
                    log::error!("{e:#}");
                    self.set_phase(Phase::Problem(format!("{e:#}")));
                    return;
                }
                let dir = persistence::data_dir();
                inputs.identity = setup::identity_state(&dir);
                inputs.account_exists = setup::account_exists(&dir);
            }
        }
        let decision = if demo_skips_setup && inputs.lock == LockOutcome::Acquired {
            Decision::Run
        } else {
            startup::decide(&inputs)
        };
        match decision {
            Decision::Run => self.run_server(),
            Decision::Setup(need) => {
                log::info!("configuration à terminer : {need:?}");
                self.set(|s| {
                    s.phase = Phase::NeedsSetup;
                    s.setup = Some(need);
                });
            }
            Decision::Blocked(Blocker::OtherApp) => {
                // Une autre fenêtre de l'application a pris la place : une seule icône.
                log::info!("Matter Home Server est déjà ouvert : cette instance se ferme.");
                std::process::exit(0);
            }
            Decision::Blocked(b) => {
                log::warn!("le serveur ne démarre pas : {}", b.explain());
                self.set_phase(Phase::Blocked(b));
            }
            Decision::Problem(e) => {
                log::error!("{e}");
                self.set_phase(Phase::Problem(e));
            }
        }
    }

    fn run_server(&mut self) {
        let data_dir = if self.cfg.demo {
            self.cfg.data_dir.clone()
        } else {
            persistence::data_dir()
        };
        if self.cfg.demo && !self.cfg.demo_setup {
            ensure_demo_account(&data_dir);
        }
        let config = ServerConfig::new(data_dir, self.cfg.listen, self.cfg.demo);
        match Server::start(config) {
            Ok(server) => {
                let url = system::server_url(server.addr, system::local_host_name().as_deref());
                log::info!("écoute sur {url}");
                log::info!("empreinte du certificat (SHA-256) : {}", server.fingerprint);
                match SleepGuard::start() {
                    Ok(g) => self.sleep = Some(g),
                    Err(e) => log::warn!("mise en veille non empêchée : {e}"),
                }
                self.set(|s| {
                    s.phase = Phase::Running;
                    s.url = Some(url);
                    s.fingerprint = Some(server.fingerprint.clone());
                    s.setup = None;
                });
                self.server = Some(server);
            }
            Err(e) => {
                log::error!("démarrage impossible : {e:#}");
                self.set_phase(Phase::Problem(format!(
                    "Le serveur n'a pas pu démarrer : {e:#}. Si une autre application utilise déjà ce port, fermez-la."
                )));
            }
        }
    }

    fn stop_server(&mut self) -> bool {
        self.sleep = None;
        match self.server.take() {
            Some(server) => {
                self.set(|s| s.phase = Phase::Stopping);
                server.stop()
            }
            None => true,
        }
    }

    fn finish_and_exit(&mut self, code: i32) -> ! {
        let clean = self.stop_server();
        if !clean {
            log::error!("arrêt incomplet : certains threads n'ont pas fini à temps.");
        }
        if self.cfg.temp_data_dir {
            let _ = std::fs::remove_dir_all(&self.cfg.data_dir);
            let _ = std::fs::remove_file(persistence::instance_lock_path());
        }
        log::info!("Au revoir.");
        self.exited.set();
        std::process::exit(code)
    }

    /// Arrête le serveur ; s'il ne s'arrête pas proprement (des fils tournent encore), on ne
    /// redémarre jamais un second serveur dans ce processus sur la même identité : on sort en
    /// erreur. launchd relance l'application si elle est supervisée, sinon l'utilisateur la rouvre.
    fn stop_or_exit(&mut self) {
        if !self.stop_server() {
            log::error!("arrêt incomplet du serveur : sortie pour ne pas en démarrer un second dans ce processus");
            self.finish_and_exit(1);
        }
    }

    fn wait(&mut self) -> Next {
        let mut last_health = Instant::now() - Duration::from_secs(10);
        let last_retry = Instant::now();
        loop {
            let phase = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phase
                .clone();
            let timeout = if self.server.is_some() {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(1)
            };
            match self.rx.recv_timeout(timeout) {
                Ok(cmd) => {
                    if let Some(next) = self.handle(cmd) {
                        return next;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.finish_and_exit(0),
            }
            if let Some(server) = &self.server {
                if let Some(outcome) = server.poll() {
                    if outcome == Outcome::ImportStaged {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    self.after_outcome(outcome);
                    continue;
                }
                if last_health.elapsed() >= Duration::from_secs(2) {
                    last_health = Instant::now();
                    let health = health_of(server);
                    self.set(|s| s.health = health);
                }
                if !self.cfg.demo && self.last_desktop_check.elapsed() >= DESKTOP_CHECK_EVERY {
                    self.last_desktop_check = Instant::now();
                    let scan = self.desktop_scan();
                    let doubt = !scan.uncertain.is_empty();
                    if doubt
                        != self
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .desktop_doubt
                    {
                        if doubt {
                            log::warn!(
                                "processus matter-home-desktop incertain (pid {:?}) : le serveur continue de tourner",
                                scan.uncertain
                            );
                        }
                        self.set(|s| s.desktop_doubt = doubt);
                    }
                    if scan.v02 {
                        log::warn!(
                            "l'ancienne application de bureau (v0.2) est apparue : arrêt du serveur"
                        );
                        self.stop_or_exit();
                        self.set(|s| s.desktop_doubt = false);
                        self.set_phase(Phase::Blocked(Blocker::DesktopV02));
                    }
                }
            } else if matches!(phase, Phase::Blocked(_))
                && last_retry.elapsed() >= Duration::from_secs(10)
            {
                // Le programme qui tenait les données s'est peut-être arrêté.
                return Next::Reboot;
            }
        }
    }

    fn after_outcome(&mut self, outcome: Outcome) {
        match after_outcome(outcome, self.supervised()) {
            AfterStop::Quit => self.finish_and_exit(0),
            AfterStop::ExitFailure => {
                log::error!("erreur fatale du serveur : sortie en erreur pour que launchd relance l'application");
                self.finish_and_exit(1)
            }
            AfterStop::StayInProblem => {
                log::error!("erreur fatale du serveur");
                self.stop_or_exit();
                self.set_phase(Phase::Problem(
                    "Le serveur s'est arrêté après une erreur. Consultez le journal, puis rouvrez Matter Home Server.".into(),
                ));
            }
            AfterStop::Relaunch => {
                log::warn!("import en attente : arrêt propre puis relance pour l'appliquer");
                self.stop_or_exit();
                self.lock = None;
                self.relaunch();
            }
        }
    }

    /// Remplace ce processus par une nouvelle instance de l'application (même numéro de processus).
    fn relaunch(&mut self) {
        use std::os::unix::process::CommandExt;
        match std::env::current_exe() {
            Ok(exe) => {
                let err = std::process::Command::new(exe).args(&self.cfg.args).exec();
                log::error!("relance impossible : {err}");
                self.set_phase(Phase::Problem(format!(
                    "Relance impossible : {err}. Rouvrez Matter Home Server."
                )));
            }
            Err(e) => self.set_phase(Phase::Problem(format!(
                "Relance impossible : {e}. Rouvrez Matter Home Server."
            ))),
        }
    }

    /// Traite une commande. `Some` : repartir de zéro.
    fn handle(&mut self, cmd: Cmd) -> Option<Next> {
        let in_setup =
            self.state.lock().unwrap_or_else(|e| e.into_inner()).phase == Phase::NeedsSetup;
        match cmd {
            Cmd::Quit => self.finish_and_exit(0),
            Cmd::Retry => return Some(Next::Reboot),
            Cmd::Import {
                file,
                replace_identity,
            } if in_setup => {
                return self.operation("Reprise de la sauvegarde…", |_| {
                    import(&file, replace_identity)
                });
            }
            Cmd::AbandonImport if in_setup => {
                return self.operation("Abandon de l'import…", |_| abandon());
            }
            Cmd::NewHome if in_setup => {
                return self.operation("Création de la maison…", |_| new_home());
            }
            Cmd::CreateAccount { username, password } if in_setup => {
                return self.operation("Création du compte…", |_| {
                    create_account(&username, &password, false).map(|()| "Compte créé.".to_string())
                });
            }
            Cmd::ReplaceAccount { username, password } => {
                let result = create_account(&username, &password, true);
                match result {
                    Ok(()) => {
                        self.set(|s| s.notice = Some(Notice::Ok("Compte remplacé : les anciennes connexions sont fermées, le serveur redémarre.".into())));
                        self.stop_or_exit();
                        return Some(Next::Reboot);
                    }
                    Err(e) => self.set(|s| s.notice = Some(Notice::Err(e))),
                }
            }
            other => log::warn!("commande ignorée dans cet état : {other:?}"),
        }
        None
    }

    fn operation(
        &mut self,
        busy: &str,
        run: impl FnOnce(&mut Self) -> Result<String, String>,
    ) -> Option<Next> {
        self.set(|s| {
            s.busy = Some(busy.into());
            s.notice = None;
        });
        let result = run(self);
        let ok = result.is_ok();
        self.set(|s| {
            s.busy = None;
            s.notice = Some(match result {
                Ok(m) => Notice::Ok(m),
                Err(e) => Notice::Err(e),
            });
        });
        // Un succès ouvre la suite (compte, puis démarrage) ; un échec laisse l'écran tel quel.
        ok.then_some(Next::Reboot)
    }
}

fn health_of(server: &Server) -> Health {
    let snapshot = server.hub().current_snapshot();
    let mut health = Health {
        total: snapshot.devices.len(),
        ..Health::default()
    };
    for d in &snapshot.devices {
        match d.status {
            WireStatus::Online => health.online += 1,
            WireStatus::Unreachable { .. } => health.unreachable.push(d.device.name.clone()),
            WireStatus::Connecting => {}
        }
    }
    health.unreachable.sort();
    health
}

/// Les gardes de `import` : jamais d'identité remplacée sans accord, jamais deux sauvegardes mêlées.
fn import(file: &std::path::Path, replace_identity: bool) -> Result<String, String> {
    let dir = persistence::data_dir();
    match setup::pending_for_import(&dir, file).map_err(|e| format!("{e:#}"))? {
        PendingKind::Other => {
            return Err("Une autre sauvegarde est déjà en attente (import interrompu). Renoncez-y d'abord avec « Renoncer à l'import en attente ».".into())
        }
        PendingKind::None | PendingKind::SameFile => {}
    }
    let comparison = setup::compare_identities(&dir, file).map_err(|e| format!("{e:#}"))?;
    if comparison.differs && !replace_identity {
        return Err(setup::replace_refusal_for_app().into());
    }
    let done = backup::import_file(&dir, file).map_err(|e| format!("{e:#}"))?;
    Ok(format!(
        "Sauvegarde reprise : {}. Vos anciennes données sont gardées dans {}.",
        done.summary.describe(),
        done.previous.display()
    ))
}

fn abandon() -> Result<String, String> {
    let lines = backup::abandon(&persistence::data_dir()).map_err(|e| format!("{e:#}"))?;
    Ok(if lines.is_empty() {
        "Aucun import en attente.".to_string()
    } else {
        lines.join(" ")
    })
}

fn new_home() -> Result<String, String> {
    setup::init_identity(&persistence::data_dir())
        .map(|_| "Nouvelle maison créée.".to_string())
        .map_err(|e| e.to_string())
}

fn create_account(username: &str, password: &str, replace: bool) -> Result<(), String> {
    setup::check_credentials(username, password)?;
    let dir = persistence::data_dir();
    security::server_dir(&dir)
        .and_then(|d| account::create(&d, username, password, replace))
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

/// En démo, un compte « demo » est créé s'il n'y en a pas ; son mot de passe est tiré au sort
/// et écrit sur la sortie standard, jamais dans le journal.
fn ensure_demo_account(data_dir: &std::path::Path) {
    use matter_home_server::security::account::AccountStore;
    let result = security::server_dir(data_dir).and_then(|dir| {
        if AccountStore::open(&dir)?.exists() {
            return Ok(None);
        }
        let mut random = [0u8; 8];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut random)?;
        let password = format!("demo-{:016x}", u64::from_le_bytes(random));
        account::create(&dir, "demo", &password, false)?;
        Ok(Some(password))
    });
    match result {
        Ok(Some(p)) => println!("Compte de démonstration : demo / {p}"),
        Ok(None) => {}
        Err(e) => log::error!("compte de démonstration impossible : {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_never_loop() {
        assert_eq!(after_outcome(Outcome::Stopped, true), AfterStop::Quit);
        assert_eq!(
            after_outcome(Outcome::ImportStaged, true),
            AfterStop::Relaunch
        );
        assert_eq!(
            after_outcome(Outcome::ImportStaged, false),
            AfterStop::Relaunch
        );
        assert_eq!(after_outcome(Outcome::Fatal, true), AfterStop::ExitFailure);
        // Sans launchd, une sortie en erreur ferait disparaître l'icône sans relance.
        assert_eq!(
            after_outcome(Outcome::Fatal, false),
            AfterStop::StayInProblem
        );
    }

    #[test]
    fn debug_never_shows_a_password() {
        let secret = "tres-secret-123";
        for cmd in [
            Cmd::CreateAccount {
                username: "hamdi".into(),
                password: secret.into(),
            },
            Cmd::ReplaceAccount {
                username: "hamdi".into(),
                password: secret.into(),
            },
        ] {
            let text = format!("{cmd:?}");
            assert!(!text.contains(secret), "{text}");
            assert!(text.contains("hamdi") && text.contains("***"), "{text}");
        }
        assert_eq!(format!("{:?}", Cmd::Quit), "Quit");
    }

    #[test]
    fn exit_signal_wakes_the_waiter_and_is_bounded() {
        let signal = ExitSignal::default();
        let t = Instant::now();
        assert!(!signal.wait(Duration::from_millis(50)), "borné sans signal");
        assert!(t.elapsed() >= Duration::from_millis(50));
        let other = signal.clone();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.set();
        });
        let t = Instant::now();
        assert!(signal.wait(Duration::from_secs(20)));
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "réveillé tout de suite"
        );
        setter.join().unwrap();
        assert!(signal.wait(Duration::ZERO), "reste levé");
    }

    #[test]
    fn app_lock_is_a_sibling_of_the_instance_lock() {
        let inst = std::path::Path::new("/x/y/Dossier.lock");
        assert_eq!(
            app_lock_path_for(inst),
            std::path::Path::new("/x/y/Dossier.app.lock")
        );
    }

    #[test]
    fn second_app_lock_is_refused_while_the_first_is_held() {
        let dir = std::env::temp_dir().join(format!("mh-applock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("D.app.lock");
        let first = acquire_app_lock_at(&path).unwrap();
        assert!(first.is_some());
        assert!(
            acquire_app_lock_at(&path).unwrap().is_none(),
            "une seule icône"
        );
        drop(first);
        assert!(acquire_app_lock_at(&path).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_explanations_promise_nothing_is_started() {
        // Aucun chemin « Blocked » ne mène à une sortie : l'application reste ouverte (code 78 jamais émis).
        assert!(crate::startup::Blocker::Service
            .explain()
            .contains("Rien n'est démarré"));
    }
}
