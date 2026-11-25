//! Tests d'intégration : l'adresse du client derrière un relais (reverse proxy, `tailscale serve`) et
//! l'adresse publique. Le vrai binaire en mode démo, dossier et HOME factices, 127.0.0.1 : aucun
//! réseau réel, aucun nom réel.
//!
//! - un pair de confiance (rien n'est de confiance par défaut : ici la boucle locale, le seul pair
//!   possible, est ajoutée dans `serveur/reseau.json`) qui envoie
//!   `X-Forwarded-For` donne l'adresse du client : « à distance » et limiteur d'essais sur cette
//!   adresse, pas sur celle du relais ;
//! - un pair qui n'est pas de confiance (c'est le cas par défaut) : l'en-tête est ignoré, forgé ou
//!   non ;
//! - un `X-Forwarded-For` à plusieurs valeurs se lit de droite à gauche ;
//! - l'adresse publique : un `Host` égal compte « à distance », le serveur l'annonce à la place des
//!   adresses du tailnet (qui restent annoncées), les raccourcis l'acceptent ;
//! - la ligne de commande `ostal-server network`.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use ostal_server::audit::{Action, AuditLog, Entry, Filter};
use protocol::wire::Via;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const PUBLIC: &str = "https://maison.exemple.fr";
const PUBLIC_HOST: &str = "maison.exemple.fr";
/// Un relais configuré : le pair des essais (la boucle locale) est un proxy de confiance.
const LOOPBACK_RELAY: &str = r#"{"version":1,"trusted_proxies":["127.0.0.1/32"]}"#;

fn seed(dir: &Path, text: &str) {
    let server_dir = dir.join("serveur");
    std::fs::create_dir_all(&server_dir).unwrap();
    std::fs::write(server_dir.join("reseau.json"), text).unwrap();
}

fn login_body(user: &str, password: &str) -> Vec<u8> {
    json!({"username": user, "password": password, "protocol_version": PROTOCOL})
        .to_string()
        .into_bytes()
}

async fn login_with(client: &Client, user: &str, password: &str, headers: &[(&str, &str)]) -> Resp {
    client
        .request_with(
            "POST",
            "/api/login",
            None,
            Some(&login_body(user, password)),
            headers,
        )
        .await
}

fn logins(dir: &Path) -> Vec<Entry> {
    AuditLog::of_data_dir(dir)
        .read(&Filter::default())
        .into_iter()
        .filter(|e| e.action == Action::Login)
        .collect()
}

fn network(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(BIN)
        .arg("network")
        .args(args)
        .arg("--data-dir")
        .arg(dir)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_trusted_proxy_gives_the_real_client_to_the_limiter_and_the_journal() {
    let dir = temp_dir("client-ip-trusted");
    seed(&dir, LOOPBACK_RELAY);
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // Le pair (127.0.0.1) est un proxy de confiance réglé : la limite se compte sur 100.64.0.1.
        let tail = [("X-Forwarded-For", "100.64.0.1")];
        let mut locked = false;
        for i in 0..8 {
            // Un identifiant différent à chaque essai : seul le compteur par adresse peut bloquer.
            let r = login_with(&client, &format!("inconnu{i}"), "faux-mot-de-passe", &tail).await;
            if r.code() == "too_many_attempts" {
                locked = true;
                break;
            }
            assert_eq!(r.code(), "invalid_credentials");
        }
        assert!(locked, "100.64.0.1 devait être mis en attente");
        // Cette adresse-là est bloquée, même avec le bon mot de passe...
        let r = login_with(&client, USER, PASSWORD, &tail).await;
        assert_eq!(r.code(), "too_many_attempts");
        // ...mais pas une autre adresse du tailnet, ni le relais lui-même (sans en-tête).
        let other = [("X-Forwarded-For", "100.64.0.2")];
        let ok = login_with(&client, USER, PASSWORD, &other).await;
        assert_eq!(ok.status, 200, "{}", ok.text());
        let direct = login_with(&client, USER, PASSWORD, &[]).await;
        assert_eq!(direct.status, 200, "{}", direct.text());
    });
    // Le journal dit l'adresse du client et « à distance » ; la connexion sans en-tête, « sur place ».
    let all = logins(&dir);
    let remote = all
        .iter()
        .find(|e| e.ip.as_deref() == Some("100.64.0.2"))
        .expect("connexion du 100.64.0.2");
    assert_eq!(remote.via, Some(Via::Tailscale));
    let local = all
        .iter()
        .find(|e| {
            e.ip.as_deref() == Some("127.0.0.1") && e.result == ostal_server::audit::Outcome::Ok
        })
        .expect("connexion directe");
    assert_eq!(local.via, Some(Via::Local));
}

