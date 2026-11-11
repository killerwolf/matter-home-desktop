//! L'app web mobile (ticket n° 15) : le service des fichiers intégrés, les en-têtes de sécurité,
//! l'accès sans jeton à la seule coquille, et le jeton d'un navigateur pour le WebSocket
//! (sous-protocole). Vrai binaire en mode démo, sur 127.0.0.1.

mod common;

use common::*;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Error as WsError;

const ASSETS: &[(&str, &str)] = &[
    ("/app/", "text/html"),
    ("/app/index.html", "text/html"),
    ("/app/app.css", "text/css"),
    ("/app/tokens.css", "text/css"),
    ("/app/app.js", "text/javascript"),
    ("/app/api.js", "text/javascript"),
    ("/app/tiles.js", "text/javascript"),
    ("/app/icons.js", "text/javascript"),
    ("/app/charts.js", "text/javascript"),
    ("/app/temps.js", "text/javascript"),
    ("/app/cameras.js", "text/javascript"),
    ("/app/sw.js", "text/javascript"),
    ("/app/manifest.webmanifest", "application/manifest+json"),
    ("/app/icon-192.png", "image/png"),
    ("/app/icon-512.png", "image/png"),
];

#[test]
fn shell_is_served_without_a_token_with_strict_headers() {
    let dir = temp_dir("webapp-shell");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        for (path, content_type) in ASSETS {
            let r = client.request("GET", path, None, None).await;
            assert_eq!(r.status, 200, "{path}");
            assert!(
                r.headers["content-type"].starts_with(content_type),
                "{path} : {}",
                r.headers["content-type"]
            );
            assert!(!r.body.is_empty(), "{path}");
            let csp = &r.headers["content-security-policy"];
            assert!(csp.contains("default-src 'self'"), "{path} : {csp}");
            assert!(csp.contains("script-src 'self'") && !csp.contains("unsafe-inline"));
            assert_eq!(r.headers["x-content-type-options"], "nosniff", "{path}");
            assert_eq!(r.headers["x-frame-options"], "DENY");
            // Pas de CORS : l'app est de la même origine que l'API.
            assert!(
                !r.headers.keys().any(|k| k.starts_with("access-control-")),
                "{path} : en-tête CORS"
            );
        }
        // `/app` redirige vers `/app/` (les chemins relatifs de la coquille en dépendent).
        let r = client.request("GET", "/app", None, None).await;
        assert_eq!(r.status, 308);
        assert_eq!(r.headers["location"], "/app/");
        // Le service worker ne contrôle que /app/.
        let sw = client.request("GET", "/app/sw.js", None, None).await;
        assert_eq!(sw.headers["service-worker-allowed"], "/app/");
    });
}

#[test]
fn served_design_tokens_are_the_design_system_file() {
    let dir = temp_dir("webapp-tokens");
    let server = Server::start(&dir);
    let client = server.client();
    let on_disk = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../design/tokens.css"
    ))
    .unwrap();
    runtime().block_on(async {
        let r = client.request("GET", "/app/tokens.css", None, None).await;
        assert_eq!(r.body, on_disk);
    });
}

#[test]
fn unknown_and_traversal_paths_are_json_404_and_never_files() {
    let dir = temp_dir("webapp-404");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        for path in [
            "/app/nope.js",
            "/app/..%2fCargo.toml",
            "/app/%2e%2e/%2e%2e/etc/passwd",
            "/app/css/../app.js",
            "/app/index.html/",
        ] {
            let r = client.request("GET", path, None, None).await;
            assert_ne!(r.status, 200, "{path}");
            assert!(r.status == 404 || r.status == 400, "{path} : {}", r.status);
        }
        // Un POST sur la coquille n'existe pas.
        let r = client.request("POST", "/app/", None, Some(b"{}")).await;
        assert_eq!(r.status, 405);
        assert_eq!(r.code(), "method_not_allowed");
    });
}

