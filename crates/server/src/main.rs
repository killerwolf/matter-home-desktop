//! Serveur Matter Home, sans fenêtre.
//!
//! - `matter-home-server`                : mode normal (réseau réel)
//! - `matter-home-server --demo`         : appareils simulés, dossier de données temporaire
//! - `matter-home-server --discover`     : diagnostic en ligne de commande (mDNS)
//! - `matter-home-server create-account` : crée le compte unique (mot de passe demandé sans écho)
//! - `matter-home-server fingerprint`    : affiche l'empreinte du certificat TLS
//!
//! Le serveur quitte avec un code non nul sur une erreur fatale, pour qu'un superviseur
//! (launchd, systemd) le relance. Ctrl-C ou SIGTERM l'arrête proprement (code 0). Après un
//! import de sauvegarde par l'API, il quitte avec le code 75 pour être relancé et appliquer
//! l'import.

use std::io::{BufRead, IsTerminal};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rand::RngCore;

use matter_home_server::app::{Outcome, Server, ServerConfig};
use matter_home_server::security::account::{self, AccountStore};
use matter_home_server::security::{self, tls};
use matter_home_server::{backup, matter, persistence};

const HELP: &str = "\
matter-home-server — serveur Matter Home (sans fenêtre)

Usage :
  matter-home-server [--listen ADRESSE:PORT] [--data-dir DOSSIER]
                                 lance le serveur (réseau réel)
  matter-home-server --demo      lance le serveur avec des appareils simulés, dans un
                                 dossier temporaire (écoute sur 127.0.0.1:8443)
  matter-home-server --discover  liste les nœuds Matter vus en mDNS puis quitte
  matter-home-server create-account --username NOM [--password-stdin] [--replace]
                     [--data-dir DOSSIER]
                                 crée le compte unique ; le mot de passe est demandé sans
                                 écho, ou lu sur l'entrée standard avec --password-stdin
                                 (jamais en argument). --replace remplace le compte et ferme
                                 ses sessions (redémarrer ensuite le serveur)
  matter-home-server fingerprint [--data-dir DOSSIER]
                                 affiche l'empreinte SHA-256 du certificat TLS
  matter-home-server --help      affiche cette aide

Options : --listen accepte « adresse:port » ou un simple port (défaut : 0.0.0.0:8443, ou
127.0.0.1:8443 avec --demo). --data-dir remplace le dossier de données standard.

Codes de sortie : 0 arrêt demandé, 1 erreur, 2 usage, 75 import à appliquer (relancer).
Variables : RUST_LOG=debug pour des journaux détaillés.";

/// Sortie après un import de sauvegarde par l'API (EX_TEMPFAIL) : non nul, pour qu'un
/// superviseur configuré pour ne relancer que les échecs relance aussi le serveur.
const EXIT_IMPORT_STAGED: u8 = 75;

const DEFAULT_PORT: u16 = 8443;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,matc=warn"))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    // Une panique dans une sous-tâche ne doit pas laisser un serveur à moitié vivant :
    // on journalise puis on quitte, le superviseur relancera.
    install_panic_hook();
    match args.first().map(String::as_str) {
        Some("create-account") => run_create_account(&args[1..]),
        Some("fingerprint") => run_fingerprint(&args[1..]),
        _ => run_server(&args),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{message}\n\n{HELP}");
    ExitCode::from(2)
}

/// Valeur de l'option `name` (`--option valeur`), en la retirant de `args`.
fn take_value(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        return Err(format!("{name} attend une valeur"));
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != name);
    args.len() != before
}

fn parse_listen(text: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    text.parse::<u16>()
        .map(|port| SocketAddr::from(([0, 0, 0, 0], port)))
        .map_err(|_| format!("adresse d'écoute invalide : {text}"))
}

// ---------------------------------------------------------------------------
// Serveur
// ---------------------------------------------------------------------------

