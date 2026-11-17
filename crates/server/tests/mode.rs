//! Le mode global de la maison (ticket n° 8), de bout en bout : `PUT /api/mode` avec une session
//! ou avec le jeton de mode (Raccourcis), l'événement `mode_changed`, la limitation des essais, et
//! les caméras éteintes. Un vrai serveur HTTPS dans le processus, dossier jetable, 127.0.0.1.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::time::Duration;

use protocol::wire::{
    camera_snapshot_path, camera_stream_path, CameraModes, CameraPlacement, StreamKind,
    PROTOCOL_VERSION,
};
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

fn client(rig: &in_process::Rig) -> Client {
    Client::new(rig.server.addr, &rig.fingerprint)
}

async fn login(client: &Client) -> String {
    let resp = client
        .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["token"].as_str().unwrap().to_string()
}

async fn put_mode(client: &Client, token: Option<&str>, mode: &str) -> common::Resp {
    client
        .request(
            "PUT",
            "/api/mode",
            token,
            Some(json!({ "mode": mode }).to_string().as_bytes()),
        )
        .await
}

#[test]
fn a_session_changes_the_mode_and_everyone_hears_about_it() {
    let rig = rig("mode-session");
    let client = client(&rig);
    common::runtime().block_on(async {
        let token = login(&client).await;
        assert_eq!(
            client.get("/api/state", &token).await.json()["mode"],
            "present"
        );
        let mut ws = client.ws(&token, PROTOCOL_VERSION).await.unwrap();
        let first = common::next_json(&mut ws, Duration::from_secs(5)).await;
        assert_eq!(first.unwrap()["type"], "hello");

        let changed = put_mode(&client, Some(&token), "absent").await;
        assert_eq!(changed.status, 200, "{}", changed.text());
        assert_eq!(changed.json(), json!({"mode": "absent", "changed": true}));
        let event = common::wait_event(&mut ws, "mode_changed", Duration::from_secs(5), |e| {
            e["mode"] == "absent"
        })
        .await;
        assert_eq!(event["event"]["mode"], "absent");
        assert_eq!(
            client.get("/api/state", &token).await.json()["mode"],
            "absent"
        );

        // Même mode : accepté, mais rien ne change (ni diffusion, ni déclenchement).
        let same = put_mode(&client, Some(&token), "absent").await;
        assert_eq!(same.json(), json!({"mode": "absent", "changed": false}));

        // Corps ou mode invalides : refus propre.
        let bad = put_mode(&client, Some(&token), "dehors").await;
        assert_eq!((bad.status, bad.code().as_str()), (400, "bad_request"));
        let junk = client
            .request("PUT", "/api/mode", Some(&token), Some(b"pas du json"))
            .await;
        assert_eq!(junk.code(), "bad_request");
        // Pas de jeton du tout, ou une autre méthode.
        let anonymous = put_mode(&client, None, "nuit").await;
        assert_eq!(
            (anonymous.status, anonymous.code().as_str()),
            (401, "unauthorized")
        );
        let get = client.get("/api/mode", &token).await;
        assert_eq!(get.status, 405);
    });
    assert_eq!(rig.hub.mode(), protocol::mode::HomeMode::Absent);
}

#[test]
fn the_mode_token_changes_the_mode_and_can_do_nothing_else() {
    let rig = rig("mode-token");
    let client = client(&rig);
    let token = rig.hub.generate_mode_token().unwrap();
    common::runtime().block_on(async {
        let ok = put_mode(&client, Some(&token), "vacances").await;
        assert_eq!(ok.status, 200, "{}", ok.text());
        assert_eq!(ok.json(), json!({"mode": "vacances", "changed": true}));
        assert_eq!(rig.hub.mode(), protocol::mode::HomeMode::Vacances);

        // Le jeton de mode n'ouvre aucune autre porte : ni l'état, ni le plan, ni les règles,
        // ni l'export, ni le WebSocket, ni les caméras.
        for (method, path) in [
            ("GET", "/api/state"),
            ("GET", "/api/plan"),
            ("PUT", "/api/plan"),
            ("GET", "/api/rules"),
            ("GET", "/api/cameras"),
            ("GET", "/api/export"),
            ("POST", "/api/logout"),
        ] {
            let r = client
                .request(method, path, Some(&token), Some(b"{}"))
                .await;
            assert_eq!(
                (r.status, r.code().as_str()),
                (401, "unauthorized"),
                "{path}"
            );
        }
        assert!(client.ws(&token, PROTOCOL_VERSION).await.is_err());
        // Et il ne passe pas dans l'URL.
        let in_url = client
            .request(
                "PUT",
                &format!("/api/mode?token={token}"),
                None,
                Some(b"{\"mode\":\"nuit\"}"),
            )
            .await;
        assert_eq!(in_url.status, 401);

        // Un nouveau jeton révoque l'ancien.
        let newer = rig.hub.generate_mode_token().unwrap();
        let old = put_mode(&client, Some(&token), "nuit").await;
        assert_eq!(old.status, 401);
        let new = put_mode(&client, Some(&newer), "nuit").await;
        assert_eq!(new.json(), json!({"mode": "nuit", "changed": true}));

        // Sans jeton généré (révoqué), plus d'accès par jeton.
        rig.hub.revoke_mode_token().unwrap();
        assert_eq!(put_mode(&client, Some(&newer), "present").await.status, 401);
    });
}