#[test]
fn api_still_needs_a_token_and_the_shell_carries_no_house_data() {
    let dir = temp_dir("webapp-auth");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        for (method, path) in [
            ("GET", "/api/state"),
            ("GET", "/api/cameras"),
            ("GET", "/api/plan"),
            ("GET", "/api/devices/1/history"),
            ("POST", "/api/devices/1/on-off"),
            ("POST", "/api/devices/1/cover"),
            ("GET", "/api/ws?protocol=2"),
            ("GET", "/api/cameras/18446744073709551615/stream"),
        ] {
            let r = client.request(method, path, None, Some(b"{}")).await;
            assert_eq!(r.status, 401, "{method} {path}");
            assert_eq!(r.code(), "unauthorized");
        }
        // Un jeton bidon ne passe pas non plus.
        let bad = client.get("/api/state", &"0".repeat(64)).await;
        assert_eq!(bad.status, 401);
        // La coquille ne contient aucun nom d'appareil ni aucun jeton.
        let token = client.login().await;
        let state = client.get("/api/state", &token).await.text();
        assert!(state.contains("Salon"));
        for (path, _) in ASSETS {
            let body = client.request("GET", path, None, None).await.text();
            assert!(!body.contains("Salon"), "{path}");
            assert!(!body.contains(&token), "{path}");
        }
    });
}

async fn ws_with_protocols(
    client: &Client,
    path: &str,
    protocols: &str,
    authorization: Option<&str>,
    origin: Option<&str>,
) -> Result<(String, serde_json::Value), u16> {
    let tls = client.connect().await;
    let mut request = format!("wss://localhost:{}{path}", client.addr.port())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", protocols.parse().unwrap());
    if let Some(o) = origin {
        request.headers_mut().insert("Origin", o.parse().unwrap());
    }
    if let Some(a) = authorization {
        request
            .headers_mut()
            .insert("Authorization", a.parse().unwrap());
    }
    match tokio_tungstenite::client_async(request, tls).await {
        Ok((mut ws, response)) => {
            let chosen = response
                .headers()
                .get("sec-websocket-protocol")
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default();
            let hello = next_json(&mut ws, std::time::Duration::from_secs(5))
                .await
                .unwrap_or_default();
            Ok((chosen, hello))
        }
        Err(WsError::Http(response)) => Err(response.status().as_u16()),
        Err(WsError::Protocol(_)) => Err(0),
        Err(e) => panic!("WebSocket : {e}"),
    }
}

#[test]
fn browser_websocket_authenticates_with_a_subprotocol_never_the_url() {
    let dir = temp_dir("webapp-ws");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        // Navigateur : sous-protocole, pas d'en-tête Authorization. Le serveur répond avec
        // `matter-home.v1`, jamais avec l'entrée qui porte le jeton.
        let (chosen, hello) = ws_with_protocols(
            &client,
            "/api/ws?protocol=2",
            &format!("matter-home.v1, bearer.{token}"),
            None,
            None,
        )
        .await
        .expect("connexion acceptée");
        assert_eq!(chosen, "matter-home.v1");
        assert!(!chosen.contains(&token));
        assert_eq!(hello["type"], "hello");

        // Un mauvais jeton : refusé.
        let refused = ws_with_protocols(
            &client,
            "/api/ws?protocol=2",
            &format!("matter-home.v1, bearer.{}", "1".repeat(64)),
            None,
            None,
        )
        .await;
        assert_eq!(refused.unwrap_err(), 401);

        // Le client natif (en-tête Authorization, sans sous-protocole) n'est pas touché.
        let native = client.ws(&token, PROTOCOL).await;
        assert!(native.is_ok());

        // Le sous-protocole ne vaut que pour un WebSocket : une requête ordinaire qui le
        // porterait reste refusée.
        let mut stream = client.connect().await;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let head = format!(
            "GET /api/state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nSec-WebSocket-Protocol: bearer.{token}\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 401"));
    });
}

