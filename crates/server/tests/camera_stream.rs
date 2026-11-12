//! La route du flux d'une caméra (`GET /api/cameras/{id}/stream`), de bout en bout : un vrai
//! serveur HTTPS dans le processus, une fausse caméra RTSP locale, des clients WebSocket qui épinglent
//! le certificat. Aucun trafic hors de 127.0.0.1.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use protocol::wire::{
    camera_snapshot_path, camera_stream_path, StreamKind, StreamPacket, StreamStatus, CODEC_H264,
    PROTOCOL_VERSION,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use common::{Client, Resp, Tls};

const PASSWORD: &str = "mot-de-passe-jetable-du-test";

fn rig(tag: &str, camera: fake_camera::Options, write_timeout: Duration) -> in_process::Rig {
    let dir = common::temp_dir(tag);
    in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: PASSWORD,
        camera_addr: "127.0.0.1:0",
        camera,
        client_write_timeout: write_timeout,
    })
}

async fn login(client: &Client) -> String {
    let resp = client
        .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["token"].as_str().unwrap().to_string()
}

async fn stream(
    client: &Client,
    token: &str,
    path: &str,
    recv_buffer: Option<u32>,
) -> Result<WebSocketStream<Tls>, Resp> {
    let tls = match recv_buffer {
        None => client.connect().await,
        // Un tampon de réception minuscule : le client « avale » très peu.
        Some(size) => {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.set_recv_buffer_size(size).unwrap();
            let tcp = socket.connect(client.addr).await.unwrap();
            tokio_rustls::TlsConnector::from(common::tls_config(&client.fingerprint))
                .connect(
                    tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                    tcp,
                )
                .await
                .unwrap()
        }
    };
    let mut request = format!("wss://localhost:{}{path}", client.addr.port())
        .into_client_request()
        .unwrap();
    if !token.is_empty() {
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    }
    match tokio_tungstenite::client_async(request, tls).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => Err(Resp {
            status: response.status().as_u16(),
            headers: Default::default(),
            body: response.body().clone().unwrap_or_default(),
        }),
        Err(e) => panic!("WebSocket : {e}"),
    }
}

/// Ce qu'un client a reçu.
#[derive(Default)]
struct Received {
    statuses: Vec<StreamStatus>,
    config: Option<StreamPacket>,
    frames: Vec<(bool, u32)>,
    closed: Option<u16>,
}

async fn read_for(ws: &mut WebSocketStream<Tls>, received: &mut Received, how_long: Duration) {
    let end = tokio::time::Instant::now() + how_long;
    while let Ok(Some(message)) = tokio::time::timeout_at(end, ws.next()).await {
        match message {
            Ok(Message::Text(text)) => received
                .statuses
                .push(serde_json::from_str(text.as_str()).unwrap()),
            Ok(Message::Binary(bytes)) => match StreamPacket::decode(&bytes).unwrap() {
                config @ StreamPacket::Config { .. } => received.config = Some(config),
                StreamPacket::Frame {
                    key, timestamp_ms, ..
                } => received.frames.push((key, timestamp_ms)),
            },
            Ok(Message::Close(frame)) => {
                received.closed = Some(frame.map_or(1005, |f| u16::from(f.code)));
                return;
            }
            Ok(_) => {}
            Err(_) => {
                received.closed.get_or_insert(1006);
                return;
            }
        }
    }
}

