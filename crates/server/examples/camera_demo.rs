//! Serveur de démonstration pour voir le direct d'une caméra sans aucune caméra réelle
//! (ticket n° 5) : le vrai réseau du serveur (HTTPS, session, route du flux), un `Hub` **sans**
//! Matter ni mDNS, et une fausse caméra RTSP locale qui joue en boucle le clip de
//! `tests/fixtures/`. Tout reste sur 127.0.0.1.
//!
//! ```text
//! MH_DEMO_PASSWORD=… cargo run -p matter-home-server --example camera_demo -- \
//!     --data-dir DOSSIER --listen 127.0.0.1:PORT [--camera-port PORT]
//! ```
//!
//! Le dossier de données est celui qu'on veut (jetable) : le compte `demo`, le certificat et
//! `cameras.json` y sont créés. Le mot de passe du compte vient de `MH_DEMO_PASSWORD`. Les caméras :
//! « Entrée » (flux principal et flux réduit) et « Jardin » (flux principal seulement).
//! Ctrl-C ou SIGTERM arrête le serveur.

use std::path::PathBuf;
use std::time::Duration;

#[path = "../tests/support/fake_camera.rs"]
mod fake_camera;
#[path = "../tests/support/in_process.rs"]
mod in_process;

fn main() {
    let mut data_dir: Option<PathBuf> = None;
    let mut listen = "127.0.0.1:18443".to_string();
    let mut camera_port = 0u16;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => data_dir = args.next().map(PathBuf::from),
            "--listen" => listen = args.next().expect("--listen ADRESSE:PORT"),
            "--camera-port" => {
                camera_port = args
                    .next()
                    .and_then(|p| p.parse().ok())
                    .expect("--camera-port PORT")
            }
            other => panic!("option inconnue : {other}"),
        }
    }
    let data_dir = data_dir.expect("--data-dir DOSSIER");
    let listen: std::net::SocketAddr = listen.parse().expect("adresse d'écoute");
    assert!(
        listen.ip().is_loopback() && listen.port() != 8443,
        "démonstration : 127.0.0.1 seulement, et jamais le port 8443"
    );
    let password = std::env::var("MH_DEMO_PASSWORD").expect("MH_DEMO_PASSWORD");
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,retina=off"))
        .init();
    let rig = in_process::start(in_process::Setup {
        data_dir: &data_dir,
        listen: &listen.to_string(),
        password: &password,
        camera_addr: &format!("127.0.0.1:{camera_port}"),
        camera: fake_camera::Options::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    // La surveillance donne leur état aux caméras (sinon elles restent « inconnues »).
    let _monitor =
        matter_home_server::cameras::start_monitor(rig.hub.clone()).expect("surveillance");
    println!("Écoute : https://{}", rig.server.addr);
    println!("Empreinte du certificat (SHA-256) : {}", rig.fingerprint);
    println!("Fausse caméra : 127.0.0.1:{}", rig.camera.port);
    // Sert jusqu'à l'arrêt du processus.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
