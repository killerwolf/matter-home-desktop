//! La couche connexion du client contre le vrai `matter-home-server --demo`, lancé comme
//! processus sur 127.0.0.1 (dossier de données temporaire, HOME factice).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use matter_home_client::net::tls::Trust;
use matter_home_client::net::{
    http, Body, Event, Link, NetError, Request, Saved, ServerAddr, Store,
};
use protocol::wire::{ErrorCode, HistoryResponse, ImportResponse, Metric, WireEvent};

const T: Duration = Duration::from_secs(30);

fn store_path(dir: &Path) -> PathBuf {
    dir.join("client").join("connexion.json")
}

fn saved_json(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(store_path(dir)).unwrap()).unwrap()
}

/// Requête directe, comme le ferait un autre client.
fn direct(server: &Server, token: &str, request: &Request) -> http::Response {
    http::exec(
        &ServerAddr::parse(&server.url()).unwrap(),
        &Trust::Pinned(server.fingerprint.clone()),
        Some(token),
        request,
    )
    .unwrap()
}

#[test]
fn first_connection_pins_the_certificate_and_receives_the_full_state() {
    let dir = temp_dir("first");
    let server = Server::start(&dir);
    let path = store_path(&dir);
    let mut client = TestClient::new(path.clone());
    client.wait_link(T, |l| {
        matches!(
            l,
            Link::LoggedOut {
                url: None,
                pinned: false,
                ..
            }
        )
    });

    // Rien n'est conservé tant que l'utilisateur n'a pas approuvé l'empreinte.
    client.backend.probe(&server.url());
    let fingerprint = client.wait_for(T, |e| match e {
        Event::Probed { fingerprint, .. } => Some(fingerprint.clone()),
        _ => None,
    });
    assert_eq!(
        fingerprint, server.fingerprint,
        "empreinte lue = empreinte du serveur"
    );
    assert!(!path.exists());
    assert!(
        !server.log().contains("connexion acceptée"),
        "la sonde n'a rien envoyé au serveur"
    );

    client
        .backend
        .login(&server.url(), &fingerprint, USER, PASSWORD);
    client.wait_link(T, |l| matches!(l, Link::Connecting));
    client.wait_link(T, |l| matches!(l, Link::Connected { .. }));
    let snapshot = client.wait_for(T, |e| match e {
        Event::Snapshot(s) => Some(s.clone()),
        _ => None,
    });
    assert!(
        snapshot.devices.len() >= 7,
        "appareils simulés : {}",
        snapshot.devices.len()
    );
    assert!(snapshot
        .devices
        .iter()
        .any(|d| d.device.node_id == LIVING
            && d.latest.iter().any(|l| l.metric == Metric::Temperature)));

    // Fichier : adresse, empreinte, jeton ; en 0600 ; pas le mot de passe.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&fingerprint) && text.contains("token"));
    assert!(!text.contains(PASSWORD));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Relance du client : la connexion se rétablit sans mot de passe, certificat déjà épinglé.
    drop(client);
    let mut again = TestClient::new(path);
    again.wait_link(T, |l| matches!(l, Link::Connected { .. }));
    again.wait_for(T, |e| matches!(e, Event::Snapshot(_)).then_some(()));
    // Le mot de passe n'a jamais été journalisé par le serveur.
    assert!(!server.log().contains(PASSWORD));
}

#[test]
fn wrong_password_is_translated_and_nothing_is_saved() {
    let dir = temp_dir("badpw");
    let server = Server::start(&dir);
    let path = store_path(&dir);
    let mut client = TestClient::new(path.clone());
    client.backend.login(
        &server.url(),
        &server.fingerprint,
        USER,
        "pas-le-bon-mot-de-passe",
    );
    let err = client.wait_for(T, |e| match e {
        Event::LoginFailed(err) => Some(err.clone()),
        _ => None,
    });
    assert_eq!(err.api_code(), Some(ErrorCode::InvalidCredentials));
    assert_eq!(err.message(), "Identifiant ou mot de passe incorrect.");
    assert!(!path.exists());
    assert!(!server.log().contains("pas-le-bon-mot-de-passe"));
}

