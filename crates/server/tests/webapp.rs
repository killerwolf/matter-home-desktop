//! L'app web mobile (ticket n° 15) : le service des fichiers intégrés, les en-têtes de sécurité,
//! l'accès sans jeton à la seule coquille, et le jeton d'un navigateur pour le WebSocket
//! (sous-protocole). Vrai binaire en mode démo, sur 127.0.0.1.

mod common;

use common::*;
use serde_json::json;
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
    ("/app/events.js", "text/javascript"),
    ("/app/plan.js", "text/javascript"),
    ("/app/mode.js", "text/javascript"),
    ("/app/push.js", "text/javascript"),
    ("/app/widgets.js", "text/javascript"),
    ("/app/connections.js", "text/javascript"),
    ("/app/sw.js", "text/javascript"),
    ("/app/manifest.webmanifest", "application/manifest+json"),
    ("/app/icon-192.png", "image/png"),
    ("/app/icon-512.png", "image/png"),
    ("/app/apple-touch-icon.png", "image/png"),
    ("/app/favicon.png", "image/png"),
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
fn plan_page_is_wired_into_the_shell_and_cached_by_the_service_worker() {
    let dir = temp_dir("webapp-plan");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let html = client.request("GET", "/app/", None, None).await.text();
        assert!(html.contains(r#"data-tab="plan""#), "onglet Plan absent");
        assert!(html.contains(">Plan<"));
        let app = client
            .request("GET", "/app/app.js", None, None)
            .await
            .text();
        assert!(
            app.contains(r#"from "./plan.js""#),
            "app.js n'importe pas plan.js"
        );
        // Le plan arrive par le snapshot du WebSocket et `plan_changed` : aucune route de plus.
        assert!(app.contains("plan_changed"));
        let plan = client.request("GET", "/app/plan.js", None, None).await;
        assert_eq!(plan.status, 200);
        assert!(plan.headers["content-security-policy"].contains("script-src 'self'"));
        let js = plan.text();
        // Construit en DOM : chaque `innerHTML =` ne reçoit que le pictogramme constant, pas d'eval, pas d'URL.
        for line in js.lines().filter(|l| l.contains("innerHTML")) {
            let code = line.trim();
            if code.starts_with("//") || code.starts_with('*') {
                continue;
            }
            assert!(
                code == "group.innerHTML = icon;",
                "innerHTML inattendu dans plan.js : {code}"
            );
        }
        assert!(
            !js.contains("eval(")
                && !js.contains("outerHTML")
                && !js.contains("insertAdjacentHTML")
        );
        // Seul l'espace de noms SVG (un identifiant, jamais chargé) est une adresse.
        let js = js.replace("http://www.w3.org/2000/svg", "");
        assert!(
            !js.contains("http://") && !js.contains("https://") && !js.contains("document.write")
        );
        assert!(js.contains("Le plan se dessine dans l'application de bureau"));
        // Le service worker garde tous les fichiers de la coquille, plan.js compris.
        let sw = client.request("GET", "/app/sw.js", None, None).await.text();
        for file in [
            "plan.js",
            "app.js",
            "tiles.js",
            "icons.js",
            "index.html",
            "tokens.css",
        ] {
            assert!(
                sw.contains(&format!("\"{file}\"")),
                "{file} absent du cache"
            );
        }
        // Le plan lui-même (donnée de la maison) reste derrière le jeton.
        let r = client.request("GET", "/api/plan", None, None).await;
        assert_eq!(r.status, 401);
        let token = client.login().await;
        let r = client.get("/api/plan", &token).await;
        assert_eq!(r.status, 200);
        assert!(r.text().contains("Salon"));
    });
}

/// Ticket n° 8 : la pastille du mode est dans la coquille, branchée sur le snapshot, l'événement
/// `mode_changed` et `PUT /api/mode` ; le service worker change de version et garde `mode.js`.
#[test]
fn mode_chip_is_wired_into_the_shell_and_the_service_worker_cache_is_bumped() {
    let dir = temp_dir("webapp-mode");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let html = client.request("GET", "/app/", None, None).await.text();
        assert!(html.contains(r#"id="mode-chip""#), "pastille absente");
        // L'entrée « Mode » de la barre du bas ouvre la feuille des modes (plus de menu déroulant).
        assert!(
            html.contains(r#"id="tab-mode""#),
            "entrée Mode de la barre du bas absente"
        );
        let app = client
            .request("GET", "/app/app.js", None, None)
            .await
            .text();
        assert!(app.contains(r#"from "./mode.js""#));
        // Le mode arrive par le snapshot et `mode_changed` ; le changement passe par la seule
        // route `PUT /api/mode`, avec le jeton de session de la page (jamais le jeton de mode).
        assert!(app.contains("mode_changed") && app.contains("s.mode"));
        // Un serveur sans modes : pas de pastille.
        assert!(app.contains("!state.hasMode"));
        assert!(app.contains(r#"api.request("PUT", "/api/mode""#));
        assert!(!app.contains("jeton-mode") && !app.contains("mode_token"));
        // Une caméra éteinte n'ouvre ni flux ni image fixe.
        assert!(app.contains("cameraOff(cam, state.mode)"));
        let mode = client.request("GET", "/app/mode.js", None, None).await;
        assert_eq!(mode.status, 200);
        let js = mode.text();
        assert!(!js.contains("eval(") && !js.contains("innerHTML"));
        // Ses quatre modes sont ceux du serveur.
        for key in ["present", "absent", "nuit", "vacances"] {
            assert!(js.contains(&format!("\"{key}\"")), "{key}");
        }
        let sw = client.request("GET", "/app/sw.js", None, None).await.text();
        assert!(sw.contains("\"mode.js\""), "mode.js absent du cache");
        assert!(
            sw.contains("ostal-shell-v16") && sw.contains("\"events.js\""),
            "le cache du service worker doit changer de version"
        );
        // Le serveur de démonstration envoie bien le mode dans l'état complet.
        let token = client.login().await;
        let state = client.get("/api/state", &token).await.json();
        assert_eq!(state["mode"], "present");
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
            ("GET", "/api/ws?protocol=3"),
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
        // `ostal.v1`, jamais avec l'entrée qui porte le jeton.
        let (chosen, hello) = ws_with_protocols(
            &client,
            "/api/ws?protocol=3",
            &format!("ostal.v1, bearer.{token}"),
            None,
            None,
        )
        .await
        .expect("connexion acceptée");
        assert_eq!(chosen, "ostal.v1");
        assert!(!chosen.contains(&token));
        assert_eq!(hello["type"], "hello");

        // Un mauvais jeton : refusé.
        let refused = ws_with_protocols(
            &client,
            "/api/ws?protocol=3",
            &format!("ostal.v1, bearer.{}", "1".repeat(64)),
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
            let (status, _) = raw_get(&client, "/api/ws?protocol=3", &headers).await;
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
        let protocols = format!("ostal.v1, bearer.{token}");
        let same = format!("https://localhost:{}", client.addr.port());
        // Même origine que l'hôte : accepté.
        assert!(
            ws_with_protocols(&client, "/api/ws?protocol=3", &protocols, None, Some(&same))
                .await
                .is_ok()
        );
        // Autre site : 403, même avec un jeton valide.
        for origin in ["https://evil.example", "http://localhost", "null"] {
            let refused = ws_with_protocols(
                &client,
                "/api/ws?protocol=3",
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
            "/api/ws?protocol=3",
            "ostal.v1",
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
        // La coquille garde sa propre politique : pas de `blob:` pour les images, seulement
        // `media-src blob:` pour le clip d'un événement de détection (ticket n° 18).
        let r = client.request("GET", "/app/", None, None).await;
        let csp = &r.headers["content-security-policy"];
        assert!(csp.contains("img-src 'self';") && csp.contains("media-src blob:;"));
        assert_eq!(csp.matches("blob:").count(), 1, "{csp}");
    });
}

/// Le jeton écrit dans un fichier préparé : `Bearer <64 hex>` (raccourci) ou `TOKEN = "<64 hex>"`.
fn token_after(text: &str, marker: &str) -> String {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} absent"))
        + marker.len();
    let token: String = text[start..].chars().take(64).collect();
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()), "{token}");
    token
}

/// Page « Widgets et raccourcis » : en un geste, sous session, le serveur prépare les quatre
/// raccourcis (nouveau jeton de mode, adresse en .ts.net écrite dedans, liens à toucher sans jeton)
/// et le widget (nouveau jeton de lecture). Signature remplacée par le plist tel quel
/// (`OSTAL_TEST_UNSIGNED_SHORTCUTS`) : les essais ne dépendent ni d'iCloud ni d'Apple.
#[test]
fn one_tap_install_prepares_personal_shortcuts_and_widget() {
    let dir = temp_dir("webapp-install");
    let server = Server::start_with(&dir, &[("OSTAL_TEST_UNSIGNED_SHORTCUTS", "1")]);
    let client = server.client();
    let origin = json!({ "origin": "https://maison.tailnet-test.ts.net" });
    let mut secrets = Vec::new();
    runtime().block_on(async {
        // Sans session : rien.
        for (method, path) in [
            ("GET", "/api/tokens"),
            ("POST", "/api/tokens/mode/shortcuts"),
            ("POST", "/api/tokens/read/widget"),
            ("DELETE", "/api/tokens/mode"),
            ("DELETE", "/api/tokens/read"),
        ] {
            let r = client
                .request(method, path, None, Some(origin.to_string().as_bytes()))
                .await;
            assert_eq!(r.status, 401, "{method} {path}");
        }
        let session = client.login().await;
        let tokens = client.get("/api/tokens", &session).await.json();
        assert_eq!(tokens["mode"]["exists"], false);
        assert_eq!(tokens["read"]["exists"], false);

        // L'adresse directe (port 8443, certificat auto-signé) ou une autre : refusées, sans
        // créer de jeton.
        for bad in [
            "https://192.168.1.20:8443",
            "https://maison.tailnet-test.ts.net:8443",
            "null",
        ] {
            for path in ["/api/tokens/mode/shortcuts", "/api/tokens/read/widget"] {
                let r = client
                    .send_json("POST", path, &session, &json!({ "origin": bad }))
                    .await;
                assert_eq!((r.status, r.code().as_str()), (400, "bad_request"), "{bad}");
                assert!(r.json()["error"]["detail"]
                    .as_str()
                    .unwrap()
                    .contains(".ts.net"));
            }
        }
        // Derrière `tailscale serve`, Host est le nom en .ts.net de ce Mac : une page qui annonce
        // un autre Mac (ou un Origin différent) est refusée.
        let body = origin.to_string();
        for extra in [
            [("Host", "autre.tailnet-test.ts.net"), ("X-Essai", "1")],
            [
                ("Host", "maison.tailnet-test.ts.net"),
                ("Origin", "https://autre.tailnet-test.ts.net"),
            ],
        ] {
            let r = client
                .request_with(
                    "POST",
                    "/api/tokens/mode/shortcuts",
                    Some(&session),
                    Some(body.as_bytes()),
                    &extra,
                )
                .await;
            assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));
            assert!(r.json()["error"]["detail"]
                .as_str()
                .unwrap()
                .contains("ce Mac"));
        }
        let tokens = client.get("/api/tokens", &session).await.json();
        assert_eq!(tokens["mode"]["exists"], false);

        // Les raccourcis : quatre liens sans accent, à toucher sans jeton.
        let prepared = client
            .send_json("POST", "/api/tokens/mode/shortcuts", &session, &origin)
            .await;
        assert_eq!(prepared.status, 200, "{}", prepared.text());
        let prepared = prepared.json();
        assert_eq!(prepared["expires_in_s"], 300);
        let links = prepared["shortcuts"].as_array().unwrap().clone();
        let names: Vec<_> = links.iter().map(|l| l["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["Je pars", "Je rentre", "Mode nuit", "Mode vacances"]
        );
        let mut mode_token = String::new();
        for (link, (path, value)) in links.iter().zip([
            ("presence", "away"),
            ("presence", "home"),
            ("mode", "nuit"),
            ("mode", "vacances"),
        ]) {
            let url = link["url"].as_str().unwrap();
            let file = format!("{}.shortcut", link["name"].as_str().unwrap());
            assert!(url.starts_with("/app/raccourcis/perso/"), "{url}");
            assert!(url.ends_with(&file.replace(' ', "%20")), "{url}");
            secrets.push(url.split('/').nth(4).unwrap().to_string());
            let r = client.request("GET", url, None, None).await;
            assert_eq!(r.status, 200, "{url}");
            assert_eq!(r.headers["content-type"], "application/octet-stream");
            assert_eq!(
                r.headers["content-disposition"],
                format!("attachment; filename=\"{file}\"")
            );
            assert_eq!(r.headers["cache-control"], "no-store");
            assert_eq!(r.headers["x-content-type-options"], "nosniff");
            assert!(r.headers["content-security-policy"].contains("default-src 'self'"));
            let plist = r.text();
            assert!(plist.contains(&format!(
                "<string>https://maison.tailnet-test.ts.net/api/{path}</string>"
            )));
            assert!(plist.contains(&format!("<string>{value}</string>")));
            assert!(plist.contains("<key>WFWorkflowImportQuestions</key><array></array>"));
            let token = token_after(&plist, "<string>Bearer ");
            if !mode_token.is_empty() {
                assert_eq!(token, mode_token, "le même jeton dans les deux raccourcis");
            }
            mode_token = token;
            // Le lien sert encore (Safari peut redemander le fichier) ; HEAD ne rend pas le corps.
            assert_eq!(client.request("GET", url, None, None).await.body, r.body);
            let head = client.request("HEAD", url, None, None).await;
            assert_eq!(head.status, 200);
            assert!(head.body.is_empty());
            // Un autre nom sous le même identifiant, ou un identifiant inventé : 404.
            let other = url
                .replace("Je%20pars", "X")
                .replace("Je%20rentre", "X")
                .replace("Mode%20nuit", "X")
                .replace("Mode%20vacances", "X");
            assert_eq!(client.request("GET", &other, None, None).await.status, 404);
        }
        secrets.push(mode_token.clone());
        // Le raccourci marche de bout en bout : « Je pars » dit où l'on est sans toucher au mode,
        // « Mode nuit » change le mode de toute la maison.
        let r = client
            .send_json(
                "PUT",
                "/api/presence",
                &mode_token,
                &json!({"state": "away"}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(
            r.json(),
            json!({"state": "away", "changed": true, "mode": "absent", "message": "Maison en mode Absent."})
        );
        let r = client
            .send_json("PUT", "/api/mode", &mode_token, &json!({"mode": "absent"}))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        for gone in [
            "/app/raccourcis/perso/00000000000000000000000000000000/Je%20pars.shortcut",
            "/app/raccourcis/mode-absent.shortcut",
            "/app/raccourcis/mode-present.shortcut",
            "/app/raccourcis/ostal.js",
            "/app/raccourcis/",
        ] {
            assert_eq!(
                client.request("GET", gone, None, None).await.status,
                404,
                "{gone}"
            );
        }

        // Recommencer depuis la même connexion (même téléphone) : nouveau jeton, les anciens liens et
        // l'ancien jeton ne marchent plus.
        let again = client
            .send_json("POST", "/api/tokens/mode/shortcuts", &session, &origin)
            .await
            .json();
        let old_url = links[0]["url"].as_str().unwrap();
        assert_eq!(client.request("GET", old_url, None, None).await.status, 404);
        let r = client
            .send_json("PUT", "/api/mode", &mode_token, &json!({"mode": "nuit"}))
            .await;
        assert_eq!(r.status, 401);
        let new_url = again["shortcuts"][1]["url"].as_str().unwrap().to_string();
        let new_plist = client.request("GET", &new_url, None, None).await.text();
        let new_mode_token = token_after(&new_plist, "<string>Bearer ");
        secrets.push(new_mode_token.clone());
        secrets.push(new_url.split('/').nth(4).unwrap().to_string());
        assert_ne!(new_mode_token, mode_token);

        // Le widget : le script prérempli avec l'adresse et un jeton de lecture.
        // Avec les en-têtes de `tailscale serve` et du navigateur, cohérents.
        let widget = client
            .request_with(
                "POST",
                "/api/tokens/read/widget",
                Some(&session),
                Some(body.as_bytes()),
                &[
                    ("Host", "maison.tailnet-test.ts.net"),
                    ("Origin", "https://maison.tailnet-test.ts.net"),
                ],
            )
            .await;
        assert_eq!(widget.status, 200, "{}", widget.text());
        let script = widget.json()["script"].as_str().unwrap().to_string();
        assert!(script.contains("const SERVEUR = \"https://maison.tailnet-test.ts.net\";"));
        assert!(script.contains("const RACCOURCI_RENTRE = \"Je rentre\";"));
        let read_token = token_after(&script, "const TOKEN = \"");
        secrets.push(read_token.clone());
        assert_eq!(client.get("/api/state", &read_token).await.status, 200);
        assert_eq!(
            client.get("/api/events?limit=1", &read_token).await.status,
            200
        );
        assert_eq!(client.get("/api/plan", &read_token).await.status, 401);
        let tokens = client.get("/api/tokens", &session).await.json();
        assert_eq!(tokens["mode"]["exists"], true);
        assert_eq!(tokens["read"]["exists"], true);
        assert!(tokens["read"]["created"].as_f64().unwrap() > 1.7e9);
        assert!(!tokens.to_string().contains(&read_token));

        // Révoquer : chaque jeton à part, et les liens disparaissent avec le jeton de mode.
        let r = client
            .request("DELETE", "/api/tokens/read", Some(&session), None)
            .await;
        assert_eq!(r.status, 204);
        assert_eq!(client.get("/api/state", &read_token).await.status, 401);
        let r = client
            .send_json(
                "PUT",
                "/api/mode",
                &new_mode_token,
                &json!({"mode": "nuit"}),
            )
            .await;
        assert_eq!(r.status, 200, "le jeton de mode n'est pas touché");
        let r = client
            .request("DELETE", "/api/tokens/mode", Some(&session), None)
            .await;
        assert_eq!(r.status, 204);
        assert_eq!(
            client.request("GET", &new_url, None, None).await.status,
            404
        );
        let r = client
            .send_json(
                "PUT",
                "/api/mode",
                &new_mode_token,
                &json!({"mode": "present"}),
            )
            .await;
        assert_eq!(r.status, 401);
        let tokens = client.get("/api/tokens", &session).await.json();
        assert_eq!(tokens["mode"]["exists"], false);
        assert_eq!(tokens["read"]["exists"], false);
    });
    // Aucun jeton ni identifiant de lien dans le journal, ni en clair sur le disque.
    let log = server.log();
    let on_disk: String = ["jetons.json", "jeton-mode.json", "jeton-lecture.json"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(dir.join("serveur").join(f)).ok())
        .collect();
    assert!(!secrets.is_empty());
    for secret in &secrets {
        assert!(!log.contains(secret.as_str()), "secret au journal");
        assert!(!on_disk.contains(secret.as_str()));
    }
    // Le journal dit ce qui a été fait, sans le secret.
    assert!(log.contains("raccourcis préparés"));
}

#[test]
fn widgets_page_is_wired_into_the_app_and_never_cached_by_the_service_worker() {
    let dir = temp_dir("webapp-widgets");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let app = client
            .request("GET", "/app/app.js", None, None)
            .await
            .text();
        assert!(app.contains(r#"from "./widgets.js""#));
        assert!(app.contains(r#"widgets.id = "widgets""#));
        assert!(app.contains("Widgets et raccourcis"));
        assert!(app.contains("originInfo(location)"));
        assert!(app.contains("Installer mes raccourcis") && app.contains("Installer le widget"));
        assert!(app.contains("Je pars") || app.contains("SHORTCUT_NAMES"));
        assert!(app.contains("ANDROID_NOTE") && app.contains("isAndroid(navigator.userAgent)"));
        assert!(
            app.contains("/api/tokens/${encodeURIComponent(token.id)}") && app.contains("Révoquer")
        );
        // Plus de jeton à créer sur le Mac ni d'adresse à coller.
        assert!(!app.contains("Créer le code secret") && !app.contains("Copier l'adresse"));
        assert!(!app.contains("Tout éteindre"));
        let widgets = client.request("GET", "/app/widgets.js", None, None).await;
        assert_eq!(widgets.status, 200);
        assert!(widgets.headers["content-type"].starts_with("text/javascript"));
        let js = widgets.text();
        assert!(!js.contains("eval(") && !js.contains("innerHTML"));
        let sw = client.request("GET", "/app/sw.js", None, None).await.text();
        assert!(sw.contains("\"widgets.js\"") && sw.contains("ostal-shell-v16"));
        // Les raccourcis et le script ne sont ni listés ni gardés par le service worker.
        assert!(sw.contains("/app/raccourcis/"));
        assert!(!sw.contains("\"raccourcis/"));
    });
}
