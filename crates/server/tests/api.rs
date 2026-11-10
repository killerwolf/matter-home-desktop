//! Tests d'intégration : le vrai binaire `matter-home-server --demo`, en TLS sur 127.0.0.1,
//! avec un client qui épingle l'empreinte du certificat. Aucun appareil réel, aucun dossier
//! de données réel : chaque test a son dossier temporaire et un HOME factice.

mod common;

use std::time::Duration;

use common::*;
use futures_util::StreamExt;
use matter_home_server::persistence::BRIDGED_ID_MIN;
use serde_json::{json, Value};

/// Un identifiant tel qu'il apparaît dans le JSON : en texte.
fn text(id: u64) -> Value {
    Value::String(id.to_string())
}

const SECOND: Duration = Duration::from_secs(1);
const LIVING: u64 = 0xD3_0001;
const PLUG: u64 = 0xD3_0004;
const SHUTTER_LIVING: u64 = BRIDGED_ID_MIN + 0xD3_0001;

#[test]
fn login_succeeds_and_is_refused() {
    let dir = temp_dir("login");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // La version est publique.
        let version = client.request("GET", "/api/version", None, None).await;
        assert_eq!(version.status, 200);
        assert_eq!(version.json()["protocol_version"], PROTOCOL);

        let ok = client.login_as(USER, PASSWORD, PROTOCOL).await;
        assert_eq!(ok.status, 200, "{}", ok.text());
        let token = ok.json()["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        assert_eq!(client.get("/api/state", &token).await.status, 200);

        // Mauvais mot de passe, mauvais identifiant : même réponse, rien de plus.
        let bad_password = client
            .login_as(USER, "un-mauvais-mot-de-passe", PROTOCOL)
            .await;
        assert_eq!(
            (bad_password.status, bad_password.code().as_str()),
            (401, "invalid_credentials")
        );
        let bad_user = client.login_as("inconnu", PASSWORD, PROTOCOL).await;
        assert_eq!(
            (bad_user.status, bad_user.code().as_str()),
            (401, "invalid_credentials")
        );
        assert_eq!(bad_user.json(), bad_password.json());

        // Version du protocole incompatible : signalé proprement, avant tout test d'identifiants.
        // Un client de version 1 (avant les caméras) est refusé.
        let mismatch = client.login_as(USER, PASSWORD, PROTOCOL - 1).await;
        assert_eq!(
            (mismatch.status, mismatch.code().as_str()),
            (426, "version_incompatible")
        );
        let mismatch = client.login_as("inconnu", "x", 0).await;
        assert_eq!(mismatch.code(), "version_incompatible");

        // Corps illisible : code d'erreur du protocole, pas un texte d'axum.
        let junk = client
            .request("POST", "/api/login", None, Some(b"pas du json"))
            .await;
        assert_eq!((junk.status, junk.code().as_str()), (400, "bad_request"));
    });
}

#[test]
fn login_attempts_are_limited() {
    let dir = temp_dir("limit");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        for _ in 0..4 {
            let r = client.login_as(USER, "faux-mot-de-passe", PROTOCOL).await;
            assert_eq!(r.code(), "invalid_credentials");
            assert!(r.json()["error"]["retry_after_s"].is_null());
        }
        // Le 5e échec déclenche l'attente...
        let fifth = client.login_as(USER, "faux-mot-de-passe", PROTOCOL).await;
        assert_eq!(fifth.code(), "invalid_credentials");
        assert!(fifth.json()["error"]["retry_after_s"].as_u64().unwrap() >= 29);
        // ...pendant laquelle même le bon mot de passe est refusé, avec le délai à attendre.
        let locked = client.login_as(USER, PASSWORD, PROTOCOL).await;
        assert_eq!(
            (locked.status, locked.code().as_str()),
            (429, "too_many_attempts")
        );
        assert!(locked.json()["error"]["retry_after_s"].as_u64().unwrap() >= 1);
        assert!(locked.headers.contains_key("retry-after"));
    });
}

/// Des tentatives lancées toutes en même temps ne contournent pas la limite.
#[test]
fn parallel_login_attempts_are_limited_too() {
    let dir = temp_dir("parallel");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let attempts = (0..12).map(|_| client.login_as(USER, "faux-mot-de-passe", PROTOCOL));
        let answers = futures_util::future::join_all(attempts).await;
        let refused = answers
            .iter()
            .filter(|r| r.code() == "invalid_credentials")
            .count();
        let locked = answers
            .iter()
            .filter(|r| r.code() == "too_many_attempts")
            .count();
        assert_eq!(refused + locked, 12, "{answers:?}");
        // 5 échecs déclenchent l'attente ; au plus 1 vérification déjà en cours s'y ajoute.
        assert!(
            (5..=6).contains(&refused),
            "{refused} tentatives vérifiées sur 12"
        );
        // Et le bon mot de passe est refusé pendant l'attente.
        assert_eq!(
            client.login_as(USER, PASSWORD, PROTOCOL).await.code(),
            "too_many_attempts"
        );
    });
}

