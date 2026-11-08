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
    "ca_private_key":"aa","ca_cert_der":"bb","controller_private_key":"cc","controller_cert_der":"dd"},
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
    let message = backup::apply_pending(&dir).expect("import en attente");
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
