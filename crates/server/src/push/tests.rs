use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use protocol::wire::{
    DetectedClass, DetectionEventView, PushCategories, PushKeys, PushPayload, PushSubscribeRequest,
    PushUpdateRequest,
};

use super::content::{self, Sanitizer};
use super::crypto::{self, b64url, b64url_decode, VapidKey};
use super::transport::{self, PushReply, PushRequest, SendError, Transport};
use super::*;
use crate::events::{Event, MeasureKind, NodeStatus};

// ---------------------------------------------------------------------------
// RFC 8291, annexe A
// ---------------------------------------------------------------------------

const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const ECDH_SECRET: &str = "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs";
const IKM: &str = "S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg";
const CEK: &str = "oIhVW04MRdy2XN9CiKLxTg";
const NONCE: &str = "4h_95klXJ5E_qnoN";
const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

fn d(text: &str) -> Vec<u8> {
    b64url_decode(text).expect("base64url")
}

fn public_of(private: &str) -> Vec<u8> {
    let secret = p256::SecretKey::from_slice(&d(private)).unwrap();
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    secret
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec()
}

fn ecdh(private: &str, public: &[u8]) -> Vec<u8> {
    let secret = p256::SecretKey::from_slice(&d(private)).unwrap();
    let peer = p256::PublicKey::from_sec1_bytes(public).unwrap();
    p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine())
        .raw_secret_bytes()
        .to_vec()
}

/// Déchiffre un corps `aes128gcm` avec la clé privée de l'appareil (ce que fait le navigateur).
fn decrypt(ua_private: &str, auth: &[u8], body: &[u8]) -> Result<Vec<u8>, &'static str> {
    let salt = &body[..16];
    let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
    assert_eq!(rs, 4096);
    let idlen = body[20] as usize;
    assert_eq!(idlen, 65);
    let as_public = &body[21..21 + idlen];
    let ua_public = public_of(ua_private);
    let secret = ecdh(ua_private, as_public);
    let keys = crypto::derive(&secret, auth, &ua_public, as_public, salt);
    let key = ring::aead::LessSafeKey::new(
        ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &keys.cek).unwrap(),
    );
    let nonce = ring::aead::Nonce::try_assume_unique_for_key(&keys.nonce).unwrap();
    let mut record = body[21 + idlen..].to_vec();
    let plain = key
        .open_in_place(nonce, ring::aead::Aad::empty(), &mut record)
        .map_err(|_| "étiquette GCM refusée")?;
    let end = plain.iter().rposition(|b| *b != 0).ok_or("vide")?;
    if plain[end] != 0x02 {
        return Err("délimiteur absent");
    }
    Ok(plain[..end].to_vec())
}

#[test]
fn rfc8291_keys_match_their_private_keys() {
    assert_eq!(public_of(AS_PRIVATE), d(AS_PUBLIC));
    assert_eq!(public_of(UA_PRIVATE), d(UA_PUBLIC));
    assert_eq!(ecdh(AS_PRIVATE, &d(UA_PUBLIC)), d(ECDH_SECRET));
    assert_eq!(ecdh(UA_PRIVATE, &d(AS_PUBLIC)), d(ECDH_SECRET));
}

#[test]
fn rfc8291_intermediate_values() {
    let keys = crypto::derive(
        &d(ECDH_SECRET),
        &d(AUTH),
        &d(UA_PUBLIC),
        &d(AS_PUBLIC),
        &d(SALT),
    );
    assert_eq!(b64url(&keys.ikm), IKM);
    assert_eq!(b64url(&keys.cek), CEK);
    assert_eq!(b64url(&keys.nonce), NONCE);
}

#[test]
fn rfc8291_appendix_a_body_is_exact() {
    let salt: [u8; 16] = d(SALT).try_into().unwrap();
    let body = crypto::encrypt_with(
        &d(ECDH_SECRET),
        &d(AS_PUBLIC),
        &d(UA_PUBLIC),
        &d(AUTH),
        &salt,
        PLAINTEXT.as_bytes(),
    )
    .unwrap();
    assert_eq!(b64url(&body), BODY);
    // Et le navigateur le relit.
    assert_eq!(
        decrypt(UA_PRIVATE, &d(AUTH), &d(BODY)).unwrap(),
        PLAINTEXT.as_bytes()
    );
}