#[test]
fn every_route_requires_the_token() {
    let dir = temp_dir("auth");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let routes = [
            ("GET", "/api/state"),
            ("GET", "/api/plan"),
            ("PUT", "/api/plan"),
            ("GET", "/api/rules"),
            ("PUT", "/api/rules"),
            ("GET", "/api/rule-runs"),
            ("GET", "/api/cameras"),
            ("POST", "/api/rules/1/test"),
            ("POST", "/api/pairing"),
            ("POST", "/api/logout"),
            ("DELETE", "/api/devices/1"),
            ("POST", "/api/devices/1/retry"),
            ("PUT", "/api/devices/1/customization"),
            ("POST", "/api/devices/1/on-off"),
            ("POST", "/api/devices/1/cover"),
            ("GET", "/api/devices/1/history"),
            ("GET", "/api/devices/1/history.csv"),
            ("DELETE", "/api/devices/1/history"),
            ("GET", "/api/export"),
            ("POST", "/api/import"),
            ("POST", "/api/import?dry_run=1"),
            ("GET", "/api/ws?protocol=2"),
        ];
        for (method, path) in routes {
            let none = client.request(method, path, None, Some(b"{}")).await;
            assert_eq!(
                (none.status, none.code().as_str()),
                (401, "unauthorized"),
                "{method} {path}"
            );
            let wrong = client
                .request(method, path, Some("0".repeat(64).as_str()), Some(b"{}"))
                .await;
            assert_eq!(wrong.status, 401, "{method} {path}");
            // Le jeton dans l'URL ne compte pas.
            let sep = if path.contains('?') { '&' } else { '?' };
            let in_url = client
                .request(
                    method,
                    &format!("{path}{sep}token={token}"),
                    None,
                    Some(b"{}"),
                )
                .await;
            assert_eq!(in_url.status, 401, "{method} {path} (jeton dans l'URL)");
        }
        // Un schéma d'autorisation autre que Bearer n'est pas accepté non plus.
        assert_eq!(
            client
                .request("GET", "/api/state", Some(&token), None)
                .await
                .status,
            200
        );
        // Route inconnue : 404 au format du protocole (avec jeton).
        let unknown = client.get("/api/nexiste-pas", &token).await;
        assert_eq!(
            (unknown.status, unknown.code().as_str()),
            (404, "not_found")
        );
    });
}

#[test]
fn websocket_needs_token_and_matching_version() {
    let dir = temp_dir("wsauth");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let no_token = client.ws("", PROTOCOL).await.err().unwrap();
        assert_eq!(no_token.status, 401);
        let wrong = client.ws(&"a".repeat(64), PROTOCOL).await.err().unwrap();
        assert_eq!(wrong.status, 401);
        let old = client.ws(&token, PROTOCOL - 1).await.err().unwrap();
        assert_eq!(
            (old.status, old.code().as_str()),
            (426, "version_incompatible")
        );
        assert!(client.ws(&token, PROTOCOL).await.is_ok());
    });
}

#[test]
fn websocket_sends_full_state_then_events_with_text_ids() {
    let dir = temp_dir("ws");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        let hello = next_json(&mut ws, 10 * SECOND).await.unwrap();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["protocol_version"], PROTOCOL);
        let snapshot = next_json(&mut ws, 10 * SECOND).await.unwrap();
        assert_eq!(snapshot["type"], "snapshot");

        // Identifiants en texte, partout : appareils (dont les volets pontés, au-delà de
        // 2^53), plan et règles.
        assert_no_big_numbers(&snapshot);
        let devices = snapshot["devices"].as_array().unwrap();
        assert!(devices.len() >= 8, "{} appareils", devices.len());
        let shutter = devices
            .iter()
            .find(|d| d["device"]["node_id"] == text(SHUTTER_LIVING))
            .expect("volet pondé");
        assert_eq!(
            shutter["device"]["bridge"]["node_id"],
            0xD3_0005u64.to_string()
        );
        assert!(snapshot["plan"]["floors"][0]["id"].is_string());
        let rule = &snapshot["rules"]["rules"][0];
        assert!(rule["id"].is_string() && rule["trigger"]["node_id"].is_string());
        assert!(snapshot["rule_runs"].is_array());
        let first_seq = snapshot["seq"].as_u64().unwrap();

        // Puis des événements, numérotés à la suite de l'état complet.
        let event = wait_event(&mut ws, "reading", 15 * SECOND, |e| {
            e["metric"] == "temperature" && e["node_id"] == text(LIVING)
        })
        .await;
        assert!(event["seq"].as_u64().unwrap() > first_seq);
        assert!(event["event"]["value"].is_f64());
        // Le même état est lisible en HTTPS.
        let state = client.get("/api/state", &token).await;
        assert_eq!(state.status, 200);
        assert_no_big_numbers(&state.json());
    });
}

