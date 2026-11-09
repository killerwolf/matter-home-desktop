//! Tests d'intégration : redémarrage, import de sauvegarde, création du compte.
//! Le piège de l'import : `apply_pending` met l'ancien dossier de données de côté ; le compte,
//! les sessions et le certificat ne doivent ni partir dans un export ni disparaître.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use matter_home_server::backup;
use serde_json::json;
use tokio_rustls::TlsConnector;

const CONTROLLER: &str = r#"{"version":1,"identity":{"fabric_id":1,"controller_node_id":2,
    "ca_private_key":"0101010101010101010101010101010101010101010101010101010101010101","ca_cert_der":"308201973082013da00302010202022711300a06082a8648ce3d04030230223120301e060a2b0601040182a27c01040c1030303030303030303030303030303031301e170d3236313130393034343031315a170d3436313130343034343031315a30223120301e060a2b0601040182a27c01040c10303030303030303030303030303030313059301306072a8648ce3d020106082a8648ce3d030107034200046ff03b949241ce1dadd43519e6960e0a85b41a69a05c328103aa2bce1594ca163c4f753a55bf01dc53f6c0b0c7eee78b40c6ff7d25a96e2282b989cef71c144aa3633061300f0603551d130101ff040530030101ff300e0603551d0f0101ff040403020106301d0603551d0e041604142e062c79665e7cd2c32e461b7c57c32c00f41430301f0603551d230418301680142e062c79665e7cd2c32e461b7c57c32c00f41430300a06082a8648ce3d0403020348003045022100a0ad077f7009fe47e1b2e99f902dd9d488ff83c2d24a8c920b471bbecbf076d4022027849229042f7bab0ee158034ef6acc3771bcaf82569d4f79c1dee30a9a1164b","controller_private_key":"0101010101010101010101010101010101010101010101010101010101010101","controller_cert_der":"308201db30820180a00302010202022711300a06082a8648ce3d04030230223120301e060a2b0601040182a27c01040c1030303030303030303030303030303031301e170d3236313130393034343031315a170d3436313130343034343031315a30443120301e060a2b0601040182a27c01010c10303030303030303030303030303030323120301e060a2b0601040182a27c01050c10303030303030303030303030303030313059301306072a8648ce3d020106082a8648ce3d030107034200046ff03b949241ce1dadd43519e6960e0a85b41a69a05c328103aa2bce1594ca163c4f753a55bf01dc53f6c0b0c7eee78b40c6ff7d25a96e2282b989cef71c144aa38183308180300c0603551d130101ff04023000300e0603551d0f0101ff04040302078030200603551d250101ff0416301406082b0601050507030206082b06010505070301301d0603551d0e041604142e062c79665e7cd2c32e461b7c57c32c00f41430301f0603551d230418301680142e062c79665e7cd2c32e461b7c57c32c00f41430300a06082a8648ce3d0403020349003046022100a81b41f4af9c837b22b558e3da844b9d2392a52ab5ac13dd6bf7f6ec7d0ac8b1022100c665d7c8927aca81a8507f3c117edbdd2d58dd87261db6e153021a5b53a1ed9b"},
    "nodes":[{"node_id":7,"name":"Capteur"}]}"#;

const SECRET_FILES: [&str; 3] = ["tls-cert.pem", "tls-key.pem", "compte.json"];

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn fingerprint_command(dir: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_matter-home-server"))
        .args(["fingerprint", "--data-dir"])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn token_and_fingerprint_survive_a_restart() {
    let dir = temp_dir("restart");
    let mut first = Server::start(&dir);
    let fingerprint = first.fingerprint.clone();
    let token = runtime().block_on(first.client().login());

    // Droits : dossier du serveur en 0700, fichiers en 0600.
    let secret_dir = dir.join("serveur");
    assert_eq!(mode(&secret_dir), 0o700);
    for name in SECRET_FILES.iter().chain(&["sessions.json"]) {
        assert_eq!(mode(&secret_dir.join(name)), 0o600, "{name}");
    }
    // Le jeton n'est pas stocké en clair.
    assert!(!std::fs::read_to_string(secret_dir.join("sessions.json"))
        .unwrap()
        .contains(&token));

    assert_eq!(
        first.terminate().code(),
        Some(0),
        "arrêt propre sur SIGTERM"
    );
    assert!(
        first.log().contains(&fingerprint),
        "l'empreinte est journalisée"
    );

    let mut second = Server::start(&dir);
    assert_eq!(
        second.fingerprint, fingerprint,
        "même certificat après redémarrage"
    );
    assert_eq!(fingerprint_command(&dir), fingerprint);
    let client = second.client();
    runtime().block_on(async {
        // Le jeton d'avant le redémarrage est toujours valable.
        assert_eq!(client.get("/api/state", &token).await.status, 200);

        // Un client qui a mémorisé une autre empreinte refuse la connexion.
        let wrong = tls_config(&["00"; 32].join(":"));
        let tcp = tokio::net::TcpStream::connect(client.addr).await.unwrap();
        let attempt = TlsConnector::from(wrong)
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                tcp,
            )
            .await;
        assert!(attempt.is_err(), "un certificat inattendu doit être refusé");

        // La déconnexion aussi survit au redémarrage.
        assert_eq!(
            client
                .request("POST", "/api/logout", Some(&token), None)
                .await
                .status,
            204
        );
    });
    assert_eq!(second.terminate().code(), Some(0));
    let third = Server::start(&dir);
    assert_eq!(third.fingerprint, fingerprint);
    runtime().block_on(async {
        assert_eq!(third.client().get("/api/state", &token).await.status, 401);
        // Le compte, lui, fonctionne toujours.
        assert_eq!(third.client().login_as(USER, PASSWORD, 1).await.status, 200);
    });
}