#[test]
fn a_real_message_is_readable_only_with_the_right_secret() {
    let body = crypto::encrypt(&d(UA_PUBLIC), &d(AUTH), b"{\"title\":\"x\"}").unwrap();
    assert_eq!(
        decrypt(UA_PRIVATE, &d(AUTH), &body).unwrap(),
        b"{\"title\":\"x\"}"
    );
    assert!(decrypt(UA_PRIVATE, &[7u8; 16], &body).is_err());
    // Une clé éphémère et un sel neufs à chaque message.
    let again = crypto::encrypt(&d(UA_PUBLIC), &d(AUTH), b"{\"title\":\"x\"}").unwrap();
    assert_ne!(body[..86], again[..86]);
    // Clés invalides : refusées.
    assert!(crypto::encrypt(&[4u8; 65], &d(AUTH), b"x").is_err());
    assert!(crypto::encrypt(&d(UA_PUBLIC), &[0u8; 8], b"x").is_err());
    assert!(crypto::encrypt(&d(UA_PUBLIC), &d(AUTH), &vec![b'a'; 5000]).is_err());
}

// ---------------------------------------------------------------------------
// VAPID
// ---------------------------------------------------------------------------

#[test]
fn the_vapid_jwt_is_an_es256_token_signed_by_the_server_key() {
    let key = VapidKey::generate().unwrap();
    let jwt = key
        .jwt("https://web.push.apple.com", 1_800_000_000)
        .unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: serde_json::Value = serde_json::from_slice(&d(parts[0])).unwrap();
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["typ"], "JWT");
    let claims: serde_json::Value = serde_json::from_slice(&d(parts[1])).unwrap();
    assert_eq!(claims["aud"], "https://web.push.apple.com");
    assert_eq!(claims["exp"], 1_800_000_000u64);
    let sub = claims["sub"].as_str().unwrap();
    assert!(sub.starts_with("https://") || sub.starts_with("mailto:"));
    let signature = d(parts[2]);
    assert_eq!(signature.len(), 64, "r ‖ s, 32 octets chacun");
    let public = ring::signature::UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        key.public_key(),
    );
    let input = format!("{}.{}", parts[0], parts[1]);
    public.verify(input.as_bytes(), &signature).unwrap();
    assert!(public.verify(b"autre chose", &signature).is_err());
    // L'en-tête complet.
    let auth = key
        .authorization("https://web.push.apple.com", 1_000)
        .unwrap();
    assert!(auth.starts_with("vapid t="));
    assert!(auth.ends_with(&format!(", k={}", key.public_key_b64())));
    assert_eq!(key.public_key().len(), 65);
    // Relue depuis son PKCS#8 : la même clé.
    let again = VapidKey::from_pkcs8(key.pkcs8()).unwrap();
    assert_eq!(again.public_key(), key.public_key());
}

// ---------------------------------------------------------------------------
// Liste des services
// ---------------------------------------------------------------------------

#[test]
fn only_known_push_services_over_https_are_accepted() {
    for (url, service) in [
        ("https://web.push.apple.com/QGuQyavXutnMH...", "Apple"),
        ("https://api.push.apple.com/3/device/x", "Apple"),
        ("https://fcm.googleapis.com/fcm/send/abc:def", "Google"),
        (
            "https://updates.push.services.mozilla.com/wpush/v2/gAAA",
            "Mozilla",
        ),
        (
            "https://wns2-par02p.notify.windows.com/w/?token=AQE",
            "Microsoft",
        ),
        ("https://WEB.PUSH.APPLE.COM/x", "Apple"),
    ] {
        assert_eq!(transport::service_of(url), Some(service), "{url}");
    }
    for url in [
        "http://web.push.apple.com/x",
        "https://web.push.apple.com:8443/x",
        "https://user:pw@web.push.apple.com/x",
        "https://push.apple.com/x",
        "https://evilpush.apple.com/x",
        "https://web.push.apple.com.evil.example/x",
        "https://notify.windows.com/x",
        "https://127.0.0.1/x",
        "https://[::1]/x",
        "https://192.168.1.1/x",
        "https://localhost/x",
        "rtsp://web.push.apple.com/x",
        "https://web.push.apple.com/a\"b",
        "https://web.push.apple.com/a b",
        "https://web.push.apple.com/a\\b",
        "https://web.push.apple.com/x#frag",
        "",
    ] {
        assert_eq!(transport::service_of(url), None, "{url}");
    }
    assert_eq!(
        transport::origin_of("https://web.push.apple.com/abc").as_deref(),
        Some("https://web.push.apple.com")
    );
}