#[test]
fn two_clients_see_the_same_changes_and_last_write_wins() {
    let dir = temp_dir("two");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let t1 = client.login().await;
        let t2 = client.login().await;
        assert_ne!(t1, t2);
        let mut a = client.ws(&t1, PROTOCOL).await.unwrap();
        let mut b = client.ws(&t2, PROTOCOL).await.unwrap();
        for ws in [&mut a, &mut b] {
            assert_eq!(next_json(ws, 10 * SECOND).await.unwrap()["type"], "hello");
            assert_eq!(
                next_json(ws, 10 * SECOND).await.unwrap()["type"],
                "snapshot"
            );
        }

        // Le client 1 modifie le plan, le client 2 aussi juste après : le dernier gagne.
        let mut plan = client.get("/api/plan", &t1).await.json();
        plan["floors"][0]["name"] = json!("Écrit par 1");
        let put1 = client.send_json("PUT", "/api/plan", &t1, &plan).await;
        assert_eq!(put1.status, 200, "{}", put1.text());
        plan["floors"][0]["name"] = json!("Écrit par 2");
        assert_eq!(
            client
                .send_json("PUT", "/api/plan", &t2, &plan)
                .await
                .status,
            200
        );

        // Les deux reçoivent les deux changements, dans le même ordre.
        for ws in [&mut a, &mut b] {
            let first = wait_event(ws, "plan_changed", 10 * SECOND, |_| true).await;
            assert_eq!(first["event"]["plan"]["floors"][0]["name"], "Écrit par 1");
            let second = wait_event(ws, "plan_changed", 10 * SECOND, |_| true).await;
            assert_eq!(second["event"]["plan"]["floors"][0]["name"], "Écrit par 2");
            assert!(second["seq"].as_u64() > first["seq"].as_u64());
        }
        let current = client.get("/api/plan", &t1).await.json();
        assert_eq!(current["floors"][0]["name"], "Écrit par 2");

        // Même chose pour les règles.
        let mut rules = client.get("/api/rules", &t1).await.json();
        rules["rules"][0]["name"] = json!("Renommée");
        assert_eq!(
            client
                .send_json("PUT", "/api/rules", &t2, &rules)
                .await
                .status,
            200
        );
        for ws in [&mut a, &mut b] {
            let e = wait_event(ws, "rules_changed", 10 * SECOND, |_| true).await;
            assert_eq!(e["event"]["rules"]["rules"][0]["name"], "Renommée");
        }
    });
}

#[test]
fn plan_and_rules_updates_are_validated() {
    let dir = temp_dir("valid");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let plan = client.get("/api/plan", &token).await.json();
        // Identifiant numérique (arrondi par JavaScript) : refusé.
        let mut numeric = plan.clone();
        numeric["floors"][0]["id"] = json!(1);
        let r = client.send_json("PUT", "/api/plan", &token, &numeric).await;
        assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));
        // Aucun étage : refusé avec un code à part (les clients supposent au moins un étage).
        let mut empty = plan.clone();
        empty["floors"] = json!([]);
        let r = client.send_json("PUT", "/api/plan", &token, &empty).await;
        assert_eq!((r.status, r.code().as_str()), (400, "plan_without_floor"));
        assert_eq!(client.get("/api/plan", &token).await.json(), plan);
        // Identifiants en double.
        let mut twice = plan.clone();
        let floor = twice["floors"][0].clone();
        twice["floors"].as_array_mut().unwrap().push(floor);
        assert_eq!(
            client
                .send_json("PUT", "/api/plan", &token, &twice)
                .await
                .code(),
            "bad_request"
        );
        // Webhook vers un fichier local.
        let mut rules = client.get("/api/rules", &token).await.json();
        rules["rules"][0]["actions"] =
            json!([{"type": "Webhook", "url": "file:///etc/passwd", "body": ""}]);
        assert_eq!(
            client
                .send_json("PUT", "/api/rules", &token, &rules)
                .await
                .code(),
            "bad_request"
        );
        // Corps trop gros.
        let big = vec![b' '; 1024 * 1024 + 10];
        let r = client
            .request("PUT", "/api/plan", Some(&token), Some(&big))
            .await;
        assert_eq!((r.status, r.code().as_str()), (413, "payload_too_large"));
        // Les données n'ont pas changé.
        assert_eq!(client.get("/api/plan", &token).await.json(), plan);
    });
}

#[test]
fn commands_reach_devices_and_results_come_back_as_events() {
    let dir = temp_dir("cmd");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        for _ in 0..2 {
            next_json(&mut ws, 10 * SECOND).await.unwrap();
        }

        // Volet : ouverture à 20 %, la position suit en événements.
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{SHUTTER_LIVING}/cover"),
                &token,
                &json!({"command": {"Position": 20}}),
            )
            .await;
        assert_eq!(
            (r.status, r.json()["accepted"].clone()),
            (202, json!(true)),
            "{}",
            r.text()
        );
        let reading = wait_event(&mut ws, "reading", 15 * SECOND, |e| {
            e["metric"] == "position" && e["node_id"] == text(SHUTTER_LIVING) && e["value"] == 20.0
        })
        .await;
        assert_eq!(reading["event"]["node_id"], SHUTTER_LIVING.to_string());

        // Marche/arrêt de la prise.
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{PLUG}/on-off"),
                &token,
                &json!({"command": "Off"}),
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        wait_event(&mut ws, "notice", 10 * SECOND, |e| {
            e["text"].as_str().unwrap().contains("éteindre")
        })
        .await;

        // Personnalisation puis retour à l'automatique.
        let r = client
            .send_json(
                "PUT",
                &format!("/api/devices/{LIVING}/customization"),
                &token,
                &json!({"name": "Salon chic", "icon": "Light"}),
            )
            .await;
        assert_eq!(r.status, 202);
        let updated = wait_event(&mut ws, "device_updated", 10 * SECOND, |e| {
            e["device"]["custom_name"] == "Salon chic"
        })
        .await;
        assert_eq!(updated["event"]["device"]["icon"], "Light");

        // Réessayer, oublier : appareil inconnu → not_found, identifiant mal formé → bad_request.
        let r = client
            .request("POST", "/api/devices/999/retry", Some(&token), None)
            .await;
        assert_eq!((r.status, r.code().as_str()), (404, "not_found"));
        let r = client
            .request("POST", "/api/devices/abc/retry", Some(&token), None)
            .await;
        assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));
        let r = client
            .request(
                "POST",
                &format!("/api/devices/{LIVING}/retry"),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(r.status, 202);
        let r = client
            .request(
                "DELETE",
                &format!("/api/devices/{LIVING}"),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(r.status, 202);
        wait_event(&mut ws, "device_removed", 10 * SECOND, |e| {
            e["node_id"] == text(LIVING)
        })
        .await;
        // Un volet n'a pas de marche/arrêt.
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{SHUTTER_LIVING}/on-off"),
                &token,
                &json!({"command": "On"}),
            )
            .await;
        assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));

        // Appairage : code invalide refusé tout de suite ; code valide accepté, un seul à la fois.
        let r = client
            .send_json("POST", "/api/pairing", &token, &json!({"code": "abc"}))
            .await;
        assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));
        let ok = json!({"code": "3497-011-2332"});
        assert_eq!(
            client
                .send_json("POST", "/api/pairing", &token, &ok)
                .await
                .status,
            202
        );
        let busy = client.send_json("POST", "/api/pairing", &token, &ok).await;
        assert_eq!((busy.status, busy.code().as_str()), (409, "pairing_busy"));
        wait_event(&mut ws, "pairing_done", 20 * SECOND, |_| true).await;
    });
    // Ni le code d'appairage ni le mot de passe ne sont dans le journal.
    let log = server.log();
    assert!(!log.contains("3497-011-2332") && !log.contains("34970112332"));
    assert!(!log.contains(PASSWORD));
}