async fn until(what: &str, how_long: Duration, mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + how_long;
    while Instant::now() < end {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("délai dépassé : {what}");
}

#[test]
fn the_route_refuses_what_it_should_and_says_why() {
    let rig = rig(
        "stream-refusals",
        Default::default(),
        Duration::from_secs(10),
    );
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let sub = camera_stream_path(rig.with_sub, StreamKind::Sub);

        // Sans jeton : refusé, comme tout le reste de l'API.
        let refused = stream(&client, "", &sub, None).await.err().unwrap();
        assert_eq!(refused.status, 401);

        // Une caméra inconnue : 404 avec un détail (c'est ce qui la distingue d'une route inconnue).
        let unknown = camera_stream_path(protocol::wire::CAMERA_ID_MIN + 12345, StreamKind::Sub);
        let refused = stream(&client, &token, &unknown, None).await.err().unwrap();
        assert_eq!(refused.status, 404);
        assert!(
            refused.json()["error"]["detail"].is_string(),
            "{}",
            refused.text()
        );

        // Une route qui n'existe pas : 404 sans détail (un serveur trop ancien répond ainsi).
        let refused = stream(&client, &token, "/api/cameras/1/inexistante", None)
            .await
            .err()
            .unwrap();
        assert_eq!(refused.status, 404);
        assert!(refused.json()["error"]["detail"].is_null());

        // Pas de flux réduit sur « Jardin », un `kind` faux, un identifiant qui n'en est pas un.
        let no_sub = camera_stream_path(rig.main_only, StreamKind::Sub);
        assert_eq!(
            stream(&client, &token, &no_sub, None)
                .await
                .err()
                .unwrap()
                .status,
            400
        );
        let bad_kind = format!("/api/cameras/{}/stream?kind=enorme", rig.with_sub);
        assert_eq!(
            stream(&client, &token, &bad_kind, None)
                .await
                .err()
                .unwrap()
                .status,
            400
        );
        let bad_id = "/api/cameras/abc/stream";
        assert_eq!(
            stream(&client, &token, bad_id, None)
                .await
                .err()
                .unwrap()
                .status,
            400
        );
        let not_a_camera = "/api/cameras/42/stream";
        assert_eq!(
            stream(&client, &token, not_a_camera, None)
                .await
                .err()
                .unwrap()
                .status,
            400
        );

        // Aucune de ces tentatives n'a touché à la caméra.
        assert_eq!(
            rig.camera
                .stats
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(rig.hub.camera_viewers(rig.with_sub), 0);
    });
}

#[test]
fn clients_share_one_camera_connection_and_it_closes_after_the_last() {
    let rig = rig("stream-share", Default::default(), Duration::from_secs(10));
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let path = camera_stream_path(rig.with_sub, StreamKind::Sub);

        let mut a = stream(&client, &token, &path, None).await.expect("flux A");
        let mut seen_a = Received::default();
        read_for(&mut a, &mut seen_a, Duration::from_millis(1500)).await;
        assert!(
            seen_a.statuses.contains(&StreamStatus::Live),
            "{:?}",
            seen_a.statuses
        );
        let Some(StreamPacket::Config {
            codec,
            width,
            height,
            ..
        }) = &seen_a.config
        else {
            panic!("pas de configuration")
        };
        assert_eq!((*codec, *width, *height), (CODEC_H264, 640, 360));
        assert!(
            seen_a.frames.len() >= 5 && seen_a.frames[0].0,
            "{:?}",
            seen_a.frames
        );

        let mut b = stream(&client, &token, &path, None).await.expect("flux B");
        let mut seen_b = Received::default();
        read_for(&mut b, &mut seen_b, Duration::from_millis(800)).await;
        assert!(seen_b.config.is_some() && !seen_b.frames.is_empty());
        assert!(seen_b.frames[0].0, "B repart d'une image clé");
        assert_eq!(
            rig.camera.plays(),
            1,
            "une seule session de caméra pour deux spectateurs"
        );
        assert_eq!(rig.hub.camera_viewers(rig.with_sub), 1);
        // La caméra est en ligne, sans que la surveillance (absente ici) y soit pour quelque chose.
        let status = rig
            .hub
            .cameras()
            .into_iter()
            .find(|c| c.id == rig.with_sub)
            .unwrap()
            .status;
        assert_eq!(status, protocol::wire::CameraStatus::Online);

        // Le départ du dernier spectateur coupe la caméra après quelques secondes.
        drop(a);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(rig.camera.playing(), 1);
        drop(b);
        until("la caméra est coupée", Duration::from_secs(12), || {
            rig.camera.open() == 0
        })
        .await;
        assert_eq!(rig.hub.camera_viewers(rig.with_sub), 0);
    });
}