#[test]
fn the_curl_config_carries_the_address_and_headers_never_the_arguments() {
    let request = PushRequest {
        endpoint: "https://web.push.apple.com/abc".into(),
        authorization: "vapid t=a.b.c, k=xyz".into(),
        ttl_s: 3600,
        urgency: "high",
        body: vec![1, 2, 3],
    };
    let config = transport::curl_config(&request).unwrap();
    assert!(config.contains("url = \"https://web.push.apple.com/abc\""));
    assert!(config.contains("header = \"Authorization: vapid t=a.b.c, k=xyz\""));
    assert!(config.contains("header = \"TTL: 3600\""));
    assert!(config.contains("header = \"Urgency: high\""));
    assert!(config.contains("Content-Encoding: aes128gcm"));
    let args = transport::curl_args(
        std::path::Path::new("/tmp/corps.bin"),
        Duration::from_secs(15),
    );
    let joined = args.join(" ");
    assert!(!joined.contains("apple"), "l'adresse n'est pas un argument");
    assert!(!joined.contains("vapid"));
    assert!(joined.contains("--proto =https"));
    assert!(joined.contains("--noproxy *"));
    assert!(joined.ends_with("@/tmp/corps.bin -K -"));
}

// ---------------------------------------------------------------------------
// Faux service de push
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Fake {
    sent: Mutex<Vec<PushRequest>>,
    replies: Mutex<VecDeque<Result<u16, bool>>>,
}

impl Fake {
    fn reply(&self, replies: &[Result<u16, bool>]) {
        self.replies.lock().unwrap().extend(replies.iter().copied());
    }
    fn count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
    /// Le dernier message, déchiffré avec la clé de l'appareil du vecteur de test.
    fn last(&self) -> PushPayload {
        let sent = self.sent.lock().unwrap();
        let body = &sent.last().expect("un envoi").body;
        serde_json::from_slice(&decrypt(UA_PRIVATE, &d(AUTH), body).unwrap()).unwrap()
    }
}