#[test]
fn import_keeps_account_certificate_and_sessions() {
    let dir = temp_dir("import");
    std::fs::create_dir_all(dir.join("history")).unwrap();
    std::fs::write(dir.join("controller.json"), CONTROLLER).unwrap();
    std::fs::write(dir.join("maison.json"), "{\"version\":1,\"floors\":[]}").unwrap();
    std::fs::write(
        dir.join("history/B992B180DAC7AD3B.jsonl"),
        "{\"t\":1,\"k\":\"temperature\",\"v\":20}\n",
    )
    .unwrap();

    let mut first = Server::start(&dir);
    let fingerprint = first.fingerprint.clone();
    let client = first.client();
    let secret_dir = dir.join("serveur");
    let before: Vec<Vec<u8>> = SECRET_FILES
        .iter()
        .map(|n| std::fs::read(secret_dir.join(n)).unwrap())
        .collect();

    let token = runtime().block_on(async {
        let token = client.login().await;

        // Export : la sauvegarde complète (clés en clair), sans rien du serveur réseau.
        let export = client.get("/api/export", &token).await;
        assert_eq!(export.status, 200, "{}", export.text());
        assert!(export.headers["content-disposition"].contains(".matterhome"));
        let text = export.text();
        assert!(text.contains("ca_private_key"), "l'export contient les clés");
        for forbidden in ["tls-key", "tls-cert", "compte.json", "sessions.json", "argon2", "BEGIN"] {
            assert!(!text.contains(forbidden), "l'export contient « {forbidden} »");
        }

        // Imports refusés : le serveur continue, sans fuite de chemin local.
        let garbage = client.request("POST", "/api/import", Some(&token), Some(b"pas une sauvegarde")).await;
        assert_eq!((garbage.status, garbage.code().as_str()), (400, "invalid_import"));
        let evil = json!({
            "format": "matter-home-desktop-sauvegarde", "version": 1, "created": "2026-01-01T00:00:00+01:00",
            "files": {"controller.json": CONTROLLER, "serveur/tls-key.pem": "cle de l'attaquant"}
        });
        let evil = client.send_json("POST", "/api/import", &token, &evil).await;
        assert_eq!((evil.status, evil.code().as_str()), (400, "invalid_import"));
        for refused in [&garbage, &evil] {
            assert!(!refused.text().contains(dir.to_str().unwrap()), "{}", refused.text());
        }
        // Contenu qui ne se chargerait pas (clé du contrôleur invalide) : refusé avant tout dépôt.
        let bad_key = CONTROLLER.replace(&"01".repeat(32), "aa");
        let bad = json!({
            "format": "matter-home-desktop-sauvegarde", "version": 1, "created": "2026-01-01T00:00:00+01:00",
            "files": {"controller.json": bad_key}
        });
        let bad = client.send_json("POST", "/api/import", &token, &bad).await;
        assert_eq!((bad.status, bad.code().as_str()), (400, "invalid_import"));
        assert!(!bad.text().contains("aa\"") && !bad.text().contains(&"01".repeat(32)));
        assert!(!dir.join("import-en-attente.matterhome").exists());
        assert_eq!(client.get("/api/state", &token).await.status, 200);

        // Inspection (dry_run) : résumé, sans rien mettre de côté ni arrêter le serveur.
        let inspected = client
            .request("POST", "/api/import?dry_run=1", Some(&token), Some(text.as_bytes()))
            .await;
        assert_eq!(inspected.status, 200, "{}", inspected.text());
        let body = inspected.json();
        assert_eq!(body["restarting"], false);
        assert_eq!(body["summary"]["devices"], 1);
        assert_eq!(body["summary"]["rooms"], 0);
        assert_eq!(body["summary"]["rules"], 0);
        assert!(body["summary"]["created"].as_str().unwrap().starts_with("20"));
        let refused = client
            .request("POST", "/api/import?dry_run=1", Some(&token), Some(b"pas une sauvegarde"))
            .await;
        assert_eq!((refused.status, refused.code().as_str()), (400, "invalid_import"));
        let odd = client
            .request("POST", "/api/import?dry_run=peut-etre", Some(&token), Some(text.as_bytes()))
            .await;
        assert_eq!((odd.status, odd.code().as_str()), (400, "bad_request"));
        assert!(!dir.join("import-en-attente.matterhome").exists());
        assert_eq!(client.get("/api/state", &token).await.status, 200);

        // Import valide : accepté, puis le serveur quitte pour être relancé.
        let changed = text.replace("Capteur", "Capteur importé");
        let r = client.request("POST", "/api/import", Some(&token), Some(changed.as_bytes())).await;
        assert_eq!(r.status, 202, "{}", r.text());
        assert_eq!(r.json()["restarting"], true);
        assert_eq!(r.json()["summary"]["devices"], 1);
        token
    });
    let status = first
        .wait_exit(Duration::from_secs(20))
        .expect("le serveur doit quitter après l'import");
    assert_eq!(status.code(), Some(75), "code de sortie « à relancer »");

    // Le lancement suivant applique l'import (comme le fait main avant de démarrer).
    let message = backup::apply_pending(&dir)
        .expect("l'import doit s'appliquer")
        .expect("import en attente");
    assert!(message.contains("Import terminé"), "{message}");
    assert!(std::fs::read_to_string(dir.join("controller.json"))
        .unwrap()
        .contains("Capteur importé"));
    // Le dossier du serveur est passé tel quel dans le nouveau dossier de données.
    let after: Vec<Vec<u8>> = SECRET_FILES
        .iter()
        .map(|n| std::fs::read(secret_dir.join(n)).unwrap())
        .collect();
    assert_eq!(
        before, after,
        "certificat, clé et compte identiques après l'import"
    );
    assert_eq!(mode(&secret_dir), 0o700);

    let second = Server::start(&dir);
    assert_eq!(
        second.fingerprint, fingerprint,
        "l'import ne change pas l'empreinte"
    );
    runtime().block_on(async {
        let client = second.client();
        assert_eq!(
            client.login_as(USER, PASSWORD, 1).await.status,
            200,
            "le compte existe encore"
        );
        assert_eq!(
            client.get("/api/state", &token).await.status,
            200,
            "la session aussi"
        );
    });
}

