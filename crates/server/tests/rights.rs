//! La fondation des droits : chaque route qui modifie la maison passe par `can()`.
//!
//! - en V1 `can` répond toujours oui : aucune route de la table n'est refusée ;
//! - quand on retire une capacité (le crochet de test `Rights::deny`), toutes les routes qui la
//!   demandent répondent 403 `forbidden`, avant d'avoir lu le corps ou cherché l'équipement, et
//!   aucune autre n'est refusée ;
//! - un inventaire des `.route(` du code source oblige à classer toute nouvelle route : soit
//!   elle est dans la table (donc testée), soit elle est déclarée sans effet sur la maison.
//!
//! Un vrai serveur HTTPS dans le processus, dossier jetable, 127.0.0.1.

mod common;
#[path = "support/fake_camera.rs"]
mod fake_camera;
#[path = "support/in_process.rs"]
mod in_process;

use std::collections::BTreeSet;
use std::time::Duration;

use common::Client;
use ostal_server::rights::Capability;
use protocol::wire::PROTOCOL_VERSION;
use serde_json::{json, Value};

const PASSWORD: &str = "mot-de-passe-jetable-du-test";

/// Une route qui modifie la maison : sa méthode, son chemin tel qu'écrit dans le code, un chemin
/// réel, un corps valide et la capacité qu'elle demande.
struct Gated {
    method: &'static str,
    route: &'static str,
    path: &'static str,
    body: Option<Value>,
    needs: Capability,
}

fn gated() -> Vec<Gated> {
    use Capability::*;
    let g = |method, route, path, body: Option<Value>, needs| Gated {
        method,
        route,
        path,
        body,
        needs,
    };
    vec![
        g(
            "PUT",
            "/api/plan",
            "/api/plan",
            Some(json!({"floors": []})),
            EditPlan,
        ),
        g(
            "PUT",
            "/api/rules",
            "/api/rules",
            Some(json!({"rules": []})),
            EditAutomations,
        ),
        g(
            "POST",
            "/api/rules/{id}/test",
            "/api/rules/1/test",
            None,
            EditAutomations,
        ),
        g(
            "POST",
            "/api/pairing",
            "/api/pairing",
            Some(json!({"code": "00000000000"})),
            Pair,
        ),
        g("DELETE", "/api/devices/{id}", "/api/devices/1", None, Pair),
        g(
            "POST",
            "/api/devices/{id}/retry",
            "/api/devices/1/retry",
            None,
            ControlDevices,
        ),
        g(
            "PUT",
            "/api/devices/{id}/customization",
            "/api/devices/1/customization",
            Some(json!({"name": "Salon"})),
            EditPlan,
        ),
        g(
            "POST",
            "/api/devices/{id}/on-off",
            "/api/devices/1/on-off",
            Some(json!({"command": "On"})),
            ControlDevices,
        ),
        g(
            "POST",
            "/api/devices/{id}/cover",
            "/api/devices/1/cover",
            Some(json!({"command": "Open"})),
            ControlDevices,
        ),
        g(
            "DELETE",
            "/api/devices/{id}/history",
            "/api/devices/1/history",
            None,
            ControlDevices,
        ),
        g("GET", "/api/export", "/api/export", None, ExportImport),
        g(
            "POST",
            "/api/import",
            "/api/import?dry_run=1",
            Some(json!({})),
            ExportImport,
        ),
        g(
            "PUT",
            "/api/mode",
            "/api/mode",
            Some(json!({"mode": "nuit"})),
            ChangeMode,
        ),
        g(
            "PUT",
            "/api/presence",
            "/api/presence",
            Some(json!({"state": "home"})),
            ChangeMode,
        ),
        g(
            "PUT",
            "/api/push",
            "/api/push",
            Some(json!({"enabled": true})),
            EditAutomations,
        ),
        // Les extensions : activer, régler, chercher un lieu (un appel vers Internet une fois
        // activée) demandent « extensions » ; la lecture de la liste et de la météo est libre.
        g(
            "PUT",
            "/api/extensions/{id}",
            "/api/extensions/weather",
            Some(json!({})),
            ManageExtensions,
        ),
        g(
            "GET",
            "/api/extensions/weather/search",
            "/api/extensions/weather/search?q=Lyon",
            None,
            ManageExtensions,
        ),
        // Créer un agent en lecture : « voir la maison ». Les droits « commandes » et « mode » qu'il
        // peut porter demandent en plus ceux du compte (prouvé par
        // agents_web::the_account_can_only_give_the_rights_it_has_itself).
        g(
            "POST",
            "/api/agents",
            "/api/agents",
            Some(json!({"label": "Table des droits", "scopes": ["read"]})),
            View,
        ),
        g(
            "DELETE",
            "/api/events/{id}",
            "/api/events/1763500000000-1",
            None,
            ControlDevices,
        ),
    ]
}

