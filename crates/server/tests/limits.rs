//! Tests d'intégration des limites du serveur (entrées hostiles, connexions, délais). Ils
//! vivent dans leur propre exécutable : cargo les lance après `api`, pas en même temps, ce qui
//! évite de charger la machine pendant les tests de clients lents de `api`, sensibles au temps.
//! Même principe que `api` : le vrai binaire en `--demo`, sur 127.0.0.1, dossier temporaire.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;

/// Un code d'appairage avec un caractère UTF-8 multi-octets ne fait pas tomber le serveur.
#[test]
fn pairing_code_with_multibyte_characters_is_refused_without_a_panic() {
    let dir = temp_dir("pairutf8");
    let mut server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        for code in [
            "aaé123",
            "é",
            "MT:é",
            "mt:日本語日本語",
            "😀😀😀😀",
            "34970112332é",
        ] {
            let r = client
                .send_json("POST", "/api/pairing", &token, &json!({ "code": code }))
                .await;
            assert_eq!(
                (r.status, r.code().as_str()),
                (400, "bad_request"),
                "{code}"
            );
        }
        assert_eq!(client.get("/api/state", &token).await.status, 200);
    });
    assert!(
        server.wait_exit(Duration::from_millis(200)).is_none(),
        "le serveur tourne encore"
    );
    assert!(!server.log().contains("panicked"), "{}", server.log());
}

/// 405 et 413 sont au format du protocole (`{"error": {"code": …}}`), pas du texte ou du vide.
#[test]
fn method_and_size_errors_use_the_protocol_format() {
    let dir = temp_dir("errors");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        for (method, path) in [
            ("PUT", "/api/state"),
            ("DELETE", "/api/plan"),
            ("GET", "/api/login"),
        ] {
            let r = client
                .request(method, path, Some(&token), Some(b"{}"))
                .await;
            assert_eq!(
                (r.status, r.code().as_str()),
                (405, "method_not_allowed"),
                "{method} {path}"
            );
            assert!(r.headers.contains_key("allow"), "{method} {path}");
            // Les en-têtes communs sont aussi sur ces réponses refabriquées.
            assert_eq!(r.headers["cache-control"], "no-store", "{method} {path}");
            assert_eq!(
                r.headers["x-content-type-options"], "nosniff",
                "{method} {path}"
            );
        }
        // Un corps de plus de 1 Mo sur une route JSON ordinaire.
        let big = vec![b' '; 1024 * 1024 + 1];
        let r = client
            .request("PUT", "/api/plan", Some(&token), Some(&big))
            .await;
        assert_eq!((r.status, r.code().as_str()), (413, "payload_too_large"));
        assert_eq!(r.headers["cache-control"], "no-store");
        // Le serveur répond encore.
        assert_eq!(client.get("/api/state", &token).await.status, 200);
    });
}

/// Un échec de connexion coûte un petit délai : le débit d'une attaque est limité.
#[test]
fn failed_login_is_slightly_delayed() {
    let dir = temp_dir("pause");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let start = std::time::Instant::now();
        let bad = client
            .login_as(USER, "un-mauvais-mot-de-passe", PROTOCOL)
            .await;
        assert_eq!(bad.code(), "invalid_credentials");
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "{:?}",
            start.elapsed()
        );
        let start = std::time::Instant::now();
        client.login().await;
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "{:?}",
            start.elapsed()
        );
    });
}

/// Un client qui n'envoie plus le corps de sa requête est abandonné (`request_timeout`), un
/// client lent mais qui avance ne l'est pas.
#[test]
fn stalled_request_body_times_out_but_slow_progress_does_not() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = temp_dir("bodyidle");
    let server = Server::start_with(&dir, &[("MATTER_HOME_TEST_BODY_IDLE_MS", "700")]);
    let client = server.client();
    runtime().block_on(async {
        // Corps annoncé de 100 octets, 10 envoyés, puis plus rien.
        let mut stream = client.connect().await;
        stream
            .write_all(
                b"POST /api/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"username",
            )
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let started = std::time::Instant::now();
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw)).await;
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(text.starts_with("HTTP/1.1 408"), "{text}");
        assert!(text.contains("\"request_timeout\""), "{text}");
        assert!(started.elapsed() < Duration::from_secs(5));

        // Corps envoyé en trois morceaux espacés de 450 ms (au total plus que le délai) : accepté.
        let body = json!({"username": USER, "password": PASSWORD, "protocol_version": PROTOCOL}).to_string();
        let (a, rest) = body.split_at(10);
        let (b, c) = rest.split_at(20);
        let mut stream = client.connect().await;
        let head = format!(
            "POST /api/login HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        for part in [a, b, c] {
            stream.write_all(part.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(450)).await;
        }
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    });
}

/// 16 connexions simultanées au plus par adresse : la 17e est fermée, une place rendue est
/// réutilisable, et les autres requêtes de l'adresse passent de nouveau.
#[test]
fn connections_per_address_are_capped() {
    use tokio::io::AsyncReadExt;
    let dir = temp_dir("perip");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(tokio::net::TcpStream::connect(server.addr).await.unwrap());
        }
        // Laisse au serveur le temps d'accepter les 16 premières.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut seventeenth = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        let mut byte = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(5), seventeenth.read(&mut byte))
            .await
            .expect("la 17e connexion doit être fermée aussitôt");
        assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
        // Les 16 premières sont toujours là ; en libérer une rend une place.
        drop(held.pop());
        let mut login = None;
        for _ in 0..30 {
            let tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
            let mut probe = tcp;
            let alive =
                tokio::time::timeout(Duration::from_millis(300), probe.read(&mut byte)).await;
            if alive.is_err() {
                // Pas fermée par le serveur : place obtenue.
                login = Some(());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(login.is_some(), "une place rendue doit être réutilisable");
        drop(held);
        // Tout relâché : une requête complète passe.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(client.login_as(USER, PASSWORD, PROTOCOL).await.status, 200);
    });
}