#[test]
fn a_changed_certificate_is_refused_with_an_explicit_way_out() {
    let dir_a = temp_dir("pin-a");
    let mut server_a = Server::start(&dir_a);
    let path = store_path(&dir_a);
    let mut client = TestClient::new(path.clone());
    connect_first_time(&mut client, &server_a);
    let port = server_a.addr.port();
    let pinned = server_a.fingerprint.clone();
    drop(client);
    server_a.kill();

    // Un autre serveur (autre certificat) prend la même adresse.
    let dir_b = temp_dir("pin-b");
    let server_b = Server::start_on(&dir_b, &format!("127.0.0.1:{port}"));
    assert_ne!(server_b.fingerprint, pinned);
    let before = std::fs::read_to_string(&path).unwrap();

    let mut client = TestClient::new(path.clone());
    let link = client.wait_link(Duration::from_secs(60), |l| {
        matches!(l, Link::CertificateChanged { .. })
    });
    match link {
        Link::CertificateChanged {
            expected, found, ..
        } => {
            assert_eq!(expected, pinned);
            assert_eq!(found, server_b.fingerprint);
        }
        _ => unreachable!(),
    }
    // Pas de contournement silencieux : aucune reconnexion, aucune donnée, fichier intact.
    client.expect_quiet(Duration::from_secs(3), "après le refus du certificat");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert!(!server_b.log().contains("connexion acceptée"));

    // Même pour une nouvelle connexion avec le mot de passe : refusée avant tout envoi.
    client
        .backend
        .login(&server_b.url(), &pinned, USER, PASSWORD);
    let err = client.wait_for(T, |e| match e {
        Event::LoginFailed(err) => Some(err.clone()),
        _ => None,
    });
    assert!(matches!(err, NetError::CertificateChanged { .. }));
    assert!(
        !server_b.log().contains("connexion acceptée"),
        "le mot de passe n'a pas été envoyé au faux serveur"
    );

    // Geste explicite : réinitialiser l'empreinte, puis approuver la nouvelle.
    client
        .backend
        .reset_pinned_certificate(Some(server_b.url()));
    client.wait_link(T, |l| {
        matches!(
            l,
            Link::LoggedOut {
                url: Some(_),
                pinned: false,
                ..
            }
        )
    });
    assert!(!path.exists());
    client.backend.probe(&server_b.url());
    let fp = client.wait_for(T, |e| match e {
        Event::Probed { fingerprint, .. } => Some(fingerprint.clone()),
        _ => None,
    });
    assert_eq!(fp, server_b.fingerprint);
}

#[test]
fn incompatible_protocol_is_reported_clearly() {
    let dir = temp_dir("proto");
    let server = Server::start(&dir);
    let path = store_path(&dir);
    // Un client qui parlerait la version 99.
    let mut client = TestClient::with_protocol(path.clone(), Some(99));
    client
        .backend
        .login(&server.url(), &server.fingerprint, USER, PASSWORD);
    let err = client.wait_for(T, |e| match e {
        Event::LoginFailed(err) => Some(err.clone()),
        _ => None,
    });
    assert_eq!(err.api_code(), Some(ErrorCode::VersionIncompatible));
    assert!(
        err.message().contains("version du protocole"),
        "{}",
        err.message()
    );
    assert!(!path.exists());

    // Jeton valide mais protocole différent : message clair, sans boucle de reconnexion.
    let mut ok = TestClient::new(path.clone());
    connect_first_time(&mut ok, &server);
    drop(ok);
    let mut stale = TestClient::with_protocol(path, Some(99));
    let link = stale.wait_link(T, |l| matches!(l, Link::Incompatible { .. }));
    match link {
        Link::Incompatible { message } => {
            assert!(
                message.contains("version 99") && message.contains("version 1"),
                "{message}"
            )
        }
        _ => unreachable!(),
    }
    stale.expect_quiet(Duration::from_secs(2), "après l'incompatibilité");
}

