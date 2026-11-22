//! Serveur Ostal, sans fenêtre.
//!
//! - `ostal-server`                : mode normal (réseau réel) ; refuse de démarrer sans
//!   identité Matter (jamais de nouvelle fabric par accident)
//! - `ostal-server --demo`         : appareils simulés, dossier de données temporaire
//! - `ostal-server --discover`     : diagnostic en ligne de commande (mDNS)
//! - `ostal-server create-account` : crée le premier compte (mot de passe demandé sans écho)
//! - `ostal-server user …`         : comptes nominatifs (`add`, `list`, `passwd`, `disable`, `enable`)
//! - `ostal-server audit`          : lit le journal d'audit (lecture seule)
//! - `ostal-server fingerprint`    : affiche l'empreinte du certificat TLS
//! - `ostal-server init`           : crée l'identité Matter d'une installation neuve
//!   (sans sauvegarde à importer), puis quitte
//! - `ostal-server import FICHIER` : importe une sauvegarde (`.matterhome` de la v0.2.1 ou de Matter Home, `.ostal` du serveur)
//!   dans le dossier de données, puis quitte, sans démarrer Matter ni aucun réseau
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

use ostal_server::app::{Outcome, Server, ServerConfig};
use ostal_server::audit::{self, Action, AuditLog, Entry, Filter, Who};
use ostal_server::security::account::{self, AccountStore};
use ostal_server::security::{self, tls};
use ostal_server::{backup, matter, persistence};

const HELP: &str = "\
ostal-server — serveur Ostal (sans fenêtre)

