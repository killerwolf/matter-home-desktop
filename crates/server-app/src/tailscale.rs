//! « Accès web (Tailscale) » : active ou coupe `tailscale serve` pour le port du serveur, sans
//! passer par le Terminal.
//!
//! L'état est porté par Tailscale lui-même (`serve --bg` est persistant) : rien n'est rangé ici.
//! Tout ce qui se décide est pur et se teste sur des fixtures JSON (arguments, lecture du statut,
//! décision d'activer ou de couper) ; seul [`RealRunner`] lance un processus, avec un chemin
//! absolu et des arguments fixes, jamais à travers un shell. Les tests n'emploient que des faux
//! lanceurs (et un script jetable), jamais le vrai binaire `tailscale`.
//!
//! Seule la publication privée sur le tailnet est proposée : l'ouverture à tout Internet n'existe
//! pas ici, et un test le garantit.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// Variable d'environnement pour les essais : chemin d'un faux `tailscale`. Quand elle est
/// posée, elle est le seul binaire employé (aucune recherche ailleurs).
pub const BINARY_ENV: &str = "MATTER_HOME_TAILSCALE_BIN";

/// Où chercher le binaire, dans l'ordre (puis le `PATH`).
pub const CANDIDATES: [&str; 3] = [
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
];

/// Délai maximal d'une lecture d'état.
pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Délai maximal d'une activation ou d'une coupure.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ce que le menu sait de l'accès web.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAccess {
    /// Pas encore lu (ou opération en cours).
    Checking,
    NotInstalled,
    /// Installé, mais pas connecté au tailnet.
    Disconnected,
    /// Connecté, rien n'est publié sur le port 443.
    Off {
        dns: Option<String>,
    },
    /// Notre `serve` est en place : le client est joignable à `https://<dns>/app/`.
    On {
        dns: String,
    },
    /// Un autre `serve` occupe déjà le port 443 : on n'y touche pas.
    PortBusy,
    /// Le port 443 est ouvert à tout Internet (Funnel) : on ne l'active ni ne le coupe, il se
    /// coupe dans Tailscale.
    Funnel,
    /// Serve n'est pas autorisé sur le tailnet ; `url` mène à l'autorisation.
    NeedsAuth {
        url: String,
    },
    /// État illisible (délai dépassé, réponse inattendue).
    Unavailable,
}

impl WebAccess {
    /// L'adresse du client web, quand l'accès est actif.
    pub fn web_url(&self) -> Option<String> {
        match self {
            WebAccess::On { dns } => Some(format!("https://{dns}/app/")),
            _ => None,
        }
    }
}

// ---- le binaire ------------------------------------------------------------------------------

/// Cherche le binaire : la variable d'essai seule si elle est posée ; sinon les emplacements
/// connus, puis le `PATH`. Seuls des chemins absolus sont retenus.
pub fn find_binary(
    env_override: Option<&str>,
    path_var: Option<&str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(forced) = env_override.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(forced);
        return (p.is_absolute() && exists(&p)).then_some(p);
    }
    for c in CANDIDATES {
        let p = PathBuf::from(c);
        if exists(&p) {
            return Some(p);
        }
    }
    path_var?
        .split(':')
        .filter(|d| Path::new(d).is_absolute())
        .map(|d| Path::new(d).join("tailscale"))
        .find(|p| exists(p))
}

/// Le binaire de ce Mac. En démonstration, jamais le vrai : seulement `BINARY_ENV`.
pub fn locate(demo: bool) -> Option<PathBuf> {
    // La variable d'essai n'existe qu'en démonstration ou dans un build de développement : une
    // app livrée ne lance jamais qu'un vrai Tailscale.
    let forced = if demo || cfg!(debug_assertions) {
        std::env::var(BINARY_ENV).ok()
    } else {
        None
    };
    if demo && forced.as_deref().is_none_or(str::is_empty) {
        return None;
    }
    let path = std::env::var("PATH").ok();
    find_binary(forced.as_deref(), path.as_deref(), |p| p.is_file())
}