impl Transport for Fake {
    fn post(&self, request: &PushRequest) -> Result<PushReply, SendError> {
        self.sent.lock().unwrap().push(request.clone());
        match self.replies.lock().unwrap().pop_front().unwrap_or(Ok(201)) {
            Ok(status) => Ok(PushReply {
                status,
                retry_after: None,
            }),
            Err(retryable) => Err(SendError {
                retryable,
                why: "réseau".into(),
            }),
        }
    }
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mh-push-test-{tag}-{}-{:x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rig(tag: &str) -> (Arc<Push>, Arc<Fake>, PathBuf) {
    let dir = temp(tag);
    let push = Arc::new(Push::new(&dir, false));
    let fake = Arc::new(Fake::default());
    push.set_transport(fake.clone());
    push.set_retry_default(Duration::from_millis(1));
    push.manual();
    (push, fake, dir)
}

fn request(endpoint: &str) -> PushSubscribeRequest {
    PushSubscribeRequest {
        endpoint: endpoint.into(),
        keys: PushKeys {
            p256dh: UA_PUBLIC.into(),
            auth: AUTH.into(),
        },
        label: "iPhone\nde test".into(),
        categories: None,
    }
}

const APPLE: &str = "https://web.push.apple.com/QGuQyavXutnMH";

fn rule_message() -> PushPayload {
    content::payload(&|s| s.to_string(), "Porte", "Ouverte", "/app/".into(), None)
}

#[test]
fn subscriptions_are_stored_0600_listed_updated_and_revoked() {
    let (push, _fake, dir) = rig("store");
    let v = push
        .subscribe(request(APPLE), "Mozilla/5.0 (iPhone)")
        .unwrap();
    assert_eq!(v.label, "iPhone de test");
    assert_eq!(v.service, "Apple");
    assert!(protocol::wire::is_push_id(&v.id));
    assert_eq!(v.categories, PushCategories::default());
    // Même adresse : même abonnement.
    let again = push.subscribe(request(APPLE), "Mozilla/5.0").unwrap();
    assert_eq!(again.id, v.id);
    assert_eq!(push.status().subscriptions.len(), 1);
    assert!(push.status().enabled, "activé par défaut");
    // Le fichier et la clé VAPID : 0600, et l'adresse n'apparaît jamais dans la vue.
    use std::os::unix::fs::PermissionsExt;
    for name in [store::FILE, store::VAPID_FILE] {
        let path = dir.join(crate::persistence::SERVER_DIR).join(name);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
    let json = serde_json::to_string(&push.status()).unwrap();
    assert!(!json.contains("QGuQyavXutnMH"));
    assert!(!json.contains(AUTH));
    // Rechargé depuis le disque : la même clé, le même abonnement.
    let key = push.public_key();
    let reloaded = Push::new(&dir, false);
    assert_eq!(reloaded.public_key(), key);
    assert_eq!(reloaded.status().subscriptions[0].id, v.id);
    // Choix et révocation.
    let updated = push
        .update(
            &v.id,
            PushUpdateRequest {
                categories: Some(PushCategories {
                    detections: false,
                    ..PushCategories::default()
                }),
                label: None,
            },
        )
        .unwrap();
    assert!(!updated.categories.detections);
    assert_eq!(push.remove(&v.id), Ok(()));
    assert_eq!(push.remove(&v.id), Err(PushError::NotFound));
    assert!(Push::new(&dir, false).status().subscriptions.is_empty());
}

#[test]
fn bad_subscriptions_are_refused() {
    let (push, _fake, _dir) = rig("bad");
    assert!(matches!(
        push.subscribe(request("https://192.168.1.10/push"), ""),
        Err(PushError::Invalid(_))
    ));
    let mut r = request(APPLE);
    r.keys.p256dh = "AAAA".into();
    assert!(matches!(push.subscribe(r, ""), Err(PushError::Invalid(_))));
    let mut r = request(APPLE);
    r.keys.auth = b64url(&[1u8; 8]);
    assert!(matches!(push.subscribe(r, ""), Err(PushError::Invalid(_))));
    // Un point hors de la courbe.
    let mut r = request(APPLE);
    let mut bad = d(UA_PUBLIC);
    bad[64] ^= 1;
    r.keys.p256dh = b64url(&bad);
    assert!(matches!(push.subscribe(r, ""), Err(PushError::Invalid(_))));
    // Plafond du nombre d'abonnements.
    for i in 0..protocol::wire::PUSH_MAX_SUBSCRIPTIONS {
        push.subscribe(request(&format!("{APPLE}{i}")), "").unwrap();
    }
    assert_eq!(
        push.subscribe(request(&format!("{APPLE}x")), ""),
        Err(PushError::TooMany)
    );
}

#[test]
fn a_new_vapid_key_forgets_the_subscriptions() {
    let (push, _fake, dir) = rig("vapid");
    push.subscribe(request(APPLE), "").unwrap();
    std::fs::remove_file(
        dir.join(crate::persistence::SERVER_DIR)
            .join(store::VAPID_FILE),
    )
    .unwrap();
    let reloaded = Push::new(&dir, false);
    assert_ne!(reloaded.public_key(), push.public_key());
    assert!(reloaded.status().subscriptions.is_empty());
}

#[test]
fn a_message_is_encrypted_signed_and_sent_to_those_who_want_it() {
    let (push, fake, _dir) = rig("send");
    // Personne d'abonné : rien.
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 0);
    let v = push.subscribe(request(APPLE), "").unwrap();
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 1);
    let sent = fake.sent.lock().unwrap()[0].clone();
    assert_eq!(sent.urgency, "high");
    assert_eq!(sent.ttl_s, 3600);
    assert!(sent.authorization.starts_with("vapid t="));
    let jwt = sent.authorization["vapid t=".len()..]
        .split(',')
        .next()
        .unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&d(jwt.split('.').nth(1).unwrap())).unwrap();
    assert_eq!(claims["aud"], "https://web.push.apple.com");
    assert_eq!(fake.last().title, "Porte");
    assert!(push.status().subscriptions[0].last_sent_at.is_some());
    // Catégorie décochée : rien.
    push.update(
        &v.id,
        PushUpdateRequest {
            categories: Some(PushCategories {
                rules: false,
                ..PushCategories::default()
            }),
            label: None,
        },
    )
    .unwrap();
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 1);
    // Coupé : rien, et l'essai est refusé.
    push.set_enabled(false).unwrap();
    push.notify(Category::Battery, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 1);
    assert_eq!(push.send_test(&v.id), Err(PushError::Disabled));
    push.set_enabled(true).unwrap();
    push.send_test(&v.id).unwrap();
    push.run_pending();
    assert_eq!(fake.count(), 2);
    assert_eq!(fake.last().title, "Notifications activées");
}

