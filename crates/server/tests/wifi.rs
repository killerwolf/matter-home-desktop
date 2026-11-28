//! La présence par le Wi-Fi de la box, de bout en bout : la vraie liaison (`curl`) avec une **fausse
//! box HTTP locale** (jamais la vraie), les routes de l'app web, et ce que voit chaque membre de la
//! maison. Un vrai serveur HTTPS dans le processus, dossier jetable, 127.0.0.1.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ostal_server::audit::Who;
use ostal_server::hub::{Hub, HubConfig};
use ostal_server::net::{self, Auth, NetConfig, NetServer};
use ostal_server::security::account::{self, AccountStore};
use ostal_server::security::sessions::SessionStore;
use ostal_server::security::{self, tls};
use ostal_server::wifi::Link;
use protocol::mode::HomeMode;
use protocol::wire::PROTOCOL_VERSION;
use serde_json::{json, Value};

use common::Client;

const PASSWORD: &str = "mot-de-passe-jetable-du-test";
/// Avec des caractères qu'un formulaire doit protéger.
const BOX_PASSWORD: &str = "p@ss w/ %&=ord+é";

// ---- la fausse box ----------------------------------------------------------------------------

#[derive(Default)]
struct BoxState {
    /// Le mot de passe que la fausse box accepte, et la génération de sa session (la changer coupe
    /// la session ouverte).
    password: Mutex<String>,
    generation: AtomicUsize,
    hosts: Mutex<Vec<(String, bool)>>,
    online: AtomicBool,
    logins: AtomicUsize,
    bad_logins: AtomicUsize,
    lists: AtomicUsize,
}

struct FakeBox {
    url: String,
    state: Arc<BoxState>,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

fn percent_decode(text: &str) -> String {
    let bytes = text.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(mut stream: TcpStream, state: &BoxState) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let (head, mut body) = loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            return;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break (
                String::from_utf8_lossy(&raw[..end]).into_owned(),
                raw[end + 4..].to_vec(),
            );
        }
    };
    let length = head
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while body.len() < length {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    if !state.online.load(Ordering::SeqCst) {
        return; // la connexion se ferme sans réponse
    }
    let request = head.lines().next().unwrap_or("").to_string();
    let cookie = format!(
        "BBOX_ID=session-{}",
        state.generation.load(Ordering::SeqCst)
    );
    let cookie_ok = head
        .lines()
        .any(|l| l.to_lowercase().starts_with("cookie:") && l.contains(&cookie));
    let reply = |status: &str, extra: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let response = if request.starts_with("POST /api/v1/login") {
        let form = String::from_utf8_lossy(&body).into_owned();
        let password = form
            .split('&')
            .find_map(|p| p.strip_prefix("password="))
            .map(percent_decode)
            .unwrap_or_default();
        state.logins.fetch_add(1, Ordering::SeqCst);
        if password == *state.password.lock().unwrap() {
            reply(
                "200 OK",
                &format!("Set-Cookie: {cookie}; Path=/; HttpOnly\r\n"),
                "{}",
            )
        } else {
            state.bad_logins.fetch_add(1, Ordering::SeqCst);
            reply("401 Unauthorized", "", "{}")
        }
    } else if request.starts_with("GET /api/v1/hosts") {
        if !cookie_ok {
            reply("401 Unauthorized", "", "{}")
        } else {
            state.lists.fetch_add(1, Ordering::SeqCst);
            let list: Vec<Value> = state
                .hosts
                .lock()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, (name, active))| {
                    json!({"hostname": name, "ipaddress": format!("192.168.1.{}", 20 + i),
                           "macaddress": format!("AA:BB:CC:DD:EE:{:02X}", i),
                           "active": i32::from(*active), "lastseen": 0, "link": "WiFi 5"})
                })
                .collect();
            reply(
                "200 OK",
                "",
                &json!([{"hosts": {"list": list}}]).to_string(),
            )
        }
    } else {
        reply("404 Not Found", "", "{}")
    };
    let _ = stream.write_all(response.as_bytes());
}

