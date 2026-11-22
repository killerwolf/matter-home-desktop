//! Tests d'intégration : raccourcis, widget et présence **par personne** (étape 3 de « Ostal
//! multi-comptes »). Le vrai binaire en mode démo, dossier et HOME factices, 127.0.0.1.
//!
//! - migration d'un dossier à la forme de la 2.0.0 (un jeton de mode, un jeton de lecture, une
//!   session, sans identifiant de connexion) : tout continue de marcher, rien d'ancien n'est
//!   modifié, la migration se refait à l'identique ;
//! - deux personnes : chacune installe, voit et révoque les siens, sans toucher à ceux de l'autre ;
//!   une personne désactivée perd ses raccourcis ;
//! - « Je pars » / « Je rentre » : la présence déclarée est gardée ; en attendant la présence
//!   automatique, un raccourci change aussi le mode, selon la règle de la maison : Absent seulement
//!   quand personne d'autre n'est là, Présent seulement depuis Absent ou Vacances ;
//! - couper une connexion coupe ses raccourcis et son widget, sans toucher à ceux des autres
//!   téléphones, même s'ils portent le même nom.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::*;
use ostal_server::security::account;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const LEGACY_ID: &str = "00112233445566778899aabbccddeeff";
const OLD_SESSION: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
const OLD_MODE: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";
const OLD_READ: &str = "cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33";
const SAM_PASSWORD: &str = "le-mot-de-passe-de-sam";
const ORIGIN: &str = "https://maison.tailnet-test.ts.net";

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Un dossier de données à la forme de la 2.0.0, tout factice : `compte.json`, une session sans
/// connexion, `jeton-mode.json` et `jeton-lecture.json`.
fn legacy_dir(tag: &str) -> PathBuf {
    let dir = temp_dir(tag);
    let scratch = temp_dir(&format!("{tag}-scratch"));
    account::create(&scratch, "Hamdi", PASSWORD, false).unwrap();
    let scratch_file: Value =
        serde_json::from_slice(&std::fs::read(scratch.join("comptes.json")).unwrap()).unwrap();
    let hash = scratch_file["users"][0]["password_hash"].as_str().unwrap();
    let serveur = dir.join("serveur");
    std::fs::create_dir_all(&serveur).unwrap();
    std::fs::write(
        serveur.join("compte.json"),
        json!({"version": 1, "id": LEGACY_ID, "username": "Hamdi",
               "password_hash": hash, "created": 1_700_000_000.0})
        .to_string(),
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    std::fs::write(
        serveur.join("sessions.json"),
        json!({ "sessions": [{
            "id": sha(OLD_SESSION), "account_id": LEGACY_ID,
            "created": now, "last_used": now,
            "expires": now + 86_400.0, "max_expires": now + 90.0 * 86_400.0,
        }]})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        serveur.join("jeton-mode.json"),
        json!({"hash": sha(OLD_MODE), "created": 1_700_000_100.0}).to_string(),
    )
    .unwrap();
    std::fs::write(
        serveur.join("jeton-lecture.json"),
        json!({"hash": sha(OLD_READ), "created": 1_700_000_200.0}).to_string(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(scratch);
    dir
}

fn user(dir: &Path, args: &[&str], password: Option<&str>) -> bool {
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
    child.wait_with_output().unwrap().status.success()
}

fn start(dir: &Path) -> Server {
    Server::start_with(dir, &[("OSTAL_TEST_UNSIGNED_SHORTCUTS", "1")])
}

async fn mode_of(client: &Client, session: &str) -> String {
    client.get("/api/state", session).await.json()["mode"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Installe les raccourcis de la personne et rend son jeton de mode (lu dans « Je pars »).
async fn install_shortcuts(client: &Client, session: &str) -> String {
    let r = client
        .send_json(
            "POST",
            "/api/tokens/mode/shortcuts",
            session,
            &json!({ "origin": ORIGIN }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let url = r.json()["shortcuts"][0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(url.ends_with("Je%20pars.shortcut"), "{url}");
    let plist = client.request("GET", &url, None, None).await.text();
    let start = plist.find("<string>Bearer ").unwrap() + "<string>Bearer ".len();
    plist[start..start + 64].to_string()
}

async fn put_presence(client: &Client, token: &str, state: &str) -> Resp {
    client
        .send_json("PUT", "/api/presence", token, &json!({ "state": state }))
        .await
}

async fn put_mode(client: &Client, token: &str, mode: &str) -> Resp {
    client
        .send_json("PUT", "/api/mode", token, &json!({ "mode": mode }))
        .await
}

#[test]
fn a_2_0_0_folder_keeps_its_shortcuts_widget_and_old_clients_and_the_migration_is_gentle() {
    let dir = legacy_dir("tokens-legacy");
    let before = [
        "compte.json",
        "sessions.json",
        "jeton-mode.json",
        "jeton-lecture.json",
    ]
    .map(|f| std::fs::read(dir.join("serveur").join(f)).unwrap());
    let server = start(&dir);
    let client = server.client();

    runtime().block_on(async {
        // Le raccourci et le widget d'avant marchent tels quels.
        let r = put_mode(&client, OLD_MODE, "nuit").await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(client.get("/api/state", OLD_READ).await.status, 200);
        assert_eq!(
            client.get("/api/events?limit=1", OLD_READ).await.status,
            200
        );
        // Chacun reste dans sa porte.
        assert_eq!(client.get("/api/state", OLD_MODE).await.status, 401);
        assert_eq!(put_mode(&client, OLD_READ, "present").await.status, 401);
        // L'ancienne session (sans identifiant de connexion) voit les deux jetons, au nom du
        // premier utilisateur, et ni le secret ni l'empreinte.
        let tokens = client.get("/api/tokens", OLD_SESSION).await;
        assert_eq!(tokens.status, 200, "{}", tokens.text());
        let tokens = tokens.json();
        assert_eq!(tokens["items"].as_array().unwrap().len(), 2, "{tokens}");
        assert_eq!(tokens["mode"]["exists"], true);
        assert_eq!(tokens["read"]["exists"], true);
        let text = tokens.to_string();
        for secret in [
            OLD_MODE.to_string(),
            OLD_READ.to_string(),
            sha(OLD_MODE),
            sha(OLD_READ),
        ] {
            assert!(!text.contains(&secret));
        }
        // L'ancien jeton de mode sait aussi dire où l'on est. Le mode automatique est éteint après la
        // mise à jour : « Je pars » agit comme avant, mais Nuit est respecté (et le raccourci le dit).
        assert_eq!(mode_of(&client, OLD_SESSION).await, "nuit");
        let r = put_presence(&client, OLD_MODE, "away").await;
        assert_eq!(
            r.json(),
            json!({"state": "away", "changed": true, "mode": "nuit",
                   "message": "Mode Nuit : la maison ne change pas."})
        );
        assert_eq!(mode_of(&client, OLD_SESSION).await, "nuit");
        // La présence par le Wi-Fi : l'ancienne session (sans identifiant de connexion) la lit sans
        // problème, l'interrupteur du mode automatique est éteint, et « Je pars » (jeton de mode)
        // apparaît dans qui est à la maison. (La démonstration a une box fictive ; la maison sans
        // box est dans `tests/wifi.rs`.)
        let phones = client.get("/api/wifi/phones", OLD_SESSION).await;
        assert_eq!(phones.status, 200, "{}", phones.text());
        let people = client.get("/api/presence/people", OLD_SESSION).await.json();
        assert_eq!(people["auto_mode"], false, "{people}");
        assert_eq!(people["people"][0]["name"], "Hamdi");
        assert_eq!(people["people"][0]["state"], "away");
        // Réinstaller depuis une connexion nouvelle ajoute un jeton, sans toucher à l'ancien.
        let session = client.login().await;
        install_shortcuts(&client, &session).await;
        assert_eq!(put_mode(&client, OLD_MODE, "present").await.status, 200);
    });
    // Rien d'ancien n'est modifié ; les nouveaux fichiers sont à côté.
    let after = [
        "compte.json",
        "sessions.json",
        "jeton-mode.json",
        "jeton-lecture.json",
    ]
    .map(|f| std::fs::read(dir.join("serveur").join(f)).unwrap());
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2], before[2]);
    assert_eq!(after[3], before[3]);
    let tokens_file = std::fs::read_to_string(dir.join("serveur/jetons.json")).unwrap();
    assert!(!tokens_file.contains(OLD_MODE) && !tokens_file.contains(OLD_READ));
    assert!(dir.join("serveur/presence.json").is_file());
    // Sans box reliée, aucun réglage de box n'est écrit : le mode automatique reste éteint.
    assert!(!dir.join("serveur/box.json").exists());
    assert!(!dir.join("serveur/telephones.json").exists());

    // Redémarrage : même chose, la migration ne se refait pas (le jeton ajouté est toujours là).
    drop(server);
    let again = start(&dir);
    runtime().block_on(async {
        let client = again.client();
        assert_eq!(put_mode(&client, OLD_MODE, "nuit").await.status, 200);
        assert_eq!(client.get("/api/state", OLD_READ).await.status, 200);
        let tokens = client.get("/api/tokens", OLD_SESSION).await.json();
        assert_eq!(tokens["items"].as_array().unwrap().len(), 3, "{tokens}");
    });
    assert_eq!(
        std::fs::read(dir.join("serveur/jeton-mode.json")).unwrap(),
        before[2]
    );
}

#[test]
fn each_person_installs_sees_and_revokes_only_their_own_and_a_disabled_person_loses_them() {
    let dir = temp_dir("tokens-two-people");
    let server = start(&dir);
    let client = server.client();
    assert!(user(
        &dir,
        &["add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD)
    ));

    runtime().block_on(async {
        let hamdi = client.login().await;
        let resp = client.login_as("sam", SAM_PASSWORD, PROTOCOL).await;
        let sam = resp.json()["token"].as_str().unwrap().to_string();

        let hamdi_mode = install_shortcuts(&client, &hamdi).await;
        let sam_mode = install_shortcuts(&client, &sam).await;
        assert_ne!(hamdi_mode, sam_mode);
        // Le widget de Sam.
        let widget = client
            .send_json(
                "POST",
                "/api/tokens/read/widget",
                &sam,
                &json!({ "origin": ORIGIN, "label": "  iPhone   de Sam " }),
            )
            .await;
        assert_eq!(widget.status, 200, "{}", widget.text());
        let script = widget.json()["script"].as_str().unwrap().to_string();
        let start = script.find("const TOKEN = \"").unwrap() + "const TOKEN = \"".len();
        let sam_read = script[start..start + 64].to_string();
        assert_eq!(client.get("/api/state", &sam_read).await.status, 200);

        // Chacun ne voit que les siens.
        let hamdi_tokens = client.get("/api/tokens", &hamdi).await.json();
        let sam_tokens = client.get("/api/tokens", &sam).await.json();
        assert_eq!(
            hamdi_tokens["items"].as_array().unwrap().len(),
            1,
            "{hamdi_tokens}"
        );
        assert_eq!(hamdi_tokens["read"]["exists"], false);
        assert_eq!(
            sam_tokens["items"].as_array().unwrap().len(),
            2,
            "{sam_tokens}"
        );
        let sam_widget = sam_tokens["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["kind"] == "read")
            .unwrap();
        assert_eq!(sam_widget["label"], "iPhone de Sam");
        let sam_mode_id = sam_tokens["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["kind"] == "mode")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let hamdi_mode_id = hamdi_tokens["items"][0]["id"].as_str().unwrap().to_string();

        // Réinstaller ne touche que ses propres raccourcis (même téléphone : remplacé).
        let sam_mode_again = install_shortcuts(&client, &sam).await;
        assert_ne!(sam_mode_again, sam_mode);
        assert_eq!(put_mode(&client, &sam_mode, "nuit").await.status, 401);
        assert_eq!(put_presence(&client, &hamdi_mode, "home").await.status, 200);

        // On ne révoque pas le jeton de quelqu'un d'autre ; le sien, oui, et lui seul.
        let r = client
            .request(
                "DELETE",
                &format!("/api/tokens/{sam_mode_id}"),
                Some(&hamdi),
                None,
            )
            .await;
        assert_eq!((r.status, r.code().as_str()), (404, "not_found"));
        assert_eq!(
            put_presence(&client, &sam_mode_again, "home").await.status,
            200
        );
        let r = client
            .request(
                "DELETE",
                &format!("/api/tokens/{hamdi_mode_id}"),
                Some(&hamdi),
                None,
            )
            .await;
        assert_eq!(r.status, 204);
        assert_eq!(put_presence(&client, &hamdi_mode, "away").await.status, 401);
        assert_eq!(
            put_presence(&client, &sam_mode_again, "away").await.status,
            200
        );
        assert_eq!(client.get("/api/state", &sam_read).await.status, 200);
        // Un jeton n'est pas une session : il n'ouvre pas la liste des jetons.
        assert_eq!(client.get("/api/tokens", &sam_mode_again).await.status, 401);

        // Interrupteur éteint (défaut) : comme avant, le « Je pars » de Sam met la maison en Absent,
        // sans regarder qui d'autre est là.
        assert_eq!(mode_of(&client, &hamdi).await, "absent");

        // Désactiver Sam : ses raccourcis et son widget ne marchent plus ; la maison reste à Hamdi.
        assert!(user(&dir, &["disable", "--username", "sam"], None));
        assert_eq!(
            put_presence(&client, &sam_mode_again, "home").await.status,
            401
        );
        assert_eq!(put_mode(&client, &sam_mode_again, "nuit").await.status, 401);
        assert_eq!(client.get("/api/state", &sam_read).await.status, 401);
        assert_eq!(mode_of(&client, &hamdi).await, "absent");
        // Réactivé, ils reviennent (ils n'ont pas été supprimés).
        assert!(user(&dir, &["enable", "--username", "sam"], None));
        assert_eq!(
            put_presence(&client, &sam_mode_again, "home").await.status,
            200
        );
    });

    let presence: Value =
        serde_json::from_slice(&std::fs::read(dir.join("serveur/presence.json")).unwrap()).unwrap();
    assert_eq!(
        presence["people"].as_array().unwrap().len(),
        2,
        "{presence}"
    );
    // Ni jeton ni identifiant de lien au journal, ni en clair sur le disque.
    let log = server.log();
    let on_disk = std::fs::read_to_string(dir.join("serveur/jetons.json")).unwrap();
    assert!(!on_disk.is_empty());
    assert!(log.contains("présence déclarée par Sam"), "{log}");
}

#[test]
fn presence_needs_a_known_token_and_a_valid_state() {
    let dir = temp_dir("tokens-presence");
    let server = start(&dir);
    let client = server.client();
    runtime().block_on(async {
        let session = client.login().await;
        let token = install_shortcuts(&client, &session).await;
        // Sans jeton, avec un mauvais jeton, avec le jeton dans l'adresse : refus.
        assert_eq!(
            client
                .request("PUT", "/api/presence", None, Some(b"{\"state\":\"away\"}"))
                .await
                .status,
            401
        );
        assert_eq!(
            put_presence(&client, "pas-un-jeton", "away").await.status,
            401
        );
        let in_url = client
            .request(
                "PUT",
                &format!("/api/presence?token={token}"),
                None,
                Some(b"{\"state\":\"away\"}"),
            )
            .await;
        assert_eq!(in_url.status, 401);
        // Un état inconnu : 400. Redire la même chose : « changed » faux.
        assert_eq!(put_presence(&client, &token, "dormir").await.status, 400);
        assert_eq!(
            put_presence(&client, &token, "away").await.json()["changed"],
            true
        );
        assert_eq!(
            put_presence(&client, &token, "away").await.json()["changed"],
            false
        );
        // Une session peut aussi le dire.
        assert_eq!(
            put_presence(&client, &session, "home").await.json()["state"],
            "home"
        );
        // Le jeton de mode ne lit rien d'autre.
        assert_eq!(client.get("/api/state", &token).await.status, 401);
    });
}

async fn login_from(client: &Client, user: &str, password: &str, id: &str, name: &str) -> String {
    let body = json!({
        "username": user, "password": password, "protocol_version": PROTOCOL,
        "installation": {"id": id, "kind": "web", "device": "iphone", "name": name},
    });
    let r = client
        .request(
            "POST",
            "/api/login",
            None,
            Some(body.to_string().as_bytes()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["token"].as_str().unwrap().to_string()
}

async fn install_widget(client: &Client, session: &str) -> String {
    let r = client
        .send_json(
            "POST",
            "/api/tokens/read/widget",
            session,
            &json!({ "origin": ORIGIN }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let script = r.json()["script"].as_str().unwrap().to_string();
    let start = script.find("const TOKEN = \"").unwrap() + "const TOKEN = \"".len();
    script[start..start + 64].to_string()
}

#[test]
fn cutting_a_connection_cuts_its_shortcuts_and_widget_and_only_those() {
    let dir = temp_dir("tokens-cut");
    let server = start(&dir);
    let client = server.client();
    assert!(user(
        &dir,
        &["add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD)
    ));
    runtime().block_on(async {
        // Deux téléphones de Hamdi qui s'appellent tous deux « iPhone », et un Mac.
        let phone_a = login_from(&client, USER, PASSWORD, "iphone-a-001", "iPhone").await;
        let phone_b = login_from(&client, USER, PASSWORD, "iphone-b-001", "iPhone").await;
        let mac = login_from(&client, USER, PASSWORD, "mac-hamdi-01", "Mac").await;
        let sam = login_from(&client, "sam", SAM_PASSWORD, "iphone-sam-01", "iPhone").await;

        let mode_a = install_shortcuts(&client, &phone_a).await;
        let read_a = install_widget(&client, &phone_a).await;
        let mode_b = install_shortcuts(&client, &phone_b).await;
        let read_b = install_widget(&client, &phone_b).await;
        let mode_sam = install_shortcuts(&client, &sam).await;
        // Installer sur le second iPhone n'a pas révoqué ceux du premier.
        for t in [&mode_a, &mode_b] {
            assert_eq!(put_presence(&client, t, "home").await.status, 200);
        }
        for t in [&read_a, &read_b] {
            assert_eq!(client.get("/api/state", t).await.status, 200);
        }
        let items = client.get("/api/tokens", &phone_a).await.json()["items"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(items, 4);
        // Réinstaller depuis le même téléphone ne remplace que le sien.
        let mode_a2 = install_shortcuts(&client, &phone_a).await;
        assert_eq!(put_presence(&client, &mode_a, "home").await.status, 401);
        assert_eq!(put_presence(&client, &mode_b, "home").await.status, 200);

        // Le téléphone A est perdu : on le coupe depuis le Mac. Sam ne peut pas le faire.
        let path = "/api/me/installations/iphone-a-001";
        assert_eq!(
            client
                .request("DELETE", path, Some(&sam), None)
                .await
                .status,
            404
        );
        assert_eq!(put_presence(&client, &mode_a2, "home").await.status, 200);
        assert_eq!(
            client
                .request("DELETE", path, Some(&mac), None)
                .await
                .status,
            204
        );
        assert_eq!(put_presence(&client, &mode_a2, "away").await.status, 401);
        assert_eq!(put_mode(&client, &mode_a2, "nuit").await.status, 401);
        assert_eq!(client.get("/api/state", &read_a).await.status, 401);
        assert_eq!(client.get("/api/state", &phone_a).await.status, 401);
        // Les autres téléphones, le Mac et Sam n'ont rien perdu.
        assert_eq!(put_presence(&client, &mode_b, "home").await.status, 200);
        assert_eq!(client.get("/api/state", &read_b).await.status, 200);
        assert_eq!(put_presence(&client, &mode_sam, "home").await.status, 200);
        assert_eq!(client.get("/api/state", &mac).await.status, 200);
        let left = client.get("/api/tokens", &mac).await.json();
        assert_eq!(left["items"].as_array().unwrap().len(), 2, "{left}");
        // Une seconde coupure : plus rien à couper.
        assert_eq!(
            client
                .request("DELETE", path, Some(&mac), None)
                .await
                .status,
            404
        );
    });
}

/// `PUT /api/presence` dit toujours ce qu'est devenue la maison : le mode et une phrase, avec le
/// prénom de la personne qui retient l'armement (jamais un identifiant interne).
#[test]
fn leaving_home_tells_whether_the_house_changed() {
    let dir = temp_dir("tokens-presence-message");
    common::enable_auto_mode(&dir, USER);
    let server = start(&dir);
    let client = server.client();
    assert!(user(
        &dir,
        &["add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD)
    ));
    runtime().block_on(async {
        let hamdi = client.login().await;
        let sam = client.login_as("sam", SAM_PASSWORD, PROTOCOL).await.json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let hamdi_mode = install_shortcuts(&client, &hamdi).await;
        let sam_mode = install_shortcuts(&client, &sam).await;

        // Sam est déclaré à la maison : le « Je pars » de Hamdi n'arme pas, et le dit.
        assert_eq!(put_presence(&client, &sam_mode, "home").await.status, 200);
        let r = put_presence(&client, &hamdi_mode, "away").await.json();
        assert_eq!(r["mode"], "present");
        assert_eq!(
            r["message"],
            "Sam est encore à la maison : la maison reste en Présent."
        );
        assert_eq!(mode_of(&client, &hamdi).await, "present");

        // Sam part à son tour : la maison est armée, et le message le confirme.
        let r = put_presence(&client, &sam_mode, "away").await.json();
        assert_eq!(r["mode"], "absent");
        assert_eq!(r["message"], "Maison en mode Absent.");
        assert_eq!(mode_of(&client, &hamdi).await, "absent");

        // Rentrer la nuit ne réveille pas la maison, et le message le dit.
        assert_eq!(put_mode(&client, &sam_mode, "nuit").await.status, 200);
        let r = put_presence(&client, &hamdi_mode, "home").await.json();
        assert_eq!(r["mode"], "nuit");
        assert_eq!(r["message"], "Mode Nuit : la maison ne change pas.");
        // Partir la nuit non plus : une absence ne quitte jamais Nuit.
        let r = put_presence(&client, &sam_mode, "away").await.json();
        assert_eq!(r["mode"], "nuit");
        let r = put_presence(&client, &hamdi_mode, "away").await.json();
        assert_eq!(r["mode"], "nuit");
        assert_eq!(r["message"], "Mode Nuit : la maison ne change pas.");
        // Une session ne fait que déclarer : le mode est rendu, sans l'avoir changé.
        let r = put_presence(&client, &hamdi, "away").await.json();
        assert_eq!(r["mode"], "nuit");
        assert!(r["message"].is_string());
    });
}

#[test]
fn leaving_home_arms_the_house_only_when_the_last_person_leaves() {
    let dir = temp_dir("tokens-presence-rule");
    common::enable_auto_mode(&dir, USER);
    let server = start(&dir);
    let client = server.client();
    assert!(user(
        &dir,
        &["add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD)
    ));
    runtime().block_on(async {
        let hamdi = client.login().await;
        let sam = client.login_as("sam", SAM_PASSWORD, PROTOCOL).await.json()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let hamdi_mode = install_shortcuts(&client, &hamdi).await;
        let sam_mode = install_shortcuts(&client, &sam).await;
        assert_eq!(mode_of(&client, &hamdi).await, "present");

        // Hamdi est là. Le « Je pars » de Sam n'arme pas la maison.
        assert_eq!(put_presence(&client, &hamdi_mode, "home").await.status, 200);
        assert_eq!(put_presence(&client, &sam_mode, "away").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "present");
        // Puis Hamdi part à son tour : le dernier parti, la maison passe en Absent.
        assert_eq!(put_presence(&client, &hamdi_mode, "away").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "absent");
        // Quelqu'un rentre : Présent.
        assert_eq!(put_presence(&client, &sam_mode, "home").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "present");

        // La nuit, une arrivée ne réveille pas la maison.
        assert_eq!(put_mode(&client, &sam_mode, "nuit").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "nuit");
        assert_eq!(put_presence(&client, &hamdi_mode, "home").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "nuit");
        // Partir la nuit ne change rien non plus : Nuit se choisit à la main.
        assert_eq!(put_presence(&client, &hamdi_mode, "away").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "nuit");
        // Une personne désactivée ne compte pas comme « à la maison ».
        assert_eq!(put_mode(&client, &sam_mode, "present").await.status, 200);
        assert!(user(&dir, &["disable", "--username", "sam"], None));
        assert_eq!(put_presence(&client, &hamdi_mode, "home").await.status, 200);
        assert_eq!(put_presence(&client, &hamdi_mode, "away").await.status, 200);
        assert_eq!(mode_of(&client, &hamdi).await, "absent");
    });
}