// ---- les arguments ---------------------------------------------------------------------------

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// Publier le serveur (en HTTPS, certificat auto-signé accepté en local) sur le tailnet, de façon
/// persistante.
pub fn serve_enable_args(port: u16) -> Vec<String> {
    vec![
        "serve".into(),
        "--bg".into(),
        format!("https+insecure://localhost:{port}"),
    ]
}

/// Retirer la publication du port 443.
pub fn serve_disable_args() -> Vec<String> {
    strings(&["serve", "--https=443", "off"])
}

pub fn serve_status_args() -> Vec<String> {
    strings(&["serve", "status", "--json"])
}

pub fn status_args() -> Vec<String> {
    // Sans la liste des pairs : plus léger, et `Self` y reste.
    strings(&["status", "--json", "--peers=false"])
}

// ---- la lecture des réponses -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsStatus {
    /// `BackendState` vaut « Running ».
    pub running: bool,
    /// `Self.DNSName`, sans le point final, s'il est sûr à mettre dans une adresse.
    pub dns_name: Option<String>,
}

/// Un nom MagicDNS ne contient que des lettres, chiffres, tirets et points : rien d'autre ne
/// finit dans une adresse ouverte ou copiée.
fn clean_dns(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.');
    let ok = !name.is_empty()
        && name.contains('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    ok.then(|| name.to_ascii_lowercase())
}

/// Lit `tailscale status --json`.
pub fn parse_status(json: &str) -> Result<TsStatus, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let state = v
        .get("BackendState")
        .and_then(|s| s.as_str())
        .ok_or("pas de BackendState")?;
    let dns_name = v
        .pointer("/Self/DNSName")
        .and_then(|s| s.as_str())
        .and_then(clean_dns);
    Ok(TsStatus {
        running: state == "Running",
        dns_name,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeState {
    /// Rien sur le port 443.
    Off,
    /// Notre cible, et seulement elle.
    Ours,
    /// Autre chose sur le port 443.
    Foreign,
    /// Le port 443 est ouvert à tout Internet.
    Funnel,
}

/// Lit `tailscale serve status --json` pour le port 443 : c'est à nous seulement si le seul
/// gestionnaire est `/` vers `https+insecure://localhost:<port>` (ou 127.0.0.1).
pub fn parse_serve(json: &str, port: u16) -> Result<ServeState, String> {
    // Sans aucune configuration, la commande peut ne rien afficher.
    if json.trim().is_empty() {
        return Ok(ServeState::Off);
    }
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    // `null` : aucune configuration.
    if v.is_null() {
        return Ok(ServeState::Off);
    }
    if !v.is_object() {
        return Err("réponse inattendue".into());
    }
    // Les sessions « Foreground » et les « Services » portent leur propre TCP/Web.
    let others: Vec<&serde_json::Value> = ["Foreground", "Services"]
        .iter()
        .filter_map(|k| v.get(*k).and_then(|f| f.as_object()))
        .flat_map(|m| m.values())
        .collect();
    if funnel_on_443(&v) || others.iter().any(|o| funnel_on_443(o)) {
        return Ok(ServeState::Funnel);
    }
    let others_touch = others.iter().any(|o| touches_443(o));
    let tcp_443 = v.pointer("/TCP/443").is_some();
    let web_443 = web_443_sites(&v);
    if !tcp_443 && web_443.is_empty() && !others_touch {
        return Ok(ServeState::Off);
    }
    let targets = [
        format!("https+insecure://localhost:{port}"),
        format!("https+insecure://127.0.0.1:{port}"),
    ];
    let ours = !others_touch
        && tcp_443
        && !web_443.is_empty()
        && web_443.iter().all(|site| {
            site.get("Handlers")
                .and_then(|h| h.as_object())
                .is_some_and(|h| {
                    h.len() == 1
                        && h.get("/")
                            .and_then(|r| r.get("Proxy"))
                            .and_then(|p| p.as_str())
                            .is_some_and(|p| targets.iter().any(|t| t == p))
                })
        });
    Ok(if ours {
        ServeState::Ours
    } else {
        ServeState::Foreign
    })
}

fn web_443_sites(v: &serde_json::Value) -> Vec<&serde_json::Value> {
    v.get("Web")
        .and_then(|w| w.as_object())
        .map(|w| {
            w.iter()
                .filter(|(host, _)| host.ends_with(":443"))
                .map(|(_, site)| site)
                .collect()
        })
        .unwrap_or_default()
}

fn touches_443(v: &serde_json::Value) -> bool {
    v.pointer("/TCP/443").is_some() || !web_443_sites(v).is_empty()
}

/// `AllowFunnel` vaut `true` pour une clé en « :443 ».
fn funnel_on_443(v: &serde_json::Value) -> bool {
    v.get("AllowFunnel")
        .and_then(|f| f.as_object())
        .is_some_and(|f| {
            f.iter()
                .any(|(host, on)| host.ends_with(":443") && on.as_bool() == Some(true))
        })
}

/// L'hôte d'une adresse https (sans identifiant ni port), ou `None`.
pub fn https_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty() && !host.contains(['@', ':'])).then_some(host)
}

/// Une adresse que l'app accepte d'ouvrir : l'autorisation de Tailscale ou un nom du tailnet.
pub fn is_openable(url: &str) -> bool {
    url.bytes().all(|b| b.is_ascii_graphic())
        && https_host(url).is_some_and(|h| h == "login.tailscale.com" || h.ends_with(".ts.net"))
}

/// Après un échec de `serve`, quelle que soit la phrase affichée : la première adresse https
/// dont l'hôte est exactement `login.tailscale.com`.
pub fn parse_auth_link(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_end_matches(['.', ',', ')']))
        .find(|w| https_host(w) == Some("login.tailscale.com") && is_openable(w))
        .map(str::to_string)
}