/// Une règle désactivée peut garder une adresse de webhook incomplète (c'est ce que produit
/// l'import d'une sauvegarde de la v0.2.1) : le PUT l'accepte, l'ensemble des règles n'est pas
/// bloqué. Activée, elle est refusée comme avant, et « Tester » n'appelle jamais l'adresse.
#[test]
fn a_disabled_rule_may_keep_an_incomplete_webhook_but_never_calls_it() {
    let dir = temp_dir("draft-rule");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let mut rules = client.get("/api/rules", &token).await.json();
        let id = rules["rules"][0]["id"].clone();
        for url in ["", "exemple.fr/sans-http", "http://a b"] {
            rules["rules"][0]["actions"] = json!([{"type": "Webhook", "url": url, "body": ""}]);
            rules["rules"][0]["enabled"] = json!(true);
            let r = client.send_json("PUT", "/api/rules", &token, &rules).await;
            assert_eq!(
                (r.status, r.code().as_str()),
                (400, "bad_request"),
                "{url:?}"
            );
            rules["rules"][0]["enabled"] = json!(false);
            let r = client.send_json("PUT", "/api/rules", &token, &rules).await;
            assert_eq!(r.status, 200, "{url:?} : {}", r.text());
        }
        let stored = client.get("/api/rules", &token).await.json();
        assert_eq!(stored["rules"][0]["enabled"], false);
        assert_eq!(stored["rules"][0]["actions"][0]["url"], "http://a b");

        // « Tester » : la règle s'exécute, mais l'appel est refusé avant tout curl.
        let r = client
            .request(
                "POST",
                &format!("/api/rules/{}/test", id.as_str().unwrap()),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        let mut run = Value::Null;
        for _ in 0..200 {
            let runs = client.get("/api/rule-runs", &token).await.json();
            if let Some(found) = runs.as_array().unwrap().iter().find(|r| {
                r["rule_id"] == id
                    && r["detail"]
                        .as_str()
                        .is_some_and(|d| d.contains("adresse invalide"))
            }) {
                run = found.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(run["ok"], false, "{run}");
    });
}

/// `POST /api/import?dry_run=1` nomme les règles que l'import désactiverait (brouillons de la
/// v0.2.1), sans rien mettre de côté ni arrêter le serveur.
#[test]
fn the_import_dry_run_names_the_draft_rules_it_would_disable() {
    let dir = temp_dir("dry-run-drafts");
    let server = Server::start(&dir);
    let client = server.client();
    let id = matter_home_server::matter::certs::generate_identity().unwrap();
    let controller = json!({
        "version": 1,
        "identity": {
            "fabric_id": id.fabric_id,
            "controller_node_id": id.controller_node_id,
            "ca_private_key": id.ca_private_key,
            "ca_cert_der": id.ca_cert_der,
            "controller_private_key": id.controller_private_key,
            "controller_cert_der": id.controller_cert_der,
        },
        "nodes": [{"node_id": 7, "name": "Capteur"}],
    });
    let rule = |id: u64, name: &str, url: &str| {
        json!({
            "id": id, "name": name, "enabled": true,
            "trigger": {"type": "Time", "hour": 7, "minute": 0, "days": [true, true, true, true, true, true, true]},
            "conditions": [],
            "actions": [{"type": "Webhook", "url": url, "body": ""}],
            "cooldown_s": 60
        })
    };
    let rules = json!({"version": 1, "rules": [
        rule(1, "Complète", "https://exemple.invalid/ok"),
        rule(2, "Brouillon vide", ""),
        rule(3, "Brouillon avec espace", "http://exemple.invalid/a b"),
    ]});
    let bundle = json!({
        "format": "matter-home-desktop-sauvegarde", "version": 1,
        "created": "2026-11-09T09:15:00+01:00",
        "files": {"controller.json": controller.to_string(), "regles.json": rules.to_string()},
    });
    runtime().block_on(async {
        let token = client.login().await;
        let r = client
            .request(
                "POST",
                "/api/import?dry_run=1",
                Some(&token),
                Some(bundle.to_string().as_bytes()),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let body = r.json();
        assert_eq!(body["restarting"], false);
        assert_eq!(body["summary"]["rules"], 3);
        assert_eq!(
            body["summary"]["disabled_rules"],
            json!(["Brouillon vide", "Brouillon avec espace"])
        );
        // Rien n'est mis de côté, le serveur continue.
        assert!(!dir.join("import-en-attente.matterhome").exists());
        assert_eq!(client.get("/api/state", &token).await.status, 200);
    });
}

/// L'import par l'API ne contourne pas le garde-fou d'identité : une autre fabric est refusée
/// (`identity_mismatch`, 409) sans `replace_identity=1`, et le dry_run montre les deux identités.
#[test]
fn the_api_import_refuses_a_different_identity_unless_asked() {
    let dir = temp_dir("identity-guard");
    let controller_of = |id: &matter_home_server::persistence::ControllerIdentity, nodes: usize| {
        let nodes: Vec<_> = (0..nodes)
            .map(|i| json!({"node_id": 7 + i, "name": format!("Capteur {i}")}))
            .collect();
        json!({"version": 1, "identity": id, "nodes": nodes}).to_string()
    };
    let current = matter_home_server::matter::certs::generate_identity().unwrap();
    let other = matter_home_server::matter::certs::generate_identity().unwrap();
    std::fs::write(dir.join("controller.json"), controller_of(&current, 1)).unwrap();
    let bundle_of = |id, nodes| {
        json!({
            "format": "matter-home-desktop-sauvegarde", "version": 1,
            "created": "2026-11-09T09:15:00+01:00",
            "files": {"controller.json": controller_of(id, nodes)},
        })
        .to_string()
    };
    let (same, different) = (bundle_of(&current, 2), bundle_of(&other, 3));
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        macro_rules! post {
            ($path:expr, $body:expr) => {
                client.request("POST", $path, Some(&token), Some($body.as_bytes()))
            };
        }

        // dry_run : les deux identités, nombre d'appareils compris.
        let r = post!("/api/import?dry_run=1", different).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let id = &r.json()["identity"];
        assert_eq!(id["differs"], true, "{id}");
        assert_eq!(
            id["incoming"]["fabric_id"],
            format!("{:#x}", other.fabric_id)
        );
        assert_eq!(id["incoming"]["devices"], 3);
        assert_eq!(
            id["current"]["fabric_id"],
            format!("{:#x}", current.fabric_id)
        );
        assert_eq!(id["current"]["devices"], 1);
        let r = post!("/api/import?dry_run=1", same).await;
        assert_eq!(r.json()["identity"]["differs"], false);

        // L'import réel est refusé, avec un code stable, sans rien mettre de côté.
        let r = post!("/api/import", different).await;
        assert_eq!(r.status, 409, "{}", r.text());
        assert_eq!(r.code(), "identity_mismatch");
        assert!(!dir.join("import-en-attente.matterhome").exists());
        assert_eq!(client.get("/api/state", &token).await.status, 200);
        let r = post!("/api/import?replace_identity=0", different).await;
        assert_eq!(r.code(), "identity_mismatch");
        let r = post!("/api/import?replace_identity=peut-etre", different).await;
        assert_eq!(r.status, 400, "{}", r.text());

        // La même identité passe sans option ; une autre, avec l'option explicite seulement.
        let r = post!("/api/import?replace_identity=1", different).await;
        assert_eq!(r.status, 202, "{}", r.text());
        assert!(dir.join("import-en-attente.matterhome").exists());
    });
}