fn run_server(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let demo = take_flag(&mut args, "--demo");
    let discover_only = take_flag(&mut args, "--discover");
    let data_dir_arg = match take_value(&mut args, "--data-dir") {
        Ok(v) => v.map(PathBuf::from),
        Err(e) => return usage_error(&e),
    };
    let listen_arg = match take_value(&mut args, "--listen") {
        Ok(v) => v,
        Err(e) => return usage_error(&e),
    };
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    if discover_only {
        return discover();
    }
    let listen = match listen_arg.as_deref().map(parse_listen) {
        Some(Ok(addr)) => addr,
        Some(Err(e)) => return usage_error(&e),
        None if demo => SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
        None => SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
    };

    // Dossier de données : imposé, standard, ou (démo sans option) temporaire.
    let mut temp_demo_dir = None;
    let data_dir = match (&data_dir_arg, demo) {
        (Some(dir), _) => {
            if !persistence::set_data_dir(dir.clone()) {
                eprintln!("dossier de données déjà fixé");
                return ExitCode::FAILURE;
            }
            dir.clone()
        }
        (None, true) => {
            let dir = temp_demo_directory();
            temp_demo_dir = Some(dir.clone());
            dir
        }
        (None, false) => persistence::data_dir(),
    };

    // Le mode démo ne touche ni au verrou, ni à la migration de dossier, ni à l'import en
    // attente. Le verrou est pris avant tout le reste : l'import renomme le dossier.
    let _instance_lock = if demo {
        None
    } else {
        match persistence::acquire_instance_lock(&persistence::instance_lock_path()) {
            Ok(lock) => Some(lock),
            Err(e) => {
                log::error!("{e:#}");
                return ExitCode::FAILURE;
            }
        }
    };
    let data_dir = if demo {
        data_dir
    } else {
        // Ancien nom « matter-demo » : son dossier de données est déplacé au premier lancement.
        let mut notes: Vec<String> = Vec::new();
        notes.extend(persistence::migrate_legacy_data_dir());
        // Import demandé avant l'arrêt précédent : à appliquer avant tout chargement.
        notes.extend(backup::apply_pending(&persistence::data_dir()));
        for m in &notes {
            log::warn!("{m}");
        }
        persistence::data_dir()
    };

    let code = serve(data_dir.clone(), listen, demo);
    if let Some(dir) = temp_demo_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    code
}

