//! Outils des tests d'intégration : lancement du vrai binaire `matter-home-server --demo`
//! comme processus, sur 127.0.0.1 uniquement, avec un dossier de données temporaire et un
//! HOME factice. Jamais le vrai dossier de données, jamais un vrai appareil Matter.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use matter_home_client::net::{Backend, Event, Link, NetError, Request, Response, Store};

pub const USER: &str = "hamdi";
pub const PASSWORD: &str = "un-mot-de-passe-solide-pour-les-tests";
pub const PLUG: u64 = 0xD3_0004;
pub const LIVING: u64 = 0xD3_0001;

/// Binaire du serveur, construit à côté des tests par `cargo test --workspace`
/// (`target/debug/deps/<test>` → `target/debug/matter-home-server`).
fn server_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("chemin du test");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("dossier target")
        .to_path_buf();
    let bin = profile_dir.join("matter-home-server");
    assert!(
        bin.exists(),
        "serveur introuvable ({}) : lancez `cargo test --workspace` ou `cargo build -p matter-home-server`",
        bin.display()
    );
    bin
}

pub fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mhd-client-it-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_account(dir: &Path) {
    let mut child = Command::new(server_binary())
        .args([
            "create-account",
            "--username",
            USER,
            "--password-stdin",
            "--data-dir",
        ])
        .arg(dir)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{PASSWORD}").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Un serveur `--demo` en cours d'exécution.
pub struct Server {
    child: Option<Child>,
    pub dir: PathBuf,
    pub addr: SocketAddr,
    pub fingerprint: String,
    log_path: PathBuf,
}

impl Server {
    pub fn start(dir: &Path) -> Server {
        Server::start_on(dir, "127.0.0.1:0")
    }

    /// Démarre sur `listen` (par exemple le port d'un serveur précédent, pour le relancer).
    pub fn start_on(dir: &Path, listen: &str) -> Server {
        assert!(
            listen.starts_with("127.0.0.1:"),
            "uniquement la boucle locale"
        );
        if !dir.join("serveur").join("compte.json").exists() {
            create_account(dir);
        }
        let log_path = dir.join(format!(
            "serveur-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let log = std::fs::File::create(&log_path).unwrap();
        let mut child = Command::new(server_binary())
            .args(["--demo", "--listen", listen, "--data-dir"])
            .arg(dir)
            .env("HOME", dir.join("home"))
            .env("RUST_LOG", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (mut addr, mut fingerprint) = (None, None);
        let deadline = Instant::now() + Duration::from_secs(60);
        while addr.is_none() || fingerprint.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = rx.recv_timeout(left).unwrap_or_else(|_| {
                panic!(
                    "le serveur n'a pas démarré. Journal :\n{}",
                    std::fs::read_to_string(&log_path).unwrap_or_default()
                )
            });
            if let Some(url) = line.strip_prefix("Écoute : https://") {
                addr = Some(url.trim().parse().unwrap());
            } else if let Some(f) = line.strip_prefix("Empreinte du certificat (SHA-256) : ") {
                fingerprint = Some(f.trim().to_string());
            }
        }
        Server {
            child: Some(child),
            dir: dir.to_path_buf(),
            addr: addr.unwrap(),
            fingerprint: fingerprint.unwrap(),
            log_path,
        }
    }

    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Coupure brutale (comme un plantage ou une coupure de courant).
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Un client branché sur sa propre copie du fichier de connexion.
pub struct TestClient {
    pub backend: Backend,
    pub events: Receiver<Event>,
    pub store: Store,
    /// Ce qui a été lu sans intéresser `wait_for` (pour les messages d'échec).
    pub seen: Vec<String>,
}

impl TestClient {
    pub fn new(store_path: PathBuf) -> TestClient {
        TestClient::with_protocol(store_path, None)
    }

    pub fn with_protocol(store_path: PathBuf, protocol: Option<u32>) -> TestClient {
        let store = Store::at(store_path);
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let (backend, events) = match protocol {
            Some(p) => Backend::start_with_protocol(store.clone(), notify, p),
            None => Backend::start(store.clone(), notify),
        };
        TestClient {
            backend,
            events,
            store,
            seen: Vec::new(),
        }
    }

    /// Lit les événements jusqu'à ce que `pick` en reconnaisse un. Panique au bout de `timeout`.
    pub fn wait_for<T>(
        &mut self,
        timeout: Duration,
        mut pick: impl FnMut(&Event) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => {
                    if let Some(found) = pick(&event) {
                        return found;
                    }
                    self.seen
                        .push(format!("{event:?}").chars().take(160).collect());
                }
                Err(_) => panic!(
                    "événement attendu non reçu en {timeout:?}. Vus : {:#?}",
                    self.seen
                ),
            }
        }
    }

    pub fn wait_link(&mut self, timeout: Duration, pick: impl Fn(&Link) -> bool) -> Link {
        self.wait_for(timeout, |e| match e {
            Event::Link(l) if pick(l) => Some(l.clone()),
            _ => None,
        })
    }

    /// Aucun événement dans ce délai.
    pub fn expect_quiet(&mut self, duration: Duration, what: &str) {
        let deadline = Instant::now() + duration;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Ok(event) = self.events.recv_timeout(left) {
                panic!("{what} : événement inattendu {event:?}");
            }
        }
    }

    /// Envoie une requête et attend sa réponse (quel que soit son statut, sauf erreur API).
    pub fn call(&mut self, request: Request) -> Result<Response, NetError> {
        let id = self.backend.call(request);
        self.wait_for(Duration::from_secs(60), |e| match e {
            Event::Reply { id: got, result } if *got == id => Some(result.clone()),
            _ => None,
        })
    }
}

/// Première connexion complète : empreinte lue, approuvée, session ouverte, état reçu.
pub fn connect_first_time(client: &mut TestClient, server: &Server) {
    client.backend.probe(&server.url());
    let (url, fingerprint) = client.wait_for(Duration::from_secs(20), |e| match e {
        Event::Probed { url, fingerprint } => Some((url.clone(), fingerprint.clone())),
        _ => None,
    });
    assert_eq!(fingerprint, server.fingerprint);
    client.backend.login(&url, &fingerprint, USER, PASSWORD);
    client.wait_for(Duration::from_secs(60), |e| match e {
        Event::Snapshot(_) => Some(()),
        Event::LoginFailed(err) => panic!("connexion refusée : {err}"),
        _ => None,
    });
}