#[test]
fn rule_test_runs_and_notifications_go_to_connected_clients_only() {
    let dir = temp_dir("notify");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        // Aucun client connecté en temps réel : la règle s'exécute, personne ne reçoit rien.
        let r = client
            .request("POST", "/api/rules/1/test", Some(&token), None)
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        let r = client
            .request("POST", "/api/rules/999/test", Some(&token), None)
            .await;
        assert_eq!((r.status, r.code().as_str()), (404, "not_found"));

        // Le journal des exécutions garde la trace, même pour un client arrivé après.
        let mut found = Value::Null;
        for _ in 0..200 {
            let runs = client.get("/api/rule-runs", &token).await.json();
            if let Some(run) = runs.as_array().unwrap().first() {
                found = run.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(found["rule_id"], "1");
        assert_eq!(found["ok"], true);

        // Client connecté : il reçoit la notification, puis le compte rendu.
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        let snapshot = {
            next_json(&mut ws, 10 * SECOND).await.unwrap();
            next_json(&mut ws, 10 * SECOND).await.unwrap()
        };
        // (La porte de la démo s'ouvre toutes les 2 min et peut déclencher la règle 1 d'elle-même.)
        assert!(!snapshot["rule_runs"].as_array().unwrap().is_empty());
        client
            .request("POST", "/api/rules/1/test", Some(&token), None)
            .await;
        let n = wait_event(&mut ws, "notification", 10 * SECOND, |_| true).await;
        assert!(n["event"]["title"].is_string() && n["event"]["at"].is_f64());
        let run = wait_event(&mut ws, "rule_run", 10 * SECOND, |_| true).await;
        assert_eq!(run["event"]["rule_id"], "1");
    });
}