#[test]
fn a_command_has_a_visible_effect_in_the_events() {
    let dir = temp_dir("cmd");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);

    let reply = client
        .call(Request::json(
            "POST",
            format!("/api/devices/{PLUG}/on-off"),
            &serde_json::json!({"command": "Off"}),
        ))
        .unwrap();
    assert_eq!(reply.status, 202);
    // L'effet arrive par le WebSocket : la prise s'éteint.
    client.wait_for(Duration::from_secs(20), |e| match e {
        Event::Live(ev) => match **ev {
            WireEvent::Reading {
                node_id,
                metric: Metric::OnOff,
                value,
                ..
            } if node_id == PLUG && value == 0.0 => Some(()),
            _ => None,
        },
        _ => None,
    });

    // Un identifiant inconnu : le code stable devient une phrase française.
    let err = client
        .call(Request::json(
            "POST",
            "/api/devices/42/on-off",
            &serde_json::json!({"command": "On"}),
        ))
        .unwrap_err();
    assert_eq!(err.api_code(), Some(ErrorCode::NotFound));
    assert!(
        err.message().starts_with("Introuvable"),
        "{}",
        err.message()
    );
}

#[test]
fn history_csv_and_plan_come_from_the_server() {
    let dir = temp_dir("hist");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);

    let since = protocol::events::now_unix() - 3600.0;
    let history: HistoryResponse = client
        .call(Request::get(format!(
            "/api/devices/{LIVING}/history?since={since}&metrics=temperature,humidity"
        )))
        .unwrap()
        .json()
        .unwrap();
    assert!(history
        .points
        .iter()
        .any(|p| p.metric == Metric::Temperature));
    assert!(history.points.windows(2).all(|w| w[0].at <= w[1].at));

    let csv = client
        .call(Request::get(format!(
            "/api/devices/{LIVING}/history.csv?since={since}"
        )))
        .unwrap();
    assert!(String::from_utf8(csv.body)
        .unwrap()
        .starts_with("timestamp_unix"));

    // Le plan : lu, remplacé, rediffusé à tous (dernière modification gagnante).
    let plan: serde_json::Value = client
        .call(Request::get("/api/plan"))
        .unwrap()
        .json()
        .unwrap();
    let mut changed = plan.clone();
    changed["floors"][0]["name"] = serde_json::json!("Rez-de-chaussée testé");
    let id = client
        .backend
        .call(Request::json("PUT", "/api/plan", &changed));
    // La diffusion peut arriver avant ou après la réponse : on attend les deux.
    let (mut replied, mut broadcast) = (false, false);
    client.wait_for(Duration::from_secs(20), |e| {
        match e {
            Event::Reply { id: got, result } if *got == id => {
                assert!(result.is_ok(), "{result:?}");
                replied = true;
            }
            Event::Live(ev) => {
                if let WireEvent::PlanChanged { plan } = &**ev {
                    broadcast |= plan.0.floors[0].name == "Rez-de-chaussée testé";
                }
            }
            _ => {}
        }
        (replied && broadcast).then_some(())
    });
}

/// Contrôleur minimal mais valide (même exemple que les tests du serveur).
const CONTROLLER: &str = r#"{"version":1,"identity":{"fabric_id":1,"controller_node_id":2,
    "ca_private_key":"0101010101010101010101010101010101010101010101010101010101010101","ca_cert_der":"bb","controller_private_key":"0101010101010101010101010101010101010101010101010101010101010101","controller_cert_der":"dd"},
    "nodes":[{"node_id":7,"name":"Capteur"}]}"#;