#[test]
fn export_without_paired_devices_says_there_is_nothing() {
    let dir = temp_dir("noexport");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let token = client.login().await;
        let r = client.get("/api/export", &token).await;
        assert_eq!((r.status, r.code().as_str()), (404, "no_data"));
        assert!(!r.text().contains(dir.to_str().unwrap()));
    });
}

#[test]
fn account_creation_command() {
    let bin = env!("CARGO_BIN_EXE_matter-home-server");
    let dir = temp_dir("account");

    // Mot de passe trop court : refusé, rien n'est créé.
    let (status, text) = create_account(&dir, "hamdi", "court", &[]);
    assert!(!status.success(), "{text}");
    assert!(!dir.join("serveur/compte.json").exists());
    assert!(!text.contains("court\n") || text.contains("trop court"));

    // Création, puis un seul compte : la seconde création est refusée.
    let (status, text) = create_account(&dir, "hamdi", PASSWORD, &[]);
    assert!(status.success(), "{text}");
    assert!(
        !text.contains(PASSWORD),
        "le mot de passe ne s'affiche jamais"
    );
    let (status, text) = create_account(&dir, "autre", "un-autre-mot-de-passe", &[]);
    assert!(!status.success() && text.contains("--replace"), "{text}");
    // Remplacement : l'ancien mot de passe ne marche plus, les sessions sont fermées.
    std::fs::write(dir.join("serveur/sessions.json"), "{\"sessions\":[]}").unwrap();
    let (status, _) = create_account(&dir, "autre", "un-autre-mot-de-passe", &["--replace"]);
    assert!(status.success());
    assert!(!dir.join("serveur/sessions.json").exists());
    let server = Server::start(&dir);
    runtime().block_on(async {
        let client = server.client();
        assert_eq!(client.login_as("hamdi", PASSWORD, 1).await.status, 401);
        assert_eq!(
            client
                .login_as("autre", "un-autre-mot-de-passe", 1)
                .await
                .status,
            200
        );
    });
    drop(server);

    // Jamais de mot de passe en argument.
    let out = Command::new(bin)
        .args([
            "create-account",
            "--username",
            "x",
            "--password",
            "secret-en-argument",
            "--data-dir",
        ])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    // Sans terminal ni --password-stdin : refus explicite plutôt qu'une lecture silencieuse.
    let out = Command::new(bin)
        .args(["create-account", "--username", "x", "--data-dir"])
        .arg(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--password-stdin"));
}