/// Le port d'une adresse « https://hôte:port ».
pub fn port_of_url(url: &str) -> Option<u16> {
    url.rsplit_once(':')?.1.trim_end_matches('/').parse().ok()
}

// ---- l'exécution -----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    /// Le délai a été dépassé : le processus a été tué, la sortie est partielle.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

pub trait Runner: Send + Sync {
    fn run(&self, args: &[String], timeout: Duration) -> Result<Output, String>;
}

/// Lance le binaire (chemin absolu), arguments fixes, aucun shell.
pub struct RealRunner {
    pub bin: PathBuf,
}

/// Les lecteurs de tuyaux envoient leur texte ici : on ne les attend jamais plus de ce délai
/// (un petit-enfant qui garde le tuyau ouvert ne bloque rien).
const PIPE_GRACE: Duration = Duration::from_millis(500);

fn drain<R: Read + Send + 'static>(mut r: R) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

extern "C" {
    fn killpg(pgrp: i32, sig: i32) -> i32;
}

/// Tue le groupe de processus lancé par nous (jamais un autre).
fn kill_group(pid: u32) {
    // SAFETY : `killpg` n'a pas d'effet mémoire ; le groupe est celui de notre enfant
    // (`process_group(0)` : son pid est celui du groupe).
    unsafe {
        killpg(pid as i32, 9);
    }
}

impl RealRunner {
    /// La commande : chemin absolu, arguments fixes, mode ligne de commande forcé (le binaire de
    /// l'app Tailscale devine sinon entre interface et CLI, et lancé par launchd il pourrait
    /// choisir l'interface), son propre groupe de processus.
    pub fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args)
            .env("TAILSCALE_BE_CLI", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        c
    }
}

