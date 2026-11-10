//! Le direct d'une caméra, de bout en bout : le vrai client réseau (`net::stream`, VideoToolbox)
//! contre un vrai serveur HTTPS (`--example camera_demo` du serveur : relais + fausse caméra RTSP
//! locale qui joue un clip). Tout reste sur 127.0.0.1.
//!
//! Le serveur de ces tests est l'exemple `camera_demo`, construit à côté des tests par
//! `cargo test --workspace` (sinon : `cargo build -p matter-home-server --examples`).
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use matter_home_client::net::stream::{StreamHandle, StreamState};
use matter_home_client::net::{Backend, Event, Link, Store};
use protocol::wire::{CameraView, StreamKind};

const PASSWORD: &str = "mot-de-passe-jetable-du-test-video";
const T: Duration = Duration::from_secs(20);

/// Les lecteurs vidéo lisent la session « en cours » (une seule par processus dans l'application) :
/// ces tests, qui en ouvrent chacun une, passent l'un après l'autre.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn demo_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("chemin du test");
    let profile_dir = exe.parent().and_then(Path::parent).expect("dossier target");
    let bin = profile_dir.join("examples").join("camera_demo");
    assert!(
        bin.exists(),
        "serveur de démonstration introuvable ({}) : `cargo build -p matter-home-server --examples`",
        bin.display()
    );
    bin
}

fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mhd-client-cam-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Server {
    child: Option<Child>,
    addr: std::net::SocketAddr,
    fingerprint: String,
    log: PathBuf,
}

impl Server {
    fn start(dir: &Path, listen: &str) -> Server {
        let log = dir.join(format!("serveur-{}.log", std::process::id()));
        let mut child = Command::new(demo_binary())
            .args(["--data-dir"])
            .arg(dir.join("data"))
            .args(["--listen", listen])
            .env("HOME", dir.join("home"))
            .env("MH_DEMO_PASSWORD", PASSWORD)
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&log).unwrap())
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
            let line = rx.recv_timeout(left).expect("le serveur n'a pas démarré");
            if let Some(url) = line.strip_prefix("Écoute : https://") {
                addr = Some(url.trim().parse().unwrap());
            } else if let Some(f) = line.strip_prefix("Empreinte du certificat (SHA-256) : ") {
                fingerprint = Some(f.trim().to_string());
            }
        }
        Server {
            child: Some(child),
            addr: addr.unwrap(),
            fingerprint: fingerprint.unwrap(),
            log,
        }
    }

    fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Client {
    _backend: Backend,
    events: Receiver<Event>,
    cameras: Vec<CameraView>,
}

impl Client {
    fn connect(server: &Server, dir: &Path) -> Client {
        let (backend, events) =
            Backend::start(Store::at(dir.join("connexion.json")), Arc::new(|| {}));
        backend.login(&server.url(), &server.fingerprint, "demo", PASSWORD);
        let mut client = Client {
            _backend: backend,
            events,
            cameras: vec![],
        };
        let (mut connected, mut snapshot) = (false, false);
        let deadline = Instant::now() + T;
        while !(connected && snapshot) {
            let event = client
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("connexion au serveur");
            match event {
                Event::Link(Link::Connected { .. }) => connected = true,
                Event::Snapshot(s) => {
                    client.cameras = s.cameras;
                    snapshot = true;
                }
                Event::LoginFailed(e) => panic!("connexion refusée : {}", e.message()),
                _ => {}
            }
        }
        client
    }

    fn camera(&self, name: &str) -> u64 {
        self.cameras.iter().find(|c| c.name == name).unwrap().id
    }
}

