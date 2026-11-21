//! Les notifications web push (ticket n° 17), de bout en bout : routes sous le jeton, abonnement,
//! essai, règle « Notifier » nettoyée puis chiffrée, abonnement expiré (410) oublié, révocation.
//! Un vrai serveur HTTPS dans le processus, dossier jetable, 127.0.0.1 ; l'envoi passe par un faux
//! service de push en mémoire : **rien ne sort de la machine**.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};

use ostal_server::cameras::CameraDraft;
use ostal_server::events::Event;
use ostal_server::push::crypto;
use ostal_server::push::transport::{PushReply, PushRequest, SendError, Transport};
use protocol::wire::PROTOCOL_VERSION;

use common::Client;

const PASSWORD: &str = "mot-de-passe-jetable-du-test";
// Clés de l'appareil du RFC 8291, annexe A.
const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const ENDPOINT: &str = "https://web.push.apple.com/QGuQyavXutnMH-test";

/// Le faux service : garde les envois, répond ce qu'on lui dit (201 par défaut).
#[derive(Default)]
struct FakeService {
    sent: Mutex<Vec<PushRequest>>,
    next: Mutex<Option<u16>>,
}

impl Transport for FakeService {
    fn post(&self, request: &PushRequest) -> Result<PushReply, SendError> {
        self.sent.lock().unwrap().push(request.clone());
        Ok(PushReply {
            status: self.next.lock().unwrap().take().unwrap_or(201),
            retry_after: None,
        })
    }
}