fn serve(data_dir: PathBuf, listen: SocketAddr, demo: bool) -> ExitCode {
    if demo {
        log::info!("Mode démo : appareils simulés, aucune donnée Matter n'est lue ni écrite.");
        if let Some(code) = ensure_demo_account(&data_dir) {
            return code;
        }
    }
    let mut config = ServerConfig::new(data_dir, listen, demo);
    // Réglages de diagnostic, pour les tests d'intégration (client lent) uniquement.
    if let Some(n) = env_number("MATTER_HOME_TEST_BROADCAST_CAPACITY") {
        config.broadcast_capacity = n as usize;
    }
    if let Some(ms) = env_number("MATTER_HOME_TEST_WRITE_TIMEOUT_MS") {
        config.client_write_timeout = Duration::from_millis(ms);
    }
    let server = match Server::start(config) {
        Ok(server) => server,
        Err(e) => {
            log::error!("démarrage impossible : {e:#}");
            return ExitCode::FAILURE;
        }
    };
    // Ces lignes sont destinées à l'opérateur (et aux scripts de test) : l'empreinte est
    // publique, c'est celle que le client mémorise à sa première connexion.
    println!("Écoute : https://{}", server.addr);
    println!("Empreinte du certificat (SHA-256) : {}", server.fingerprint);
    log::info!("écoute sur https://{}", server.addr);
    log::info!("empreinte du certificat (SHA-256) : {}", server.fingerprint);
    if server.certificate_created {
        log::warn!("certificat TLS créé : à mémoriser côté client avec l'empreinte ci-dessus");
    }
    if !server.has_account {
        log::warn!(
            "aucun compte : personne ne peut se connecter. Créez-le avec « matter-home-server create-account --username NOM »."
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    watch_signals(stop.clone());
    let outcome = server.wait(&stop);
    let clean = server.stop();
    if !clean {
        log::error!("arrêt incomplet : certains threads n'ont pas fini à temps.");
    }
    match (outcome, clean) {
        (Outcome::Fatal, _) | (_, false) => ExitCode::FAILURE,
        (Outcome::ImportStaged, true) => {
            log::warn!("import en attente : relancez le serveur pour l'appliquer.");
            ExitCode::from(EXIT_IMPORT_STAGED)
        }
        (Outcome::Stopped, true) => {
            log::info!("Au revoir.");
            ExitCode::SUCCESS
        }
    }
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn temp_demo_directory() -> PathBuf {
    let mut random = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut random);
    std::env::temp_dir().join(format!("matter-home-demo-{}", hex::encode(random)))
}

/// En démo, un compte « demo » est créé s'il n'y en a pas, avec un mot de passe tiré au sort
/// affiché une seule fois sur la sortie standard (jamais dans le journal).
fn ensure_demo_account(data_dir: &std::path::Path) -> Option<ExitCode> {
    let result = security::server_dir(data_dir).and_then(|dir| {
        if AccountStore::open(&dir)?.exists() {
            return Ok(None);
        }
        let mut random = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut random);
        let password = hex::encode(random);
        account::create(&dir, "demo", &password, false)?;
        Ok(Some(password))
    });
    match result {
        Ok(Some(password)) => {
            println!("Compte de démonstration : demo / {password}");
            None
        }
        Ok(None) => None,
        Err(e) => {
            log::error!("compte de démonstration impossible : {e:#}");
            Some(ExitCode::FAILURE)
        }
    }
}

// ---------------------------------------------------------------------------
// Sous-commandes
// ---------------------------------------------------------------------------

fn data_dir_for_command(args: &mut Vec<String>) -> Result<PathBuf, ExitCode> {
    match take_value(args, "--data-dir") {
        Ok(Some(dir)) => Ok(PathBuf::from(dir)),
        Ok(None) => Ok(persistence::data_dir()),
        Err(e) => Err(usage_error(&e)),
    }
}

fn run_create_account(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let from_stdin = take_flag(&mut args, "--password-stdin");
    let replace = take_flag(&mut args, "--replace");
    let username = match take_value(&mut args, "--username") {
        Ok(Some(u)) => u,
        Ok(None) => return usage_error("--username est obligatoire"),
        Err(e) => return usage_error(&e),
    };
    let data_dir = match data_dir_for_command(&mut args) {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    let password = match read_password(from_stdin) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let result = security::server_dir(&data_dir)
        .and_then(|dir| account::create(&dir, &username, &password, replace));
    match result {
        Ok(replaced) => {
            println!(
                "Compte « {username} » {}.",
                if replaced { "remplacé" } else { "créé" }
            );
            if replaced {
                println!("Les sessions ouvertes sont fermées : redémarrez le serveur s'il tourne.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Le mot de passe vient de l'entrée standard (scripts) ou d'une saisie sans écho.
/// Jamais d'un argument de la ligne de commande, qui resterait visible dans `ps`.
fn read_password(from_stdin: bool) -> Result<String, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("lecture du mot de passe : {e}"))?;
        let password = line.trim_end_matches(['\n', '\r']).to_string();
        if password.is_empty() {
            return Err("mot de passe vide sur l'entrée standard".into());
        }
        return Ok(password);
    }
    if !std::io::stdin().is_terminal() {
        return Err("l'entrée standard n'est pas un terminal : utilisez --password-stdin".into());
    }
    let first = rpassword::prompt_password("Mot de passe : ")
        .map_err(|e| format!("saisie du mot de passe : {e}"))?;
    let again = rpassword::prompt_password("Confirmez le mot de passe : ")
        .map_err(|e| format!("saisie du mot de passe : {e}"))?;
    if first != again {
        return Err("les deux saisies sont différentes".into());
    }
    Ok(first)
}

fn run_fingerprint(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let data_dir = match data_dir_for_command(&mut args) {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    match tls::read_fingerprint(&data_dir.join(matter_home_server::persistence::SERVER_DIR)) {
        Ok(fingerprint) => {
            println!("{fingerprint}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Divers
// ---------------------------------------------------------------------------

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        log::error!("panique : {info}");
        std::process::exit(1);
    }));
}

/// Positionne `stop` à la réception de Ctrl-C ou de SIGTERM.
fn watch_signals(stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("signaux".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::warn!("signaux non surveillés : {e}");
                    return;
                }
            };
            rt.block_on(async {
                use tokio::signal::unix::{signal, SignalKind};
                let mut term = match signal(SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("SIGTERM non surveillé : {e}");
                        return;
                    }
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                stop.store(true, Ordering::Relaxed);
            });
        })
        .expect("thread signaux");
}

/// Diagnostic : vérifie que la pile mDNS de matc voit les nœuds Matter du réseau.
fn discover() -> ExitCode {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");
    rt.block_on(async {
        let discovery = match matter::discovery::Discovery::start().await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Démarrage mDNS impossible : {e:#}");
                return ExitCode::FAILURE;
            }
        };
        println!("Recherche des nœuds Matter (_matter._tcp) pendant 10 s…");
        for _ in 0..5 {
            discovery.browse_operational().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let nodes = discovery.operational_snapshot().await;
        println!("{} nœud(s) opérationnel(s) vu(s) :", nodes.len());
        for (instance, found) in nodes {
            println!("  {instance}  {}", found.addresses.join(", "));
        }
        ExitCode::SUCCESS
    })
}