/// Les routes qui ne modifient pas la maison, avec la raison : lecture, ou ce qui ne concerne que
/// la personne elle-même (sa session, ses connexions, ses jetons, ses abonnements).
const EXEMPT: &[(&str, &str)] = &[
    ("/api/version", "lecture, publique"),
    (
        "/api/extensions",
        "lecture (jamais un secret : seulement « défini » ou non)",
    ),
    ("/api/weather", "lecture"),
    (
        "/mcp",
        "jeton d'agent, pas une session : can(View), can(ControlDevices) et can(ChangeMode) sont vérifiés outil par outil dans net/mcp/tools.rs::call, prouvé par the_account_right_to_control_is_checked_and_journaled et every_tool_is_refused_when_the_account_loses_the_right_to_read",
    ),
    (
        "/api/agents/{id}",
        "révoque l'un de ses propres agents d'IA (jamais celui d'une autre personne : 404) ; la lecture et la création sont dans la table",
    ),
    (
        "/api/agent-requests",
        "lecture de ses propres demandes d'agent (jamais celles d'une autre personne)",
    ),
    (
        "/api/agent-requests/{id}/approve",
        "sa propre demande d'agent : le droit demandé dépend de l'action (commander ou changer le mode), vérifié par confirmations.rs avec can() de la capacité de la demande, prouvé par the_account_right_to_change_the_mode_is_checked_at_the_call_and_at_the_approval",
    ),
    (
        "/api/agent-requests/{id}/refuse",
        "refuser sa propre demande d'agent ne change rien dans la maison",
    ),
    ("/api/login", "ouvre une session"),
    ("/api/logout", "ferme sa propre session"),
    ("/api/me/installations", "lecture de ses connexions"),
    ("/api/me/installations/{id}", "coupe sa propre connexion"),
    ("/api/state", "lecture"),
    ("/api/ws", "lecture (flux d'événements)"),
    ("/api/rule-runs", "lecture"),
    ("/api/cameras", "lecture"),
    (
        "/api/devices/{id}/history",
        "lecture (la suppression est dans la table)",
    ),
    ("/api/devices/{id}/history.csv", "lecture"),
    ("/api/tokens", "lecture de ses jetons"),
    ("/api/tokens/mode", "révoque ses propres raccourcis"),
    ("/api/tokens/read", "révoque son propre widget"),
    ("/api/tokens/{id}", "révoque l'un de ses propres jetons"),
    (
        "/api/tokens/mode/shortcuts",
        "prépare ses propres raccourcis",
    ),
    ("/api/tokens/read/widget", "prépare son propre widget"),
    (
        "/app/raccourcis/perso/{id}/{file}",
        "téléchargement de ses raccourcis",
    ),
    ("/app", "fichiers de l'app web"),
    ("/app/", "fichiers de l'app web"),
    ("/app/{name}", "fichiers de l'app web"),
    (
        "wire::PUSH_SUBSCRIPTIONS_PATH",
        "ses propres abonnements aux notifications",
    ),
    (
        "wire::PUSH_SUBSCRIPTION_PATH",
        "ses propres abonnements aux notifications",
    ),
    (
        "wire::PUSH_TEST_PATH",
        "notification d'essai vers son propre appareil",
    ),
    (
        "protocol::presence::PEOPLE_PATH",
        "lecture (prénoms et états)",
    ),
    (
        "protocol::presence::PHONES_PATH",
        "lecture (noms de téléphones libres, jamais d'adresse)",
    ),
    (
        "protocol::presence::PHONES_REFRESH_PATH",
        "relève la box en lecture seule",
    ),
    (
        "protocol::presence::PHONE_PATH",
        "choisit son propre téléphone",
    ),
    ("wire::EVENT_SNAPSHOT_PATH", "lecture"),
    ("wire::EVENT_CLIP_PATH", "lecture"),
    ("wire::CAMERA_STREAM_PATH", "lecture (flux caméra)"),
    ("wire::CAMERA_SNAPSHOT_PATH", "lecture"),
];