impl FakeBox {
    fn start(hosts: &[(&str, bool)]) -> FakeBox {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(BoxState::default());
        *state.password.lock().unwrap() = BOX_PASSWORD.to_string();
        state.online.store(true, Ordering::SeqCst);
        *state.hosts.lock().unwrap() = hosts.iter().map(|(n, a)| (n.to_string(), *a)).collect();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        handle(stream, &state);
                    }
                }
            });
        }
        FakeBox {
            url: format!("http://{addr}"),
            state,
            stop,
            addr,
        }
    }

    fn set(&self, hosts: &[(&str, bool)]) {
        *self.state.hosts.lock().unwrap() =
            hosts.iter().map(|(n, a)| (n.to_string(), *a)).collect();
    }

    /// L'administrateur de la box change son mot de passe : la session ouverte tombe.
    fn change_password(&self, password: &str) {
        *self.state.password.lock().unwrap() = password.to_string();
        self.state.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn online(&self, on: bool) {
        self.state.online.store(on, Ordering::SeqCst);
    }
}

impl Drop for FakeBox {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

// ---- le serveur -------------------------------------------------------------------------------

struct Env {
    dir: std::path::PathBuf,
    _server: NetServer,
    hub: Arc<Hub>,
    auth: Arc<Auth>,
    fingerprint: String,
    addr: std::net::SocketAddr,
    runtime: tokio::runtime::Runtime,
    _commands: tokio::sync::mpsc::UnboundedReceiver<ostal_server::events::Command>,
}

fn env(tag: &str) -> Env {
    let dir = common::temp_dir(tag);
    let sdir = security::server_dir(&dir).unwrap();
    account::add(&sdir, "sam", Some("Sam"), PASSWORD).unwrap();
    account::add(&sdir, "lea", Some("Léa"), PASSWORD).unwrap();
    account::add(&sdir, "adam", Some("Adam"), PASSWORD).unwrap();
    let identity = tls::load_or_create(&sdir).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (commands, rx) = tokio::sync::mpsc::unbounded_channel();
    let hub = Arc::new(Hub::new(HubConfig {
        data_dir: dir.clone(),
        demo: false,
        history_dir: dir.join("history"),
        demo_history: None,
        broadcast_capacity: 64,
        commands,
    }));
    let accounts = AccountStore::open(&sdir).unwrap();
    let auth = Arc::new(Auth::new(accounts, SessionStore::open(&sdir).unwrap()));
    let server = net::start(
        NetConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            client_write_timeout: Duration::from_secs(10),
            body_idle_timeout: Duration::from_secs(15),
            shortcut_signer: ostal_server::shortcuts::Signer::Unsigned,
        },
        hub.clone(),
        auth.clone(),
        identity.config.clone(),
    )
    .unwrap();
    let addr = server.addr;
    Env {
        dir,
        _server: server,
        hub,
        auth,
        fingerprint: identity.fingerprint,
        addr,
        runtime,
        _commands: rx,
    }
}

impl Env {
    fn client(&self) -> Client {
        Client::new(self.addr, &self.fingerprint)
    }

