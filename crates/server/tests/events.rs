//! Les routes des événements de détection (ticket n° 18), de bout en bout : jeton de session,
//! liste, fiche, capture, clip avec `Range`, suppression. Un vrai serveur HTTPS dans le processus,
//! dossier jetable, 127.0.0.1 ; aucun modèle : la détection est désactivée et le dit.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::time::Duration;

use protocol::wire::{DetectedClass, DetectionEventView, PROTOCOL_VERSION};

use common::Client;

const PASSWORD: &str = "mot-de-passe-jetable-du-test";

#[test]
fn events_are_listed_served_with_ranges_and_deleted_under_the_session() {
    let dir = common::temp_dir("events");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: PASSWORD,
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    let clip: Vec<u8> = (0..100u8).collect();
    let event = DetectionEventView {
        id: format!("1763500000000-{}", rig.with_sub),
        camera_id: rig.with_sub,
        camera_name: "Entrée".into(),
        classes: vec![DetectedClass::Personne, DetectedClass::Chien],
        score: 0.87,
        start: 1_763_500_000.0,
        end: 1_763_500_012.0,
        has_clip: true,
        clip_s: 30.0,
        in_progress: false,
        description: None,
        niveau: None,
    };
    common::runtime().block_on(async {
        // Sans jeton : refusé.
        let r = client.request("GET", "/api/events", None, None).await;
        assert_eq!(r.status, 401);
        let r = client
            .request("GET", &format!("/api/events/{}/clip", event.id), None, None)
            .await;
        assert_eq!(r.status, 401);

        let resp = client
            .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        let token = resp.json()["token"].as_str().unwrap().to_string();

        // Rien encore ; la détection dit pourquoi elle est coupée (pas de modèle).
        let list = client.get("/api/events", &token).await;
        assert_eq!(list.status, 200, "{}", list.text());
        let body = list.json();
        assert_eq!(body["events"].as_array().unwrap().len(), 0);
        assert_eq!(body["detection"]["available"], false);

        rig.hub
            .detection()
            .store()
            .save(event.clone(), Some(b"\xff\xd8jpeg"), Some(&clip))
            .unwrap();
        let body = client.get("/api/events?limit=10", &token).await.json();
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], event.id.as_str());
        assert_eq!(
            events[0]["camera_id"],
            rig.with_sub.to_string(),
            "identifiant en texte"
        );
        assert_eq!(events[0]["classes"][0], "personne");
        let other = client
            .get(&format!("/api/events?camera={}", rig.main_only), &token)
            .await
            .json();
        assert_eq!(other["events"].as_array().unwrap().len(), 0);
        assert_eq!(
            client.get("/api/events?camera=abc", &token).await.status,
            400
        );

        // Fiche, capture, clip entier puis par morceaux.
        let one = client
            .get(&format!("/api/events/{}", event.id), &token)
            .await;
        assert_eq!(one.status, 200);
        let snap = client
            .get(&format!("/api/events/{}/snapshot", event.id), &token)
            .await;
        assert_eq!(snap.status, 200);
        assert_eq!(snap.headers["content-type"], "image/jpeg");
        let whole = client
            .get(&format!("/api/events/{}/clip", event.id), &token)
            .await;
        assert_eq!(whole.status, 200);
        assert_eq!(whole.headers["content-type"], "video/mp4");
        assert_eq!(whole.headers["accept-ranges"], "bytes");
        assert_eq!(whole.body, clip);
        let part = client
            .request_with(
                "GET",
                &format!("/api/events/{}/clip", event.id),
                Some(&token),
                None,
                &[("Range", "bytes=10-19")],
            )
            .await;
        assert_eq!(part.status, 206);
        assert_eq!(part.headers["content-range"], "bytes 10-19/100");
        assert_eq!(part.body, clip[10..20]);
        let beyond = client
            .request_with(
                "GET",
                &format!("/api/events/{}/clip", event.id),
                Some(&token),
                None,
                &[("Range", "bytes=500-")],
            )
            .await;
        assert_eq!(beyond.status, 416);
        assert_eq!(beyond.headers["content-range"], "bytes */100");

        // Identifiants refusés avant le disque ; inconnus : 404.
        assert_eq!(
            client
                .get("/api/events/..%2Fcameras/clip", &token)
                .await
                .status,
            400
        );
        assert_eq!(
            client.get("/api/events/ABC/snapshot", &token).await.status,
            400
        );
        assert_eq!(client.get("/api/events/1-2/clip", &token).await.status, 404);

        // Suppression.
        let del = client
            .request(
                "DELETE",
                &format!("/api/events/{}", event.id),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(del.status, 202);
        let again = client
            .request(
                "DELETE",
                &format!("/api/events/{}", event.id),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(again.status, 404);
        assert_eq!(
            client
                .get(&format!("/api/events/{}/clip", event.id), &token)
                .await
                .status,
            404
        );
    });
    drop(rig);
    let _ = std::fs::remove_dir_all(dir);
}
