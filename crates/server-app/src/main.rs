//! Matter Home Server : l'application de la barre de menus.
//!
//! - `matter-home-server-app` (ou le double-clic sur l'application) : lance le serveur réel
//! - `--demo`                 : appareils simulés, aucun trafic réseau, aucun LaunchAgent
//! - `--demo-setup`           : comme `--demo`, mais en passant par la fenêtre de configuration
//! - `--data-dir DOSSIER`     : autre dossier de données (essais)
//! - `--listen ADRESSE:PORT`  : adresse d'écoute (défaut 0.0.0.0:8443 ; en démo 127.0.0.1:0)

mod ui;

use std::net::SocketAddr;
use std::path::PathBuf;

use matter_home_server::logfile::RotatingLog;
use matter_home_server::persistence;
use matter_home_server_app::controller::{self, Config};
use matter_home_server_app::signals;
use matter_home_server_app::startup::{Holder, LockOutcome};

const HELP: &str = "\
matter-home-server-app — Matter Home Server (application de la barre de menus)

Usage :
  matter-home-server-app                   lance le serveur (réseau réel)
  matter-home-server-app --demo            appareils simulés, sans réseau, sans LaunchAgent
  matter-home-server-app --demo-setup      comme --demo, avec la fenêtre de première configuration
  matter-home-server-app --data-dir DOSSIER  autre dossier de données
  matter-home-server-app --listen ADRESSE:PORT  adresse d'écoute (défaut 0.0.0.0:8443)
  matter-home-server-app --version | --help";

extern "C" {
    fn geteuid() -> u32;
}

fn usage(message: &str) -> ! {
    eprintln!("{message}\n\n{HELP}");
    std::process::exit(2);
}

fn take_value(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        usage(&format!("{name} attend une valeur"));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let n = args.len();
    args.retain(|a| a != name);
    args.len() != n
}

fn main() -> eframe::Result {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut args = raw.clone();
    if take_flag(&mut args, "--help") || take_flag(&mut args, "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if take_flag(&mut args, "--version") || take_flag(&mut args, "-V") {
        println!("matter-home-server-app {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let demo_setup = take_flag(&mut args, "--demo-setup");
    let demo = take_flag(&mut args, "--demo") || demo_setup;
    let data_dir_arg = take_value(&mut args, "--data-dir");
    let listen_arg = take_value(&mut args, "--listen");
    if let Some(unknown) = args.first() {
        usage(&format!("Option inconnue : {unknown}"));
    }
    // SAFETY : `geteuid` n'a ni argument ni effet de bord.
    if !demo && unsafe { geteuid() } == 0 {
        eprintln!("Refus : Matter Home Server ne se lance pas en root ni avec sudo.");
        std::process::exit(1);
    }

    // Le dossier de données est fixé avant tout le reste : le verrou, l'import et le serveur en
    // dépendent.
    let mut temp_data_dir = false;
    let data_dir = match data_dir_arg {
        Some(text) => match persistence::absolute_data_dir(&text) {
            Ok(d) => {
                persistence::set_data_dir(d.clone());
                d
            }
            Err(e) => usage(&format!("{e:#}")),
        },
        None if demo => {
            let d = std::env::temp_dir()
                .join(format!("matter-home-server-demo-{}", std::process::id()));
            persistence::set_data_dir(d.clone());
            temp_data_dir = true;
            d
        }
        None => persistence::data_dir(),
    };
    let listen: SocketAddr = match listen_arg {
        Some(t) => t
            .parse::<SocketAddr>()
            .or_else(|_| {
                t.parse::<u16>()
                    .map(|p| SocketAddr::from(([0, 0, 0, 0], p)))
            })
            .unwrap_or_else(|_| usage(&format!("adresse d'écoute invalide : {t}"))),
        None if demo => SocketAddr::from(([127, 0, 0, 1], 0)),
        None => SocketAddr::from(([0, 0, 0, 0], 8443)),
    };

    let log_path = if demo {
        let _ = std::fs::create_dir_all(&data_dir);
        data_dir.join("journal.log")
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join("Library/Logs/Matter Home Server/serveur.log")
    };
    init_logging(&log_path);
    install_panic_hook();

    // Une seule icône : le verrou propre à l'application reste tenu pendant toute la vie du
    // processus, même quand le serveur lui-même est bloqué (service launchd, ancienne
    // application). Une seconde ouverture se ferme sans bruit.
    let _app_lock = match controller::acquire_app_lock() {
        Ok(Some(file)) => Some(file),
        Ok(None) => {
            eprintln!("Matter Home Server est déjà ouvert.");
            return Ok(());
        }
        Err(e) => {
            log::warn!("verrou de l'application indisponible : {e:#}");
            None
        }
    };

    // Une seule instance : le verrou d'instance du serveur en tient lieu. Si c'est cette
    // application elle-même qui le tient, on s'efface sans bruit (une seconde icône prêterait
    // à confusion). Si c'est autre chose, on reste ouvert pour l'expliquer dans le menu.
    let lock = controller::acquire_lock();
    if lock.0 == LockOutcome::Held(Holder::OtherApp) {
        eprintln!("Matter Home Server est déjà ouvert.");
        return Ok(());
    }

    let handle = controller::spawn(
        Config {
            data_dir,
            listen,
            demo,
            demo_setup,
            temp_data_dir,
            log_path,
            args: raw.iter().map(Into::into).collect(),
        },
        lock,
    );
    // SIGTERM / SIGINT : arrêt propre, comme « Quitter » (avant l'interface et ses fils).
    signals::watch(&[signals::SIGTERM, signals::SIGINT], handle.tx.clone());
    ui::run(handle, demo)
}

fn init_logging(path: &std::path::Path) {
    let mut logger = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,matc=warn,wgpu=warn,naga=warn"),
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match RotatingLog::open(path) {
        Ok(file) => {
            logger.target(env_logger::Target::Pipe(Box::new(file)));
        }
        Err(e) => eprintln!("journal impossible dans {} : {e}", path.display()),
    }
    logger.init();
}

/// Une panique dans une sous-tâche ne doit pas laisser un serveur à moitié vivant : on
/// journalise puis on quitte en erreur (launchd relance l'application si elle est à l'ouverture
/// de session).
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        log::error!("panique : {info}");
        std::process::exit(1);
    }));
}
