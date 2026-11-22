//! Le jeton de lecture (widget Scriptable de l'iPhone) : il ouvre `GET /api/state` et
//! `GET /api/events`, rien d'autre. Chaque groupe de routes est essayé : API, WebSocket (en-tête et
//! sous-protocole), caméras (image et direct), événements (fiche, capture, clip, suppression),
//! notifications, export, import, mode, jetons. Un vrai serveur HTTPS dans le processus, une fausse
//! caméra locale, un dossier jetable, 127.0.0.1.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use protocol::wire::{camera_snapshot_path, camera_stream_path, StreamKind, PROTOCOL_VERSION};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use common::Client;

const PASSWORD: &str = "mot-de-passe-jetable-du-test";

fn rig(tag: &str) -> in_process::Rig {
    let dir = common::temp_dir(tag);
    in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: PASSWORD,
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    })
}

async fn login(client: &Client) -> String {
    let resp = client
        .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["token"].as_str().unwrap().to_string()
}

/// Un WebSocket (`path`) avec le jeton dans l'en-tête ou dans le sous-protocole du navigateur.
async fn ws_status(client: &Client, path: &str, token: &str, subprotocol: bool) -> u16 {
    let tcp = client.connect().await;
    let mut request = format!("wss://localhost:{}{path}", client.addr.port())
        .into_client_request()
        .unwrap();
    let value = if subprotocol {
        (
            "Sec-WebSocket-Protocol",
            format!("ostal.v1, bearer.{token}"),
        )
    } else {
        ("Authorization", format!("Bearer {token}"))
    };
    request
        .headers_mut()
        .insert(value.0, value.1.parse().unwrap());
    match tokio_tungstenite::client_async(request, tcp).await {
        Ok(_) => 101,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(e) => panic!("WebSocket : {e}"),
    }
}