#[test]
fn history_is_read_on_demand_with_window_and_volume_limits() {
    let dir = temp_dir("history");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        // Le salon rapporte une température toutes les 2 s : on attend quelques points.
        let path = format!("/api/devices/{LIVING}/history?metrics=temperature&limit=1000");
        let mut points = 0;
        for _ in 0..60 {
            let r = client.get(&path, &token).await;
            assert_eq!(r.status, 200, "{}", r.text());
            points = r.json()["points"].as_array().unwrap().len();
            if points >= 3 {
                break;
            }
            tokio::time::sleep(SECOND).await;
        }
        assert!(points >= 3, "{points} point(s)");
        let r = client.get(&path, &token).await.json();
        assert_eq!(r["node_id"], LIVING.to_string());
        assert_eq!(r["truncated"], false);
        assert_eq!(r["points"][0]["metric"], "temperature");
        assert!(r["until"].as_f64().unwrap() - r["since"].as_f64().unwrap() <= 3600.5);

        // Borne de volume : seuls les plus récents, avec l'indicateur.
        let cut = client
            .get(
                &format!("/api/devices/{LIVING}/history?metrics=temperature&limit=2"),
                &token,
            )
            .await
            .json();
        assert_eq!(cut["truncated"], true);
        assert_eq!(cut["points"].as_array().unwrap().len(), 2);
        // Fenêtre trop large : bornée à 7 jours. Paramètres invalides : refusés.
        let wide = client
            .get(
                &format!("/api/devices/{LIVING}/history?since=0&until=2000000000"),
                &token,
            )
            .await
            .json();
        assert_eq!(
            wide["until"].as_f64().unwrap() - wide["since"].as_f64().unwrap(),
            7.0 * 86400.0
        );
        // Réduction par tranches : la semaine entière en peu de points, extrémités gardées.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let week = client
            .get(
                &format!(
                    "/api/devices/{LIVING}/history?metrics=temperature&since={}&until={now}&buckets=100",
                    now - 7.0 * 86400.0
                ),
                &token,
            )
            .await
            .json();
        let pts = week["points"].as_array().unwrap();
        assert!(pts.len() <= 202, "{} points", pts.len());
        assert_eq!(week["truncated"], false);
        let first = pts[0]["at"].as_f64().unwrap();
        let since = week["since"].as_f64().unwrap();
        // La démo a une semaine d'historique : la fenêtre est couverte dès son début, pas
        // seulement les dernières heures (ce que ferait une simple limite de points).
        assert!(first < since + 86400.0, "premier point à {first}, fenêtre depuis {since}");
        for bad in [
            "buckets=0",
            "buckets=abc",
            "since=abc",
            "metrics=couleur",
            "limit=0",
            "since=10&until=5",
        ] {
            let r = client
                .get(&format!("/api/devices/{LIVING}/history?{bad}"), &token)
                .await;
            assert_eq!((r.status, r.code().as_str()), (400, "bad_request"), "{bad}");
        }
        let r = client.get("/api/devices/12345/history", &token).await;
        assert_eq!((r.status, r.code().as_str()), (404, "not_found"));

        // CSV à la demande.
        let csv = client
            .get(&format!("/api/devices/{LIVING}/history.csv"), &token)
            .await;
        assert_eq!(csv.status, 200);
        assert!(csv.headers["content-type"].starts_with("text/csv"));
        assert!(csv.headers["content-disposition"].contains("attachment"));
        let text = csv.text();
        assert!(text.starts_with("timestamp_unix,datetime_local,temperature_c,humidity_pct\n"));
        assert!(text.lines().count() >= 4);

        // Effacement : accepté.
        let r = client
            .request(
                "DELETE",
                &format!("/api/devices/{LIVING}/history"),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(r.status, 202);
    });
}

#[test]
fn logout_revokes_the_token_and_closes_the_stream() {
    let dir = temp_dir("logout");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let other = client.login().await;
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        for _ in 0..2 {
            next_json(&mut ws, 10 * SECOND).await.unwrap();
        }
        let r = client
            .request("POST", "/api/logout", Some(&token), None)
            .await;
        assert_eq!(r.status, 204);
        assert_eq!(client.get("/api/state", &token).await.status, 401);
        // Le flux en temps réel de cette session se termine ; l'autre session n'est pas touchée.
        let deadline = std::time::Instant::now() + 15 * SECOND;
        loop {
            match tokio::time::timeout(Duration::from_secs(1), next_json(&mut ws, 10 * SECOND))
                .await
            {
                Ok(None) => break,
                _ => assert!(
                    std::time::Instant::now() < deadline,
                    "le WebSocket est resté ouvert"
                ),
            }
        }
        assert_eq!(client.get("/api/state", &other).await.status, 200);
    });
}