/// Une requête brute (HTTP/1.1, connexion fermée) avec des en-têtes à la carte.
async fn raw_get(client: &Client, path: &str, extra: &[(&str, &str)]) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = client.connect().await;
    let mut head = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
    (status, text)
}

#[test]
fn subprotocol_token_is_only_for_the_two_websocket_routes_with_a_real_upgrade() {
    let dir = temp_dir("webapp-subproto");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let bearer = format!("bearer.{token}");
        // Un en-tête d'upgrade complet, mais sur une route qui n'est pas un WebSocket : 401.
        for path in ["/api/state", "/api/export", "/api/plan", "/api/cameras"] {
            let (status, _) = raw_get(
                &client,
                path,
                &[
                    ("Upgrade", "websocket"),
                    ("Connection", "Upgrade"),
                    ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
                    ("Sec-WebSocket-Version", "13"),
                    ("Sec-WebSocket-Protocol", &bearer),
                ],
            )
            .await;
            assert_eq!(status, 401, "{path}");
        }
        // Sur la bonne route mais sans upgrade complet (ni clé, ni version, ni Connection) : 401.
        for extra in [
            vec![("Upgrade", "websocket")],
            vec![("Upgrade", "websocket"), ("Connection", "Upgrade")],
            vec![
                ("Upgrade", "websocket"),
                ("Connection", "Upgrade"),
                ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
                ("Sec-WebSocket-Version", "8"),
            ],
        ] {
            let mut headers = extra.clone();
            headers.push(("Sec-WebSocket-Protocol", &bearer));
            let (status, _) = raw_get(&client, "/api/ws?protocol=2", &headers).await;
            assert_eq!(status, 401, "{extra:?}");
        }
    });
}

#[test]
fn websocket_refuses_a_foreign_origin_but_not_the_native_client() {
    let dir = temp_dir("webapp-origin");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let protocols = format!("matter-home.v1, bearer.{token}");
        let same = format!("https://localhost:{}", client.addr.port());
        // Même origine que l'hôte : accepté.
        assert!(
            ws_with_protocols(&client, "/api/ws?protocol=2", &protocols, None, Some(&same))
                .await
                .is_ok()
        );
        // Autre site : 403, même avec un jeton valide.
        for origin in ["https://evil.example", "http://localhost", "null"] {
            let refused = ws_with_protocols(
                &client,
                "/api/ws?protocol=2",
                &protocols,
                None,
                Some(origin),
            )
            .await;
            assert_eq!(refused.unwrap_err(), 403, "{origin}");
        }
        // Idem avec l'en-tête Authorization (un navigateur ne l'envoie pas, mais l'origine
        // compte quand même).
        let bearer = format!("Bearer {token}");
        let refused = ws_with_protocols(
            &client,
            "/api/ws?protocol=2",
            "matter-home.v1",
            Some(&bearer),
            Some("https://evil.example"),
        )
        .await;
        assert_eq!(refused.unwrap_err(), 403);
        // Le client natif n'envoie pas Origin : inchangé.
        assert!(client.ws(&token, PROTOCOL).await.is_ok());
    });
}

#[test]
fn api_responses_carry_a_locked_down_csp() {
    let dir = temp_dir("webapp-api-csp");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let r = client.request("GET", "/api/version", None, None).await;
        assert_eq!(
            r.headers["content-security-policy"],
            "default-src 'none'; frame-ancestors 'none'"
        );
        let r = client.request("GET", "/api/state", None, None).await;
        assert_eq!(r.status, 401);
        assert!(r.headers["content-security-policy"].contains("default-src 'none'"));
        // La coquille garde sa propre politique, sans media-src ni blob:.
        let r = client.request("GET", "/app/", None, None).await;
        let csp = &r.headers["content-security-policy"];
        assert!(
            csp.contains("img-src 'self';") && !csp.contains("media-src") && !csp.contains("blob:")
        );
    });
}