/// Attend un état, en gardant la liaison « affichée » (`touch`), 20 s au plus.
fn wait_state(
    handle: &StreamHandle,
    what: &str,
    accept: impl Fn(&StreamState) -> bool,
) -> StreamState {
    let deadline = Instant::now() + T;
    loop {
        handle.touch();
        let state = handle.state();
        if accept(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "délai dépassé : {what} (dernier état : {state:?})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn open(id: u64, kind: StreamKind) -> StreamHandle {
    StreamHandle::open(id, kind, || {})
}

#[test]
fn the_player_decodes_the_live_video_and_the_server_closes_the_camera_after_it() {
    let _serial = serial();
    let dir = temp_dir("live");
    let server = Server::start(&dir, "127.0.0.1:0");
    let client = Client::connect(&server, &dir);
    let id = client.camera("Entrée");

    let handle = open(id, StreamKind::Sub);
    wait_state(&handle, "la vidéo", |s| *s == StreamState::Live);
    let deadline = Instant::now() + T;
    while handle.frames() < 20 {
        handle.touch();
        assert!(
            Instant::now() < deadline,
            "pas assez d'images : {}",
            handle.frames()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let image = handle.take_image().expect("une image décodée");
    assert_eq!((image.width, image.height), (640, 360));
    assert!(
        image.rgba.as_chunks::<4>().0.iter().any(|p| p[0] > 200),
        "image noire ?"
    );
    let info = handle.info().expect("taille annoncée");
    assert_eq!(
        (info.width, info.height, info.pixel_aspect),
        (640, 360, (1, 1))
    );
    assert!(server.log().contains("« Entrée »") && server.log().contains("ouverte"));

    // Le lecteur n'est plus affiché : plus de `touch`, la liaison se ferme d'elle-même, et le
    // serveur coupe la caméra après sa grâce.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "la liaison est restée ouverte");
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while !server.log().contains("fermée") {
        assert!(
            Instant::now() < deadline,
            "le serveur n'a pas fermé la caméra : {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(client);
}

#[test]
fn refusals_become_clear_final_states() {
    let _serial = serial();
    let dir = temp_dir("refusals");
    let server = Server::start(&dir, "127.0.0.1:0");
    let client = Client::connect(&server, &dir);

    // « Jardin » n'a pas de flux réduit.
    let handle = open(client.camera("Jardin"), StreamKind::Sub);
    assert_eq!(
        wait_state(&handle, "refus", StreamState::is_final),
        StreamState::NoSuchStream
    );
    // Une caméra qui n'existe pas (ou plus).
    let handle = open(protocol::wire::CAMERA_ID_MIN + 4242, StreamKind::Main);
    assert_eq!(
        wait_state(&handle, "refus", StreamState::is_final),
        StreamState::CameraGone
    );
    // Le flux principal de « Jardin » marche.
    let handle = open(client.camera("Jardin"), StreamKind::Main);
    wait_state(&handle, "la vidéo", |s| *s == StreamState::Live);
    drop(client);
}

#[test]
fn the_stream_resumes_by_itself_after_the_server_comes_back() {
    let _serial = serial();
    let dir = temp_dir("resume");
    let mut server = Server::start(&dir, "127.0.0.1:0");
    let port = server.addr.port();
    let client = Client::connect(&server, &dir);
    let id = client.camera("Entrée");

    let handle = open(id, StreamKind::Sub);
    wait_state(&handle, "la vidéo", |s| *s == StreamState::Live);
    let before = handle.frames();

    // Le serveur redémarre sur le même port, avec le même certificat et le même compte.
    server.stop();
    wait_state(&handle, "interruption", |s| {
        matches!(s, StreamState::Interrupted { .. } | StreamState::Connecting)
    });
    let server = Server::start(&dir, &format!("127.0.0.1:{port}"));
    assert_eq!(server.addr.port(), port);
    // La session du client (jeton) n'existe plus côté serveur redémarré ? Elle est conservée sur
    // disque : la liaison reprend, ou le serveur répond « session terminée » (état final) si le
    // jeton n'a pas survécu — dans les deux cas, jamais de boucle ni de panique.
    let state = wait_state(&handle, "reprise", |s| {
        *s == StreamState::Live || s.is_final()
    });
    if state == StreamState::Live {
        let deadline = Instant::now() + T;
        while handle.frames() < before + 10 {
            handle.touch();
            assert!(Instant::now() < deadline, "les images ne reprennent pas");
            std::thread::sleep(Duration::from_millis(50));
        }
    } else {
        assert_eq!(state, StreamState::SessionEnded);
    }
    drop(client);
}