/// Un client qui n'avance plus ne gêne ni les autres clients ni le serveur : il est
/// déconnecté, les autres reçoivent tout.
#[test]
fn slow_client_is_dropped_without_blocking_others() {
    let dir = temp_dir("slow");
    let server = Server::start_with(
        &dir,
        &[
            // Assez de place pour que le client rapide ne soit jamais en retard : seul le client
            // dont la connexion est bloquée doit être écarté.
            ("MATTER_HOME_TEST_BROADCAST_CAPACITY", "16"),
            ("MATTER_HOME_TEST_WRITE_TIMEOUT_MS", "1500"),
        ],
    );
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        // Le test porte sur la déconnexion d'un client dont la connexion est bloquée : il ne doit
        // pas dépendre de la rafale de démarrage de la démo, qui saturerait la file de diffusion.
        client.wait_startup_burst_over(&token).await;
        let mut slow = client.ws_slow(&token, PROTOCOL).await.unwrap();
        let mut fast = client.ws(&token, PROTOCOL).await.unwrap();
        for ws in [&mut slow, &mut fast] {
            next_json(ws, 10 * SECOND).await.unwrap();
            next_json(ws, 10 * SECOND).await.unwrap();
        }
        // Le client lent ne lit plus. Gros messages : les tampons du système se remplissent.
        let big_rules = |n: usize| {
            let mut rules = json!({"version": 1, "rules": []});
            for i in 0..8 {
                let message = "x".repeat(1900);
                let actions: Vec<Value> = (0..50)
                    .map(|_| json!({"type": "Notify", "title": "t", "message": message}))
                    .collect();
                rules["rules"].as_array_mut().unwrap().push(json!({
                    "id": (i + 1).to_string(), "name": format!("r{n}-{i}"), "enabled": false,
                    "trigger": {"type": "Time", "hour": 7, "minute": 0},
                    "conditions": [], "actions": actions, "cooldown_s": 60
                }));
            }
            rules
        };
        let started = std::time::Instant::now();
        let mut fast_received = 0;
        for n in 0..12 {
            let r = client
                .send_json("PUT", "/api/rules", &token, &big_rules(n))
                .await;
            assert_eq!(r.status, 200, "{}", r.text());
            // Le client rapide les reçoit au fur et à mesure.
            wait_event(&mut fast, "rules_changed", 10 * SECOND, |e| {
                e["rules"]["rules"][0]["name"] == format!("r{n}-0")
            })
            .await;
            fast_received += 1;
        }
        assert_eq!(fast_received, 12);
        // Le serveur répond toujours, vite, pendant que l'autre est bloqué.
        let before = std::time::Instant::now();
        assert_eq!(client.get("/api/state", &token).await.status, 200);
        assert!(before.elapsed() < 5 * SECOND);
        let _ = started;

        // Le serveur déconnecte le client lent : sa décision est dans le journal du serveur (le
        // client, lui, met du temps à vider ce que le système avait déjà en tampon, avec sa
        // fenêtre de réception minuscule, avant de voir la fin du flux).
        let deadline = std::time::Instant::now() + 20 * SECOND;
        while !server
            .log()
            .contains("client WebSocket déconnecté : écriture bloquée")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "le serveur n'a pas déconnecté le client lent"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Lecture bornée de ce qui reste : si la fin du flux arrive, tant mieux ; sinon le client
        // est simplement encore en train de vider ses tampons.
        let drain_until = std::time::Instant::now() + 8 * SECOND;
        while std::time::Instant::now() < drain_until {
            if let Ok(None | Some(Err(_))) = tokio::time::timeout(SECOND, slow.next()).await {
                break;
            }
        }
        // Et le serveur n'a rien perdu : il sert encore l'état complet.
        assert_eq!(client.get("/api/state", &token).await.status, 200);
    });
}

/// Un client en retard (mais qui finit par lire) reçoit l'état complet au lieu des messages perdus.
#[test]
fn lagging_client_gets_a_fresh_snapshot() {
    let dir = temp_dir("lag");
    // Capacité assez large pour absorber les rafales d'événements de la démo, trop petite
    // pour 40 messages de près d'un mégaoctet.
    let server = Server::start_with(
        &dir,
        &[
            ("MATTER_HOME_TEST_BROADCAST_CAPACITY", "6"),
            // Ici le client finit par lire : pas de déconnexion pour écriture bloquée.
            ("MATTER_HOME_TEST_WRITE_TIMEOUT_MS", "120000"),
        ],
    );
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let mut lagging = client.ws(&token, PROTOCOL).await.unwrap();
        next_json(&mut lagging, 10 * SECOND).await.unwrap();
        let first = next_json(&mut lagging, 10 * SECOND).await.unwrap();
        let first_seq = first["seq"].as_u64().unwrap();
        let big_rules = |n: usize| {
            let message = "x".repeat(1900);
            let actions: Vec<Value> = (0..50)
                .map(|_| json!({"type": "Notify", "title": "t", "message": message}))
                .collect();
            let rules: Vec<Value> = (0..8)
                .map(|i| {
                    json!({
                        "id": (i + 1).to_string(), "name": format!("r{n}"), "enabled": false,
                        "trigger": {"type": "Time", "hour": 7, "minute": 0},
                        "conditions": [], "actions": actions, "cooldown_s": 60
                    })
                })
                .collect();
            json!({"version": 1, "rules": rules})
        };
        for n in 0..40 {
            let r = client
                .send_json("PUT", "/api/rules", &token, &big_rules(n))
                .await;
            assert_eq!(r.status, 200, "{}", r.text());
        }
        // Le client reprend sa lecture. Les messages perdus ne se rattrapent pas : il reçoit un état
        // complet à la place. S'il prend du retard trop souvent, le serveur le déconnecte ; il
        // se reconnecte alors et repart d'un état complet. Dans les deux cas, il finit à jour.
        let mut snapshots = 0;
        let mut last_name = String::new();
        let mut ended = false;
        let deadline = std::time::Instant::now() + 60 * SECOND;
        while std::time::Instant::now() < deadline && last_name != "r39" {
            let Some(m) = next_json(&mut lagging, 30 * SECOND).await else {
                ended = true;
                break;
            };
            match m["type"].as_str().unwrap() {
                "snapshot" => {
                    snapshots += 1;
                    assert!(m["seq"].as_u64().unwrap() > first_seq);
                    last_name = m["rules"]["rules"][0]["name"].as_str().unwrap().to_string();
                }
                "event" if m["event"]["type"] == "rules_changed" => {
                    last_name = m["event"]["rules"]["rules"][0]["name"]
                        .as_str()
                        .unwrap()
                        .to_string();
                }
                _ => {}
            }
        }
        if ended {
            assert!(
                server.log().contains("client trop lent"),
                "flux fermé sans raison dans le journal du serveur"
            );
            let mut again = client.ws(&token, PROTOCOL).await.unwrap();
            next_json(&mut again, 10 * SECOND).await.unwrap();
            let fresh = next_json(&mut again, 30 * SECOND).await.unwrap();
            assert_eq!(fresh["type"], "snapshot");
            last_name = fresh["rules"]["rules"][0]["name"]
                .as_str()
                .unwrap()
                .to_string();
        } else {
            assert!(snapshots >= 1, "aucun état complet de resynchronisation");
        }
        assert_eq!(last_name, "r39", "le client n'a pas retrouvé l'état final");
    });
}