impl Runner for RealRunner {
    fn run(&self, args: &[String], timeout: Duration) -> Result<Output, String> {
        let mut child = self
            .command(args)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.bin.display()))?;
        let pid = child.id();
        let out = drain(child.stdout.take().expect("stdout demandé"));
        let err = drain(child.stderr.take().expect("stderr demandé"));
        let deadline = Instant::now() + timeout;
        let (success, timed_out) = loop {
            match child.try_wait() {
                Ok(Some(status)) => break (status.success(), false),
                Ok(None) if Instant::now() >= deadline => {
                    // Tout le groupe : un script qui lance un enfant ne le laisse pas vivant.
                    kill_group(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let stdout = out.recv_timeout(PIPE_GRACE).unwrap_or_default();
        let stderr = err.recv_timeout(PIPE_GRACE).unwrap_or_default();
        if !timed_out {
            // Un petit-enfant resté en vie après la fin de la commande : on le retire.
            kill_group(pid);
        }
        Ok(Output {
            success,
            timed_out,
            stdout,
            stderr,
        })
    }
}

/// Lit l'état complet (lecture seule).
pub fn read_state(runner: &dyn Runner, port: u16) -> WebAccess {
    // Un seul budget de 3 s pour les deux commandes.
    let deadline = Instant::now() + READ_TIMEOUT;
    let status = match runner.run(&status_args(), READ_TIMEOUT) {
        Ok(o) if o.timed_out => return WebAccess::Unavailable,
        // Le démon ne répond pas : l'application Tailscale n'est pas lancée.
        Ok(o) if !o.success => return WebAccess::Disconnected,
        Ok(o) => match parse_status(&o.stdout) {
            Ok(s) => s,
            Err(_) => return WebAccess::Unavailable,
        },
        Err(_) => return WebAccess::Unavailable,
    };
    if !status.running {
        return WebAccess::Disconnected;
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return WebAccess::Unavailable;
    }
    let serve = match runner.run(&serve_status_args(), left) {
        Ok(o) if o.success && !o.timed_out => parse_serve(&o.stdout, port),
        _ => return WebAccess::Unavailable,
    };
    match (serve, status.dns_name) {
        (Ok(ServeState::Off), dns) => WebAccess::Off { dns },
        (Ok(ServeState::Ours), Some(dns)) => WebAccess::On { dns },
        (Ok(ServeState::Ours), None) => WebAccess::Unavailable,
        (Ok(ServeState::Foreign), _) => WebAccess::PortBusy,
        (Ok(ServeState::Funnel), _) => WebAccess::Funnel,
        (Err(_), _) => WebAccess::Unavailable,
    }
}

/// Active ou coupe l'accès, puis relit l'état. N'écrase jamais un autre `serve` : on relit
/// d'abord, et seul un état « Off » (pour activer) ou « On » (pour couper) mène à une commande.
pub fn toggle(runner: &dyn Runner, port: u16, enable: bool) -> WebAccess {
    let before = read_state(runner, port);
    match (&before, enable) {
        (WebAccess::Off { .. }, true) => {
            match runner.run(&serve_enable_args(port), WRITE_TIMEOUT) {
                Ok(o) => {
                    // Serve non autorisé : la commande attend l'autorisation ; on l'arrête (délai)
                    // et on garde le lien qu'elle a affiché.
                    if !o.success {
                        if let Some(url) = parse_auth_link(&o.combined()) {
                            return WebAccess::NeedsAuth { url };
                        }
                        log::warn!(
                            "tailscale serve : échec ({})",
                            o.combined().trim().lines().next().unwrap_or("")
                        );
                    }
                }
                Err(e) => log::warn!("tailscale serve : {e}"),
            }
        }
        (WebAccess::On { .. }, false) => match runner.run(&serve_disable_args(), WRITE_TIMEOUT) {
            Ok(o) if !o.success => log::warn!(
                "tailscale serve off : échec ({})",
                o.combined().trim().lines().next().unwrap_or("")
            ),
            Ok(_) => {}
            Err(e) => log::warn!("tailscale serve off : {e}"),
        },
        // Rien à faire (déjà dans l'état voulu, port occupé par autre chose, Tailscale absent…).
        _ => return before,
    }
    read_state(runner, port)
}

/// Lit l'état hors du fil principal : le résultat arrive sur le canal.
pub fn spawn_read(demo: bool, port: u16) -> Receiver<WebAccess> {
    spawn_job(demo, move |runner| read_state(runner, port))
}

/// Active ou coupe hors du fil principal.
pub fn spawn_toggle(demo: bool, port: u16, enable: bool) -> Receiver<WebAccess> {
    spawn_job(demo, move |runner| toggle(runner, port, enable))
}

fn spawn_job(
    demo: bool,
    job: impl FnOnce(&dyn Runner) -> WebAccess + Send + 'static,
) -> Receiver<WebAccess> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let state = match locate(demo) {
            Some(bin) => job(&RealRunner { bin }),
            None => WebAccess::NotInstalled,
        };
        let _ = tx.send(state);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const STATUS_RUNNING: &str = include_str!("../tests/fixtures/tailscale/status-running.json");
    const STATUS_STOPPED: &str = include_str!("../tests/fixtures/tailscale/status-stopped.json");
    const STATUS_LOGIN: &str = include_str!("../tests/fixtures/tailscale/status-needs-login.json");
    const SERVE_OURS: &str = include_str!("../tests/fixtures/tailscale/serve-ours.json");
    const SERVE_EMPTY: &str = include_str!("../tests/fixtures/tailscale/serve-empty.json");
    const SERVE_FOREIGN: &str = include_str!("../tests/fixtures/tailscale/serve-foreign.json");
    const SERVE_OTHER_PORT: &str =
        include_str!("../tests/fixtures/tailscale/serve-other-port.json");
    const SERVE_PLUS_PATH: &str =
        include_str!("../tests/fixtures/tailscale/serve-ours-plus-path.json");
    const SERVE_FUNNEL: &str = include_str!("../tests/fixtures/tailscale/serve-funnel.json");
    const SERVE_FOREGROUND: &str =
        include_str!("../tests/fixtures/tailscale/serve-foreground.json");
    const SERVE_SERVICES: &str = include_str!("../tests/fixtures/tailscale/serve-services.json");
    /// Sortie PLAUSIBLE, reconstituée d'après le texte de la CLI (non capturée : on ne lance pas
    /// d'activation pour l'obtenir) ; nœud fictif.
    const NOT_ENABLED: &str = include_str!("../tests/fixtures/tailscale/serve-not-enabled.txt");

    /// Un faux Tailscale : répond selon les arguments et se souvient de ce qu'on lui a demandé.
    struct Fake {
        status: Output,
        serve: Output,
        enable: Output,
        delay: Duration,
        timeouts: Mutex<Vec<Duration>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    fn ok(text: &str) -> Output {
        Output {
            success: true,
            stdout: text.into(),
            ..Output::default()
        }
    }

    fn fake(status: &str, serve: &str) -> Fake {
        Fake {
            status: ok(status),
            serve: ok(serve),
            enable: ok(""),
            delay: Duration::ZERO,
            timeouts: Mutex::default(),
            calls: Mutex::default(),
        }
    }

    impl Runner for Fake {
        fn run(&self, args: &[String], t: Duration) -> Result<Output, String> {
            self.timeouts.lock().unwrap().push(t);
            std::thread::sleep(self.delay);
            self.calls.lock().unwrap().push(args.to_vec());
            Ok(
                match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["status", "--json", "--peers=false"] => self.status.clone(),
                    ["serve", "status", "--json"] => self.serve.clone(),
                    ["serve", "--bg", _] => self.enable.clone(),
                    ["serve", "--https=443", "off"] => ok(""),
                    _ => panic!("commande inattendue : {args:?}"),
                },
            )
        }
    }

    fn mutations(f: &Fake) -> Vec<Vec<String>> {
        f.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a[..] != status_args()[..] && a[..] != serve_status_args()[..])
            .cloned()
            .collect()
    }

    #[test]
    fn enable_and_disable_arguments_are_exactly_the_documented_ones() {
        assert_eq!(
            serve_enable_args(8443),
            ["serve", "--bg", "https+insecure://localhost:8443"]
        );
        assert_eq!(serve_disable_args(), ["serve", "--https=443", "off"]);
        assert_eq!(serve_status_args(), ["serve", "status", "--json"]);
        assert_eq!(status_args(), ["status", "--json", "--peers=false"]);
    }

    #[test]
    fn nothing_ever_publishes_to_the_whole_internet() {
        for port in [1, 8443, 18443, 65535] {
            for args in [
                serve_enable_args(port),
                serve_disable_args(),
                serve_status_args(),
                status_args(),
            ] {
                assert!(args.iter().all(|a| !a.to_lowercase().contains("funnel")));
            }
        }
        // Ni dans le code de ce module (la partie avant les tests).
        let production = include_str!("tailscale.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .to_lowercase();
        // Le mot n'apparaît que pour LIRE l'état (AllowFunnel), jamais comme commande.
        assert!(!production.contains("\"funnel\""));
        assert!(!production.contains("[\"funnel"));
    }

    #[test]
    fn binary_is_searched_in_the_documented_order() {
        let all = |_: &Path| true;
        assert_eq!(
            find_binary(None, Some("/usr/bin"), all),
            Some(PathBuf::from(CANDIDATES[0]))
        );
        let only_brew = |p: &Path| p == Path::new(CANDIDATES[1]) || p == Path::new(CANDIDATES[2]);
        assert_eq!(
            find_binary(None, None, only_brew),
            Some(PathBuf::from("/opt/homebrew/bin/tailscale"))
        );
        let only_path = |p: &Path| p == Path::new("/somewhere/bin/tailscale");
        assert_eq!(
            find_binary(None, Some("rel:/somewhere/bin"), only_path),
            Some(PathBuf::from("/somewhere/bin/tailscale"))
        );
        assert_eq!(find_binary(None, Some("/usr/bin"), |_| false), None);
        // Un PATH relatif n'est jamais suivi.
        assert_eq!(
            find_binary(None, Some("bin:."), |p| p.starts_with("bin")),
            None
        );
    }

    #[test]
    fn the_test_variable_is_the_only_binary_used_when_set() {
        let fake_bin = "/tmp/faux-tailscale";
        assert_eq!(
            find_binary(Some(fake_bin), Some("/usr/bin"), |_| true),
            Some(PathBuf::from(fake_bin))
        );
        // Introuvable : pas de repli sur le vrai.
        assert_eq!(
            find_binary(Some(fake_bin), None, |p| p != Path::new(fake_bin)),
            None
        );
        // Chemin relatif refusé.
        assert_eq!(find_binary(Some("tailscale"), None, |_| true), None);
    }

    #[test]
    fn status_json_gives_the_magic_dns_name() {
        let s = parse_status(STATUS_RUNNING).unwrap();
        assert!(s.running);
        assert_eq!(
            s.dns_name.as_deref(),
            Some("mac-exemple.tailnet-exemple.ts.net")
        );
        assert!(!parse_status(STATUS_STOPPED).unwrap().running);
        let login = parse_status(STATUS_LOGIN).unwrap();
        assert!(!login.running);
        assert_eq!(login.dns_name, None);
        assert!(parse_status("pas du json").is_err());
        assert!(parse_status("{}").is_err());
    }

    #[test]
    fn a_dns_name_with_odd_characters_is_refused() {
        for bad in [
            "a b.ts.net.",
            "x;rm.ts.net.",
            "x/../y.ts.net",
            "localhost",
            "",
        ] {
            assert_eq!(clean_dns(bad), None, "{bad}");
        }
    }

    #[test]
    fn serve_json_tells_ours_from_foreign() {
        assert_eq!(parse_serve(SERVE_EMPTY, 8443), Ok(ServeState::Off));
        assert_eq!(parse_serve(SERVE_OURS, 8443), Ok(ServeState::Ours));
        // Même cible, mais un autre port de serveur : ce n'est pas la nôtre.
        assert_eq!(parse_serve(SERVE_OURS, 18443), Ok(ServeState::Foreign));
        assert_eq!(parse_serve(SERVE_FOREIGN, 8443), Ok(ServeState::Foreign));
        assert_eq!(parse_serve(SERVE_PLUS_PATH, 8443), Ok(ServeState::Foreign));
        // Un autre port que 443 ne gêne pas.
        assert_eq!(parse_serve(SERVE_OTHER_PORT, 8443), Ok(ServeState::Off));
        assert_eq!(parse_serve("", 8443), Ok(ServeState::Off));
        assert_eq!(parse_serve("  \n", 8443), Ok(ServeState::Off));
        assert_eq!(parse_serve("null", 8443), Ok(ServeState::Off));
        assert!(parse_serve("[]", 8443).is_err());
    }

    #[test]
    fn funnel_foreground_and_services_on_443_are_never_ours() {
        assert_eq!(parse_serve(SERVE_FUNNEL, 8443), Ok(ServeState::Funnel));
        assert_eq!(parse_serve(SERVE_FOREGROUND, 8443), Ok(ServeState::Foreign));
        assert_eq!(parse_serve(SERVE_SERVICES, 8443), Ok(ServeState::Foreign));
        for (serve, wanted) in [
            (SERVE_FUNNEL, WebAccess::Funnel),
            (SERVE_FOREGROUND, WebAccess::PortBusy),
            (SERVE_SERVICES, WebAccess::PortBusy),
        ] {
            assert_eq!(read_state(&fake(STATUS_RUNNING, serve), 8443), wanted);
            for enable in [true, false] {
                let f = fake(STATUS_RUNNING, serve);
                assert_eq!(toggle(&f, 8443, enable), wanted);
                assert!(mutations(&f).is_empty());
            }
        }
    }

    #[test]
    fn the_authorisation_link_is_read_from_the_refusal() {
        assert_eq!(
            parse_auth_link(NOT_ENABLED).as_deref(),
            Some("https://login.tailscale.com/f/serve?node=n0000000000000001")
        );
        // Quelle que soit la phrase : le premier lien dont l'hôte est exactement login.tailscale.com.
        assert_eq!(
            parse_auth_link("autre phrase https://x.test/ puis https://login.tailscale.com/a/1.")
                .as_deref(),
            Some("https://login.tailscale.com/a/1")
        );
        for bad in [
            "https://x.test/",
            "https://login.tailscale.com.evil.test/a",
            "https://evil.test/login.tailscale.com",
            "https://login.tailscale.com@evil.test/a",
            "http://login.tailscale.com/a",
        ] {
            assert_eq!(parse_auth_link(bad), None, "{bad}");
        }
        assert!(is_openable("https://login.tailscale.com/f/serve"));
        assert!(is_openable("https://mac.tailnet.ts.net/app/"));
        assert!(!is_openable("https://evil.test/"));
        assert!(!is_openable("file:///etc/passwd"));
    }

    #[test]
    fn port_comes_from_the_server_address() {
        assert_eq!(port_of_url("https://mac.local:8443"), Some(8443));
        assert_eq!(port_of_url("https://127.0.0.1:50123"), Some(50123));
        assert_eq!(port_of_url("https://mac.local"), None);
    }

    #[test]
    fn states_are_read_from_the_fixtures() {
        let state = |status, serve| read_state(&fake(status, serve), 8443);
        assert_eq!(
            state(STATUS_RUNNING, SERVE_OURS),
            WebAccess::On {
                dns: "mac-exemple.tailnet-exemple.ts.net".into()
            }
        );
        assert_eq!(
            state(STATUS_RUNNING, SERVE_EMPTY),
            WebAccess::Off {
                dns: Some("mac-exemple.tailnet-exemple.ts.net".into())
            }
        );
        assert_eq!(state(STATUS_RUNNING, SERVE_FOREIGN), WebAccess::PortBusy);
        assert_eq!(state(STATUS_STOPPED, SERVE_EMPTY), WebAccess::Disconnected);
        assert_eq!(state(STATUS_LOGIN, SERVE_EMPTY), WebAccess::Disconnected);
        assert_eq!(state("n'importe quoi", SERVE_EMPTY), WebAccess::Unavailable);
        assert_eq!(
            state(STATUS_RUNNING, SERVE_OURS).web_url().as_deref(),
            Some("https://mac-exemple.tailnet-exemple.ts.net/app/")
        );
    }

    #[test]
    fn a_daemon_that_does_not_answer_means_disconnected_and_a_timeout_unavailable() {
        let mut f = fake(STATUS_RUNNING, SERVE_EMPTY);
        f.status = Output::default();
        assert_eq!(read_state(&f, 8443), WebAccess::Disconnected);
        f.status = Output {
            timed_out: true,
            ..Output::default()
        };
        assert_eq!(read_state(&f, 8443), WebAccess::Unavailable);
    }

    #[test]
    fn a_read_has_one_budget_of_three_seconds_for_both_commands() {
        let mut f = fake(STATUS_RUNNING, SERVE_EMPTY);
        f.delay = Duration::from_millis(300);
        read_state(&f, 8443);
        let t = f.timeouts.lock().unwrap().clone();
        assert_eq!(t[0], READ_TIMEOUT);
        assert!(t[1] <= READ_TIMEOUT - Duration::from_millis(300));
    }

    #[test]
    fn enabling_runs_only_the_enable_command_and_rereads() {
        let f = fake(STATUS_RUNNING, SERVE_EMPTY);
        toggle(&f, 8443, true);
        assert_eq!(
            mutations(&f),
            [vec!["serve", "--bg", "https+insecure://localhost:8443"]]
        );
    }

    #[test]
    fn disabling_runs_only_the_off_command() {
        let f = fake(STATUS_RUNNING, SERVE_OURS);
        toggle(&f, 8443, false);
        assert_eq!(mutations(&f), [vec!["serve", "--https=443", "off"]]);
    }

    #[test]
    fn a_foreign_serve_on_443_is_never_overwritten_or_removed() {
        for enable in [true, false] {
            let f = fake(STATUS_RUNNING, SERVE_FOREIGN);
            assert_eq!(toggle(&f, 8443, enable), WebAccess::PortBusy);
            assert!(mutations(&f).is_empty());
        }
    }

    #[test]
    fn nothing_runs_when_already_in_the_wanted_state_or_disconnected() {
        let f = fake(STATUS_RUNNING, SERVE_OURS);
        toggle(&f, 8443, true);
        let f2 = fake(STATUS_RUNNING, SERVE_EMPTY);
        toggle(&f2, 8443, false);
        let f3 = fake(STATUS_STOPPED, SERVE_EMPTY);
        toggle(&f3, 8443, true);
        assert!(mutations(&f).is_empty());
        assert!(mutations(&f2).is_empty());
        assert!(mutations(&f3).is_empty());
    }

    #[test]
    fn a_refusal_because_serve_is_not_enabled_gives_the_link() {
        let mut f = fake(STATUS_RUNNING, SERVE_EMPTY);
        f.enable = Output {
            success: false,
            timed_out: true,
            stdout: String::new(),
            stderr: NOT_ENABLED.into(),
        };
        assert_eq!(
            toggle(&f, 8443, true),
            WebAccess::NeedsAuth {
                url: "https://login.tailscale.com/f/serve?node=n0000000000000001".into()
            }
        );
    }

    #[test]
    fn the_real_runner_passes_fixed_arguments_and_kills_on_timeout() {
        use std::os::unix::fs::PermissionsExt;
        // Un script jetable, jamais le vrai Tailscale.
        let dir = std::env::temp_dir().join(format!("mhs-ts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("faux-tailscale");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = lent ]; then exec sleep 30; fi\necho \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = RealRunner { bin: script };
        let out = runner
            .run(
                &strings(&["serve", "--bg", "$(touch pwned)"]),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "serve --bg $(touch pwned)");
        assert!(!dir.join("pwned").exists());
        let started = Instant::now();
        let slow = runner
            .run(&strings(&["lent"]), Duration::from_millis(200))
            .unwrap();
        assert!(slow.timed_out && !slow.success);
        assert!(started.elapsed() < Duration::from_secs(5));

        // Le mode ligne de commande est forcé.
        let runner2 = RealRunner {
            bin: dir.join("faux-tailscale"),
        };
        let cmd = runner2.command(&strings(&["status"]));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "TAILSCALE_BE_CLI" && v == Some(std::ffi::OsStr::new("true"))));

        // Un script qui lance un enfant bloquant (qui garde les tuyaux) ne bloque pas au-delà du
        // délai : tout le groupe est tué.
        let child_script = dir.join("faux-enfant");
        std::fs::write(&child_script, "#!/bin/sh\nsleep 30 &\nwait\n").unwrap();
        std::fs::set_permissions(&child_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let out = RealRunner { bin: child_script }
            .run(&strings(&["x"]), Duration::from_millis(300))
            .unwrap();
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(4));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