impl FakeService {
    fn wait_for(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.sent.lock().unwrap().len() < n {
            assert!(Instant::now() < deadline, "pas d'envoi n° {n}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Le message n° `i`, déchiffré comme le ferait le navigateur.
    fn message(&self, i: usize) -> Value {
        let body = self.sent.lock().unwrap()[i].body.clone();
        let d = |t: &str| URL_SAFE_NO_PAD.decode(t).unwrap();
        let as_public = &body[21..86];
        let secret = p256::SecretKey::from_slice(&d(UA_PRIVATE)).unwrap();
        let peer = p256::PublicKey::from_sec1_bytes(as_public).unwrap();
        let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
        let keys = crypto::derive(
            shared.raw_secret_bytes(),
            &d(AUTH),
            &d(UA_PUBLIC),
            as_public,
            &body[..16],
        );
        let key = ring::aead::LessSafeKey::new(
            ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &keys.cek).unwrap(),
        );
        let nonce = ring::aead::Nonce::try_assume_unique_for_key(&keys.nonce).unwrap();
        let mut record = body[86..].to_vec();
        let plain = key
            .open_in_place(nonce, ring::aead::Aad::empty(), &mut record)
            .unwrap();
        assert_eq!(plain.last(), Some(&0x02));
        serde_json::from_slice(&plain[..plain.len() - 1]).unwrap()
    }
}

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

async fn send(client: &Client, method: &str, path: &str, token: &str, body: Value) -> common::Resp {
    client
        .request(method, path, Some(token), Some(body.to_string().as_bytes()))
        .await
}

#[test]
fn a_subscribed_device_gets_encrypted_clean_notifications_and_can_be_revoked() {
    let rig = rig("push");
    let fake = Arc::new(FakeService::default());
    rig.hub.push().set_transport(fake.clone());
    // Une caméra avec identifiant et mot de passe : ils ne doivent jamais apparaître.
    rig.hub
        .add_camera(&CameraDraft {
            name: "Garage".into(),
            url: format!("rtsp://127.0.0.1:{}/live/ch02_0", rig.camera.port),
            username: "gardien".into(),
            password: Some("Sesame-1234".into()),
            ..CameraDraft::default()
        })
        .unwrap();
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    common::runtime().block_on(async {
        // Sans jeton : refusé.
        let anonymous = client.request("GET", "/api/push", None, None).await;
        assert_eq!(anonymous.status, 401);
        let login = client
            .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
            .await;
        let token = login.json()["token"].as_str().unwrap().to_string();

        let status = client.get("/api/push", &token).await;
        assert_eq!(status.status, 200, "{}", status.text());
        let status = status.json();
        assert_eq!(status["enabled"], true);
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(status["public_key"].as_str().unwrap())
                .unwrap()
                .len(),
            65
        );
        assert_eq!(status["subscriptions"], json!([]));

        // Une adresse qui n'est pas un service de push : refusée (pas de SSRF).
        let bad = send(&client, "POST", "/api/push/subscriptions", &token, json!({
            "endpoint": "https://192.168.1.1/push", "keys": {"p256dh": UA_PUBLIC, "auth": AUTH}
        }))
        .await;
        assert_eq!((bad.status, bad.code().as_str()), (400, "bad_request"));

        let sub = send(&client, "POST", "/api/push/subscriptions", &token, json!({
            "endpoint": ENDPOINT, "expirationTime": null,
            "keys": {"p256dh": UA_PUBLIC, "auth": AUTH}, "label": "iPhone"
        }))
        .await;
        assert_eq!(sub.status, 200, "{}", sub.text());
        let sub = sub.json();
        let id = sub["id"].as_str().unwrap().to_string();
        assert_eq!(sub["service"], "Apple");
        assert!(sub.get("endpoint").is_none());
        let listed = client.get("/api/push", &token).await.text();
        assert!(!listed.contains("QGuQyavXutnMH"), "l'adresse n'est jamais renvoyée");

        // L'essai.
        let test = client
            .request("POST", &format!("/api/push/subscriptions/{id}/test"), Some(&token), None)
            .await;
        assert_eq!(test.status, 202, "{}", test.text());
        fake.wait_for(1);
        assert_eq!(fake.message(0)["title"], "Notifications activées");
        {
            let sent = fake.sent.lock().unwrap();
            assert_eq!(sent[0].endpoint, ENDPOINT);
            assert!(sent[0].authorization.starts_with("vapid t="));
        }

        // Une règle « Notifier » dont le texte cite la caméra : nettoyé avant l'envoi.
        rig.hub.apply(&Event::Notification {
            title: "Garage".into(),
            message: format!(
                "Flux rtsp://gardien:Sesame-1234@127.0.0.1:{}/live/ch02_0 coupé (gardien, Sesame-1234, 00000000DEADBEEF)",
                rig.camera.port
            ),
            at: 0.0,
        });
        fake.wait_for(2);
        let message = fake.message(1);
        let text = message.to_string();
        for leak in ["rtsp", "127.0.0.1", "gardien", "Sesame", "DEADBEEF"] {
            assert!(!text.contains(leak), "{leak} dans {text}");
        }
        assert_eq!(message["url"], "/app/");
        assert_eq!(fake.sent.lock().unwrap()[1].urgency, "high");

        // Catégorie décochée : la règle ne part plus vers cet appareil.
        let off = send(&client, "PUT", &format!("/api/push/subscriptions/{id}"), &token, json!({
            "categories": {"rules": false}
        }))
        .await;
        assert_eq!(off.json()["categories"]["rules"], false);
        rig.hub.apply(&Event::Notification {
            title: "x".into(),
            message: "y".into(),
            at: 0.0,
        });
        // Réactivée ; le service répond 410 : l'abonnement est oublié.
        send(&client, "PUT", &format!("/api/push/subscriptions/{id}"), &token, json!({
            "categories": {"rules": true}
        }))
        .await;
        *fake.next.lock().unwrap() = Some(410);
        rig.hub.apply(&Event::Notification {
            title: "Porte".into(),
            message: "Ouverte".into(),
            at: 0.0,
        });
        fake.wait_for(3);
        assert_eq!(fake.message(2)["title"], "Porte", "le message décoché n'est pas parti");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let subs = client.get("/api/push", &token).await.json()["subscriptions"].clone();
            if subs == json!([]) {
                break;
            }
            assert!(Instant::now() < deadline, "abonnement expiré gardé");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Réabonné puis révoqué ; coupé, l'essai est refusé.
        let again = send(&client, "POST", "/api/push/subscriptions", &token, json!({
            "endpoint": ENDPOINT, "keys": {"p256dh": UA_PUBLIC, "auth": AUTH}, "label": "iPhone"
        }))
        .await
        .json();
        let id = again["id"].as_str().unwrap().to_string();
        let off = send(&client, "PUT", "/api/push", &token, json!({"enabled": false})).await;
        assert_eq!(off.json()["enabled"], false);
        let test = client
            .request("POST", &format!("/api/push/subscriptions/{id}/test"), Some(&token), None)
            .await;
        assert_eq!(test.status, 503);
        let gone = client
            .request("DELETE", &format!("/api/push/subscriptions/{id}"), Some(&token), None)
            .await;
        assert_eq!(gone.status, 202);
        let missing = client
            .request("DELETE", &format!("/api/push/subscriptions/{id}"), Some(&token), None)
            .await;
        assert_eq!(missing.status, 404);
        let invalid = client
            .request("DELETE", "/api/push/subscriptions/..%2F..", Some(&token), None)
            .await;
        assert_eq!(invalid.status, 400);
    });
    assert_eq!(fake.sent.lock().unwrap().len(), 3);
}
