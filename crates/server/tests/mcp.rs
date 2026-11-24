//! Le serveur MCP intégré (`POST /mcp`, étape E1a) de bout en bout : de vraies requêtes JSON-RPC
//! sur un vrai écouteur HTTPS, contre un serveur de démonstration (`--demo`, 127.0.0.1, dossier
//! jetable, appareils simulés) ou contre un serveur dans le processus (vrai `Hub`, fausse caméra).
//! Aucun vrai client MCP : c'est l'essai de l'étape E5.
//!
//! Les jetons d'agent ne sont jamais écrits dans un message d'échec.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::{Client, Resp, Server};
use ostal_server::security::{account, agent_tokens, server_dir};
use protocol::wire::PROTOCOL_VERSION;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const LEGACY: (&str, &str) = ("MCP-Protocol-Version", "2025-11-25");
const MODERN: &str = "2026-07-28";

// ---------------------------------------------------------------------------
// Aides
// ---------------------------------------------------------------------------

/// `ostal-server agent …` sur ce dossier de données, avec un HOME factice.
fn agent_cli(dir: &Path, args: &[&str]) -> (ExitStatus, String) {
    let out = Command::new(BIN)
        .arg("agent")
        .args(args)
        .arg("--data-dir")
        .arg(dir)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status, text)
}

/// Crée un agent avec la ligne de commande et rend son jeton (lu sur la sortie standard).
fn add_agent(dir: &Path, label: &str) -> String {
    let (status, text) = agent_cli(dir, &["add", "--username", common::USER, "--label", label]);
    assert!(status.success(), "agent add a échoué");
    text.lines()
        .find(|l| l.starts_with("ostal_agent_"))
        .unwrap_or_else(|| panic!("aucun jeton dans la sortie de agent add"))
        .trim()
        .to_string()
}