#[test]
fn a_gone_subscription_is_forgotten_and_busy_services_get_one_retry() {
    let (push, fake, _dir) = rig("gone");
    push.subscribe(request(APPLE), "").unwrap();
    // 503 puis 201 : un nouvel essai, réussi.
    fake.reply(&[Ok(503), Ok(201)]);
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 2);
    assert!(push.status().subscriptions[0].last_error.is_none());
    // 429 deux fois : abandon après un seul nouvel essai, l'échec est noté.
    fake.reply(&[Ok(429), Ok(429)]);
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 4);
    assert_eq!(
        push.status().subscriptions[0].last_error.as_deref(),
        Some("HTTP 429")
    );
    // 400 : pas de nouvel essai.
    fake.reply(&[Ok(400)]);
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 5);
    // 410 : oublié.
    fake.reply(&[Ok(410)]);
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert_eq!(fake.count(), 6);
    assert!(push.status().subscriptions.is_empty());
    // 404 aussi.
    push.subscribe(request(APPLE), "").unwrap();
    fake.reply(&[Ok(404)]);
    push.notify(Category::Rules, rule_message());
    push.run_pending();
    assert!(push.status().subscriptions.is_empty());
}

fn event(id: &str, description: Option<&str>) -> DetectionEventView {
    DetectionEventView {
        id: id.into(),
        camera_id: 0xFFFF_FFFE_0000_0001,
        camera_name: "Entrée".into(),
        classes: vec![DetectedClass::Personne],
        score: 0.9,
        start: 1_800_000_000.0,
        end: 1_800_000_010.0,
        has_clip: true,
        clip_s: 12.0,
        in_progress: false,
        description: description.map(str::to_string),
        niveau: description.map(|_| 1),
    }
}

#[test]
fn one_message_per_passage_at_its_end_or_with_its_description() {
    let (push, fake, _dir) = rig("det");
    push.subscribe(request(APPLE), "").unwrap();
    let id = |s: &str| s.to_string();
    // Sans description attendue : à la fin, une seule fois.
    push.detection_closed(&event("1800000000-1", None), || false, &id);
    push.detection_closed(&event("1800000000-1", None), || false, &id);
    push.detection_described(&event("1800000000-1", Some("x")), &id);
    push.run_pending();
    assert_eq!(fake.count(), 1);
    let p = fake.last();
    assert_eq!(p.title, "Personne · Entrée");
    assert!(p.body.starts_with("Passage détecté à "));
    assert_eq!(p.url, "/app/#evenement=1800000000-1");
    // Description attendue : rien à la fin, le message part avec elle.
    push.detection_closed(&event("1800000000-2", None), || true, &id);
    push.run_pending();
    assert_eq!(fake.count(), 1);
    push.detection_described(&event("1800000000-2", Some("Un livreur sonne.")), &id);
    push.detection_described(&event("1800000000-2", Some("Un livreur sonne.")), &id);
    push.run_pending();
    assert_eq!(fake.count(), 2);
    assert_eq!(fake.last().body, "Un livreur sonne. (menace : suspect)");
    // Une description arrivée en cours de passage (« dès l'ouverture ») n'envoie rien avant la fin.
    push.detection_described(&event("1800000000-3", Some("tôt")), &id);
    push.run_pending();
    assert_eq!(fake.count(), 2);
}