/// Le délai d'écriture ordinaire (ici 300 ms au lieu de 10 s) ne tue pas un client vidéo qui
/// n'avance plus, et ce client ne ralentit pas les autres.
#[test]
fn a_stalled_video_client_survives_and_does_not_slow_the_others() {
    let rig = rig(
        "stream-slow",
        fake_camera::Options {
            fps: 600,
            ..Default::default()
        },
        Duration::from_millis(300),
    );
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let path = camera_stream_path(rig.with_sub, StreamKind::Sub);

        // A ne lit jamais, avec un tampon de réception minuscule : le serveur ne peut plus lui écrire.
        let mut stalled = stream(&client, &token, &path, Some(2048))
            .await
            .expect("flux A");
        let mut fast = stream(&client, &token, &path, None).await.expect("flux B");
        let mut seen = Received::default();
        let started = tokio::time::Instant::now();
        read_for(&mut fast, &mut seen, Duration::from_secs(5)).await;
        assert!(seen.closed.is_none(), "le client rapide n'est pas coupé");
        // ~600 images par seconde (de quoi remplir les tampons du client arrêté) : le client rapide
        // les reçoit presque toutes, dans l'ordre (l'horodatage est celui de l'arrivée, croissant).
        let elapsed = started.elapsed().as_secs_f64();
        assert!(
            seen.frames.len() as f64 > elapsed * 600.0 * 0.5,
            "{} images en {elapsed:.1} s",
            seen.frames.len()
        );
        assert!(seen.frames.windows(2).all(|w| w[0].1 <= w[1].1));
        // A a été « tué » par le délai ordinaire ? Non : il est encore ouvert côté serveur.
        assert_eq!(rig.hub.camera_viewers(rig.with_sub), 1);

        // A reprend : il reçoit de la vidéo (au pire après avoir perdu des images), jusqu'à une clé.
        let mut after = Received::default();
        read_for(&mut stalled, &mut after, Duration::from_secs(3)).await;
        assert!(
            after.closed.is_none(),
            "A n'a pas été déconnecté : {:?}",
            after.closed
        );
        assert!(
            after.frames.iter().any(|(key, _)| *key),
            "A a retrouvé une image clé"
        );
    });
}

#[test]
fn logging_out_closes_the_stream() {
    let rig = rig("stream-logout", Default::default(), Duration::from_secs(10));
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let path = camera_stream_path(rig.with_sub, StreamKind::Sub);
        let mut ws = stream(&client, &token, &path, None).await.expect("flux");
        let mut seen = Received::default();
        read_for(&mut ws, &mut seen, Duration::from_millis(700)).await;
        assert!(!seen.frames.is_empty());
        let resp = client
            .request("POST", "/api/logout", Some(&token), None)
            .await;
        assert_eq!(resp.status, 204, "{}", resp.text());
        read_for(&mut ws, &mut seen, Duration::from_secs(6)).await;
        assert_eq!(seen.closed, Some(1008), "session terminée");
    });
}

// ---------------------------------------------------------------------------
// L'image fixe (`GET /api/cameras/{id}/snapshot`, ticket n° 12)
// ---------------------------------------------------------------------------

fn is_jpeg(body: &[u8]) -> bool {
    body.len() > 1000 && body.starts_with(&[0xFF, 0xD8]) && body.ends_with(&[0xFF, 0xD9])
}

#[test]
fn the_snapshot_route_refuses_what_it_should_and_says_why() {
    let rig = rig(
        "snapshot-refusals",
        Default::default(),
        Duration::from_secs(10),
    );
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let path = camera_snapshot_path(rig.main_only);

        // Sans jeton : refusé, comme tout le reste de l'API.
        let refused = client.request("GET", &path, None, None).await;
        assert_eq!(refused.status, 401);
        assert_eq!(refused.code(), "unauthorized");
        let refused = client.get(&path, "jeton-inconnu").await;
        assert_eq!(refused.status, 401);

        // Une caméra inconnue : 404 avec un détail ; un identifiant invalide : 400.
        let unknown = camera_snapshot_path(protocol::wire::CAMERA_ID_MIN + 12345);
        let refused = client.get(&unknown, &token).await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (404, "not_found")
        );
        assert!(refused.json()["error"]["detail"].is_string());
        for bad in ["/api/cameras/abc/snapshot", "/api/cameras/42/snapshot"] {
            let refused = client.get(bad, &token).await;
            assert_eq!(refused.status, 400, "{bad}");
            assert_eq!(refused.code(), "bad_request");
        }
        // Une lecture seule : pas de POST.
        let refused = client
            .request("POST", &path, Some(&token), Some(b"{}"))
            .await;
        assert_eq!(refused.status, 405);

        // Aucune de ces tentatives n'a touché à la caméra.
        assert_eq!(
            rig.camera
                .stats
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(rig.hub.camera_viewers(rig.main_only), 0);
    });
}