fn body(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

async fn post(
    client: &Client,
    token: Option<&str>,
    headers: &[(&str, &str)],
    message: &Value,
) -> Resp {
    client
        .request_with(
            "POST",
            "/mcp",
            token,
            Some(message.to_string().as_bytes()),
            headers,
        )
        .await
}

/// Une requête de l'ère héritée (en-tête de version posé).
async fn legacy(client: &Client, token: &str, method: &str, params: Value) -> Resp {
    post(client, Some(token), &[LEGACY], &body(1, method, params)).await
}

/// Un appel d'outil réussi : le contenu structuré, après avoir vérifié que le bloc `text` en est la
/// sérialisation exacte.
async fn tool(client: &Client, token: &str, name: &str, arguments: Value) -> Value {
    let resp = legacy(
        client,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let result = resp.json()["result"].clone();
    assert_eq!(result["isError"], false, "{result}");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        text, result["structuredContent"],
        "text = structuredContent"
    );
    result["structuredContent"].clone()
}

/// Un appel d'outil refusé en exécution : (code de motif, phrase).
async fn tool_error(
    client: &Client,
    token: &str,
    name: &str,
    arguments: Value,
) -> (String, String) {
    let resp = legacy(
        client,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let result = resp.json()["result"].clone();
    assert_eq!(result["isError"], true, "{result}");
    let s = &result["structuredContent"];
    (
        s["code"].as_str().unwrap().to_string(),
        s["message"].as_str().unwrap().to_string(),
    )
}

/// Un serveur de démonstration, son client, et un agent déjà créé, accès allumé.
struct Demo {
    server: Server,
    client: Client,
    token: String,
}

fn demo(tag: &str) -> Demo {
    let dir = common::temp_dir(tag);
    let server = Server::start(&dir);
    let client = server.client();
    let token = add_agent(&dir, "Claude Code");
    let (status, _) = agent_cli(&dir, &["enable"]);
    assert!(status.success());
    // La rafale de démarrage de la démo finie : les nombres ne bougent plus d'un appel à l'autre.
    common::runtime().block_on(async {
        let session = client.login().await;
        client.wait_startup_burst_over(&session).await;
    });
    Demo {
        server,
        client,
        token,
    }
}

fn all_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                all_keys(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| all_keys(v, out)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// L'interrupteur et la ligne de commande
// ---------------------------------------------------------------------------

#[test]
fn the_switch_is_off_by_default_and_mcp_then_does_not_exist() {
    let dir = common::temp_dir("mcp-switch");
    let server = Server::start(&dir);
    let client = server.client();
    let token = add_agent(&dir, "Portable");
    let rt = common::runtime();
    rt.block_on(async {
        let nothing = client.get("/rien-du-tout", &token).await;
        assert_eq!(nothing.status, 404);
        // Éteint : un bon jeton, un mauvais, aucun jeton, toutes les méthodes : 404, et la réponse
        // est celle d'une route qui n'existe pas.
        for (method, tok) in [
            ("POST", Some(token.as_str())),
            ("POST", None),
            ("GET", Some(token.as_str())),
            ("DELETE", Some(token.as_str())),
        ] {
            let resp = client
                .request_with(
                    method,
                    "/mcp",
                    tok,
                    (method == "POST")
                        .then(|| body(1, "ping", json!({})).to_string().into_bytes())
                        .as_deref(),
                    &[LEGACY],
                )
                .await;
            assert_eq!(resp.status, 404, "{method} {}", tok.is_some());
            assert_eq!(resp.body, nothing.body, "même corps qu'une route inconnue");
            assert_eq!(
                resp.headers.get("content-type"),
                nothing.headers.get("content-type")
            );
        }
    });
    // On l'allume : la ligne de commande agit sur le serveur qui tourne.
    assert!(agent_cli(&dir, &["enable"]).0.success());
    rt.block_on(async {
        let resp = legacy(&client, &token, "ping", json!({})).await;
        assert_eq!(resp.status, 200, "{}", resp.text());
    });
    // Et on l'éteint : de nouveau 404, sans redémarrage.
    assert!(agent_cli(&dir, &["disable"]).0.success());
    rt.block_on(async {
        let resp = legacy(&client, &token, "ping", json!({})).await;
        assert_eq!(resp.status, 404);
    });
}

#[test]
fn the_command_line_only_creates_read_only_tokens_and_never_shows_them_again() {
    let dir = common::temp_dir("mcp-cli");
    let _server = Server::start(&dir);
    for option in ["--scope", "--scopes", "--control", "--mode", "--occupation"] {
        let (status, text) = agent_cli(
            &dir,
            &[
                "add",
                "--username",
                common::USER,
                "--label",
                "X",
                option,
                "control",
            ],
        );
        assert!(!status.success(), "{option} aurait dû être refusé");
        assert!(text.contains("lecture seule"), "{text}");
    }
    assert!(
        !dir.join("serveur/agents.json").exists(),
        "rien n'a été créé par les essais refusés"
    );
    let token = add_agent(&dir, "Claude Code");
    // Compte inconnu, durée hors bornes, libellé en double.
    assert!(
        !agent_cli(&dir, &["add", "--username", "inconnu", "--label", "Y"])
            .0
            .success()
    );
    assert!(!agent_cli(
        &dir,
        &[
            "add",
            "--username",
            common::USER,
            "--label",
            "Y",
            "--days",
            "366"
        ]
    )
    .0
    .success());
    assert!(!agent_cli(
        &dir,
        &["add", "--username", common::USER, "--label", "claude code"]
    )
    .0
    .success());

    let file = std::fs::read_to_string(dir.join("serveur/agents.json")).unwrap();
    assert!(!file.contains(&token), "seule l'empreinte est gardée");
    let parsed: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(parsed["enabled"], false);
    assert_eq!(parsed["agents"][0]["scopes"], json!(["read"]));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.join("serveur/agents.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let (status, listing) = agent_cli(&dir, &["list"]);
    assert!(status.success());
    assert!(
        listing.contains("Claude Code") && listing.contains("éteint") && listing.contains("read")
    );
    assert!(!listing.contains(&token) && !listing.contains("ostal_agent_"));
    let hash = parsed["agents"][0]["hash"].as_str().unwrap();
    assert!(!listing.contains(hash));

    // Le journal d'audit sait qui a créé quoi, jamais le jeton.
    let audit = std::fs::read_dir(dir.join("serveur/audit"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(audit.contains("\"action\":\"agent_created\"") && audit.contains("Claude Code"));
    assert!(!audit.contains(&token));
}

#[test]
fn jetons_json_is_untouched_by_agents() {
    let dir = common::temp_dir("mcp-jetons");
    let _server = Server::start(&dir);
    let tokens = dir.join("serveur/jetons.json");
    std::fs::write(&tokens, br#"{"version":1,"tokens":[]}"#).unwrap();
    let before = std::fs::read(&tokens).ok();
    add_agent(&dir, "A");
    assert!(agent_cli(&dir, &["enable"]).0.success());
    assert_eq!(std::fs::read(&tokens).ok(), before);
}

// ---------------------------------------------------------------------------
// Le transport
// ---------------------------------------------------------------------------

#[test]
fn transport_rules_of_the_streamable_http_endpoint() {
    let d = demo("mcp-transport");
    let (client, token) = (&d.client, d.token.as_str());
    common::runtime().block_on(async {
        // `initialize` sans en-tête de version : accepté (ère héritée), sans session.
        let init = post(
            client,
            Some(token),
            &[],
            &body(1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})),
        )
        .await;
        assert_eq!(init.status, 200, "{}", init.text());
        assert!(!init.headers.contains_key("mcp-session-id"), "aucune session");
        let result = init.json()["result"].clone();
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["capabilities"], json!({"tools": {}}));
        assert!(result.get("resultType").is_none(), "pas de resultType en ère héritée");
        assert_eq!(init.headers["content-type"], "application/json");
        // Version inconnue demandée : la plus récente de l'ère héritée.
        let other = post(client, Some(token), &[], &body(1, "initialize", json!({"protocolVersion": "1999-01-01"}))).await;
        assert_eq!(other.json()["result"]["protocolVersion"], "2025-11-25");

        // Une notification et une réponse de client : 202, sans corps.
        for message in [
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {}}),
        ] {
            let resp = post(client, Some(token), &[LEGACY], &message).await;
            assert_eq!(resp.status, 202);
            assert!(resp.body.is_empty());
        }

        // Toute autre requête sans en-tête de version : 400 et -32020.
        let missing = post(client, Some(token), &[], &body(2, "tools/list", json!({}))).await;
        assert_eq!(missing.status, 400);
        assert_eq!(missing.json()["error"]["code"], -32020);
        assert_eq!(missing.json()["id"], 2);
        // Version inconnue : -32022 avec la liste des versions prises en charge.
        let unknown = post(client, Some(token), &[("MCP-Protocol-Version", "2031-01-01")], &body(2, "tools/list", json!({}))).await;
        assert_eq!(unknown.status, 400);
        assert_eq!(unknown.json()["error"]["code"], -32022);
        assert!(unknown.json()["error"]["data"]["supported"].as_array().unwrap().contains(&json!(MODERN)));

        // `ping` : l'ère héritée seulement.
        let ping = legacy(client, token, "ping", json!({})).await;
        assert_eq!(ping.json()["result"], json!({}));
        // Méthode inconnue : erreur JSON-RPC.
        let nope = legacy(client, token, "resources/list", json!({})).await;
        assert_eq!(nope.status, 200);
        assert_eq!(nope.json()["error"]["code"], -32601);
        // `server/discover` n'existe pas en ère héritée.
        let discover = legacy(client, token, "server/discover", json!({})).await;
        assert_eq!(discover.json()["error"]["code"], -32601);

        // Ère moderne : Mcp-Method obligatoire et égal à la méthode du corps.
        let modern = |method: &'static str| [("MCP-Protocol-Version", MODERN), ("Mcp-Method", method)];
        let no_method = post(client, Some(token), &[("MCP-Protocol-Version", MODERN)], &body(3, "tools/list", json!({}))).await;
        assert_eq!(no_method.status, 400);
        assert_eq!(no_method.json()["error"]["code"], -32020);
        let wrong = post(client, Some(token), &modern("tools/call"), &body(3, "tools/list", json!({}))).await;
        assert_eq!(wrong.status, 400);
        assert_eq!(wrong.json()["error"]["code"], -32020);
        let list = post(client, Some(token), &modern("tools/list"), &body(3, "tools/list", json!({}))).await;
        assert_eq!(list.status, 200, "{}", list.text());
        let result = list.json()["result"].clone();
        assert_eq!(result["resultType"], "complete");
        assert!(result["ttlMs"].is_number() && result["cacheScope"].is_string());
        assert!(result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"].is_string());
        assert_eq!(result["tools"].as_array().unwrap().len(), 3);
        let discover = post(client, Some(token), &modern("server/discover"), &body(4, "server/discover", json!({}))).await;
        assert_eq!(discover.status, 200, "{}", discover.text());
        let found = discover.json()["result"].clone();
        assert!(found["supportedVersions"].as_array().unwrap().contains(&json!(MODERN)));
        assert_eq!(found["capabilities"], json!({"tools": {}}));
        assert_eq!(found["resultType"], "complete");
        // `ping` a disparu de la révision moderne.
        let ping = post(client, Some(token), &modern("ping"), &body(5, "ping", json!({}))).await;
        assert_eq!(ping.json()["error"]["code"], -32601);

        // `Mcp-Name` : le nom de l'outil, en clair ou en Base64 ; autre chose : -32020.
        let call = body(6, "tools/call", json!({"name": "ostal_home_summary", "arguments": {}}));
        let plain = post(client, Some(token), &[("MCP-Protocol-Version", MODERN), ("Mcp-Method", "tools/call"), ("Mcp-Name", "ostal_home_summary")], &call).await;
        assert_eq!(plain.status, 200, "{}", plain.text());
        assert_eq!(plain.json()["result"]["resultType"], "complete");
        let encoded = "=?base64?b3N0YWxfaG9tZV9zdW1tYXJ5?=";
        let b64 = post(client, Some(token), &[("MCP-Protocol-Version", MODERN), ("Mcp-Method", "tools/call"), ("Mcp-Name", encoded)], &call).await;
        assert_eq!(b64.status, 200, "{}", b64.text());
        for name in [Some("ostal_get_device"), None] {
            let headers: Vec<(&str, &str)> = [("MCP-Protocol-Version", MODERN), ("Mcp-Method", "tools/call")]
                .into_iter()
                .chain(name.map(|n| ("Mcp-Name", n)))
                .collect();
            let resp = post(client, Some(token), &headers, &call).await;
            assert_eq!(resp.status, 400);
            assert_eq!(resp.json()["error"]["code"], -32020);
        }
        // Ère héritée : aucun de ces en-têtes n'est exigé.
        let plain = legacy(client, token, "tools/call", json!({"name": "ostal_home_summary"})).await;
        assert_eq!(plain.status, 200, "{}", plain.text());

        // GET et DELETE : 405.
        for method in ["GET", "DELETE"] {
            let resp = client.request_with(method, "/mcp", Some(token), None, &[LEGACY]).await;
            assert_eq!(resp.status, 405, "{method}");
            assert_eq!(resp.headers["allow"], "POST");
        }
        // Corps : JSON invalide, lot, objet sans méthode, identifiant nul, trop gros.
        let bad = client.request_with("POST", "/mcp", Some(token), Some(b"{pas du json"), &[LEGACY]).await;
        assert_eq!((bad.status, bad.json()["error"]["code"].clone()), (400, json!(-32700)));
        for message in [json!([body(1, "ping", json!({}))]), json!({"jsonrpc": "2.0", "id": 1}), json!({"jsonrpc": "2.0", "id": null, "method": "ping"}), json!("texte")] {
            let resp = post(client, Some(token), &[LEGACY], &message).await;
            assert_eq!((resp.status, resp.json()["error"]["code"].clone()), (400, json!(-32600)), "{message}");
        }
        let big = vec![b' '; 64 * 1024 + 1];
        let resp = client.request_with("POST", "/mcp", Some(token), Some(&big), &[LEGACY]).await;
        assert_eq!(resp.status, 413);
        // 64 Kio exactement passent.
        let mut exact = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string().into_bytes();
        exact.resize(64 * 1024, b' ');
        let resp = client.request_with("POST", "/mcp", Some(token), Some(&exact), &[LEGACY]).await;
        assert_eq!(resp.status, 200, "{}", resp.text());
    });
}

#[test]
fn any_origin_is_refused_even_when_it_equals_the_host() {
    let d = demo("mcp-origin");
    let port = d.server.addr.port();
    let host = format!("localhost:{port}");
    let origin = format!("https://localhost:{port}");
    common::runtime().block_on(async {
        for origin in [origin.as_str(), "https://evil.example", "null"] {
            let resp = d
                .client
                .request_with(
                    "POST",
                    "/mcp",
                    Some(&d.token),
                    Some(body(1, "ping", json!({})).to_string().as_bytes()),
                    &[LEGACY, ("Host", host.as_str()), ("Origin", origin)],
                )
                .await;
            assert_eq!(resp.status, 403, "{origin}");
        }
        // Même un GET, même sans jeton.
        let resp = d
            .client
            .request_with(
                "GET",
                "/mcp",
                None,
                None,
                &[("Origin", "https://evil.example")],
            )
            .await;
        assert_eq!(resp.status, 403);
    });
}

// ---------------------------------------------------------------------------
// L'authentification
// ---------------------------------------------------------------------------

#[test]
fn only_a_valid_agent_token_opens_mcp_and_it_opens_nothing_else() {
    let d = demo("mcp-auth");
    let dir = d.server.dir.clone();
    common::runtime().block_on(async {
        let (client, token) = (&d.client, d.token.as_str());
        let session = client.login().await;
        // Ni session, ni mauvais jeton, ni absence de jeton, ni jeton d'une autre forme.
        for tok in [
            None,
            Some(session.as_str()),
            Some("ostal_agent_0000000000000000000000000000000000000000000000000000000000000000"),
            Some("n'importe quoi"),
        ] {
            let started = Instant::now();
            let resp = post(client, tok, &[LEGACY], &body(1, "ping", json!({}))).await;
            assert_eq!(resp.status, 401);
            assert_eq!(resp.headers["www-authenticate"], "Bearer");
            assert!(
                started.elapsed() >= Duration::from_millis(200),
                "pause de 250 ms"
            );
        }
        // Le jeton d'agent n'ouvre aucune route de l'API.
        let mode = br#"{"mode":"absent"}"#.as_slice();
        for (method, path) in [
            ("GET", "/api/state"),
            ("GET", "/api/events"),
            ("PUT", "/api/mode"),
            ("POST", "/api/devices/1/on-off"),
            ("GET", "/api/export"),
        ] {
            let resp = client
                .request(method, path, Some(token), (method != "GET").then_some(mode))
                .await;
            assert_eq!(resp.status, 401, "{method} {path}");
        }
        let ws = client.request("GET", "/api/ws", Some(token), None).await;
        assert_eq!(ws.status, 401);

        // Plusieurs essais ratés ne bloquent jamais le bon jeton.
        for _ in 0..4 {
            let resp = post(
                client,
                Some("ostal_agent_faux"),
                &[LEGACY],
                &body(1, "ping", json!({})),
            )
            .await;
            assert_eq!(resp.status, 401);
        }
        let good = legacy(client, token, "ping", json!({})).await;
        assert_eq!(good.status, 200, "{}", good.text());

        // Révoqué par la ligne de commande pendant que le serveur tourne : refusé aussitôt.
        let other = add_agent(&dir, "Second");
        assert_eq!(legacy(client, &other, "ping", json!({})).await.status, 200);
        assert!(agent_cli(&dir, &["revoke", "--agent", "second"])
            .0
            .success());
        assert_eq!(legacy(client, &other, "ping", json!({})).await.status, 401);
        assert_eq!(legacy(client, token, "ping", json!({})).await.status, 200);
        // Et le journal d'audit l'a noté.
        let audit = std::fs::read_dir(dir.join("serveur/audit"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect::<String>();
        assert!(audit.contains("\"action\":\"agent_revoked\""), "{audit}");
    });
    // Jamais le jeton dans le journal technique ni dans l'audit.
    let log = d.server.log();
    let audit = std::fs::read_dir(dir.join("serveur/audit"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!log.contains(&d.token) && !audit.contains(&d.token));
    assert!(
        !log.contains("ostal_agent_"),
        "aucun jeton, même partiel, au journal"
    );
}

#[test]
fn expired_tokens_and_disabled_accounts_are_refused_while_the_server_runs() {
    let dir = common::temp_dir("mcp-expiry");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: "mot-de-passe-jetable-du-test",
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let sdir = server_dir(&dir).unwrap();
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    agent_tokens::set_enabled(&sdir, true).unwrap();
    let (_, token) = agent_tokens::add(&sdir, &rig.user_id, "Portable", 30).unwrap();
    // Un second compte, avec son agent.
    let sam = account::add(&sdir, "sam", Some("Sam"), "le-mot-de-passe-de-sam").unwrap();
    let (_, sam_token) = agent_tokens::add(&sdir, &sam.id, "Sam", 30).unwrap();
    common::runtime().block_on(async {
        assert_eq!(legacy(&client, &token, "ping", json!({})).await.status, 200);
        assert_eq!(
            legacy(&client, &sam_token, "ping", json!({})).await.status,
            200
        );

        // Le compte de Sam est désactivé : son agent ne vaut plus rien, celui de l'autre oui.
        account::set_active(&sdir, "sam", false).unwrap();
        assert_eq!(
            legacy(&client, &sam_token, "ping", json!({})).await.status,
            401
        );
        assert_eq!(legacy(&client, &token, "ping", json!({})).await.status, 200);
        account::set_active(&sdir, "sam", true).unwrap();
        assert_eq!(
            legacy(&client, &sam_token, "ping", json!({})).await.status,
            200
        );

        // Un jeton arrivé à échéance.
        let path = sdir.join(agent_tokens::FILE);
        let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["agents"][0]["expires"] = json!(1.0);
        std::fs::write(&path, file.to_string()).unwrap();
        assert_eq!(legacy(&client, &token, "ping", json!({})).await.status, 401);
        assert_eq!(
            legacy(&client, &sam_token, "ping", json!({})).await.status,
            200
        );

        // Un fichier illisible coupe tout (jamais l'inverse).
        std::fs::write(&path, "{ pas du JSON").unwrap();
        assert_eq!(
            legacy(&client, &sam_token, "ping", json!({})).await.status,
            404
        );
    });
}

#[test]
fn a_token_without_the_read_right_sees_no_tool_and_is_refused_the_ones_it_names() {
    let dir = common::temp_dir("mcp-scope");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: "mot-de-passe-jetable-du-test",
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let sdir = server_dir(&dir).unwrap();
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    agent_tokens::set_enabled(&sdir, true).unwrap();
    let (_, token) = agent_tokens::add(&sdir, &rig.user_id, "Sans droit", 30).unwrap();
    let path = sdir.join(agent_tokens::FILE);
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["agents"][0]["scopes"] = json!(["occupation", "teleportation"]);
    std::fs::write(&path, file.to_string()).unwrap();
    common::runtime().block_on(async {
        let list = legacy(&client, &token, "tools/list", json!({})).await;
        assert_eq!(list.json()["result"]["tools"], json!([]));
        let (code, _) = tool_error(&client, &token, "ostal_home_summary", json!({})).await;
        assert_eq!(code, "forbidden");
    });
}

#[test]
fn session_mode_and_read_tokens_do_not_open_mcp() {
    let dir = common::temp_dir("mcp-other-tokens");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: "mot-de-passe-jetable-du-test",
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let sdir = server_dir(&dir).unwrap();
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    agent_tokens::set_enabled(&sdir, true).unwrap();
    let (_, agent) = agent_tokens::add(&sdir, &rig.user_id, "Portable", 30).unwrap();
    let read = rig.hub.generate_read_token(&rig.user_id, "iPhone").unwrap();
    let mode = rig.hub.generate_mode_token(&rig.user_id, "iPhone").unwrap();
    let before = std::fs::read(sdir.join("jetons.json")).unwrap();
    common::runtime().block_on(async {
        let session = client
            .login_as(
                in_process::USER,
                "mot-de-passe-jetable-du-test",
                PROTOCOL_VERSION,
            )
            .await
            .json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        for (what, tok) in [("session", &session), ("lecture", &read), ("mode", &mode)] {
            let resp = legacy(&client, tok, "ping", json!({})).await;
            assert_eq!(resp.status, 401, "le jeton de {what} ne vaut rien sur /mcp");
        }
        assert_eq!(legacy(&client, &agent, "ping", json!({})).await.status, 200);
    });
    assert_eq!(
        std::fs::read(sdir.join("jetons.json")).unwrap(),
        before,
        "jetons.json intact"
    );
}

#[test]
fn tool_calls_are_limited_per_token() {
    let dir = common::temp_dir("mcp-rate");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: "mot-de-passe-jetable-du-test",
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let sdir = server_dir(&dir).unwrap();
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    agent_tokens::set_enabled(&sdir, true).unwrap();
    let (_, first) = agent_tokens::add(&sdir, &rig.user_id, "Premier", 30).unwrap();
    let (_, second) = agent_tokens::add(&sdir, &rig.user_id, "Second", 30).unwrap();
    common::runtime().block_on(async {
        for n in 0..agent_tokens::READS_PER_MINUTE {
            let resp = legacy(
                &client,
                &first,
                "tools/call",
                json!({"name": "ostal_home_summary"}),
            )
            .await;
            assert_eq!(resp.status, 200, "appel {n} : {}", resp.text());
        }
        let over = legacy(
            &client,
            &first,
            "tools/call",
            json!({"name": "ostal_home_summary"}),
        )
        .await;
        assert_eq!(over.status, 429);
        let wait: u64 = over.headers["retry-after"].parse().unwrap();
        assert!((1..=60).contains(&wait));
        // Les autres méthodes ne sont pas comptées, un autre jeton n'est pas touché.
        assert_eq!(
            legacy(&client, &first, "tools/list", json!({}))
                .await
                .status,
            200
        );
        let other = legacy(
            &client,
            &second,
            "tools/call",
            json!({"name": "ostal_home_summary"}),
        )
        .await;
        assert_eq!(other.status, 200, "{}", other.text());
    });
}

// ---------------------------------------------------------------------------
// Les outils
// ---------------------------------------------------------------------------

#[test]
fn the_three_read_tools_over_the_demo_house() {
    let d = demo("mcp-tools");
    common::runtime().block_on(async {
        let (client, token) = (&d.client, d.token.as_str());
        let list =
            legacy(client, token, "tools/list", json!({})).await.json()["result"]["tools"].clone();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "ostal_home_summary",
                "ostal_list_devices",
                "ostal_get_device"
            ]
        );
        for t in list.as_array().unwrap() {
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["inputSchema"]["additionalProperties"], false);
            assert!(t.get("_meta").is_none());
        }

        // Le résumé : des nombres et des phrases, ni mode ni adresse.
        let summary = tool(client, token, "ostal_home_summary", json!({})).await;
        let total = summary["devices"]["total"].as_u64().unwrap();
        assert!(total > 0);
        assert!(summary["status"]["headline"].as_str().unwrap().len() > 3);
        assert!(summary["status"]["detail"].as_str().unwrap().len() > 3);
        assert!(summary["cameras"]["total"].as_u64().unwrap() > 0);
        let mut keys = Vec::new();
        all_keys(&summary, &mut keys);
        for forbidden in [
            "mode",
            "behavior_now",
            "url",
            "rules",
            "webhook",
            "plan",
            "presence",
        ] {
            assert!(
                !keys.iter().any(|k| k == forbidden),
                "{forbidden} dans le résumé"
            );
        }
        let text = summary.to_string();
        for secret in ["rtsp", "192.0.2.", "demo-camera-mdp", "Vacances", "Absent"] {
            assert!(!text.contains(secret), "{secret} dans le résumé : {text}");
        }

        // La liste.
        let all = tool(client, token, "ostal_list_devices", json!({})).await;
        let devices = all["devices"].as_array().unwrap();
        assert_eq!(all["total"].as_u64().unwrap() as usize, devices.len());
        assert_eq!(devices.len() as u64, total);
        assert_eq!(all["truncated"], false);
        for device in devices {
            let id = device["id"].as_str().unwrap();
            assert!(
                id.bytes().all(|b| b.is_ascii_digit()) && (id == "0" || !id.starts_with('0')),
                "{id}"
            );
            assert!(
                ["cover", "on_off", "sensor", "other"].contains(&device["kind"].as_str().unwrap())
            );
            assert!(device["status"]["state"].is_string());
            for hidden in ["vendor", "product", "device_types", "bridge"] {
                assert!(device.get(hidden).is_none(), "{hidden}");
            }
        }
        // Les filtres.
        let covers = tool(
            client,
            token,
            "ostal_list_devices",
            json!({"kind": "cover"}),
        )
        .await;
        assert!(covers["devices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["kind"] == "cover"));
        let one = tool(client, token, "ostal_list_devices", json!({"limit": 1})).await;
        assert_eq!(one["devices"].as_array().unwrap().len(), 1);
        assert_eq!(one["truncated"], devices.len() > 1);
        let problems = tool(
            client,
            token,
            "ostal_list_devices",
            json!({"only_problems": true}),
        )
        .await;
        assert!(problems["devices"].as_array().unwrap().iter().all(|v| {
            v["status"]["state"] == "unreachable"
                || v["latest"]
                    .get("battery_low")
                    .is_some_and(|b| b["value"].as_f64() >= Some(0.5))
        }));

        // Un appareil.
        let first_id = devices[0]["id"].as_str().unwrap();
        let one = tool(client, token, "ostal_get_device", json!({"id": first_id})).await;
        assert_eq!(one["device"]["id"], first_id);
        assert!(one["device"]["capabilities"].is_array());
        assert!(one["device"].get("open_since").is_some());

        // Erreurs d'exécution : un code de motif et une phrase.
        for (name, args, code) in [
            ("ostal_get_device", json!({}), "invalid_argument"),
            ("ostal_get_device", json!({"id": "007"}), "invalid_argument"),
            ("ostal_get_device", json!({"id": 5}), "invalid_argument"),
            (
                "ostal_get_device",
                json!({"id": "99999999999999999999999"}),
                "invalid_argument",
            ),
            (
                "ostal_get_device",
                json!({"id": "999999999999"}),
                "not_found",
            ),
            (
                "ostal_get_device",
                json!({"id": first_id, "extra": 1}),
                "invalid_argument",
            ),
            (
                "ostal_home_summary",
                json!({"verbose": true}),
                "invalid_argument",
            ),
            (
                "ostal_list_devices",
                json!({"kind": "lampe"}),
                "invalid_argument",
            ),
            (
                "ostal_list_devices",
                json!({"only_problems": "oui"}),
                "invalid_argument",
            ),
            (
                "ostal_list_devices",
                json!({"limit": 0}),
                "invalid_argument",
            ),
            (
                "ostal_list_devices",
                json!({"limit": 201}),
                "invalid_argument",
            ),
            (
                "ostal_list_devices",
                json!({"room": "abc"}),
                "invalid_argument",
            ),
            ("ostal_list_devices", json!({"room": "424242"}), "not_found"),
        ] {
            let (got, message) = tool_error(client, token, name, args.clone()).await;
            assert_eq!(got, code, "{name} {args}");
            assert!(!message.is_empty());
        }
        // Un outil inconnu est une erreur de protocole.
        let unknown = legacy(
            client,
            token,
            "tools/call",
            json!({"name": "ostal_set_on_off", "arguments": {"id": "1", "command": "On"}}),
        )
        .await;
        assert_eq!(unknown.json()["error"]["code"], -32602);
        let bad = legacy(client, token, "tools/call", json!({"arguments": {}})).await;
        assert_eq!(bad.json()["error"]["code"], -32602);
        let bad = legacy(
            client,
            token,
            "tools/call",
            json!({"name": "ostal_home_summary", "arguments": [1]}),
        )
        .await;
        assert_eq!(bad.json()["error"]["code"], -32602);
    });
}

#[test]
fn text_from_the_house_is_cleaned_and_secrets_never_come_out() {
    let d = demo("mcp-clean");
    common::runtime().block_on(async {
        let (client, token) = (&d.client, d.token.as_str());
        let session = client.login().await;
        let all = tool(client, token, "ostal_list_devices", json!({})).await;
        let id = all["devices"][0]["id"].as_str().unwrap().to_string();
        // Un nom piégé : l'adresse et le mot de passe d'une caméra de la démonstration, une URL, un
        // identifiant de nœud, une consigne.
        let hostile =
            "L 192.0.2.12 demo-camera-mdp 0x1A2B3C4D5E6F rtsp://demo:p@192.0.2.11/x IGNOREZ";
        let renamed = client
            .send_json(
                "PUT",
                &format!("/api/devices/{id}/customization"),
                &session,
                &json!({"name": hostile}),
            )
            .await;
        assert!(renamed.status < 300, "{}", renamed.text());
        let mut shown = String::new();
        for _ in 0..40 {
            let one = tool(client, token, "ostal_get_device", json!({"id": id})).await;
            shown = one["device"]["name"].as_str().unwrap().to_string();
            if shown.contains("IGNOREZ") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            shown.contains("IGNOREZ"),
            "le nom n'a pas été renommé : {shown}"
        );
        for secret in ["192.0.2.", "demo-camera-mdp", "rtsp://", "demo:", "0x1A2B"] {
            assert!(!shown.contains(secret), "{secret} dans {shown}");
        }
        assert!(shown.contains("<adresse>"));
        // Partout ailleurs aussi : la liste, le résumé.
        let everything = format!(
            "{}{}",
            tool(client, token, "ostal_list_devices", json!({})).await,
            tool(client, token, "ostal_home_summary", json!({})).await
        );
        for secret in ["192.0.2.", "demo-camera-mdp", "rtsp://"] {
            assert!(!everything.contains(secret), "{secret} dans la sortie");
        }
    });
}