#[test]
fn low_battery_is_announced_once_until_it_recovers() {
    let (push, fake, dir) = rig("pile");
    push.subscribe(request(APPLE), "").unwrap();
    let now = crate::events::now_unix();
    let battery = |value: f64, at: f64| Event::Measurement {
        node_id: 7,
        kind: MeasureKind::Battery,
        value,
        at,
    };
    // Une vieille mesure (historique rejoué) : rien.
    push.observe(&battery(5.0, now - 3600.0), Some("Capteur salon"), true);
    push.run_pending();
    assert_eq!(fake.count(), 0);
    push.observe(&battery(12.0, now), Some("Capteur salon"), true);
    push.observe(&battery(11.0, now), Some("Capteur salon"), true);
    push.run_pending();
    assert_eq!(fake.count(), 1);
    let p = fake.last();
    assert_eq!(p.title, "Pile faible");
    assert_eq!(
        p.body,
        "Capteur salon : pile à 12 %. Pensez à la remplacer."
    );
    assert_eq!(fake.sent.lock().unwrap()[0].urgency, "normal");
    // Un redémarrage ne réannonce pas.
    let reloaded = Arc::new(Push::new(&dir, false));
    reloaded.set_transport(fake.clone());
    reloaded.manual();
    reloaded.observe(&battery(10.0, now), Some("Capteur salon"), true);
    reloaded.run_pending();
    assert_eq!(fake.count(), 1);
    // 17 % : toujours pas réarmé ; 20 % : réarmé.
    push.observe(&battery(17.0, now), None, true);
    push.observe(&battery(14.0, now), None, true);
    push.run_pending();
    assert_eq!(fake.count(), 1);
    push.observe(&battery(80.0, now), None, true);
    push.observe(&battery(9.0, now), None, true);
    push.run_pending();
    assert_eq!(fake.count(), 2);
    // Sans nom : « Un appareil », jamais l'identifiant.
    assert!(fake.last().body.starts_with("Un appareil : pile à 9 %"));
}

/// Le pourcentage et le drapeau « pile faible » d'un même appareil se contredisent : une seule
/// annonce, même après un redémarrage, et pas d'écriture du fichier à chaque mesure.
#[test]
fn a_battery_percentage_and_a_contradicting_low_flag_never_repeat_the_alert() {
    let (push, fake, dir) = rig("pile-sources");
    push.subscribe(request(APPLE), "").unwrap();
    let now = crate::events::now_unix();
    let percent = |node_id: u64, value: f64| Event::Measurement {
        node_id,
        kind: MeasureKind::Battery,
        value,
        at: now,
    };
    let flag = |node_id: u64, value: bool| Event::State {
        node_id,
        kind: crate::events::StateKind::BatteryLow,
        value,
        at: now,
    };
    // 12 % alors que l'appareil dit « pile correcte » : une annonce, jamais réarmée par le drapeau.
    for _ in 0..3 {
        push.observe(&percent(1, 12.0), Some("Capteur A"), true);
        push.observe(&flag(1, false), Some("Capteur A"), true);
    }
    // 25 % alors que l'appareil dit « pile faible » : le pourcentage fait foi, rien.
    for _ in 0..3 {
        push.observe(&percent(2, 25.0), Some("Capteur B"), true);
        push.observe(&flag(2, true), Some("Capteur B"), true);
    }
    push.run_pending();
    assert_eq!(fake.count(), 1);
    assert!(fake.last().body.starts_with("Capteur A : pile à 12 %"));
    // Le fichier n'est plus réécrit par les mesures qui ne changent rien.
    let file = dir.join(crate::persistence::SERVER_DIR).join(store::FILE);
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    push.observe(&percent(1, 11.0), None, true);
    push.observe(&flag(1, false), None, true);
    push.observe(&percent(2, 30.0), None, true);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    // Après un redémarrage, le drapeau arrive le premier : il ne réarme toujours pas.
    let reloaded = Arc::new(Push::new(&dir, false));
    reloaded.set_transport(fake.clone());
    reloaded.manual();
    reloaded.observe(&flag(1, false), None, true);
    reloaded.observe(&percent(1, 12.0), Some("Capteur A"), true);
    reloaded.observe(&flag(2, true), None, true);
    reloaded.run_pending();
    assert_eq!(fake.count(), 1);
    // Un appareil sans pourcentage : le drapeau annonce et réarme.
    reloaded.observe(&flag(3, true), Some("Capteur C"), true);
    reloaded.observe(&flag(3, true), Some("Capteur C"), true);
    reloaded.observe(&flag(3, false), None, true);
    reloaded.observe(&flag(3, true), Some("Capteur C"), true);
    reloaded.run_pending();
    assert_eq!(fake.count(), 3);
    assert_eq!(
        fake.last().body,
        "Capteur C : pile faible. Pensez à la remplacer."
    );
}