#[test]
fn a_snapshot_is_a_shared_jpeg_and_the_camera_closes_after_it() {
    let rig = rig("snapshot-jpeg", Default::default(), Duration::from_secs(10));
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        // La caméra sans flux réduit : l'image vient du flux principal.
        let path = camera_snapshot_path(rig.main_only);

        // Trois demandes en même temps : une seule capture, la même image.
        let (a, b, c) = tokio::join!(
            client.get(&path, &token),
            client.get(&path, &token),
            client.get(&path, &token)
        );
        for resp in [&a, &b, &c] {
            assert_eq!(resp.status, 200, "{}", resp.text());
            assert_eq!(
                resp.headers.get("content-type").map(String::as_str),
                Some("image/jpeg")
            );
            assert_eq!(
                resp.headers.get("cache-control").map(String::as_str),
                Some("no-store")
            );
            assert!(resp.headers.contains_key("age"));
            assert!(is_jpeg(&resp.body), "{} octets", resp.body.len());
        }
        assert_eq!(a.body, b.body);
        assert_eq!(a.body, c.body);
        assert_eq!(
            rig.camera.plays(),
            1,
            "une seule connexion pour trois demandes"
        );

        // Plus tard, le cache sert la même image sans rouvrir la caméra.
        let again = client.get(&path, &token).await;
        assert_eq!(again.body, a.body);
        assert_eq!(rig.camera.plays(), 1);

        // La session courte se ferme d'elle-même : plus de spectateur, plus de connexion.
        until("la caméra est coupée", Duration::from_secs(12), || {
            rig.camera.open() == 0
        })
        .await;
        assert_eq!(rig.hub.camera_viewers(rig.main_only), 0);
    });
}

#[test]
fn a_snapshot_of_a_watched_camera_joins_the_stream_already_open() {
    let rig = rig(
        "snapshot-joined",
        Default::default(),
        Duration::from_secs(10),
    );
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let sub = camera_stream_path(rig.with_sub, StreamKind::Sub);
        let mut watcher = stream(&client, &token, &sub, None).await.expect("flux");
        let mut seen = Received::default();
        read_for(&mut watcher, &mut seen, Duration::from_millis(800)).await;
        assert_eq!(rig.camera.plays(), 1);

        let snapshot = client
            .get(&camera_snapshot_path(rig.with_sub), &token)
            .await;
        assert_eq!(snapshot.status, 200, "{}", snapshot.text());
        assert!(is_jpeg(&snapshot.body));
        assert_eq!(rig.camera.plays(), 1, "l'image vient du flux déjà ouvert");
        assert_eq!(rig.hub.camera_viewers(rig.with_sub), 1);
        drop(watcher);
    });
}

// Le verrou std est tenu pendant la requête : c'est ce qu'on veut montrer (la capture ne l'attend pas).
#[allow(clippy::await_holding_lock)]
#[test]
fn a_snapshot_waits_for_nothing_when_a_test_holds_the_camera() {
    let rig = rig("snapshot-busy", Default::default(), Duration::from_secs(10));
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let path = camera_snapshot_path(rig.main_only);

        // Un test ou une vérification de la caméra tient son verrou.
        let lock = rig.hub.camera_probe_lock(rig.main_only);
        let held = lock.try_lock().unwrap();
        let started = Instant::now();
        let busy = client.get(&path, &token).await;
        assert_eq!((busy.status, busy.code().as_str()), (503, "unavailable"));
        assert!(busy.json()["error"]["retry_after_s"].is_u64());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            rig.camera
                .stats
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(rig.hub.camera_viewers(rig.main_only), 0);

        drop(held);
        assert_eq!(client.get(&path, &token).await.status, 200);
    });
}

#[test]
fn a_camera_that_does_not_answer_gives_a_clear_error_within_the_deadline() {
    let rig = rig(
        "snapshot-silent",
        Default::default(),
        Duration::from_secs(10),
    );
    rig.camera
        .stats
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    rig.runtime.block_on(async {
        let client = Client::new(rig.server.addr, &rig.fingerprint);
        let token = login(&client).await;
        let started = Instant::now();
        let resp = client
            .get(&camera_snapshot_path(rig.main_only), &token)
            .await;
        assert_eq!(
            (resp.status, resp.code().as_str()),
            (502, "device_unreachable"),
            "{}",
            resp.text()
        );
        // Le délai maximal du serveur (12 s) n'est jamais dépassé.
        assert!(
            started.elapsed() < Duration::from_secs(14),
            "{:?}",
            started.elapsed()
        );
        // Ni adresse ni mot de passe dans le message.
        let text = resp.text();
        assert!(
            !text.contains("127.0.0.1") && !text.contains("rtsp"),
            "{text}"
        );
        until("plus de spectateur", Duration::from_secs(12), || {
            rig.hub.camera_viewers(rig.main_only) == 0
        })
        .await;
    });
}