#[test]
fn import_is_checked_first_then_applied_and_the_client_reconnects() {
    let dir = temp_dir("import");
    std::fs::write(dir.join("controller.json"), CONTROLLER).unwrap();
    let mut server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);

    // Corps de plusieurs Mo : sans `flush` du flux TLS, le serveur attendrait la fin du corps
    // jusqu'à son délai (408 après 15 s). Ici il répond tout de suite que ce n'est pas une
    // sauvegarde.
    let junk = vec![b'x'; 3 * 1024 * 1024];
    let started = std::time::Instant::now();
    let err = client
        .call(Request {
            body: Body::Bytes(junk),
            ..Request::new("POST", "/api/import?dry_run=1")
        })
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(err.api_code(), Some(ErrorCode::InvalidImport), "{err:?}");
    assert!(err
        .message()
        .starts_with("Ce fichier n'est pas une sauvegarde"));

    // Une vraie sauvegarde, celle que le serveur exporte lui-même.
    let export = client.call(Request::get("/api/export")).unwrap();
    assert!(export.body.len() > 100);

    // 1. Vérification (dry_run) : un résumé, rien n'est engagé.
    let check: ImportResponse = client
        .call(Request {
            body: Body::Bytes(export.body.clone()),
            ..Request::new("POST", "/api/import?dry_run=1")
        })
        .unwrap()
        .json()
        .unwrap();
    assert!(!check.restarting);
    assert_eq!(check.summary.devices, 1);
    assert!(!dir.join("import-en-attente.matterhome").exists());

    // 2. Import réel : accepté, puis le serveur s'arrête (code 75) pour être relancé.
    let applied: ImportResponse = client
        .call(Request {
            body: Body::Bytes(export.body),
            ..Request::new("POST", "/api/import")
        })
        .unwrap()
        .json()
        .unwrap();
    assert!(applied.restarting);
    let status = server
        .wait_exit(Duration::from_secs(20))
        .expect("le serveur doit quitter");
    assert_eq!(status.code(), Some(75));

    // Le client voit la liaison tomber, puis se reconnecte tout seul quand le serveur revient.
    client.wait_link(T, |l| matches!(l, Link::Reconnecting { .. }));
    let _server = Server::start_on(&dir, &format!("127.0.0.1:{}", server.addr.port()));
    client.wait_link(Duration::from_secs(90), |l| {
        matches!(l, Link::Connected { .. })
    });
    client.wait_for(T, |e| matches!(e, Event::Snapshot(_)).then_some(()));
}

#[test]
fn http_parallelism_is_bounded_below_the_server_limit_per_address() {
    let dir = temp_dir("par");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);
    // 60 requêtes d'un coup : le serveur ferme sans réponse au-delà de 16 connexions par
    // adresse. Toutes doivent aboutir.
    let mut pending: std::collections::HashSet<u64> = (0..60)
        .map(|_| client.backend.call(Request::get("/api/rule-runs")))
        .collect();
    let mut ok = 0;
    while !pending.is_empty() {
        let (id, result) = client.wait_for(Duration::from_secs(60), |e| match e {
            Event::Reply { id, result } => Some((*id, result.clone())),
            _ => None,
        });
        assert!(pending.remove(&id));
        assert!(result.is_ok(), "{result:?}");
        ok += 1;
    }
    assert_eq!(ok, 60);
}

#[test]
fn connection_loss_then_automatic_reconnection_with_a_fresh_state() {
    let dir = temp_dir("cut");
    let mut server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);
    let addr = server.addr;
    let pinned = server.fingerprint.clone();
    server.kill();

    // Coupure : l'interface est prévenue (elle affichera « déconnecté »).
    client.wait_link(T, |l| matches!(l, Link::Reconnecting { .. }));
    // Pendant la coupure, une commande échoue franchement.
    let err = client.call(Request::get("/api/state")).unwrap_err();
    assert!(
        matches!(err, NetError::Unreachable(_) | NetError::Timeout),
        "{err:?}"
    );

    // Le serveur revient (même dossier, même port, même certificat) : reconnexion sans
    // intervention, avec un nouvel état complet.
    let server = Server::start_on(&dir, &format!("127.0.0.1:{}", addr.port()));
    assert_eq!(server.fingerprint, pinned);
    client.wait_link(Duration::from_secs(90), |l| {
        matches!(l, Link::Connected { .. })
    });
    client.wait_for(T, |e| matches!(e, Event::Snapshot(_)).then_some(()));
    drop(server);
}

#[test]
fn a_revoked_token_sends_the_client_back_to_the_login_screen() {
    let dir = temp_dir("revoked");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);
    let token = saved_json(&dir)["token"].as_str().unwrap().to_string();

    // Une autre session (un autre client) révoque ce jeton.
    let revoke = direct(&server, &token, &Request::new("POST", "/api/logout"));
    assert_eq!(revoke.status, 204);

    let link = client.wait_link(Duration::from_secs(20), |l| {
        matches!(l, Link::LoggedOut { .. })
    });
    match link {
        Link::LoggedOut {
            url,
            pinned,
            reason,
        } => {
            assert!(pinned, "le certificat reste approuvé");
            assert_eq!(url.as_deref(), Some(server.url().as_str()));
            assert!(reason.unwrap().contains("reconnectez-vous"));
        }
        _ => unreachable!(),
    }
    // Le jeton est oublié, l'adresse et l'empreinte restent (pas de nouvelle confirmation).
    let v = saved_json(&dir);
    assert!(v["token"].is_null() && v["fingerprint"] == server.fingerprint.as_str());
    client.expect_quiet(
        Duration::from_secs(2),
        "après le retour à l'écran de connexion",
    );

    // Reconnexion avec le mot de passe, empreinte déjà épinglée.
    client
        .backend
        .login(&server.url(), &server.fingerprint, USER, PASSWORD);
    client.wait_for(T, |e| matches!(e, Event::Snapshot(_)).then_some(()));
}

