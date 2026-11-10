//! Un serveur complet **dans le processus** (vrai réseau HTTPS, session, route du flux, `Hub`), sans
//! Matter ni mDNS, avec une fausse caméra locale. Pour les tests de la route du flux et pour
//! l'exemple `camera_demo`. Tout reste sur 127.0.0.1, dans un dossier jetable.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use matter_home_server::cameras::CameraDraft;
use matter_home_server::hub::{Hub, HubConfig};
use matter_home_server::net::{self, Auth, NetConfig, NetServer};
use matter_home_server::security::account::{self, AccountStore};
use matter_home_server::security::sessions::SessionStore;
use matter_home_server::security::{self, tls};

use super::fake_camera::{self, FakeCamera};

pub const USER: &str = "demo";

pub struct Rig {
    pub server: NetServer,
    pub hub: Arc<Hub>,
    pub fingerprint: String,
    pub camera: FakeCamera,
    /// « Entrée » : flux principal et flux réduit.
    pub with_sub: u64,
    /// « Jardin » : flux principal seulement.
    pub main_only: u64,
    pub runtime: tokio::runtime::Runtime,
}

pub struct Setup<'a> {
    pub data_dir: &'a Path,
    pub listen: &'a str,
    pub password: &'a str,
    pub camera_addr: &'a str,
    pub camera: fake_camera::Options,
    pub client_write_timeout: Duration,
}

pub fn start(setup: Setup<'_>) -> Rig {
    std::fs::create_dir_all(setup.data_dir).unwrap();
    let dir = security::server_dir(setup.data_dir).unwrap();
    account::create(&dir, USER, setup.password, true).unwrap();
    let identity = tls::load_or_create(&dir).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let camera = runtime.block_on(fake_camera::start_on(setup.camera_addr, setup.camera));
    let draft = |name: &str, path: &str, with_sub: bool| CameraDraft {
        name: name.into(),
        url: format!("rtsp://127.0.0.1:{}/{path}", camera.port),
        sub_url: if with_sub {
            format!("rtsp://127.0.0.1:{}/{path}_sub", camera.port)
        } else {
            String::new()
        },
        ..CameraDraft::default()
    };
    let (commands, _rx) = mpsc::unbounded_channel();
    let hub = Arc::new(Hub::new(HubConfig {
        data_dir: setup.data_dir.to_path_buf(),
        demo: false,
        history_dir: setup.data_dir.join("history"),
        demo_history: None,
        broadcast_capacity: 64,
        commands,
    }));
    let with_sub = hub
        .add_camera(&draft("Entrée", "live/ch00_0", true))
        .unwrap();
    let main_only = hub
        .add_camera(&draft("Jardin", "live/ch01_0", false))
        .unwrap();
    let auth = Arc::new(Auth::new(
        AccountStore::open(&dir).unwrap(),
        SessionStore::open(&dir).unwrap(),
    ));
    let server = net::start(
        NetConfig {
            listen: setup.listen.parse().expect("adresse d'écoute"),
            client_write_timeout: setup.client_write_timeout,
            body_idle_timeout: Duration::from_secs(15),
        },
        hub.clone(),
        auth,
        identity.config.clone(),
    )
    .unwrap();
    Rig {
        server,
        hub,
        fingerprint: identity.fingerprint,
        camera,
        with_sub,
        main_only,
        runtime,
    }
}