/// Les chemins de la table, tels que le code source les écrit (les constantes du protocole y
/// gardent leur nom).
fn gated_routes_in_source_form() -> BTreeSet<String> {
    gated()
        .iter()
        .map(|g| match g.route {
            "/api/push" => "wire::PUSH_PATH".to_string(),
            "/api/events/{id}" => "wire::EVENT_PATH".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// Tous les `.route(` du code de `src/net`, hors modules de test.
fn routes_in_source() -> BTreeSet<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/net");
    let mut found = BTreeSet::new();
    // Les sous-dossiers aussi (`net/mcp/`).
    let mut files = Vec::new();
    let mut folders = vec![dir];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap().flatten() {
            if entry.path().is_dir() {
                folders.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    for file in files {
        let text = std::fs::read_to_string(file).unwrap();
        let code = text.split("#[cfg(test)]").next().unwrap();
        for part in code.split(".route(").skip(1) {
            let expr = part.split(',').next().unwrap().trim();
            found.insert(expr.trim_matches('"').to_string());
        }
    }
    // Les routes de lecture seule d'un module qui expose aussi une écriture portent la même
    // expression : l'ensemble les confond, c'est voulu (la table teste chaque méthode).
    found.remove("wire::EVENTS_PATH");
    found.insert("wire::EVENTS_PATH".to_string());
    found
}

#[test]
fn every_route_is_either_gated_or_declared_harmless() {
    let known: BTreeSet<String> = gated_routes_in_source_form()
        .into_iter()
        .chain(EXEMPT.iter().map(|(r, _)| r.to_string()))
        .chain(["wire::EVENTS_PATH".to_string()])
        .collect();
    let found = routes_in_source();
    let unclassified: Vec<_> = found.difference(&known).collect();
    assert!(
        unclassified.is_empty(),
        "route(s) ni dans la table de rights.rs (donc sans can()), ni déclarée(s) sans effet sur la \
         maison : {unclassified:?}"
    );
    let vanished: Vec<_> = known.difference(&found).collect();
    assert!(
        vanished.is_empty(),
        "classée(s) mais absente(s) du code : {vanished:?}"
    );
}

#[test]
fn each_modifying_route_asks_can_for_its_capability() {
    let dir = common::temp_dir("rights");
    let rig = in_process::start(in_process::Setup {
        data_dir: &dir,
        listen: "127.0.0.1:0",
        password: PASSWORD,
        camera_addr: "127.0.0.1:0",
        camera: Default::default(),
        client_write_timeout: Duration::from_secs(10),
    });
    let client = Client::new(rig.server.addr, &rig.fingerprint);
    let mut table = gated();
    common::runtime().block_on(async {
        let resp = client
            .login_as(in_process::USER, PASSWORD, PROTOCOL_VERSION)
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        let token = resp.json()["token"].as_str().unwrap().to_string();

        // Le plan et les règles : on renvoie ceux que le serveur a (un corps valide).
        for g in table.iter_mut() {
            if g.path == "/api/plan" || g.path == "/api/rules" {
                g.body = Some(client.get(g.path, &token).await.json());
            }
        }
        let table = table;

        let call = |g: &Gated| {
            let body = g.body.as_ref().map(|b| b.to_string());
            let (client, token) = (client.clone(), token.clone());
            let (method, path) = (g.method, g.path);
            async move {
                client
                    .request(
                        method,
                        path,
                        Some(&token),
                        body.as_deref().map(str::as_bytes),
                    )
                    .await
            }
        };

        // V1 : tout le monde peut tout faire, rien n'est refusé.
        rig.hub.rights().allow_all();
        for g in &table {
            let r = call(g).await;
            assert_ne!(
                r.status, 403,
                "{} {} refusée alors que can() dit oui",
                g.method, g.path
            );
        }

        // Une capacité retirée : exactement les routes qui la demandent répondent 403.
        for capability in Capability::ALL {
            rig.hub.rights().allow_all();
            rig.hub.rights().deny(capability);
            for g in &table {
                let r = call(g).await;
                if g.needs == capability {
                    assert_eq!(
                        (r.status, r.code().as_str()),
                        (403, "forbidden"),
                        "{} {} ne demande pas {capability:?} : {}",
                        g.method,
                        g.path,
                        r.text()
                    );
                } else {
                    assert_ne!(
                        r.status, 403,
                        "{} {} refusée alors que seule {capability:?} est retirée",
                        g.method, g.path
                    );
                }
            }
        }

        // Tout refusé : rien n'a changé dans la maison (mode, plan).
        rig.hub.rights().allow_all();
        let mode_before = client.get("/api/state", &token).await.json()["mode"].clone();
        let plan_before = client.get("/api/plan", &token).await.json();
        assert!(
            !mode_before.is_null(),
            "le mode doit figurer dans /api/state"
        );
        rig.hub.rights().deny_all();
        for g in &table {
            assert_eq!(call(g).await.status, 403, "{} {}", g.method, g.path);
        }
        // Les lectures, elles, restent ouvertes.
        for path in [
            "/api/state",
            "/api/plan",
            "/api/rules",
            "/api/cameras",
            "/api/events",
        ] {
            assert_eq!(client.get(path, &token).await.status, 200, "{path}");
        }
        assert_eq!(
            client.get("/api/state", &token).await.json()["mode"],
            mode_before
        );
        assert_eq!(client.get("/api/plan", &token).await.json(), plan_before);
    });
}