    async fn login(&self, user: &str) -> String {
        let r = self
            .client()
            .login_as(user, PASSWORD, PROTOCOL_VERSION)
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn id_of(&self, user: &str) -> String {
        self.auth.accounts.find_by_username(user).unwrap().0
    }

    /// Relève la box, au temps `dt` secondes après maintenant (relevé forcé).
    fn poll(&self, dt: f64) -> ostal_server::hub::WifiReport {
        self.hub.wifi_poll_at(
            ostal_server::security::now() + dt,
            &self.auth.accounts,
            true,
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn link(env: &Env, fake: &FakeBox) {
    env.hub
        .wifi_link(&fake.url, BOX_PASSWORD, &Who::server_app())
        .unwrap();
}

#[test]
fn the_box_is_linked_with_its_password_read_through_the_login_and_never_exposed() {
    let env = env("wifi-link");
    let fake = FakeBox::start(&[("iPhone-de-Sam", true)]);
    // Rien n'est gardé tant que la box n'a pas été reliée.
    assert!(!env.dir.join("serveur/box.json").exists());
    assert!(!env.dir.join("serveur/extensions.json").exists());
    // La box éteinte : « ne répond pas ».
    fake.online(false);
    assert_eq!(
        env.hub
            .wifi_link(&fake.url, BOX_PASSWORD, &Who::server_app())
            .unwrap_err(),
        ostal_server::wifi::LinkError::Unreachable
    );
    fake.online(true);
    link(&env, &fake);
    let status = env.hub.wifi().status();
    assert!(status.linked && status.link == Link::Ok && !status.auto_mode);
    // La session de la box est gardée : un relevé de plus ne se reconnecte pas.
    let logins = fake.state.logins.load(Ordering::SeqCst);
    env.poll(0.0);
    env.poll(1.0);
    assert_eq!(fake.state.logins.load(Ordering::SeqCst), logins);
    // Le mot de passe n'est que dans le fichier secret des extensions (extension « Bbox »), plus
    // dans `box.json` (qui ne garde que les réglages de la présence).
    let file = std::fs::read_to_string(env.dir.join("serveur/extensions.json")).unwrap();
    assert!(file.contains("%&=ord"));
    let bbox_entry: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(bbox_entry["extensions"]["bbox"]["enabled"], true);
    assert_eq!(bbox_entry["extensions"]["bbox"]["config"]["url"], fake.url);
    if let Ok(presence) = std::fs::read_to_string(env.dir.join("serveur/box.json")) {
        assert!(!presence.contains("%&=ord") && !presence.contains("password"));
    }
    let mut all = String::new();
    for entry in std::fs::read_dir(&env.dir).unwrap().flatten() {
        if entry.path().is_file() {
            all.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    assert!(!all.contains("%&=ord"));
    // Un mauvais mot de passe : refusé, la box a vu un seul essai, le bon reste en place, une ligne
    // au journal ; un nouvel essai tout de suite est refusé sans appeler la box (limite après refus).
    let err = env
        .hub
        .wifi_link(&fake.url, "pas-le-bon", &Who::server_app())
        .unwrap_err();
    assert_eq!(err, ostal_server::wifi::LinkError::Refused);
    assert_eq!(fake.state.bad_logins.load(Ordering::SeqCst), 1);
    // Le premier essai qui suit un refus est libre ; le suivant attend (limite après refus).
    let second = env
        .hub
        .wifi_link(&fake.url, "pas-le-bon-non-plus", &Who::server_app())
        .unwrap_err();
    assert_eq!(second, ostal_server::wifi::LinkError::Refused);
    let again = env
        .hub
        .wifi_link(&fake.url, "pas-le-bon-du-tout", &Who::server_app())
        .unwrap_err();
    assert!(
        matches!(again, ostal_server::wifi::LinkError::TooSoon(_)),
        "{again:?}"
    );
    assert_eq!(fake.state.bad_logins.load(Ordering::SeqCst), 2);
    assert!(
        std::fs::read_to_string(env.dir.join("serveur/extensions.json"))
            .unwrap()
            .contains("%&=ord")
    );
}

#[test]
fn a_member_chooses_a_phone_by_name_only_and_never_sees_an_address() {
    let env = env("wifi-choose");
    let fake = FakeBox::start(&[
        ("iPhone-de-Sam", true),
        ("Galaxy-de-Lea", true),
        ("Redmi-de-Adam", true),
        ("iPhone-7", true),
        ("MacBook-Pro", true),
    ]);
    // Sans box reliée, l'app ne propose rien.
    let client = env.client();
    env.runtime.block_on(async {
        let lea = env.login("lea").await;
        let r = client.get("/api/wifi/phones", &lea).await;
        assert_eq!(r.status, 200);
        assert_eq!(r.json()["linked"], false);
        assert_eq!(r.json()["box_link"], "none");
        assert_eq!(r.json()["free"], json!([]));
        // Ni qui est à la maison, ni mode automatique : une maison sans box se comporte comme avant.
        let people = client.get("/api/presence/people", &lea).await.json();
        assert_eq!(people["people"], json!([]));
        assert_eq!(people["auto_mode"], false);
        assert_eq!(people["box_link"], "none");
        let r = client
            .send_json(
                "PUT",
                "/api/wifi/phone",
                &lea,
                &json!({"phone": "Galaxy-de-Lea"}),
            )
            .await;
        assert_eq!(r.status, 503, "{}", r.text());
    });
    // Rien n'est écrit tant qu'aucune box n'est reliée.
    assert!(!env.dir.join("serveur/box.json").exists());
    assert!(!env.dir.join("serveur/telephones.json").exists());
    link(&env, &fake);
    env.runtime.block_on(async {
        let (lea, sam) = (env.login("lea").await, env.login("sam").await);
        let r = client.get("/api/wifi/phones", &lea).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let text = r.text();
        for secret in ["192.168", "AA:BB", "macaddress", "ipaddress", "MacBook"] {
            assert!(!text.contains(secret), "{secret} : {text}");
        }
        let view = r.json();
        assert_eq!(view["linked"], true);
        assert_eq!(view["box_link"], "ok");
        assert_eq!(view["mine"], Value::Null);
        assert_eq!(
            view["free"],
            json!([
                "Galaxy-de-Lea",
                "iPhone-7",
                "iPhone-de-Sam",
                "Redmi-de-Adam"
            ])
        );
        // Léa choisit le sien ; il n'est plus libre pour personne.
        let r = client
            .send_json(
                "PUT",
                "/api/wifi/phone",
                &lea,
                &json!({"phone": "galaxy-de-lea"}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["mine"], "Galaxy-de-Lea");
        let r = client.get("/api/wifi/phones", &sam).await.json();
        assert_eq!(r["mine"], Value::Null);
        assert!(!r["free"].to_string().contains("Galaxy"));
        // Sam ne peut pas prendre celui de Léa (409), ni un appareil qui n'est pas un téléphone
        // ou qu'on n'a pas vu (404).
        let take = |name: &'static str| {
            let (client, sam) = (client.clone(), sam.clone());
            async move {
                client
                    .send_json("PUT", "/api/wifi/phone", &sam, &json!({"phone": name}))
                    .await
            }
        };
        assert_eq!(take("Galaxy-de-Lea").await.status, 409);
        assert_eq!(take("MacBook-Pro").await.status, 404);
        assert_eq!(take("iPhone-inconnu").await.status, 404);
        assert_eq!(take("iPhone-de-Sam").await.status, 200);
        // Retirer son téléphone : il redevient libre.
        let r = client
            .send_json("PUT", "/api/wifi/phone", &lea, &json!({"phone": null}))
            .await;
        assert_eq!(r.json()["mine"], Value::Null);
        assert!(r.json()["free"].to_string().contains("Galaxy-de-Lea"));
        // Une session absente ou un corps faux n'ouvrent rien.
        assert_eq!(
            client
                .request("GET", "/api/wifi/phones", None, None)
                .await
                .status,
            401
        );
        assert_eq!(
            client
                .request("PUT", "/api/wifi/phone", Some(&lea), Some(b"{\"tel\":1}"))
                .await
                .status,
            400
        );
    });
    // L'administrateur voit et corrige depuis Ostal Server : ici Adam reçoit son téléphone.
    let who = Who::server_app();
    assert!(env
        .hub
        .wifi_assign(
            &env.id_of("adam"),
            Some("Redmi-de-Adam"),
            false,
            &who,
            "Adam"
        )
        .unwrap());
    assert_eq!(
        env.hub.wifi().phone_of(&env.id_of("adam")).as_deref(),
        Some("Redmi-de-Adam")
    );
}

#[test]
fn everyone_sees_who_is_home_by_first_name_and_the_mode_follows_only_when_asked() {
    let env = env("wifi-home");
    let fake = FakeBox::start(&[
        ("iPhone-de-Sam", true),
        ("Galaxy-de-Lea", true),
        ("TV", true),
    ]);
    link(&env, &fake);
    let who = Who::server_app();
    env.hub
        .wifi_assign(&env.id_of("sam"), Some("iPhone-de-Sam"), true, &who, "Sam")
        .unwrap();
    env.hub
        .wifi_assign(&env.id_of("lea"), Some("Galaxy-de-Lea"), true, &who, "Léa")
        .unwrap();
    env.poll(0.0);
    let client = env.client();
    env.runtime.block_on(async {
        let sam = env.login("sam").await;
        let view = client.get("/api/presence/people", &sam).await;
        assert_eq!(view.status, 200, "{}", view.text());
        let text = view.text();
        for secret in ["iPhone-de-Sam", "Galaxy", "192.168", "AA:BB"] {
            assert!(!text.contains(secret), "{secret} : {text}");
        }
        let view = view.json();
        assert_eq!(view["auto_mode"], false);
        assert_eq!(view["box_link"], "ok");
        let people = view["people"].as_array().unwrap();
        assert_eq!(people.len(), 2, "{view}");
        assert_eq!(people[0]["name"], "Léa");
        assert_eq!(people[0]["state"], "home");
        assert_eq!(people[0]["me"], false);
        assert_eq!(people[1]["name"], "Sam");
        assert_eq!(people[1]["me"], true);
    });
    // Interrupteur éteint : tout le monde part, le mode ne bouge pas.
    fake.set(&[
        ("iPhone-de-Sam", false),
        ("Galaxy-de-Lea", false),
        ("TV", true),
    ]);
    env.poll(100.0);
    env.poll(800.0);
    assert_eq!(env.hub.mode(), HomeMode::Present);
    env.runtime.block_on(async {
        let sam = env.login("sam").await;
        let view = client.get("/api/presence/people", &sam).await.json();
        let states: Vec<&str> = view["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, vec!["away", "away"], "{view}");
    });
    // Interrupteur allumé : Sam revient (Présent déjà), puis tout le monde repart → Absent.
    env.hub.wifi_set_auto_mode(true, &who).unwrap();
    fake.set(&[
        ("iPhone-de-Sam", true),
        ("Galaxy-de-Lea", false),
        ("TV", true),
    ]);
    env.poll(1_000.0);
    fake.set(&[
        ("iPhone-de-Sam", false),
        ("Galaxy-de-Lea", false),
        ("TV", true),
    ]);
    env.poll(1_100.0);
    assert_eq!(env.hub.mode(), HomeMode::Present);
    let report = env.poll(1_100.0 + 601.0);
    assert_eq!(report.mode, Some(HomeMode::Absent));
    assert_eq!(env.hub.mode(), HomeMode::Absent);
    env.runtime.block_on(async {
        let sam = env.login("sam").await;
        let view = client.get("/api/presence/people", &sam).await.json();
        assert_eq!(view["auto_mode"], true);
    });
}

#[test]
fn a_box_that_goes_away_leaves_the_house_as_it_was_and_the_app_says_so() {
    let env = env("wifi-offline");
    let fake = FakeBox::start(&[("iPhone-de-Sam", true), ("Galaxy-de-Lea", true)]);
    link(&env, &fake);
    let who = Who::server_app();
    env.hub
        .wifi_assign(&env.id_of("sam"), Some("iPhone-de-Sam"), true, &who, "Sam")
        .unwrap();
    env.hub.wifi_set_auto_mode(true, &who).unwrap();
    env.poll(0.0);
    fake.online(false);
    for dt in [200.0, 5_000.0, 90_000.0] {
        let report = env.poll(dt);
        assert_eq!(report.link, Link::Unreachable);
        assert_eq!(report.mode, None);
    }
    assert_eq!(env.hub.mode(), HomeMode::Present);
    env.runtime.block_on(async {
        let sam = env.login("sam").await;
        let view = env.client().get("/api/presence/people", &sam).await.json();
        assert_eq!(view["box_link"], "unreachable");
        // Sam est toujours « à la maison » : la box muette n'a rien conclu.
        assert_eq!(view["people"][0]["state"], "home");
    });
    // La box revient : le relevé reprend tout seul.
    fake.online(true);
    assert_eq!(env.poll(100_000.0).link, Link::Ok);
}

#[test]
fn a_refused_password_is_tried_once_then_left_alone_until_another_is_given() {
    let env = env("wifi-refused");
    let fake = FakeBox::start(&[("iPhone-de-Sam", true), ("Galaxy-de-Lea", true)]);
    link(&env, &fake);
    let who = Who::server_app();
    env.hub
        .wifi_assign(&env.id_of("sam"), Some("iPhone-de-Sam"), true, &who, "Sam")
        .unwrap();
    env.hub.wifi_set_auto_mode(true, &who).unwrap();
    env.poll(0.0);
    // L'administrateur de la box change son mot de passe : la session tombe, le nôtre est refusé.
    fake.change_password("nouveau-mot-de-passe-de-la-box");
    let logins = fake.state.logins.load(Ordering::SeqCst);
    for dt in [200.0, 400.0, 600.0, 90_000.0] {
        let report = env.poll(dt);
        assert_eq!(report.link, Link::Refused);
        assert_eq!(report.mode, None);
    }
    // Un seul essai : la box ne doit pas nous bloquer.
    assert_eq!(fake.state.logins.load(Ordering::SeqCst), logins + 1);
    assert_eq!(env.hub.mode(), HomeMode::Present);
    env.runtime.block_on(async {
        let sam = env.login("sam").await;
        let view = env.client().get("/api/presence/people", &sam).await.json();
        assert_eq!(view["box_link"], "refused");
        assert_eq!(view["people"][0]["state"], "home");
    });
    // Avec le nouveau mot de passe, les relevés reprennent.
    env.hub
        .wifi_link(&fake.url, "nouveau-mot-de-passe-de-la-box", &who)
        .unwrap();
    assert_eq!(env.poll(100_000.0).link, Link::Ok);
}