/// Sans destinataire, une pile faible n'est pas retenue comme « annoncée » : elle le sera quand
/// un appareil voudra la recevoir.
#[test]
fn a_low_battery_nobody_receives_is_not_marked_as_announced() {
    let (push, fake, _dir) = rig("pile-personne");
    let now = crate::events::now_unix();
    let low = Event::Measurement {
        node_id: 4,
        kind: MeasureKind::Battery,
        value: 8.0,
        at: now,
    };
    push.observe(&low, Some("Capteur"), true);
    let v = push.subscribe(request(APPLE), "").unwrap();
    push.set_enabled(false).unwrap();
    push.observe(&low, Some("Capteur"), true);
    push.set_enabled(true).unwrap();
    push.update(
        &v.id,
        PushUpdateRequest {
            categories: Some(PushCategories {
                battery: false,
                ..PushCategories::default()
            }),
            label: None,
        },
    )
    .unwrap();
    push.observe(&low, Some("Capteur"), true);
    push.run_pending();
    assert_eq!(fake.count(), 0);
    push.update(
        &v.id,
        PushUpdateRequest {
            categories: Some(PushCategories::default()),
            label: None,
        },
    )
    .unwrap();
    push.observe(&low, Some("Capteur"), true);
    push.run_pending();
    assert_eq!(fake.count(), 1);
}

#[test]
fn the_subscription_address_is_stored_normalised() {
    let (push, fake, _dir) = rig("normalise");
    let v = push
        .subscribe(request("https://WEB.Push.Apple.com/abc"), "")
        .unwrap();
    let again = push
        .subscribe(request("https://web.push.apple.com/abc"), "")
        .unwrap();
    assert_eq!(v.id, again.id);
    // Un nom choisi survit au renvoi de l'abonnement (démarrage de l'app web).
    push.update(
        &v.id,
        PushUpdateRequest {
            categories: None,
            label: Some("iPhone du salon".into()),
        },
    )
    .unwrap();
    let resent = push
        .subscribe(request("https://web.push.apple.com/abc"), "")
        .unwrap();
    assert_eq!(resent.label, "iPhone du salon");
    push.send_test(&v.id).unwrap();
    push.run_pending();
    assert_eq!(
        fake.sent.lock().unwrap()[0].endpoint,
        "https://web.push.apple.com/abc"
    );
}