#[test]
fn mode_token_failures_are_limited_without_locking_the_login_out() {
    let rig = rig("mode-limit");
    let client = client(&rig);
    let token = rig.hub.generate_mode_token().unwrap();
    common::runtime().block_on(async {
        for _ in 0..5 {
            let r = put_mode(&client, Some("un-mauvais-jeton"), "nuit").await;
            assert_eq!(r.code(), "unauthorized");
        }
        // L'adresse est en attente pour le jeton de mode : même le bon jeton est refusé...
        let locked = put_mode(&client, Some(&token), "nuit").await;
        assert_eq!(
            (locked.status, locked.code().as_str()),
            (429, "too_many_attempts")
        );
        assert!(locked.headers.contains_key("retry-after"));
        assert_eq!(rig.hub.mode(), protocol::mode::HomeMode::Present);
        // ...mais la connexion par mot de passe, elle, n'est pas touchée.
        let session = login(&client).await;
        assert_eq!(put_mode(&client, Some(&session), "nuit").await.status, 200);
    });
}

#[test]
fn an_off_camera_refuses_snapshot_and_live_with_the_mode_in_the_message() {
    let rig = rig("mode-camera");
    let client = client(&rig);
    let indoor = rig
        .hub
        .add_camera(&matter_home_server::cameras::CameraDraft {
            name: "Salon".into(),
            url: format!("rtsp://127.0.0.1:{}/live/salon", rig.camera.port),
            emplacement: CameraPlacement::Interieure,
            modes: CameraModes::for_placement(CameraPlacement::Interieure),
            ..Default::default()
        })
        .unwrap();
    common::runtime().block_on(async {
        let token = login(&client).await;
        // L'état complet donne l'emplacement et le tableau, sans adresse.
        let state = client.get("/api/state", &token).await.json();
        let salon = state["cameras"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Salon")
            .unwrap();
        assert_eq!(salon["emplacement"], "interieure");
        assert_eq!(salon["modes"]["present"], "eteinte");
        assert_eq!(salon["modes"]["absent"], "surveillance");
        assert!(!salon.to_string().contains("rtsp://"));

        // Présent : éteinte. Image fixe et direct sont refusés, sans « réessayer plus tard ».
        let snap = client.get(&camera_snapshot_path(indoor), &token).await;
        assert_eq!((snap.status, snap.code().as_str()), (503, "unavailable"));
        assert_eq!(
            snap.json()["error"]["detail"],
            "caméra éteinte en mode Présent"
        );
        assert!(snap.json()["error"]["retry_after_s"].is_null());
        let tcp = client.connect().await;
        let mut request = format!(
            "wss://localhost:{}{}",
            client.addr.port(),
            camera_stream_path(indoor, StreamKind::Main)
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tokio_tungstenite::client_async(request, tcp).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status().as_u16(), 503);
            }
            other => panic!("refus attendu : {:?}", other.map(|_| ())),
        }

        // Une caméra extérieure d'avant le ticket (jamais éteinte) n'est pas concernée : la
        // fausse caméra répond, on reçoit une vraie image.
        let outdoor = client
            .get(&camera_snapshot_path(rig.main_only), &token)
            .await;
        assert_eq!(outdoor.status, 200);
        assert!(outdoor.body.starts_with(&[0xFF, 0xD8]), "un JPEG");

        // Absent : l'intérieure surveille, elle n'est plus refusée pour cette raison.
        assert_eq!(put_mode(&client, Some(&token), "absent").await.status, 200);
        let snap = client.get(&camera_snapshot_path(indoor), &token).await;
        assert_eq!(snap.status, 200);
        assert!(snap.body.starts_with(&[0xFF, 0xD8]), "un JPEG");
    });
}