#[test]
fn an_unknown_token_at_startup_is_the_same_as_a_revoked_one() {
    let dir = temp_dir("stale");
    let server = Server::start(&dir);
    let path = store_path(&dir);
    Store::at(path.clone())
        .save(&Saved {
            url: server.url(),
            fingerprint: server.fingerprint.clone(),
            token: Some("jeton-qui-n-existe-pas".into()),
        })
        .unwrap();
    let mut client = TestClient::new(path);
    client.wait_link(T, |l| {
        matches!(
            l,
            Link::LoggedOut {
                url: Some(_),
                pinned: true,
                reason: Some(_)
            }
        )
    });
}

#[test]
fn logout_revokes_the_token_and_keeps_only_the_address_and_the_fingerprint() {
    let dir = temp_dir("logout");
    let server = Server::start(&dir);
    let path = store_path(&dir);
    let mut client = TestClient::new(path.clone());
    connect_first_time(&mut client, &server);
    let token = saved_json(&dir)["token"].as_str().unwrap().to_string();

    client.backend.logout();
    let link = client.wait_link(T, |l| matches!(l, Link::LoggedOut { .. }));
    assert_eq!(
        link,
        Link::LoggedOut {
            url: Some(server.url()),
            pinned: true,
            reason: None
        }
    );
    let notice = client.wait_for(T, |e| match e {
        Event::Notice(n) => Some(n.clone()),
        _ => None,
    });
    assert!(notice.contains("révoqué"), "{notice}");
    // Le fichier garde l'adresse et l'empreinte, jamais le jeton révoqué.
    let kept = saved_json(&dir);
    assert_eq!(kept["url"], server.url());
    assert_eq!(kept["fingerprint"], server.fingerprint);
    assert!(kept["token"].is_null(), "{kept}");
    assert!(!std::fs::read_to_string(&path).unwrap().contains(&token));
    // Le jeton ne vaut plus rien côté serveur.
    let check = direct(&server, &token, &Request::get("/api/state"));
    assert_eq!(check.status, 401);
    // Plus aucun événement de données après la déconnexion, et plus de commande possible.
    client.expect_quiet(Duration::from_secs(2), "après la déconnexion");
    let err = client.call(Request::get("/api/state")).unwrap_err();
    assert!(matches!(err, NetError::Unreachable(_)));
}

#[test]
fn events_arrive_in_unbroken_sequence_without_resync() {
    let dir = temp_dir("seq");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);
    let mut seen = 0;
    client.wait_for(Duration::from_secs(20), |e| match e {
        Event::Live(_) => {
            seen += 1;
            (seen >= 5).then_some(())
        }
        Event::Link(Link::Resyncing | Link::Reconnecting { .. }) => {
            panic!("resynchronisation inattendue")
        }
        _ => None,
    });
}

/// Le serveur ferme une connexion muette après 60 s sans signe de vie du client : le client doit
/// répondre à ses pings (toutes les 20 s). La liaison doit donc survivre bien au-delà.
#[test]
fn the_link_survives_the_servers_pings_beyond_its_idle_limit() {
    let dir = temp_dir("pings");
    let server = Server::start(&dir);
    let mut client = TestClient::new(store_path(&dir));
    connect_first_time(&mut client, &server);
    let until = std::time::Instant::now() + Duration::from_secs(75);
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match client.events.recv_timeout(left) {
            Ok(Event::Link(link)) => panic!("la liaison a changé d'état : {link:?}"),
            Ok(Event::Snapshot(_)) => panic!("nouvel état complet inattendu"),
            Ok(_) | Err(_) => {}
        }
    }
}
