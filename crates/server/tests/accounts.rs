//! Tests d'intégration : comptes nominatifs. Migration du compte unique de la 2.0.0 (données
//! factices à sa forme), commandes `user …` pendant que le serveur tourne, sessions coupées par
//! utilisateur.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use common::*;
use ostal_server::security::account;
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const LEGACY_ID: &str = "00112233445566778899aabbccddeeff";
const OLD_TOKEN: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
const SAM_PASSWORD: &str = "le-mot-de-passe-de-sam";

/// Lance `ostal-server user …` (mot de passe par l'entrée standard quand il y en a un).
fn user(dir: &Path, args: &[&str], password: Option<&str>) -> (ExitStatus, String) {
    let mut command = Command::new(BIN);
    command.arg("user").args(args);
    if password.is_some() {
        command.arg("--password-stdin");
    }
    let mut child = command
        .arg("--data-dir")
        .arg(dir)
        .env("HOME", dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(p) = password {
        writeln!(stdin, "{p}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status, text)
}

/// Un dossier de données à la forme de la 2.0.0 : `compte.json` et une session ouverte
/// (`account_id`, pas d'époque), sans `comptes.json`. Tout est factice.
fn legacy_dir(tag: &str) -> std::path::PathBuf {
    let dir = temp_dir(tag);
    let scratch = temp_dir(&format!("{tag}-scratch"));
    account::create(&scratch, "Hamdi", PASSWORD, false).unwrap();
    let scratch_file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.join("comptes.json")).unwrap()).unwrap();
    let hash = scratch_file["users"][0]["password_hash"].as_str().unwrap();
    let serveur = dir.join("serveur");
    std::fs::create_dir_all(&serveur).unwrap();
    std::fs::write(
        serveur.join("compte.json"),
        serde_json::json!({
            "version": 1, "id": LEGACY_ID, "username": "Hamdi",
            "password_hash": hash, "created": 1_700_000_000.0,
        })
        .to_string(),
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    std::fs::write(
        serveur.join("sessions.json"),
        serde_json::json!({ "sessions": [{
            "id": hex::encode(Sha256::digest(OLD_TOKEN.as_bytes())),
            "account_id": LEGACY_ID,
            "created": now, "last_used": now,
            "expires": now + 86_400.0, "max_expires": now + 90.0 * 86_400.0,
        }]})
        .to_string(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(scratch);
    dir
}

#[test]
fn a_2_0_0_data_folder_keeps_working_and_the_account_becomes_the_first_user() {
    let dir = legacy_dir("accounts-legacy");
    let legacy_before = std::fs::read(dir.join("serveur/compte.json")).unwrap();
    let server = Server::start(&dir);
    let client = server.client();

    runtime().block_on(async {
        // La session de la 2.0.0 marche telle quelle, et le mot de passe aussi.
        assert_eq!(client.get("/api/state", OLD_TOKEN).await.status, 200);
        assert_eq!(
            client.login_as("Hamdi", PASSWORD, PROTOCOL).await.status,
            200
        );
        assert_eq!(
            client.login_as("hamdi", PASSWORD, PROTOCOL).await.status,
            200
        );
        assert_eq!(
            client
                .login_as("hamdi", "mauvais-mot-de-passe", PROTOCOL)
                .await
                .status,
            401
        );
    });
    // Migration non destructive : l'ancien fichier est intact, le nouveau est à côté.
    assert_eq!(
        std::fs::read(dir.join("serveur/compte.json")).unwrap(),
        legacy_before
    );
    let users: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("serveur/comptes.json")).unwrap()).unwrap();
    assert_eq!(users["users"].as_array().unwrap().len(), 1);
    assert_eq!(users["users"][0]["id"], LEGACY_ID);
    assert_eq!(users["users"][0]["role"], "admin");

    // Redémarrage : la même session est toujours valable, rien n'est refait.
    drop(server);
    let again = Server::start(&dir);
    runtime().block_on(async {
        assert_eq!(
            again.client().get("/api/state", OLD_TOKEN).await.status,
            200
        );
    });
    assert_eq!(
        std::fs::read(dir.join("serveur/compte.json")).unwrap(),
        legacy_before
    );
}

#[test]
fn user_commands_act_on_a_running_server_and_cut_only_that_users_sessions() {
    let dir = temp_dir("accounts-cli");
    let server = Server::start(&dir);
    let client = server.client();

    let (status, text) = user(
        &dir,
        &["add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD),
    );
    assert!(status.success(), "{text}");
    let (status, text) = user(&dir, &["add", "--username", "SAM"], Some(SAM_PASSWORD));
    assert!(!status.success() && text.contains("déjà pris"), "{text}");
    let (status, text) = user(&dir, &["list"], None);
    assert!(status.success(), "{text}");
    assert!(
        text.contains("hamdi") && text.contains("sam") && text.contains("Sam"),
        "{text}"
    );
    assert!(
        !text.contains("argon2") && !text.contains(SAM_PASSWORD),
        "{text}"
    );

    runtime().block_on(async {
        let hamdi = client.login().await;
        let resp = client.login_as("sam", SAM_PASSWORD, PROTOCOL).await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        let sam = resp.json()["token"].as_str().unwrap().to_string();
        assert_eq!(client.get("/api/state", &sam).await.status, 200);

        // Nouveau mot de passe de Sam : ses sessions tombent tout de suite, pas celles de Hamdi.
        let (status, text) = user(
            &dir,
            &["passwd", "--username", "sam"],
            Some("un-nouveau-mot-de-passe"),
        );
        assert!(status.success(), "{text}");
        assert_eq!(client.get("/api/state", &sam).await.status, 401);
        assert_eq!(client.get("/api/state", &hamdi).await.status, 200);
        assert_eq!(
            client.login_as("sam", SAM_PASSWORD, PROTOCOL).await.status,
            401
        );
        let resp = client
            .login_as("sam", "un-nouveau-mot-de-passe", PROTOCOL)
            .await;
        assert_eq!(resp.status, 200, "{}", resp.text());
        let sam = resp.json()["token"].as_str().unwrap().to_string();

        // Désactivation : sessions coupées, connexion refusée, la maison reste à Hamdi.
        let (status, text) = user(&dir, &["disable", "--username", "sam"], None);
        assert!(status.success(), "{text}");
        assert_eq!(client.get("/api/state", &sam).await.status, 401);
        assert_eq!(
            client
                .login_as("sam", "un-nouveau-mot-de-passe", PROTOCOL)
                .await
                .status,
            401
        );
        assert_eq!(client.get("/api/state", &hamdi).await.status, 200);

        // Le dernier compte actif ne se désactive pas.
        let (status, text) = user(&dir, &["disable", "--username", "hamdi"], None);
        assert!(
            !status.success() && text.contains("dernier compte actif"),
            "{text}"
        );
        assert_eq!(client.get("/api/state", &hamdi).await.status, 200);

        // Réactivation : on peut se reconnecter ; l'ancienne session, elle, ne revient pas.
        let (status, text) = user(&dir, &["enable", "--username", "sam"], None);
        assert!(status.success(), "{text}");
        assert_eq!(client.get("/api/state", &sam).await.status, 401);
        assert_eq!(
            client
                .login_as("sam", "un-nouveau-mot-de-passe", PROTOCOL)
                .await
                .status,
            200
        );
    });
    let log = server.log();
    assert!(!log.contains(SAM_PASSWORD) && !log.contains("un-nouveau-mot-de-passe"));
}

#[test]
fn sessions_cut_while_the_server_was_stopped_stay_cut_after_restart() {
    let dir = temp_dir("accounts-stopped");
    let mut server = Server::start(&dir);
    let token = runtime().block_on(async { server.client().login().await });
    let (status, text) = user(&dir, &["add", "--username", "sam"], Some(SAM_PASSWORD));
    assert!(status.success(), "{text}");
    let sam = runtime().block_on(async {
        let resp = server
            .client()
            .login_as("sam", SAM_PASSWORD, PROTOCOL)
            .await;
        resp.json()["token"].as_str().unwrap().to_string()
    });
    server.terminate();
    drop(server);
    let (status, text) = user(
        &dir,
        &["passwd", "--username", "sam"],
        Some("encore-un-mot-de-passe"),
    );
    assert!(status.success(), "{text}");
    let server = Server::start(&dir);
    runtime().block_on(async {
        let client = server.client();
        assert_eq!(client.get("/api/state", &sam).await.status, 401);
        assert_eq!(client.get("/api/state", &token).await.status, 200);
    });
}