#[test]
fn an_untrusted_peer_cannot_forge_x_forwarded_for() {
    let dir = temp_dir("client-ip-untrusted");
    // Rien n'est réglé : aucun proxy de confiance par défaut, même la boucle locale n'est pas crue.
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // Changer d'adresse annoncée à chaque essai n'échappe pas au limiteur : c'est 127.0.0.1 qui compte.
        let mut locked = false;
        for i in 0..8 {
            let forged = format!("100.64.1.{i}");
            let r = login_with(
                &client,
                &format!("inconnu{i}"),
                "faux-mot-de-passe",
                &[("X-Forwarded-For", &forged)],
            )
            .await;
            if r.code() == "too_many_attempts" {
                locked = true;
                break;
            }
            assert_eq!(r.code(), "invalid_credentials");
        }
        assert!(
            locked,
            "127.0.0.1 devait être mis en attente malgré les en-têtes forgés"
        );
        // Une adresse forgée « propre » n'ouvre pas non plus la porte.
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.9.9")],
        )
        .await;
        assert_eq!(r.code(), "too_many_attempts");
    });
    // Sans verrou : l'adresse du journal reste celle du pair, sur place.
    let dir2 = temp_dir("client-ip-untrusted-2");
    // Et une liste explicitement vide donne la même chose.
    seed(&dir2, r#"{"trusted_proxies":[]}"#);
    let server2 = Server::start(&dir2);
    let client2 = server2.client();
    runtime().block_on(async {
        let r = login_with(
            &client2,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.9.9")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    });
    let ok = logins(&dir2);
    let entry = ok.last().expect("une connexion");
    assert_eq!(entry.ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(entry.via, Some(Via::Local));
    drop(server);
    drop(server2);
}

#[test]
fn several_forwarded_values_are_read_from_the_right() {
    let dir = temp_dir("client-ip-chain");
    seed(&dir, LOOPBACK_RELAY);
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // Le client a inscrit une fausse adresse du tailnet à gauche ; le relais a ajouté la vraie.
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.0.99, 203.0.113.5")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        // Deux lignes d'en-tête : la dernière est celle du relais.
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[
                ("X-Forwarded-For", "203.0.113.7"),
                ("X-Forwarded-For", "100.64.0.7"),
            ],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        // Une valeur illisible : on garde le pair, jamais une valeur à gauche.
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.0.98, pas-une-adresse")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    });
    let all = logins(&dir);
    let by_ip = |ip: &str| all.iter().find(|e| e.ip.as_deref() == Some(ip));
    assert_eq!(
        by_ip("203.0.113.5").expect("203.0.113.5").via,
        Some(Via::Local)
    );
    assert!(
        by_ip("100.64.0.99").is_none(),
        "la valeur de gauche ne doit jamais être lue"
    );
    assert_eq!(
        by_ip("100.64.0.7").expect("100.64.0.7").via,
        Some(Via::Tailscale)
    );
    assert!(by_ip("203.0.113.7").is_none());
    assert!(by_ip("100.64.0.98").is_none());
    assert!(all.iter().any(|e| e.ip.as_deref() == Some("127.0.0.1")));
}

#[test]
fn the_public_address_counts_as_remote_and_is_announced_to_the_client() {
    let dir = temp_dir("public-address");
    seed(
        &dir,
        &format!(r#"{{"version":1,"public_address":"{PUBLIC}"}}"#),
    );
    // Raccourcis non signés : le test ne dépend ni d'iCloud ni de `/usr/bin/shortcuts` (CI).
    let server = Server::start_with(&dir, &[("OSTAL_TEST_UNSIGNED_SHORTCUTS", "1")]);
    let client = server.client();
    runtime().block_on(async {
        // Le relais garde Host : « à distance », même sans X-Forwarded-For.
        let r = login_with(&client, USER, PASSWORD, &[("Host", PUBLIC_HOST)]).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let body = r.json();
        // L'adresse publique s'ajoute aux adresses du tailnet, annoncées comme sans elle (ici le
        // serveur écoute sur 127.0.0.1 : la liste du tailnet est la même que celle du serveur).
        assert_eq!(body["public_url"], PUBLIC);
        let expected = ostal_server::net::tailnet_urls(server.addr);
        let announced: Vec<String> = body
            .get("tailnet_urls")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        assert_eq!(announced, expected, "{body}");
        let token = body["token"].as_str().unwrap().to_string();
        // Un nom qui ressemble seulement : sur place.
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("Host", "evilmaison.exemple.fr")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        // Un client de la version 3 ne reçoit rien de nouveau.
        let old = client
            .request(
                "POST",
                "/api/login",
                None,
                Some(
                    json!({"username": USER, "password": PASSWORD, "protocol_version": 3})
                        .to_string()
                        .as_bytes(),
                ),
            )
            .await;
        assert_eq!(old.status, 200, "{}", old.text());
        assert!(old.json().get("public_url").is_none());
        // La page apprend l'adresse publique.
        let tokens = client.get("/api/tokens", &token).await.json();
        assert_eq!(tokens["public_origin"], PUBLIC);
        // Les raccourcis acceptent l'adresse publique réglée (Origin et Host de la page)...
        let prepared = client
            .request_with(
                "POST",
                "/api/tokens/mode/shortcuts",
                Some(&token),
                Some(json!({ "origin": PUBLIC }).to_string().as_bytes()),
                &[("Host", PUBLIC_HOST), ("Origin", PUBLIC)],
            )
            .await;
        assert_eq!(prepared.status, 200, "{}", prepared.text());
        // ...pas un autre nom, ni l'adresse publique avec un autre Host.
        for (origin, host) in [
            ("https://evilmaison.exemple.fr", "evilmaison.exemple.fr"),
            ("https://autre.exemple.fr", "autre.exemple.fr"),
            (PUBLIC, "maison.tailnet-1.ts.net"),
        ] {
            let r = client
                .request_with(
                    "POST",
                    "/api/tokens/read/widget",
                    Some(&token),
                    Some(json!({ "origin": origin }).to_string().as_bytes()),
                    &[("Host", host)],
                )
                .await;
            assert_eq!(r.status, 400, "{origin} / {host} : {}", r.text());
        }
    });
    let all = logins(&dir);
    let remote = all
        .iter()
        .find(|e| e.via == Some(Via::Tailscale))
        .expect("une connexion à distance");
    assert_eq!(remote.ip.as_deref(), Some("127.0.0.1"));
    assert!(
        all.iter().any(|e| e.via == Some(Via::Local)),
        "le nom voisin reste sur place"
    );
}

#[test]
fn without_a_public_address_nothing_changes_for_the_client() {
    let dir = temp_dir("no-public-address");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let r = login_with(&client, USER, PASSWORD, &[("Host", PUBLIC_HOST)]).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let body = r.json();
        assert!(body.get("public_url").is_none(), "{body}");
        let token = body["token"].as_str().unwrap();
        let tokens = client.get("/api/tokens", token).await.json();
        assert!(tokens.get("public_origin").is_none(), "{tokens}");
        // Un nom public qui n'est pas réglé n'est pas « à distance ».
    });
    assert!(logins(&dir).iter().all(|e| e.via == Some(Via::Local)));
}

#[test]
fn the_command_line_sets_the_settings_and_a_running_server_follows() {
    let dir = temp_dir("network-cli");
    let server = Server::start(&dir);
    let client = server.client();
    let (ok, text) = network(&dir, &["show"]);
    assert!(ok, "{text}");
    assert!(text.contains("Adresse publique : aucune"), "{text}");
    assert!(text.contains("Proxys de confiance : aucun"), "{text}");
    // Refus : pas https, un chemin, un port, une IP, un réseau mal écrit.
    for bad in [
        "http://maison.exemple.fr",
        "https://maison.exemple.fr/app",
        "https://maison.exemple.fr:8443",
        "https://100.64.0.1",
        "maison.exemple.fr",
    ] {
        let (ok, text) = network(&dir, &["public-address", bad]);
        assert!(!ok, "{bad} : {text}");
    }
    let (ok, text) = network(&dir, &["trusted-proxies", "10.0.0.0/33"]);
    assert!(!ok, "{text}");
    assert!(
        !dir.join("serveur/reseau.json").exists(),
        "aucun fichier après un refus"
    );
    // Le serveur qui tourne suit, sans redémarrage.
    let (ok, text) = network(&dir, &["public-address", "https://Maison.Exemple.Fr/"]);
    assert!(ok, "{text}");
    assert!(text.contains(PUBLIC), "{text}");
    runtime().block_on(async {
        let r = login_with(&client, USER, PASSWORD, &[]).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["public_url"], PUBLIC);
    });
    // Les proxys se règlent à part, sans toucher à l'adresse publique.
    let (ok, text) = network(
        &dir,
        &["trusted-proxies", "127.0.0.1", "10.9.8.0/24", "::1"],
    );
    assert!(ok, "{text}");
    let (_, shown) = network(&dir, &["show"]);
    assert!(
        shown.contains(PUBLIC) && shown.contains("127.0.0.1/32, 10.9.8.0/24, ::1/128"),
        "{shown}"
    );
    // 127.0.0.1 est de confiance : l'en-tête est cru.
    runtime().block_on(async {
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.0.3")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    });
    assert!(logins(&dir)
        .iter()
        .any(|e| e.ip.as_deref() == Some("100.64.0.3")));
    // Retiré de la liste, 127.0.0.1 n'est plus de confiance : l'en-tête forgé n'a plus d'effet.
    let (ok, _) = network(&dir, &["trusted-proxies", "10.9.8.0/24"]);
    assert!(ok);
    runtime().block_on(async {
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.0.5")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    });
    assert!(
        logins(&dir)
            .iter()
            .all(|e| e.ip.as_deref() != Some("100.64.0.5")),
        "un pair qui n'est pas de confiance ne choisit pas son adresse"
    );
    // Les changements sont au journal d'audit, sans adresse ni nom.
    let audited: Vec<_> = AuditLog::of_data_dir(&dir)
        .read(&Filter::default())
        .into_iter()
        .filter(|e| e.action == Action::NetworkSettings)
        .collect();
    assert!(audited.len() >= 3, "{audited:?}");
    assert!(audited.iter().all(|e| e
        .detail
        .as_deref()
        .is_some_and(|d| !d.contains("exemple") && !d.contains("10.9.8"))));
    let (ok, _) = network(&dir, &["trusted-proxies", "--none"]);
    assert!(ok);
    let (ok, _) = network(&dir, &["public-address", "--clear"]);
    assert!(ok);
    runtime().block_on(async {
        let r = login_with(&client, USER, PASSWORD, &[]).await;
        assert!(r.json().get("public_url").is_none());
    });
    // Un fichier illisible n'est jamais écrasé par la ligne de commande.
    std::fs::write(dir.join("serveur/reseau.json"), "{ pas du json").unwrap();
    let (ok, _) = network(&dir, &["public-address", PUBLIC]);
    assert!(!ok);
    assert_eq!(
        std::fs::read_to_string(dir.join("serveur/reseau.json")).unwrap(),
        "{ pas du json"
    );
    // Et le serveur tient : plus aucun proxy de confiance, mais il répond.
    runtime().block_on(async {
        let r = login_with(
            &client,
            USER,
            PASSWORD,
            &[("X-Forwarded-For", "100.64.0.4")],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    });
    let _: Value = json!(null);
}