#[test]
fn the_read_token_reads_state_and_events_and_nothing_else() {
    let rig = rig("read-token-scope");
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    let read = rig.hub.generate_read_token(&rig.user_id, "iPhone").unwrap();
    let mode = rig.hub.generate_mode_token(&rig.user_id, "iPhone").unwrap();
    let camera = rig.main_only;
    common::runtime().block_on(async {
        // Les deux portes qu'il ouvre (la requête `?…` ne change rien).
        for path in ["/api/state", "/api/events", "/api/events?limit=1"] {
            let r = client.get(path, &read).await;
            assert_eq!(r.status, 200, "{path} : {}", r.text());
        }
        assert!(client.get("/api/state", &read).await.json()["devices"].is_array());

        // Toutes les autres : 401, comme sans jeton.
        let event = "/api/events/20261120T101010Z-0000000000000001";
        let snapshot = camera_snapshot_path(camera);
        let stream = camera_stream_path(camera, StreamKind::Main);
        let routes: Vec<(&str, String)> = vec![
            ("HEAD", "/api/state".into()),
            ("POST", "/api/state".into()),
            ("POST", "/api/logout".into()),
            ("GET", "/api/ws?protocol=3".into()),
            ("GET", "/api/rule-runs".into()),
            ("GET", "/api/cameras".into()),
            ("GET", snapshot.clone()),
            ("GET", stream.clone()),
            ("GET", event.into()),
            ("DELETE", event.into()),
            ("GET", format!("{event}/snapshot")),
            ("GET", format!("{event}/clip")),
            ("GET", "/api/push".into()),
            ("PUT", "/api/push".into()),
            ("POST", "/api/push/subscriptions".into()),
            ("PUT", "/api/push/subscriptions/abc".into()),
            ("DELETE", "/api/push/subscriptions/abc".into()),
            ("POST", "/api/push/subscriptions/abc/test".into()),
            ("GET", "/api/plan".into()),
            ("PUT", "/api/plan".into()),
            ("GET", "/api/rules".into()),
            ("PUT", "/api/rules".into()),
            ("POST", "/api/rules/1/test".into()),
            ("POST", "/api/pairing".into()),
            ("DELETE", "/api/devices/1".into()),
            ("POST", "/api/devices/1/retry".into()),
            ("PUT", "/api/devices/1/customization".into()),
            ("POST", "/api/devices/1/on-off".into()),
            ("POST", "/api/devices/1/cover".into()),
            ("GET", "/api/devices/1/history".into()),
            ("DELETE", "/api/devices/1/history".into()),
            ("GET", "/api/devices/1/history.csv".into()),
            ("GET", "/api/export".into()),
            ("POST", "/api/import".into()),
            ("POST", "/api/import?dry_run=1".into()),
            ("PUT", "/api/mode".into()),
            ("GET", "/api/tokens".into()),
            ("DELETE", "/api/tokens/mode".into()),
            ("DELETE", "/api/tokens/read".into()),
            ("POST", "/api/tokens/mode/shortcuts".into()),
            ("POST", "/api/tokens/read/widget".into()),
        ];
        for (method, path) in &routes {
            let body: &[u8] = if path == "/api/mode" {
                b"{\"mode\":\"nuit\"}"
            } else {
                b"{\"origin\":\"https://mac.tailnet-test.ts.net\"}"
            };
            let r = client.request(method, path, Some(&read), Some(body)).await;
            assert_eq!(r.status, 401, "{method} {path} : {}", r.text());
            if *method != "HEAD" {
                assert_eq!(r.code(), "unauthorized", "{method} {path}");
            }
        }
        // Les WebSocket (état en direct, direct d'une caméra), par l'en-tête ou par le
        // sous-protocole du navigateur.
        for path in ["/api/ws?protocol=3", stream.as_str()] {
            for subprotocol in [false, true] {
                assert_eq!(
                    ws_status(&client, path, &read, subprotocol).await,
                    401,
                    "{path} {subprotocol}"
                );
            }
        }
        // Le jeton de lecture ne passe ni par le sous-protocole sur une route de lecture, ni dans
        // l'URL.
        let r = client
            .request_with(
                "GET",
                "/api/state",
                None,
                None,
                &[(
                    "Sec-WebSocket-Protocol",
                    &format!("ostal.v1, bearer.{read}"),
                )],
            )
            .await;
        assert_eq!(r.status, 401);
        let r = client
            .request("GET", &format!("/api/state?token={read}"), None, None)
            .await;
        assert_eq!(r.status, 401);
        // Le mode n'a pas changé, la caméra n'a jamais été contactée.
        assert_eq!(rig.hub.mode(), protocol::mode::HomeMode::Present);
        assert_eq!(rig.camera.stats.accepted.load(Ordering::SeqCst), 0);

        // Les jetons ne se mélangent pas : le jeton de mode ne lit rien, le jeton de lecture ne
        // change pas le mode, aucun n'ouvre les routes des jetons.
        assert_eq!(client.get("/api/state", &mode).await.status, 401);
        assert_eq!(client.get("/api/events", &mode).await.status, 401);
        for token in [&mode, &read] {
            for (method, path) in [
                ("GET", "/api/tokens"),
                ("POST", "/api/tokens/mode/shortcuts"),
                ("POST", "/api/tokens/read/widget"),
            ] {
                let r = client
                    .send_json(method, path, token, &json!({"origin": "https://m.ts.net"}))
                    .await;
                assert_eq!(r.status, 401, "{method} {path}");
            }
        }
        let r = client
            .send_json("PUT", "/api/mode", &mode, &json!({"mode": "nuit"}))
            .await;
        assert_eq!(r.status, 200);

        // Une session marche toujours, jeton de lecture ou pas.
        let session = login(&client).await;
        assert_eq!(client.get("/api/plan", &session).await.status, 200);
        assert_eq!(client.get("/api/state", &session).await.status, 200);

        // Un nouveau jeton pour le même téléphone remplace l'ancien ; une révocation ferme tout.
        let newer = rig.hub.generate_read_token(&rig.user_id, "iPhone").unwrap();
        assert_eq!(client.get("/api/state", &read).await.status, 401);
        assert_eq!(client.get("/api/state", &newer).await.status, 200);
        rig.hub
            .revoke_tokens_of(
                &rig.user_id,
                ostal_server::security::user_tokens::Kind::Read,
            )
            .unwrap();
        assert_eq!(client.get("/api/state", &newer).await.status, 401);
    });
}

#[test]
fn bad_read_tokens_are_slowed_but_never_block_the_good_one() {
    let rig = rig("read-token-limit");
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    let read = rig.hub.generate_read_token(&rig.user_id, "iPhone").unwrap();
    common::runtime().block_on(async {
        // Derrière `tailscale serve`, tout vient de 127.0.0.1 : des échecs en rafale ne bloquent
        // jamais le bon jeton. Chaque échec attend 250 ms.
        for _ in 0..6 {
            let started = Instant::now();
            let r = client.get("/api/state", &"0".repeat(64)).await;
            assert_eq!((r.status, r.code().as_str()), (401, "unauthorized"));
            assert!(r.json()["error"]["retry_after_s"].is_null());
            assert!(started.elapsed() >= Duration::from_millis(250));
        }
        assert_eq!(client.get("/api/state", &read).await.status, 200);
        assert_eq!(client.get("/api/events", &read).await.status, 200);
        assert_eq!(login(&client).await.len(), 64);
    });
}
