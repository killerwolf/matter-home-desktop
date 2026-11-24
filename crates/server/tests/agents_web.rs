//! « Mes agents » (étape E4) de bout en bout : une personne connectée crée, voit et révoque ses
//! agents d'IA depuis l'app web (`/api/agents`). De vraies requêtes HTTPS contre un serveur de
//! démonstration (`--demo`, 127.0.0.1, dossier jetable, appareils simulés) ou contre un serveur dans
//! le processus pour les droits du compte. Aucun vrai client MCP, aucun service extérieur.
//!
//! Les jetons ne sont jamais écrits dans un message d'échec.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::path::Path;
use std::time::Duration;

use common::{Client, Server};
use ostal_server::security::{account, agent_tokens, server_dir};
use serde_json::{json, Value};

const LEGACY: (&str, &str) = ("MCP-Protocol-Version", "2025-11-25");

async fn mcp(client: &Client, token: &str, method: &str, params: Value) -> common::Resp {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    client
        .request_with(
            "POST",
            "/mcp",
            Some(token),
            Some(message.to_string().as_bytes()),
            &[LEGACY],
        )
        .await
}

async fn tool_names(client: &Client, token: &str) -> Vec<String> {
    let resp = mcp(client, token, "tools/list", json!({})).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

async fn call(client: &Client, token: &str, name: &str, arguments: Value) -> Value {
    let resp = mcp(
        client,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["result"].clone()
}

/// Les identifiants (texte décimal) des appareils de la démo de cette sorte (`on_off`, `cover`…).
async fn ids_of_kind(client: &Client, token: &str, kind: &str) -> Vec<String> {
    let all = call(client, token, "ostal_list_devices", json!({})).await;
    all["structuredContent"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == kind)
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect()
}

/// L'identifiant (texte décimal) d'un appareil de la démo, d'après un bout de son nom.
async fn id_of(client: &Client, token: &str, part: &str) -> String {
    let all = call(client, token, "ostal_list_devices", json!({})).await;
    all["structuredContent"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"].as_str().unwrap().contains(part))
        .unwrap_or_else(|| panic!("aucun appareil « {part} » : {all}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create(client: &Client, session: &str, body: &Value) -> common::Resp {
    client.send_json("POST", "/api/agents", session, body).await
}

fn audit_lines(dir: &Path) -> Vec<Value> {
    let Ok(read) = std::fs::read_dir(dir.join("serveur/audit")) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for file in read {
        let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
        lines.extend(
            text.lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()),
        );
    }
    lines
}

fn audit_of(dir: &Path, action: &str) -> Vec<Value> {
    audit_lines(dir)
        .into_iter()
        .filter(|l| l["action"] == action)
        .collect()
}

struct Demo {
    #[allow(dead_code)]
    server: Server,
    client: Client,
    dir: std::path::PathBuf,
    session: String,
}

fn demo(tag: &str) -> Demo {
    let dir = common::temp_dir(tag);
    let server = Server::start(&dir);
    let client = server.client();
    let session = common::runtime().block_on(async {
        let session = client.login().await;
        client.wait_startup_burst_over(&session).await;
        session
    });
    Demo {
        server,
        client,
        dir,
        session,
    }
}

fn switch_on(dir: &Path) {
    agent_tokens::set_enabled(&server_dir(dir).unwrap(), true).unwrap();
}

#[test]
fn only_a_session_opens_my_agents_not_an_agent_a_mode_or_a_read_token() {
    let dir = common::temp_dir("web-agents-tokens");
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
    let (mine, agent) = agent_tokens::add(&sdir, &rig.user_id, "Portable", 30).unwrap();
    let read = rig.hub.generate_read_token(&rig.user_id, "iPhone").unwrap();
    let mode = rig.hub.generate_mode_token(&rig.user_id, "iPhone").unwrap();
    let body = json!({"label": "Pirate", "scopes": ["read", "control", "mode"], "devices": ["1"]});
    common::runtime().block_on(async {
        let item = format!("/api/agents/{}", mine.id);
        // Sans jeton, avec un jeton d'agent (même celui qu'on viserait), de lecture ou de mode :
        // 401 partout. Un agent ne se crée pas d'agent, ne se donne pas de droits et ne révoque
        // pas ses voisins.
        for (what, token) in [
            ("aucun", None),
            ("agent", Some(agent.as_str())),
            ("lecture", Some(read.as_str())),
            ("mode", Some(mode.as_str())),
        ] {
            for (method, path, payload) in [
                ("GET", "/api/agents", None),
                ("POST", "/api/agents", Some(body.to_string())),
                ("DELETE", item.as_str(), None),
            ] {
                let resp = client
                    .request(method, path, token, payload.as_deref().map(str::as_bytes))
                    .await;
                assert_eq!(resp.status, 401, "{what} {method} {path}");
            }
        }
        // Rien n'a été créé ni révoqué.
        assert_eq!(agent_tokens::list(&sdir).unwrap().1.len(), 1);
        let session = client
            .login_as(
                in_process::USER,
                "mot-de-passe-jetable-du-test",
                common::PROTOCOL,
            )
            .await
            .json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let resp = client.get("/api/agents", &session).await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        assert_eq!(resp.json()["agents"].as_array().unwrap().len(), 1);
    });
}

#[test]
fn a_token_is_created_with_the_ticked_rights_shown_once_and_revoked_at_once() {
    let d = demo("web-agents-life");
    switch_on(&d.dir);
    let (client, session) = (&d.client, d.session.as_str());
    common::runtime().block_on(async {
        // Pour trouver des appareils, un premier agent en lecture seule.
        let first = create(
            client,
            session,
            &json!({"label": "Lecture", "scopes": ["read"], "days": 7}),
        )
        .await;
        assert_eq!(first.status, 201, "{}", first.text());
        let reader = first.json()["token"].as_str().unwrap().to_string();
        // Un nom de 48 caractères (accents compris) est gardé tel quel, jamais coupé en silence.
        let long = "é".repeat(48);
        let kept = create(client, session, &json!({"label": long, "scopes": ["read"]})).await;
        assert_eq!(kept.status, 201, "{}", kept.text());
        assert_eq!(kept.json()["agent"]["label"], long);
        let plug = id_of(client, &reader, "frigo").await;
        let other = id_of(client, &reader, "Volet du salon").await;
        assert_ne!(plug, other);

        let state = client.get("/api/agents", session).await.json();
        assert_eq!(state["enabled"], true);
        assert_eq!(state["default_days"], 90);
        assert_eq!(state["max_days"], 365);
        assert_eq!(state["can"], json!({"control": true, "mode": true}));

        // La création : les droits cochés, la durée, l'appareil permis.
        let created = create(
            client,
            session,
            &json!({"label": "Claude Code sur le portable", "days": 30,
                    "scopes": ["control", "mode"], "devices": [plug]}),
        )
        .await;
        assert_eq!(created.status, 201, "{}", created.text());
        assert_eq!(
            created.headers.get("cache-control").map(String::as_str),
            Some("no-store")
        );
        let body = created.json();
        let token = body["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("ostal_agent_") && token.len() == 12 + 64);
        let agent = &body["agent"];
        assert_eq!(agent["label"], "Claude Code sur le portable");
        // « mode » suppose « occupation », « lecture » y est toujours.
        assert_eq!(
            agent["scopes"],
            json!(["read", "occupation", "control", "mode"])
        );
        assert_eq!(agent["devices"], json!([plug]));
        let days =
            (agent["expires"].as_f64().unwrap() - agent["created"].as_f64().unwrap()) / 86_400.0;
        assert!((days - 30.0).abs() < 0.01, "{days}");
        let id = agent["id"].as_str().unwrap().to_string();
        assert!(agent.get("token").is_none() && agent.get("hash").is_none());

        // Le jeton ne se relit nulle part : ni dans la liste, ni dans le fichier, ni au journal.
        let listed = client.get("/api/agents", session).await;
        assert!(!listed.text().contains(&token));
        assert!(!listed.text().contains("hash"));
        let file = std::fs::read_to_string(d.dir.join("serveur/agents.json")).unwrap();
        assert!(!file.contains(&token));
        let journal = serde_json::to_string(&audit_lines(&d.dir)).unwrap();
        assert!(!journal.contains(&token), "jeton au journal");
        let made = audit_of(&d.dir, "agent_created");
        assert_eq!(made.len(), 3);
        assert!(made
            .iter()
            .any(|l| l["target"] == "Claude Code sur le portable" && l["user_name"].is_string()));

        // Le serveur MCP donne exactement ce qui a été coché.
        let reader_tools = tool_names(client, &reader).await;
        assert!(!reader_tools.contains(&"ostal_set_on_off".to_string()));
        assert!(!reader_tools.contains(&"ostal_set_mode".to_string()));
        let tools = tool_names(client, &token).await;
        assert!(tools.contains(&"ostal_set_on_off".to_string()));
        assert!(tools.contains(&"ostal_set_mode".to_string()));
        assert!(tools.contains(&"ostal_get_mode".to_string()));
        // L'appareil permis se commande ; un autre, non.
        let sent = call(
            client,
            &token,
            "ostal_set_on_off",
            json!({"id": plug, "command": "Off"}),
        )
        .await;
        assert_eq!(sent["isError"], false, "{sent}");
        let refused = call(
            client,
            &token,
            "ostal_set_on_off",
            json!({"id": other, "command": "Off"}),
        )
        .await;
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["structuredContent"]["code"], "forbidden");

        // Révoquer : coupé à la requête suivante.
        let gone = client
            .request("DELETE", &format!("/api/agents/{id}"), Some(session), None)
            .await;
        assert_eq!(gone.status, 202, "{}", gone.text());
        let after = mcp(client, &token, "tools/list", json!({})).await;
        assert_eq!(after.status, 401);
        let again = client
            .request("DELETE", &format!("/api/agents/{id}"), Some(session), None)
            .await;
        assert_eq!(again.status, 404);
        assert_eq!(audit_of(&d.dir, "agent_revoked").len(), 1);
        let left = client.get("/api/agents", session).await.json();
        assert_eq!(left["agents"].as_array().unwrap().len(), 2);
    });
}

#[test]
fn every_guard_of_the_creation_refuses_and_writes_nothing() {
    let d = demo("web-agents-guards");
    switch_on(&d.dir);
    let (client, session) = (&d.client, d.session.as_str());
    let sdir = server_dir(&d.dir).unwrap();
    common::runtime().block_on(async {
        let reader = create(
            client,
            session,
            &json!({"label": "Lecture", "scopes": ["read"]}),
        )
        .await
        .json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let plug = id_of(client, &reader, "frigo").await;
        let door = id_of(client, &reader, "Porte").await;
        // Un volet de la démo joue l'appareil de la liste « jamais » (on_off ou cover : permis).
        let freezer_like = id_of(client, &reader, "Volet du salon").await;
        assert!(ids_of_kind(client, &reader, "cover")
            .await
            .contains(&freezer_like));
        agent_tokens::set_lists(&sdir, &[freezer_like.parse().unwrap()], &[]).unwrap();
        let count = || agent_tokens::list(&sdir).unwrap().1.len();
        assert_eq!(count(), 1);
        let bad = |body: Value, why: &'static str| {
            let client = client.clone();
            let session = session.to_string();
            async move {
                let resp = create(&client, &session, &body).await;
                assert_eq!(resp.status, 400, "{why} : {}", resp.text());
                assert_eq!(resp.code(), "bad_request", "{why}");
            }
        };
        bad(json!({"label": "", "scopes": ["read"]}), "libellé vide").await;
        bad(json!({"label": "   ", "scopes": ["read"]}), "libellé blanc").await;
        bad(
            json!({"label": "x".repeat(49), "scopes": ["read"]}),
            "libellé long",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read"], "days": 0}),
            "0 jour",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read"], "days": 366}),
            "366 jours",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["telepathie"]}),
            "droit inconnu",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read", "Control"]}),
            "casse",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read"], "enabled": true}),
            "champ en trop",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read"], "never": []}),
            "liste « jamais » en trop",
        )
        .await;
        bad(json!({"label": "A"}), "droits absents").await;
        bad(
            json!({"label": "A", "scopes": ["control"]}),
            "commandes sans appareil",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": []}),
            "commandes sans appareil (vide)",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["read"], "devices": [plug]}),
            "appareil sans droit de commander",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": ["0042"]}),
            "identifiant non canonique",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": [42]}),
            "identifiant numérique",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": ["999999999999"]}),
            "appareil inexistant",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": [door]}),
            "un capteur ne se commande pas",
        )
        .await;
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": [plug, freezer_like]}),
            "appareil de la liste « jamais »",
        )
        .await;
        let many: Vec<String> = (1..=21).map(|i| i.to_string()).collect();
        bad(
            json!({"label": "A", "scopes": ["control"], "devices": many}),
            "21 appareils",
        )
        .await;
        assert_eq!(count(), 1, "aucun refus n'a écrit un agent");

        // Du JSON invalide, et un corps trop gros : refusés avant tout.
        let garbled = client
            .request("POST", "/api/agents", Some(session), Some(b"{pas du json"))
            .await;
        assert_eq!(garbled.status, 400);
        let huge = json!({"label": "A", "scopes": ["read"], "pad": "x".repeat(8 * 1024)});
        let resp = create(client, session, &huge).await;
        assert_eq!(resp.status, 413, "{}", resp.text());
        assert_eq!(count(), 1);

        // Le libellé est unique par personne (sans tenir compte de la casse).
        let one = create(
            client,
            session,
            &json!({"label": "Cursor", "scopes": ["read"]}),
        )
        .await;
        assert_eq!(one.status, 201);
        bad(
            json!({"label": "cursor", "scopes": ["read"]}),
            "libellé déjà pris",
        )
        .await;
        // Seize agents au plus par personne : le dix-septième est refusé.
        for i in 0..14 {
            let ok = create(
                client,
                session,
                &json!({"label": format!("Agent {i}"), "scopes": ["read"]}),
            )
            .await;
            assert_eq!(ok.status, 201, "agent {i} : {}", ok.text());
        }
        assert_eq!(count(), 16);
        bad(
            json!({"label": "Le dix-septième", "scopes": ["read"]}),
            "17e agent",
        )
        .await;
        assert_eq!(count(), 16);

        // Aucune réponse d'erreur ne laisse passer un jeton.
        let nothing = create(
            client,
            session,
            &json!({"label": "A", "scopes": ["control"]}),
        )
        .await;
        assert!(!nothing.text().contains("ostal_agent_"));
    });
}

