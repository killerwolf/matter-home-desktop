//! Tests d'intégration : connexions (un téléphone ou un ordinateur où un compte est ouvert).
//! Le login annonce l'installation, `GET`/`DELETE /api/me/installations`, anciens clients tolérés
//! (version 3 du protocole, sans installation : « connexion inconnue »).

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const SAM_PASSWORD: &str = "le-mot-de-passe-de-sam";

fn add_user(dir: &std::path::Path, username: &str, password: &str) {
    let mut child = Command::new(BIN)
        .args(["user", "add", "--username", username, "--password-stdin"])
        .arg("--data-dir")
        .arg(dir)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{password}").unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
}

fn installation(id: &str, name: &str) -> Value {
    json!({"id": id, "kind": "web", "device": "iphone", "name": name})
}

async fn login_with(
    client: &Client,
    user: &str,
    password: &str,
    installation: Option<Value>,
) -> Resp {
    let mut body = json!({"username": user, "password": password, "protocol_version": PROTOCOL});
    if let Some(i) = installation {
        body["installation"] = i;
    }
    client
        .request(
            "POST",
            "/api/login",
            None,
            Some(body.to_string().as_bytes()),
        )
        .await
}

fn token_of(resp: &Resp) -> String {
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["token"].as_str().unwrap().to_string()
}

async fn mine(client: &Client, token: &str) -> Vec<Value> {
    let resp = client.get("/api/me/installations", token).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["connections"].as_array().unwrap().clone()
}

#[test]
fn connections_are_listed_per_person_and_cut_only_by_their_owner() {
    let dir = temp_dir("conn-own");
    let server = Server::start(&dir);
    add_user(&dir, "sam", SAM_PASSWORD);
    let client = server.client();

    runtime().block_on(async {
        let phone = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("iphone-0001", "iPhone de Hamdi")),
            )
            .await,
        );
        let ipad = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("ipad-0002", "iPad du salon")),
            )
            .await,
        );
        let sam = token_of(
            &login_with(
                &client,
                "sam",
                SAM_PASSWORD,
                Some(installation("sam-phone-03", "iPhone de Sam")),
            )
            .await,
        );

        let list = mine(&client, &phone).await;
        assert_eq!(list.len(), 2, "{list:?}");
        let current: Vec<&Value> = list.iter().filter(|c| c["current"] == true).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0]["name"], "iPhone de Hamdi");
        assert_eq!(current[0]["via"], "local");
        assert_eq!(current[0]["unknown"], false);
        // Les connexions de Sam ne sont jamais dans la liste de Hamdi.
        assert!(list.iter().all(|c| c["name"] != "iPhone de Sam"));
        assert_eq!(mine(&client, &sam).await.len(), 1);

        // Hamdi ne peut pas couper la connexion de Sam : elle n'existe pas pour lui.
        let resp = client
            .request(
                "DELETE",
                "/api/me/installations/sam-phone-03",
                Some(&phone),
                None,
            )
            .await;
        assert_eq!(resp.status, 404);
        assert_eq!(client.get("/api/state", &sam).await.status, 200);

        // Il coupe son iPad : son jeton cesse de marcher tout de suite, l'iPhone reste.
        let resp = client
            .request(
                "DELETE",
                "/api/me/installations/ipad-0002",
                Some(&phone),
                None,
            )
            .await;
        assert_eq!(resp.status, 204, "{}", resp.text());
        assert_eq!(client.get("/api/state", &ipad).await.status, 401);
        assert_eq!(client.get("/api/state", &phone).await.status, 200);
        assert_eq!(mine(&client, &phone).await.len(), 1);
        let resp = client
            .request(
                "DELETE",
                "/api/me/installations/ipad-0002",
                Some(&phone),
                None,
            )
            .await;
        assert_eq!(resp.status, 404, "déjà coupée");
    });
}

#[test]
fn cutting_a_connection_closes_its_open_stream() {
    let dir = temp_dir("conn-ws");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let a = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("mac-aaaa-01", "Mac du salon")),
            )
            .await,
        );
        let b = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("web-bbbb-02", "iPad")),
            )
            .await,
        );
        let mut ws = client.ws(&a, PROTOCOL).await.unwrap();
        let hello = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
        assert_eq!(hello["type"], "hello");
        let resp = client
            .request(
                "DELETE",
                "/api/me/installations/mac-aaaa-01",
                Some(&b),
                None,
            )
            .await;
        assert_eq!(resp.status, 204);
        // Le flux de la connexion coupée se ferme (le contrôle de session tourne toutes les
        // quelques secondes).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match next_json(&mut ws, Duration::from_secs(2)).await {
                Some(_) if std::time::Instant::now() < deadline => continue,
                Some(_) => panic!("le flux de la connexion coupée est resté ouvert"),
                None => break,
            }
        }
    });
}