#[test]
fn an_unreachable_device_is_announced_once_per_outage() {
    let (push, fake, _dir) = rig("panne");
    push.subscribe(request(APPLE), "").unwrap();
    let down = Event::Status {
        node_id: 9,
        status: NodeStatus::Unreachable {
            error: "délai".into(),
            retry_in_s: 30,
        },
    };
    push.observe(&down, Some("Prise du four"), true);
    let start = Instant::now();
    push.check_outages(start, Duration::from_secs(600));
    push.run_pending();
    assert_eq!(fake.count(), 0, "pas avant 10 minutes");
    // « Connexion… » ne remet pas le compteur à zéro.
    push.observe(
        &Event::Status {
            node_id: 9,
            status: NodeStatus::Connecting,
        },
        None,
        true,
    );
    push.observe(&down, None, true);
    push.check_outages(start + Duration::from_secs(700), Duration::from_secs(600));
    push.check_outages(start + Duration::from_secs(900), Duration::from_secs(600));
    push.run_pending();
    assert_eq!(fake.count(), 1);
    let p = fake.last();
    assert_eq!(p.title, "Appareil injoignable");
    assert!(
        p.body.starts_with("Prise du four ne répond plus depuis 1"),
        "{}",
        p.body
    );
    // De retour puis de nouveau en panne : une nouvelle annonce.
    push.observe(
        &Event::Status {
            node_id: 9,
            status: NodeStatus::Online,
        },
        None,
        true,
    );
    push.observe(&down, Some("Prise du four"), true);
    push.check_outages(
        Instant::now() + Duration::from_secs(601),
        Duration::from_secs(600),
    );
    push.run_pending();
    assert_eq!(fake.count(), 2);
}

// ---------------------------------------------------------------------------
// Contenu
// ---------------------------------------------------------------------------

#[test]
fn notification_text_never_carries_addresses_passwords_or_raw_ids() {
    let s = Sanitizer::new([
        "192.168.1.30".to_string(),
        "admin".to_string(),
        "MotDePasse!42".to_string(),
        "x".to_string(),
    ]);
    let cleaned = s.clean(
        "Caméra rtsp://admin:MotDePasse!42@192.168.1.30:554/live/ch00_0 hors ligne,\nnœud 0x1A2B3C4D \
         ou 00000000DEADBEEF, voir http://exemple.fr/a et 10.0.0.7:554. Mot MotDePasse!42 (admin).",
    );
    for leak in [
        "rtsp",
        "192.168",
        "10.0.0.7",
        "MotDePasse",
        "admin",
        "DEADBEEF",
        "1A2B3C4D",
        "exemple.fr",
        "\n",
    ] {
        assert!(!cleaned.contains(leak), "{leak} dans « {cleaned} »");
    }
    assert!(cleaned.contains("<adresse>"));
    assert!(cleaned.contains("<appareil>"));
    // Les mots ordinaires restent (« x » est trop court pour être masqué).
    assert!(cleaned.starts_with("Caméra "));
    assert!(
        s.clean("La porte est ouverte à 14:05, 2 fois.") == "La porte est ouverte à 14:05, 2 fois."
    );
    // Coupé.
    let long = "a".repeat(500);
    let p = content::payload(&|t| s.clean(t), &long, &long, "/app/".into(), None);
    assert_eq!(p.title.chars().count(), content::TITLE_MAX);
    assert_eq!(p.body.chars().count(), content::BODY_MAX);
    assert!(p.body.ends_with('…'));
    let empty = content::payload(&|t| s.clean(t), "", "", "/app/".into(), None);
    assert_eq!(empty.title, "Ostal");
    // Le nom d'un appareil qui n'est qu'un identifiant ne s'affiche pas.
    let b = content::battery(&|t| t.to_string(), Some("00000000DEADBEEF"), None);
    assert_eq!(b.body, "Un appareil : pile faible. Pensez à la remplacer.");
    // Sans tenir compte de la casse, et des mots coupés aussi par `=` et l'espace insécable.
    let other = s.clean("ADMIN Motdepasse!42 hote=10.0.0.9\u{a0}0x1F2E url=rtsp://x/y");
    for leak in ["ADMIN", "Motdepasse", "10.0.0.9", "0x1F2E", "rtsp"] {
        assert!(!other.contains(leak), "{leak} dans « {other} »");
    }
    assert!(other.contains("hote=<adresse>"));
    // L'adresse d'un événement ne garde que des caractères sûrs.
    assert_eq!(content::event_url("17-ab\"<x>"), "/app/#evenement=17-abx");
}