Usage :
  ostal-server [--listen ADRESSE:PORT] [--data-dir DOSSIER]
                                 lance le serveur (réseau réel). Refuse de démarrer si le dossier
                                 de données n'a pas d'identité Matter (code 78) : il n'en crée
                                 jamais une tout seul, voir « init » et « import »
  ostal-server --demo      lance le serveur avec des appareils simulés, dans un
                                 dossier temporaire (écoute sur 127.0.0.1:8443)
  ostal-server --discover  liste les nœuds Matter vus en mDNS puis quitte
  ostal-server create-account --username NOM [--password-stdin] [--replace]
                     [--data-dir DOSSIER]
                                 crée le premier compte ; le mot de passe est demandé sans
                                 écho, ou lu sur l'entrée standard avec --password-stdin
                                 (jamais en argument). --replace remplace TOUS les comptes
                                 par celui-là et ferme toutes les sessions. Pour en ajouter
                                 d'autres : « user add »
  ostal-server user add --username ID [--display-name NOM] [--password-stdin]
                     [--data-dir DOSSIER]
                                 ajoute un compte (accès complet pour tous en V1)
  ostal-server user list [--data-dir DOSSIER]
                                 liste les comptes (jamais de mot de passe)
  ostal-server user passwd --username ID [--password-stdin] [--data-dir DOSSIER]
                                 définit un nouveau mot de passe et ferme toutes les sessions
                                 de ce compte, sans toucher aux autres
  ostal-server user disable --username ID [--data-dir DOSSIER]
  ostal-server user enable --username ID [--data-dir DOSSIER]
                                 désactive (ferme ses sessions) ou réactive un compte ; le
                                 dernier compte actif ne peut pas être désactivé.
                                 Ces commandes agissent aussi sur un serveur qui tourne,
                                 sans redémarrage
  ostal-server audit [--user COMPTE] [--days N] [--data-dir DOSSIER]
                                 lit le journal d'audit (qui a fait quoi, depuis quel téléphone
                                 ou ordinateur), la ligne la plus récente d'abord : date, qui,
                                 depuis, quoi. 7 jours par défaut ; --user garde un seul compte
                                 (identifiant ou nom affiché). Gardé 6 mois, jamais de mot de
                                 passe ni de jeton. Lecture seule
  ostal-server fingerprint [--data-dir DOSSIER]
                                 affiche l'empreinte SHA-256 du certificat TLS
  ostal-server init [--data-dir DOSSIER]
                                 installation neuve SANS sauvegarde : crée l'identité Matter
                                 (une nouvelle fabric : aucun appareil n'est encore appairé).
                                 Refuse si une identité existe déjà, si des données existent
                                 sans identité, ou si un import a laissé une identité à côté
  ostal-server import FICHIER.ostal [--yes] [--replace-identity] [--data-dir DOSSIER]
                                 importe une sauvegarde (même identité Matter, sans réappairage),
                                 y compris dans un dossier vide (premier déploiement). Refuse si
                                 un serveur tourne ; affiche le résumé et les identités (celle
                                 du dossier et celle du fichier) et demande confirmation (sauf
                                 avec --yes). Si l'identité (fabric) du fichier diffère de
                                 celle du dossier, l'import est refusé sans --replace-identity,
                                 même avec --yes : les appareils appairés avec l'identité
                                 actuelle ne seraient plus reconnus. Applique l'import puis
                                 quitte : ni Matter ni réseau ne sont démarrés. Un import
                                 interrompu se reprend en relançant la même commande. La v0.2.1
                                 doit être arrêtée sur TOUS vos Mac, celui-ci compris. Les
                                 anciennes données sont conservées dans « …avant-import-… ».
  ostal-server import --abandon [--data-dir DOSSIER]
                                 renonce à un import en attente ou interrompu : retire la
                                 sauvegarde en attente et met de côté le dossier préparé
                                 (« …import-abandonne-… », à supprimer à la main).
                                 Procédure : docs/migration.md
  ostal-server --version   affiche la version
  ostal-server --help      affiche cette aide

Options : --listen accepte « adresse:port » ou un simple port (défaut : 0.0.0.0:8443, ou
127.0.0.1:8443 avec --demo). --data-dir remplace le dossier de données standard (un chemin
relatif est rendu absolu). --require-identity est accepté sans effet (c'est le comportement
par défaut hors --demo). import, init, create-account et le serveur refusent de tourner en
root (sudo). --log-file CHEMIN écrit le journal dans ce fichier (0600, rotation à 5 Mo, 3 anciens fichiers)
au lieu de la sortie d'erreur.

Codes de sortie : 0 arrêt demandé, 1 erreur, 2 usage, 75 import à appliquer (relancer),
78 identité Matter absente (importez ou « init »).
Variables : RUST_LOG=debug pour des journaux détaillés.";

use ostal_server::setup::{self, EXIT_IMPORT_STAGED, EXIT_NO_IDENTITY, OTHER_INSTANCES_REMINDER};

const DEFAULT_PORT: u16 = 8443;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ostal-server {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let log_file = match take_value(&mut args, "--log-file") {
        Ok(v) => v.map(PathBuf::from),
        Err(e) => return usage_error(&e),
    };
    let mut logger = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,matc=warn,retina=off"),
    );
    if let Some(path) = &log_file {
        match ostal_server::logfile::RotatingLog::open(path) {
            Ok(file) => {
                logger.target(env_logger::Target::Pipe(Box::new(file)));
            }
            Err(e) => {
                eprintln!("journal impossible dans {} : {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    logger.init();
    // Une panique dans une sous-tâche ne doit pas laisser un serveur à moitié vivant :
    // on journalise puis on quitte, le superviseur relancera.
    install_panic_hook();
    match args.first().map(String::as_str) {
        Some("create-account") => refuse_root("« create-account »")
            .map_or_else(|c| c, |()| run_create_account(&args[1..])),
        Some("user") => refuse_root("« user »").map_or_else(|c| c, |()| run_user(&args[1..])),
        Some("fingerprint") => run_fingerprint(&args[1..]),
        Some("audit") => run_audit(&args[1..]),
        Some("init") => refuse_root("« init »").map_or_else(|c| c, |()| run_init(&args[1..])),
        Some("import") => refuse_root("« import »").map_or_else(|c| c, |()| run_import(&args[1..])),
        _ => run_server(&args),
    }
}

extern "C" {
    fn geteuid() -> u32;
}

/// Le processus tourne en root (ou le simule, pour les tests : `OSTAL_TEST_FAKE_ROOT`).
fn running_as_root() -> bool {
    // SAFETY : `geteuid` n'a ni argument ni effet de bord.
    let real = unsafe { geteuid() == 0 };
    real || std::env::var_os("OSTAL_TEST_FAKE_ROOT").is_some()
}

/// `import`, `init`, `create-account` et le serveur écrivent dans le dossier de données de
/// l'utilisateur : lancés en root (sudo), ils y laisseraient des fichiers qui lui appartiennent,
/// illisibles ensuite par le service (qui tourne sous le compte de l'utilisateur).
fn refuse_root(what: &str) -> Result<(), ExitCode> {
    if !running_as_root() {
        return Ok(());
    }
    eprintln!(
        "Refus : {what} ne se lance pas en root ni avec sudo. Il écrirait dans le dossier de \
         données des fichiers qui appartiennent à root, que le service (qui tourne sous votre \
         compte) ne pourrait plus lire. Relancez la même commande sous votre compte, sans sudo."
    );
    Err(ExitCode::FAILURE)
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

/// Valeur de `--data-dir`, rendue absolue : un chemin relatif (`donnees`, `donnees/`) n'aurait
/// pas de dossier parent que l'import puisse synchroniser.
fn parse_data_dir(text: &str) -> Result<PathBuf, String> {
    persistence::absolute_data_dir(text).map_err(|e| format!("{e:#}"))
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
    // Accepté sans effet : refuser de créer une identité est désormais le comportement par défaut
    // (les plist de la 1.0.0-rc le passaient encore).
    let _legacy_require_identity = take_flag(&mut args, "--require-identity");
    let data_dir_arg = match take_value(&mut args, "--data-dir") {
        Ok(Some(v)) => match parse_data_dir(&v) {
            Ok(dir) => Some(dir),
            Err(e) => return usage_error(&e),
        },
        Ok(None) => None,
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
    if !demo {
        if let Err(code) = refuse_root("le serveur") {
            return code;
        }
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
        match persistence::acquire_data_lock() {
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
        // Import demandé avant l'arrêt précédent : appliqué avant tout chargement.
        match prologue(true) {
            Ok(dir) => dir,
            Err(e) => {
                log::error!("{e:#}");
                return ExitCode::FAILURE;
            }
        }
    };

    // Hors démo, jamais de nouvelle identité Matter par accident : seule « init » en crée une.
    if !demo {
        if let Err(code) = require_identity_in(&data_dir) {
            return code;
        }
    }
    let code = serve(data_dir.clone(), listen, demo);
    if let Some(dir) = temp_demo_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    code
}

/// Hors démo, le dossier de données doit déjà porter une identité Matter lisible : le serveur n'en
/// crée jamais une nouvelle au premier démarrage (mauvais dossier, `HOME` différent, données
/// déplacées : tous les appareils appairés seraient oubliés). Seule « init » en crée une.
fn require_identity_in(data_dir: &std::path::Path) -> Result<(), ExitCode> {
    match backup::identity_in_dir(data_dir) {
        Ok(Some(identity)) => {
            log::info!(
                "identité Matter trouvée dans {} : {}",
                data_dir.display(),
                identity.describe()
            );
            Ok(())
        }
        Ok(None) => {
            log::error!(
                "aucune identité Matter dans {} : le serveur ne démarre pas, \
                 pour ne pas créer une nouvelle fabric. Importez votre sauvegarde avec \
                 « ostal-server import FICHIER.ostal », ou, pour une installation \
                 neuve sans sauvegarde, « ostal-server init » (voir docs/installation-serveur.md).",
                data_dir.display()
            );
            Err(ExitCode::from(EXIT_NO_IDENTITY))
        }
        Err(e) => {
            log::error!(
                "identité Matter illisible dans {} : {e:#}. Le serveur ne démarre pas.",
                data_dir.display()
            );
            Err(ExitCode::FAILURE)
        }
    }
}

fn prologue(apply: bool) -> anyhow::Result<PathBuf> {
    setup::prologue(apply)
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
    if let Some(n) = env_number("OSTAL_TEST_BROADCAST_CAPACITY") {
        config.broadcast_capacity = n as usize;
    }
    if let Some(ms) = env_number("OSTAL_TEST_WRITE_TIMEOUT_MS") {
        config.client_write_timeout = Duration::from_millis(ms);
    }
    if let Some(ms) = env_number("OSTAL_TEST_BODY_IDLE_MS") {
        config.body_idle_timeout = Duration::from_millis(ms);
    }
    // Les essais ne dépendent ni d'iCloud ni de `/usr/bin/shortcuts` : raccourcis non signés.
    if std::env::var_os("OSTAL_TEST_UNSIGNED_SHORTCUTS").is_some() {
        config.shortcut_signer = ostal_server::shortcuts::Signer::Unsigned;
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
            "aucun compte : personne ne peut se connecter. Créez-le avec « ostal-server create-account --username NOM »."
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
    std::env::temp_dir().join(format!("ostal-demo-{}", hex::encode(random)))
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

/// Dossier de données d'une sous-commande. `--data-dir` est imposé pour tout le processus
/// (donc aussi pour le verrou d'instance, qui s'en déduit) : sans cela, le verrou serait pris
/// à côté du dossier standard, jamais à côté du dossier demandé.
fn data_dir_for_command(args: &mut Vec<String>) -> Result<PathBuf, ExitCode> {
    match take_value(args, "--data-dir") {
        Ok(Some(dir)) => {
            let dir = match parse_data_dir(&dir) {
                Ok(dir) => dir,
                Err(e) => return Err(usage_error(&e)),
            };
            if !persistence::set_data_dir(dir.clone()) {
                eprintln!("dossier de données déjà fixé");
                return Err(ExitCode::FAILURE);
            }
            Ok(dir)
        }
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
    let (_lock, dir) = match prepare_accounts_dir(data_dir) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let old_ids: Vec<String> = if replace {
        account::list(&dir)
            .map(|users| users.into_iter().map(|u| u.id).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    match account::create(&dir, &username, &password, replace) {
        Ok(replaced) => {
            record_account(&dir, Action::UserCreated, &username);
            println!(
                "Compte « {username} » {}.",
                if replaced { "remplacé" } else { "créé" }
            );
            if replaced {
                println!("Les sessions ouvertes sont fermées.");
                let (tokens, subscriptions) = account::orphans_of(&dir, &old_ids);
                if tokens + subscriptions > 0 {
                    println!(
                        "{tokens} jeton(s) de raccourci ou de widget et {subscriptions} abonnement(s) aux notifications des anciens comptes restent dans les fichiers, sans effet : plus aucun compte ne les porte."
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Prépare le dossier `serveur/` pour une commande sur les comptes. Un import interrompu doit
/// être repris AVANT de créer quoi que ce soit : `server_dir` créerait un dossier de données qui
/// ne contient que `serveur/`. Si un serveur tourne, il a le verrou (et a déjà repris l'import à
/// son démarrage) : on ne touche à rien d'autre que `serveur/`, ce qui est voulu, les commandes
/// sur les comptes agissent aussi sur un serveur ouvert.
fn prepare_accounts_dir(
    data_dir: PathBuf,
) -> Result<(Option<persistence::DataLock>, PathBuf), ExitCode> {
    let (lock, data_dir) = match persistence::acquire_data_lock() {
        // Le prologue peut copier l'ancien dossier (« Matter Home », « matter-demo ») : c'est
        // le dossier qu'il rend qu'il faut utiliser, pas celui lu avant.
        Ok(lock) => match prologue(false) {
            Ok(dir) => (Some(lock), dir),
            Err(e) => {
                eprintln!("{e:#}");
                return Err(ExitCode::FAILURE);
            }
        },
        Err(e) => {
            // L'ancienne application tient l'ancien verrou : ses données ne sont pas encore
            // copiées, on n'écrit rien (ni dans l'ancien dossier, ni dans un nouveau vide).
            if e.downcast_ref::<persistence::LockHeld>()
                .is_some_and(|h| h.path != persistence::instance_lock_path())
            {
                eprintln!(
                    "{e:#}\nQuittez l'ancienne application Matter Home Server, puis relancez \
                         cette commande."
                );
                return Err(ExitCode::FAILURE);
            }
            if backup::recovery_needed(&data_dir) {
                eprintln!(
                    "Un import interrompu attend d'être repris, et un serveur tient le verrou. \
                     Arrêtez-le, puis relancez cette commande."
                );
                return Err(ExitCode::FAILURE);
            }
            (None, persistence::data_dir())
        }
    };
    match security::server_dir(&data_dir) {
        Ok(dir) => Ok((lock, dir)),
        Err(e) => {
            eprintln!("{e:#}");
            Err(ExitCode::FAILURE)
        }
    }
}

/// `user add|list|passwd|disable|enable` : comptes nominatifs.
fn run_user(args: &[String]) -> ExitCode {
    let Some((action, rest)) = args.split_first() else {
        return usage_error("user attend add, list, passwd, disable ou enable");
    };
    let mut args = rest.to_vec();
    let from_stdin = take_flag(&mut args, "--password-stdin");
    let display_name = match take_value(&mut args, "--display-name") {
        Ok(v) => v,
        Err(e) => return usage_error(&e),
    };
    let username = match take_value(&mut args, "--username") {
        Ok(v) => v,
        Err(e) => return usage_error(&e),
    };
    let data_dir = match data_dir_for_command(&mut args) {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    let action = action.as_str();
    if !matches!(action, "add" | "list" | "passwd" | "disable" | "enable") {
        return usage_error(&format!("user {action} : commande inconnue"));
    }
    if action != "add" && display_name.is_some() {
        return usage_error("--display-name ne sert qu'avec « user add »");
    }
    if action == "list" && username.is_some() {
        return usage_error("user list n'accepte pas --username");
    }
    if !matches!(action, "add" | "passwd") && from_stdin {
        return usage_error("--password-stdin ne sert qu'avec « user add » et « user passwd »");
    }
    let username = match (action, username) {
        ("list", _) => String::new(),
        (_, Some(u)) => u,
        (_, None) => return usage_error("--username est obligatoire"),
    };
    let password = if matches!(action, "add" | "passwd") {
        match read_password(from_stdin) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let (_lock, dir) = match prepare_accounts_dir(data_dir) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let password = password.unwrap_or_default();
    let result: anyhow::Result<()> = (|| {
        match action {
            "add" => {
                let user = account::add(&dir, &username, display_name.as_deref(), &password)?;
                record_account(&dir, Action::UserCreated, &user.display_name);
                println!("Compte « {} » créé ({}).", user.username, user.display_name);
            }
            "list" => {
                let users = account::list(&dir)?;
                if users.is_empty() {
                    println!("Aucun compte.");
                }
                for u in users {
                    println!(
                        "{}\t{}\t{}",
                        u.username,
                        u.display_name,
                        if u.active { "actif" } else { "désactivé" }
                    );
                }
            }
            "passwd" => {
                let user = account::set_password(&dir, &username, &password)?;
                record_account(&dir, Action::PasswordChanged, &user.display_name);
                println!(
                    "Mot de passe de « {} » changé : ses sessions ouvertes sont fermées.",
                    user.username
                );
            }
            "disable" => {
                let user = account::set_active(&dir, &username, false)?;
                record_account(&dir, Action::UserDisabled, &user.display_name);
                println!(
                    "Compte « {} » désactivé : ses sessions ouvertes sont fermées.",
                    user.username
                );
            }
            _ => {
                let user = account::set_active(&dir, &username, true)?;
                record_account(&dir, Action::UserEnabled, &user.display_name);
                println!("Compte « {} » réactivé.", user.username);
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
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

/// `import FICHIER [--yes] [--replace-identity] [--data-dir DOSSIER]` et
/// `import --abandon [--data-dir DOSSIER]` : voir `docs/migration.md`.
fn run_import(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let yes = take_flag(&mut args, "--yes");
    let replace_identity = take_flag(&mut args, "--replace-identity");
    let abandon = take_flag(&mut args, "--abandon");
    if let Err(code) = data_dir_for_command(&mut args) {
        return code;
    }
    let file = match args.as_slice() {
        [] if abandon => None,
        [file] if !file.starts_with("--") && !abandon => Some(PathBuf::from(file)),
        _ if abandon => return usage_error("import --abandon n'accepte pas de fichier"),
        [] => return usage_error("import attend le fichier .ostal à importer"),
        [other, ..] if other.starts_with("--") => {
            return usage_error(&format!("Option inconnue : {other}"))
        }
        _ => return usage_error("import n'accepte qu'un seul fichier"),
    };
    // Rien de ce qui suit ne démarre Matter ni le réseau : ni `Server::start`, ni mDNS.
    let _lock = match persistence::acquire_data_lock() {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    let Some(file) = file else {
        // Pas de reprise d'abord : après une coupure entre les deux renommages, « abandonner »
        // remet les anciennes données en place au lieu de terminer l'import.
        if let Err(e) = persistence::migrate_legacy_data_dir() {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
        return run_abandon(&persistence::data_dir());
    };
    let data_dir = match prologue(false) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    match backup::pending_kind(&data_dir, &file) {
        Ok(backup::PendingKind::None) => {}
        Ok(backup::PendingKind::SameFile) => println!(
            "Un import de ce même fichier a été interrompu : il va être repris là où il en était."
        ),
        Ok(backup::PendingKind::Other) => {
            eprintln!(
                "Une autre sauvegarde est déjà en attente dans {} (import demandé par l'API, ou \
                 import d'un autre fichier interrompu). Lancez le serveur pour qu'il l'applique, \
                 ou renoncez-y avec « ostal-server import --abandon » : cela retire ce \
                 fichier et met de côté le dossier « …import-nouveau » déjà préparé, sous le nom \
                 « …import-abandonne-… ».",
                backup::pending_path(&data_dir).display()
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    }
    let summary = match backup::inspect(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    println!("Fichier : {}", file.display());
    println!("Dossier de données : {}", data_dir.display());
    println!("Contenu : {}", summary.describe());
    if let Err(code) = check_identities(&data_dir, &file, replace_identity) {
        return code;
    }
    if data_dir.join(persistence::FILE_NAME).exists() {
        println!(
            "ATTENTION : ce dossier contient déjà des données Matter. Elles seront mises de côté \
             (dossier « …avant-import-… », conservé) et remplacées par celles du fichier."
        );
    }
    println!("{OTHER_INSTANCES_REMINDER}");
    if !yes && !confirm("Importer cette sauvegarde ? [o/N] ") {
        eprintln!("Import annulé : rien n'a été modifié.");
        return ExitCode::FAILURE;
    }
    match backup::import_file(&data_dir, &file) {
        Ok(done) => {
            AuditLog::of_data_dir(&data_dir)
                .record(Entry::new(Action::Import, &Who::command_line()));
            if let Some(note) = &done.resumed {
                println!("{note}");
            }
            println!("Import terminé : {}.", done.summary.describe());
            println!(
                "Anciennes données conservées dans {} (à supprimer à la main quand tout \
                 fonctionne).",
                done.previous.display()
            );
            println!(
                "Étapes suivantes : « ostal-server create-account --username NOM » si le \
                 compte n'existe pas encore, puis démarrer le serveur. N'ouvrez plus \
                 l'application de bureau avec ces appareils."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Montre l'identité du dossier de données et celle du fichier. Une fabric différente (ou un
/// `controller.json` illisible) n'est remplacée qu'avec `--replace-identity` : `--yes` seul ne
/// suffit jamais, et rien n'est écrasé sans lui.
fn check_identities(
    data_dir: &std::path::Path,
    file: &std::path::Path,
    replace_identity: bool,
) -> Result<(), ExitCode> {
    use setup::IdentityState;
    let comparison = match setup::compare_identities(data_dir, file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e:#}");
            return Err(ExitCode::FAILURE);
        }
    };
    println!(
        "Identité de la sauvegarde : {}",
        comparison.incoming.describe()
    );
    match &comparison.current {
        IdentityState::Missing => {
            println!("Identité actuelle du dossier : aucune (rien n'est remplacé).")
        }
        IdentityState::Present(current) => {
            println!("Identité actuelle du dossier : {}", current.describe());
            if !comparison.differs {
                println!("Même fabric : les appareils appairés restent reconnus.");
            }
        }
        IdentityState::Unreadable(e) => {
            println!("Identité actuelle du dossier : illisible ({e}).")
        }
    }
    if let Err(message) = setup::check_replace_allowed(&comparison, replace_identity) {
        eprintln!("{message}");
        return Err(ExitCode::FAILURE);
    }
    if comparison.differs {
        println!("--replace-identity : l'identité actuelle sera remplacée par celle du fichier.");
    }
    Ok(())
}

/// `import --abandon` : renoncer à un import en attente ou interrompu.
fn run_abandon(data_dir: &std::path::Path) -> ExitCode {
    match backup::abandon(data_dir) {
        Ok(done) if done.is_empty() => {
            println!("Aucun import en attente ni interrompu : rien à abandonner.");
            ExitCode::SUCCESS
        }
        Ok(done) => {
            for line in done {
                println!("{line}");
            }
            println!(
                "Les données actuelles ({}) n'ont pas été modifiées.",
                data_dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `init [--data-dir DOSSIER]` : identité Matter d'une installation neuve, sans sauvegarde.
fn run_init(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    if let Err(code) = data_dir_for_command(&mut args) {
        return code;
    }
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    let _lock = match persistence::acquire_data_lock() {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    let data_dir = match prologue(false) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    match setup::init_identity(&data_dir) {
        Ok(fabric) => {
            println!(
                "Identité Matter créée dans {} (fabric {fabric:#x}). Aucun appareil n'est encore \
                 appairé : faites-le depuis le client.",
                data_dir.display()
            );
            println!("Étape suivante : « ostal-server create-account --username NOM ».");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Question oui/non sur l'entrée standard. Sans terminal, jamais de « oui » implicite.
fn confirm(question: &str) -> bool {
    use std::io::Write;
    if !std::io::stdin().is_terminal() {
        eprintln!("L'entrée standard n'est pas un terminal : ajoutez --yes pour confirmer.");
        return false;
    }
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(
        line.trim().to_lowercase().as_str(),
        "o" | "oui" | "y" | "yes"
    )
}

/// Note au journal d'audit une opération sur un compte faite en ligne de commande. `serveur_dir`
/// est le dossier `serveur/` que la commande vient de modifier.
fn record_account(server_dir: &std::path::Path, action: Action, name: &str) {
    AuditLog::open(server_dir).record(Entry::new(action, &Who::command_line()).target(name));
}

/// `audit [--user COMPTE] [--days N] [--data-dir DOSSIER]` : lit le journal d'audit, la plus
/// récente d'abord. Lecture seule : ne démarre rien et ne prend aucun verrou.
fn run_audit(args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let user = match take_value(&mut args, "--user") {
        Ok(v) => v,
        Err(e) => return usage_error(&e),
    };
    let days = match take_value(&mut args, "--days") {
        Ok(Some(d)) => match d.parse::<u32>() {
            Ok(n) if n >= 1 => n,
            _ => return usage_error("--days attend un nombre de jours (1 ou plus)"),
        },
        Ok(None) => 7,
        Err(e) => return usage_error(&e),
    };
    let data_dir = match data_dir_for_command(&mut args) {
        Ok(d) => d,
        Err(code) => return code,
    };
    if let Some(unknown) = args.first() {
        return usage_error(&format!("Option inconnue : {unknown}"));
    }
    let server_dir = data_dir.join(persistence::SERVER_DIR);
    // `--user` accepte l'identifiant de connexion, le nom affiché ou l'identifiant interne.
    let user_id = user.as_deref().map(|wanted| {
        let key = account::username_key(wanted);
        account::list(&server_dir)
            .ok()
            .and_then(|users| {
                users.into_iter().find(|u| {
                    account::username_key(&u.username) == key
                        || account::username_key(&u.display_name) == key
                        || u.id == wanted
                })
            })
            .map_or_else(|| wanted.to_string(), |u| u.id)
    });
    let now = ostal_server::events::now_unix();
    let filter = Filter {
        user: user_id,
        since: Some(now - f64::from(days) * 86_400.0),
        ..Filter::default()
    };
    let entries = AuditLog::reader(&server_dir).read(&filter);
    if entries.is_empty() {
        println!("Aucune entrée sur les {days} derniers jours.");
        return ExitCode::SUCCESS;
    }
    for entry in &entries {
        let (_, what) = audit::describe(entry);
        println!(
            "{}\t{}\t{}\t{}",
            audit::stamp_label(entry.at),
            audit::who_label(entry),
            audit::from_label(entry),
            what
        );
    }
    ExitCode::SUCCESS
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
    // Un import coupé entre ses deux renommages laisse le dossier de données absent : l'empreinte
    // n'est pas perdue, elle est dans le dossier préparé que la prochaine commande met en place.
    if backup::recovery_needed(&data_dir) {
        eprintln!(
            "Un import interrompu attend d'être repris : le dossier de données {} est absent ou \
             sans identité, alors que le dossier préparé « …import-nouveau » est complet. \
             L'empreinte du certificat s'y trouve ; elle sera lisible dès que l'import sera \
             repris. Relancez la même commande « import » (ou « import --abandon » pour y \
             renoncer), ou démarrez le serveur, qui reprend l'échange.",
            data_dir.display()
        );
        return ExitCode::FAILURE;
    }
    match tls::read_fingerprint(&data_dir.join(ostal_server::persistence::SERVER_DIR)) {
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