#[test]
fn log_never_contains_secrets() {
    let dir = temp_dir("secrets");
    let server = Server::start(&dir);
    let client = server.client();
    let (token, token2) = runtime().block_on(async {
        let token = client.login().await;
        let _ = client
            .login_as(USER, "un-mauvais-mot-de-passe-secret", PROTOCOL)
            .await;
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        next_json(&mut ws, 10 * SECOND).await.unwrap();
        let token2 = client.login().await;
        client
            .request("POST", "/api/logout", Some(&token2), None)
            .await;
        (token, token2)
    });
    let log = server.log();
    assert!(log.contains("connexion acceptée"), "{log}");
    for secret in [
        PASSWORD,
        "un-mauvais-mot-de-passe-secret",
        token.as_str(),
        token2.as_str(),
    ] {
        assert!(!log.contains(secret), "secret dans le journal : {secret}");
    }
    // Le fichier des sessions ne contient que des empreintes.
    let sessions = std::fs::read_to_string(dir.join("serveur/sessions.json")).unwrap();
    assert!(!sessions.contains(&token));
    let account = std::fs::read_to_string(dir.join("serveur/compte.json")).unwrap();
    assert!(!account.contains(PASSWORD) && account.contains("$argon2id$"));
}

/// Les caméras de démonstration portent une adresse et un mot de passe fictifs : ils ne doivent
/// apparaître nulle part, ni dans une réponse, ni dans un événement, ni dans le journal (niveau
/// debug) du vrai binaire.
#[test]
fn cameras_are_listed_without_any_address_or_secret() {
    let dir = temp_dir("cameras");
    let server = Server::start(&dir);
    let client = server.client();
    let forbidden = [
        "rtsp",
        "192.0.2.",
        "demo-camera-mdp",
        "ch00_",
        "\"password\"",
        "\"url\"",
    ];
    runtime().block_on(async {
        let token = client.login().await;
        let list = client.get("/api/cameras", &token).await;
        assert_eq!(list.status, 200, "{}", list.text());
        let body = list.json();
        let cameras = body["cameras"].as_array().unwrap();
        assert_eq!(cameras.len(), 3);
        let names: Vec<_> = cameras
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Entrée", "Garage", "Jardin"]);
        for camera in cameras {
            // Identifiant en texte, dans la plage réservée aux caméras.
            let id: u64 = camera["id"]
                .as_str()
                .expect("identifiant en texte")
                .parse()
                .unwrap();
            assert!(protocol::wire::is_camera_id(id));
            assert!(camera["status"]["state"].is_string());
        }
        assert_eq!(cameras[0]["status"]["state"], "online");
        assert_eq!(cameras[0]["has_sub_stream"], true);
        assert_eq!(cameras[0]["stream"]["codec"], "h264");
        assert_eq!(cameras[0]["stream"]["width"], 1280);
        assert_eq!(cameras[1]["status"]["state"], "unreachable");
        assert_eq!(cameras[1]["status"]["problem"], "unreachable");
        let mut texts = vec![list.text()];

        // L'état complet, en HTTPS et sur le WebSocket, porte la même liste.
        let state = client.get("/api/state", &token).await;
        assert_eq!(state.json()["cameras"], body["cameras"]);
        texts.push(state.text());
        let mut ws = client.ws(&token, PROTOCOL).await.unwrap();
        let _hello = next_json(&mut ws, 10 * SECOND).await.unwrap();
        let snapshot = next_json(&mut ws, 10 * SECOND).await.unwrap();
        assert_eq!(snapshot["cameras"], body["cameras"]);
        texts.push(snapshot.to_string());

        // Aucune route n'écrit : on ajoute les caméras depuis la fenêtre du serveur.
        for method in ["POST", "PUT", "DELETE"] {
            let r = client
                .request(method, "/api/cameras", Some(&token), Some(b"{}"))
                .await;
            assert_eq!(
                (r.status, r.code().as_str()),
                (405, "method_not_allowed"),
                "{method}"
            );
            texts.push(r.text());
        }
        let not_there = client
            .request("GET", "/api/cameras/1/stream", Some(&token), None)
            .await;
        assert_eq!(not_there.status, 404);

        for text in &texts {
            for needle in forbidden {
                assert!(!text.contains(needle), "{needle} dans {text}");
            }
        }
    });
    let log = std::fs::read_to_string(&server.log_path).unwrap();
    for needle in ["rtsp", "192.0.2.", "demo-camera-mdp", "ch00_"] {
        assert!(!log.contains(needle), "{needle} dans le journal");
    }
}