#[test]
fn nothing_the_web_can_send_changes_the_switch_the_lists_or_someone_elses_agents() {
    let d = demo("web-agents-owner");
    let sdir = server_dir(&d.dir).unwrap();
    account::add(&sdir, "lea", Some("Léa"), "le-mot-de-passe-de-lea").unwrap();
    // L'interrupteur reste éteint : l'app web ne l'allume pas.
    let (client, hamdi) = (&d.client, d.session.as_str());
    common::runtime().block_on(async {
        let created = create(
            client,
            hamdi,
            &json!({"label": "Portable de Hamdi", "scopes": ["read"]}),
        )
        .await;
        assert_eq!(created.status, 201, "{}", created.text());
        let hamdi_agent = created.json()["agent"]["id"].as_str().unwrap().to_string();
        let state = client.get("/api/agents", hamdi).await.json();
        assert_eq!(state["enabled"], false, "créer un agent n'allume rien");
        assert!(!agent_tokens::list(&sdir).unwrap().0);

        let lea = client
            .login_as("lea", "le-mot-de-passe-de-lea", common::PROTOCOL)
            .await;
        assert_eq!(lea.status, 200, "{}", lea.text());
        let lea = lea.json()["token"].as_str().unwrap().to_string();
        // Léa ne voit pas l'agent de Hamdi.
        let seen = client.get("/api/agents", &lea).await.json();
        assert_eq!(seen["agents"], json!([]));
        // Elle ne le révoque pas : même réponse qu'un agent qui n'existe pas.
        let theirs = client
            .request(
                "DELETE",
                &format!("/api/agents/{hamdi_agent}"),
                Some(&lea),
                None,
            )
            .await;
        let ghost = client
            .request(
                "DELETE",
                &format!("/api/agents/{}", "0".repeat(32)),
                Some(&lea),
                None,
            )
            .await;
        assert_eq!(theirs.status, 404);
        assert_eq!((theirs.status, theirs.code()), (ghost.status, ghost.code()));
        assert_eq!(theirs.text(), ghost.text());
        // Un libellé ne désigne pas un agent, et une forme louche non plus.
        for path in [
            "Portable%20de%20Hamdi",
            "..%2Fagents",
            "x",
            &"a".repeat(33),
            &hamdi_agent.to_uppercase(),
        ] {
            let resp = client
                .request("DELETE", &format!("/api/agents/{path}"), Some(&lea), None)
                .await;
            assert_eq!(resp.status, 404, "{path}");
            let resp = client
                .request("DELETE", &format!("/api/agents/{path}"), Some(hamdi), None)
                .await;
            assert_eq!(resp.status, 404, "{path} pour son propre compte");
        }
        assert_eq!(agent_tokens::list(&sdir).unwrap().1.len(), 1);
        // Léa a ses propres agents, avec le même libellé s'il lui plaît.
        let hers = create(
            client,
            &lea,
            &json!({"label": "Portable de Hamdi", "scopes": ["read"]}),
        )
        .await;
        assert_eq!(hers.status, 201);
        assert_eq!(
            client.get("/api/agents", &lea).await.json()["agents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            client.get("/api/agents", hamdi).await.json()["agents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Ni l'interrupteur ni les listes ne se règlent par l'API : pas de telle route.
        for method in ["PUT", "PATCH"] {
            let resp = client
                .send_json(
                    method,
                    "/api/agents",
                    hamdi,
                    &json!({"enabled": true, "never": ["1"]}),
                )
                .await;
            assert!(
                resp.status == 405 || resp.status == 404,
                "{method} {}",
                resp.status
            );
        }
        assert!(!agent_tokens::list(&sdir).unwrap().0);
        assert_eq!(agent_tokens::lists(&sdir).unwrap(), (vec![], vec![]));
    });
}

#[test]
fn the_account_can_only_give_the_rights_it_has_itself() {
    let dir = common::temp_dir("web-agents-rights");
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
    common::runtime().block_on(async {
        let session = client
            .login_as(
                in_process::USER,
                "mot-de-passe-jetable-du-test",
                common::PROTOCOL,
            )
            .await
            .json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let read = json!({"label": "Lecture", "scopes": ["read"]});
        let control = json!({"label": "Commandes", "scopes": ["control"], "devices": ["13828100"]});
        let mode = json!({"label": "Mode", "scopes": ["mode"]});
        rig.hub
            .rights()
            .deny(ostal_server::rights::Capability::ControlDevices);
        assert_eq!(create(&client, &session, &control).await.status, 403);
        // Le mode, lui, n'est pas retiré à ce compte.
        assert_eq!(create(&client, &session, &mode).await.status, 201);
        let state = client.get("/api/agents", &session).await.json();
        assert_eq!(state["can"], json!({"control": false, "mode": true}));
        rig.hub.rights().allow_all();
        rig.hub
            .rights()
            .deny(ostal_server::rights::Capability::ChangeMode);
        assert_eq!(
            create(
                &client,
                &session,
                &json!({"label": "Mode 2", "scopes": ["mode"]})
            )
            .await
            .status,
            403
        );
        // Le droit « mode » suppose « occupation » : ni l'un ni l'autre sans le droit de changer le mode.
        assert_eq!(
            create(
                &client,
                &session,
                &json!({"label": "Occ", "scopes": ["occupation"]})
            )
            .await
            .status,
            201
        );
        rig.hub.rights().allow_all();
        rig.hub
            .rights()
            .deny(ostal_server::rights::Capability::View);
        assert_eq!(create(&client, &session, &read).await.status, 403);
        rig.hub.rights().allow_all();
        assert_eq!(create(&client, &session, &read).await.status, 201);
        // Seuls les agents des créations réussies existent.
        let labels: Vec<String> = agent_tokens::list(&sdir)
            .unwrap()
            .1
            .into_iter()
            .map(|a| a.label)
            .collect();
        assert_eq!(labels.len(), 3, "{labels:?}");
        assert!(!labels.contains(&"Commandes".to_string()));
        assert!(!labels.contains(&"Mode 2".to_string()));
    });
}

#[test]
fn cutting_everything_leaves_no_old_approval_usable_by_a_new_agent() {
    let d = demo("web-agents-cut");
    let sdir = server_dir(&d.dir).unwrap();
    switch_on(&d.dir);
    let (client, session) = (&d.client, d.session.as_str());
    common::runtime().block_on(async {
        let reader = create(
            client,
            session,
            &json!({"label": "Lecture", "scopes": ["read"]}),
        )
        .await
        .json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let plug = id_of(client, &reader, "frigo").await;
        agent_tokens::set_lists(&sdir, &[], &[plug.parse().unwrap()]).unwrap();
        let body = json!({"label": "Claude Code", "scopes": ["control"], "devices": [plug]});
        let old = create(client, session, &body).await.json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        // Un appareil sensible : la commande attend une personne, qui l'autorise.
        let asked = call(
            client,
            &old,
            "ostal_set_on_off",
            json!({"id": plug, "command": "Off"}),
        )
        .await;
        let id = asked["structuredContent"]["confirmation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let requests = client.get("/api/agent-requests", session).await.json();
        assert_eq!(requests["requests"].as_array().unwrap().len(), 1);
        let approve = client
            .request(
                "POST",
                &format!("/api/agent-requests/{id}/approve"),
                Some(session),
                None,
            )
            .await;
        assert_eq!(approve.status, 202);

        // « Tout couper » (ce que fait la fenêtre d'Ostal Server) : interrupteur éteint, agents retirés.
        let cut = agent_tokens::cut_all(&sdir).unwrap();
        assert_eq!(cut.len(), 2);
        assert_eq!(mcp(client, &old, "tools/list", json!({})).await.status, 404);
        // La demande autorisée n'est plus proposée, et ne peut plus être décidée.
        assert!(
            client.get("/api/agent-requests", session).await.json()["requests"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // On rallume et on recrée un agent de même nom, mêmes droits : l'ancienne autorisation
        // ne lui sert à rien (elle appartient à l'ancien jeton).
        switch_on(&d.dir);
        assert_eq!(mcp(client, &old, "tools/list", json!({})).await.status, 401);
        let fresh = create(client, session, &body).await.json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let replay = call(
            client,
            &fresh,
            "ostal_set_on_off",
            json!({"id": plug, "command": "Off", "confirmation_id": id}),
        )
        .await;
        assert_eq!(replay["isError"], true, "{replay}");
        assert_eq!(replay["structuredContent"]["code"], "confirmation_invalid");
    });
}