#[test]
fn signing_in_again_from_the_same_installation_replaces_its_session() {
    let dir = temp_dir("conn-again");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let first = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("same-0001", "Mac de Hamdi")),
            )
            .await,
        );
        let second = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("same-0001", "Mac de Hamdi (2)")),
            )
            .await,
        );
        assert_eq!(client.get("/api/state", &first).await.status, 401);
        let list = mine(&client, &second).await;
        assert_eq!(list.len(), 1, "une connexion, une session : {list:?}");
        assert_eq!(list[0]["name"], "Mac de Hamdi (2)");
    });
}

#[test]
fn old_clients_are_tolerated_as_an_unknown_connection_in_their_own_version() {
    let dir = temp_dir("conn-old");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // GET /api/version sans rien : la version 3, celle que compare un client 2.0.0.
        let v = client
            .request("GET", "/api/version", None, None)
            .await
            .json();
        assert_eq!(v["protocol_version"], 3);
        let v = client
            .request("GET", "/api/version?protocol=4", None, None)
            .await
            .json();
        assert_eq!(v["protocol_version"], 4);
        // Une version qu'on ne parle pas reçoit la nôtre : le client dira laquelle est en cause.
        let v = client
            .request("GET", "/api/version?protocol=9", None, None)
            .await
            .json();
        assert_eq!(v["protocol_version"], 4);

        // Login d'un client 2.0.0 : version 3, aucune installation.
        let body = json!({"username": USER, "password": PASSWORD, "protocol_version": 3});
        let resp = client
            .request(
                "POST",
                "/api/login",
                None,
                Some(body.to_string().as_bytes()),
            )
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        assert_eq!(resp.json()["protocol_version"], 3);
        assert!(
            resp.json().get("tailnet_urls").is_none(),
            "un client 3 ne reçoit rien de nouveau"
        );
        let old = resp.json()["token"].as_str().unwrap().to_string();
        // Son WebSocket est salué en version 3.
        let mut ws = client.ws(&old, 3).await.unwrap();
        let hello = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
        assert_eq!(hello["protocol_version"], 3);
        drop(ws);
        let list = mine(&client, &old).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["unknown"], true);
        assert_eq!(list[0]["name"], "Connexion inconnue");
        assert!(list[0]["id"].as_str().unwrap().starts_with("inconnue-"));

        // Un client 4 est salué en version 4.
        let new = client.login().await;
        let mut ws = client.ws(&new, 4).await.unwrap();
        let hello = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
        assert_eq!(hello["protocol_version"], 4);
        drop(ws);

        // Les versions que nous ne parlons pas restent refusées, au login comme au WebSocket.
        for bad in [2u32, 5] {
            let body = json!({"username": USER, "password": PASSWORD, "protocol_version": bad});
            let resp = client
                .request(
                    "POST",
                    "/api/login",
                    None,
                    Some(body.to_string().as_bytes()),
                )
                .await;
            assert_eq!(resp.status, 426);
            assert_eq!(resp.code(), "version_incompatible");
            assert!(client.ws(&new, bad).await.is_err());
        }
        // Un identifiant d'installation illisible ne bloque pas la connexion : « inconnue ».
        let resp = login_with(
            &client,
            USER,
            PASSWORD,
            Some(installation("x", "trop court")),
        )
        .await;
        let t = token_of(&resp);
        assert!(mine(&client, &t)
            .await
            .iter()
            .any(|c| c["current"] == true && c["unknown"] == true));
    });
}

#[test]
fn the_last_access_says_whether_the_request_came_by_tailscale() {
    let dir = temp_dir("conn-via");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let t = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("away-00001", "Mac de Sam")),
            )
            .await,
        );
        let resp = client
            .request_with(
                "GET",
                "/api/me/installations",
                Some(&t),
                None,
                &[("Host", "mac-mini.exemple.ts.net")],
            )
            .await;
        assert_eq!(resp.json()["connections"][0]["via"], "tailscale");
        let list = mine(&client, &t).await;
        assert_eq!(list[0]["via"], "local");
    });
}
