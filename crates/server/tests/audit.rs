//! Tests d'intégration : le journal d'audit (`serveur/audit/AAAA-MM.jsonl`). Le vrai binaire en mode
//! démo, dossier et HOME factices, 127.0.0.1.
//!
//! - chaque événement du plan y entre, avec la personne, sa connexion et son adresse ;
//! - jamais de mot de passe, de jeton ni de code d'appairage, ni d'identifiant tapé qui n'est pas un
//!   compte ;
//! - un changement de mode dit d'où il vient : personne, raccourci, automatique ;
//! - la ligne de commande `ostal-server audit` le relit, et le journal reste hors des sauvegardes ;
//! - un ancien client (sans identifiant de connexion) se connecte et s'écrit « connexion inconnue ».

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use ostal_server::audit::{Action, AuditLog, Entry, Filter, Origin, Outcome};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");
const SAM_PASSWORD: &str = "le-mot-de-passe-de-sam";
const WRONG_PASSWORD: &str = "un-mauvais-mot-de-passe-a-ne-pas-garder";
/// Quelqu'un qui tape son mot de passe dans la case de l'identifiant.
const TYPED_IN_THE_USERNAME_BOX: &str = "MotDePasseTapeAuMauvaisEndroit42";
const ORIGIN: &str = "https://maison.tailnet-test.ts.net";
const PLUG: u64 = 0xD3_0004;

fn entries(dir: &Path) -> Vec<Entry> {
    AuditLog::of_data_dir(dir).read(&Filter::default())
}

fn of(entries: &[Entry], action: Action) -> Vec<&Entry> {
    entries.iter().filter(|e| e.action == action).collect()
}

/// Tout ce que contiennent les fichiers du journal, en un texte.
fn raw(dir: &Path) -> String {
    let audit = dir.join("serveur/audit");
    let mut text = String::new();
    for file in std::fs::read_dir(audit).unwrap().flatten() {
        text.push_str(&std::fs::read_to_string(file.path()).unwrap());
    }
    text
}

fn run(dir: &Path, args: &[&str], password: Option<&str>) -> (bool, String) {
    let mut command = Command::new(BIN);
    command.args(args);
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
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn installation(id: &str, name: &str) -> Value {
    json!({"id": id, "kind": "web", "device": "iphone", "name": name})
}

async fn login_with(client: &Client, user: &str, password: &str, inst: Option<Value>) -> Resp {
    let mut body = json!({"username": user, "password": password, "protocol_version": PROTOCOL});
    if let Some(i) = inst {
        body["installation"] = i;
    }
    client
        .request(
            "POST",
            "/api/login",
            None,
            Some(body.to_string().as_bytes()),
        )
        .await
}

fn token_of(resp: &Resp) -> String {
    assert_eq!(resp.status, 200, "{}", resp.text());
    resp.json()["token"].as_str().unwrap().to_string()
}

async fn put_mode(client: &Client, token: &str, body: Value) -> Resp {
    client.send_json("PUT", "/api/mode", token, &body).await
}

/// Installe les raccourcis de la personne et rend son jeton de mode.
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
    let plist = client.request("GET", &url, None, None).await.text();
    let start = plist.find("<string>Bearer ").unwrap() + "<string>Bearer ".len();
    plist[start..start + 64].to_string()
}

#[test]
fn every_event_of_the_plan_is_written_with_who_where_and_the_result() {
    let dir = temp_dir("audit-events");
    let server = Server::start(&dir);
    let client = server.client();
    // Le premier compte est créé par la ligne de commande avant le démarrage.
    let first = entries(&dir);
    assert_eq!(of(&first, Action::UserCreated).len(), 1);
    let created = of(&first, Action::UserCreated)[0];
    assert_eq!(created.connection.as_ref().unwrap().kind, "cli");
    assert_eq!(created.user, None);

    let (ok, _) = run(
        &dir,
        &["user", "add", "--username", "sam", "--display-name", "Sam"],
        Some(SAM_PASSWORD),
    );
    assert!(ok);

    runtime().block_on(async {
        // Connexion réussie : une ligne « login », et « nouvelle connexion » la première fois.
        let phone = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("iphone-0001", "iPhone de Hamdi")),
            )
            .await,
        );
        let again = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("iphone-0001", "iPhone de Hamdi")),
            )
            .await,
        );
        // Se reconnecter depuis la même installation ferme sa session précédente.
        assert_eq!(client.get("/api/state", &phone).await.status, 401);
        let phone = again;
        let now = entries(&dir);
        let logins = of(&now, Action::Login);
        assert_eq!(logins.len(), 2);
        assert!(logins.iter().all(|e| e.result == Outcome::Ok));
        assert_eq!(
            logins[0].connection.as_ref().unwrap().name,
            "iPhone de Hamdi"
        );
        assert_eq!(logins[0].ip.as_deref(), Some("127.0.0.1"));
        assert!(logins[0].user.is_some() && logins[0].user_name.is_some());
        assert_eq!(
            of(&now, Action::NewConnection).len(),
            1,
            "la même installation ne compte qu'une fois"
        );
        let hamdi_id = logins[0].user.clone().unwrap();

        // Un ancien client (sans identifiant de connexion) se connecte : « connexion inconnue ».
        let old = token_of(&login_with(&client, USER, PASSWORD, None).await);
        assert_eq!(client.get("/api/state", &old).await.status, 200);
        let now = entries(&dir);
        let unknown = of(&now, Action::Login)[0];
        assert!(unknown.connection.is_none() && unknown.result == Outcome::Ok);
        assert_eq!(
            ostal_server::audit::describe(unknown).1,
            "Connexion inconnue, sur place : l'app n'est pas encore à jour"
        );
        assert_eq!(of(&now, Action::NewConnection).len(), 1);

        // Connexion refusée : le compte est reconnu, le mot de passe n'est jamais gardé. Trois
        // essais d'affilée ne font qu'une ligne.
        for _ in 0..3 {
            let r = login_with(&client, USER, WRONG_PASSWORD, None).await;
            assert_eq!(r.status, 401);
        }
        let r = login_with(&client, TYPED_IN_THE_USERNAME_BOX, WRONG_PASSWORD, None).await;
        assert_eq!(r.status, 401);
        let now = entries(&dir);
        let refused: Vec<&Entry> = now
            .iter()
            .filter(|e| e.action == Action::Login && e.result == Outcome::Refused)
            .collect();
        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(refused
            .iter()
            .any(|e| e.user.as_deref() == Some(hamdi_id.as_str())));
        assert!(refused.iter().any(|e| e.user.is_none()));

        // Changement de mode par une personne, dans l'app web.
        let r = put_mode(
            &client,
            &phone,
            json!({"mode": "nuit", "source": {"kind": "web"}}),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let r = put_mode(&client, &phone, json!({"mode": "nuit"})).await;
        assert_eq!(r.json()["changed"], false);
        let now = entries(&dir);
        let modes = of(&now, Action::ModeChanged);
        assert_eq!(modes.len(), 1, "un mode inchangé n'écrit rien");
        assert_eq!(modes[0].target.as_deref(), Some("nuit"));
        assert_eq!(modes[0].source, Some(Origin::Person));
        assert_eq!(modes[0].user.as_deref(), Some(hamdi_id.as_str()));
        assert_eq!(
            modes[0].connection.as_ref().unwrap().name,
            "iPhone de Hamdi"
        );
        assert_eq!(
            ostal_server::audit::describe(modes[0]).1,
            "Mode Nuit, choisi dans l'app web"
        );

        // Commande d'un équipement : par son nom, avec la commande ; un échec est noté aussi.
        let r = client
            .send_json(
                "POST",
                &format!("/api/devices/{PLUG}/on-off"),
                &phone,
                &json!({"command": "Off"}),
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        let r = client
            .send_json(
                "POST",
                "/api/devices/999999/on-off",
                &phone,
                &json!({"command": "On"}),
            )
            .await;
        assert_eq!(r.status, 404);
        let now = entries(&dir);
        let commands = of(&now, Action::DeviceCommand);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[1].detail.as_deref(), Some("off"));
        assert!(commands[1].target.as_deref().is_some_and(|t| !t.is_empty()));
        assert_eq!(commands[1].result, Outcome::Ok);
        assert_eq!(commands[0].result, Outcome::Error);

        // Automatisations : modifiées, testées.
        let rules = client.get("/api/rules", &phone).await.json();
        let rule_id = rules["rules"][0]["id"].as_str().unwrap().to_string();
        let r = client.send_json("PUT", "/api/rules", &phone, &rules).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let r = client
            .request(
                "POST",
                &format!("/api/rules/{rule_id}/test"),
                Some(&phone),
                None,
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.text());
        let now = entries(&dir);
        assert_eq!(of(&now, Action::AutomationChanged).len(), 1);
        let tested = of(&now, Action::AutomationTested);
        assert_eq!(tested.len(), 1);
        assert!(tested[0].target.as_deref().is_some_and(|t| !t.is_empty()));

        // Appairage : le code n'entre jamais. Oubli d'un équipement inconnu : une erreur notée.
        let r = client
            .send_json(
                "POST",
                "/api/pairing",
                &phone,
                &json!({"code": "34970112332"}),
            )
            .await;
        let paired_status = r.status;
        let r = client
            .request("DELETE", "/api/devices/999999", Some(&phone), None)
            .await;
        assert_eq!(r.status, 404);
        let now = entries(&dir);
        assert_eq!(
            of(&now, Action::DevicePaired).len(),
            1,
            "appairage {paired_status}"
        );
        let removed = of(&now, Action::DeviceRemoved);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].result, Outcome::Error);

        // Export et import : écrits, même refusés (le mode démo n'exporte pas). Une vérification
        // d'import (`dry_run`) n'écrit rien. Rien de la sauvegarde n'entre au journal.
        let export = client
            .request("GET", "/api/export", Some(&phone), None)
            .await;
        assert_eq!(export.status, 404, "{}", export.text());
        let bundle = json!({
            "format": "ostal-sauvegarde", "version": 1,
            "created": "2026-11-09T09:15:00+01:00",
            "files": {"regles.json": json!({"version": 1, "rules": []}).to_string()},
        })
        .to_string();
        let r = client
            .request(
                "POST",
                "/api/import?dry_run=1",
                Some(&phone),
                Some(bundle.as_bytes()),
            )
            .await;
        assert_ne!(r.status, 500, "{}", r.text());
        let now = entries(&dir);
        assert_eq!(of(&now, Action::Export).len(), 1);
        assert_eq!(of(&now, Action::Export)[0].result, Outcome::Error);
        assert_eq!(of(&now, Action::Import).len(), 0);
        let r = client
            .request(
                "POST",
                "/api/import",
                Some(&phone),
                Some(b"pas une sauvegarde"),
            )
            .await;
        assert_eq!(r.status, 400, "{}", r.text());
        let now = entries(&dir);
        assert_eq!(of(&now, Action::Import).len(), 1);
        assert_eq!(of(&now, Action::Import)[0].result, Outcome::Error);

        // Connexions coupées : par leur propriétaire, ou par une déconnexion.
        let ipad = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("ipad-0002", "iPad du salon")),
            )
            .await,
        );
        let r = client
            .request(
                "DELETE",
                "/api/me/installations/ipad-0002",
                Some(&phone),
                None,
            )
            .await;
        assert_eq!(r.status, 204);
        assert_eq!(client.get("/api/state", &ipad).await.status, 401);
        let tablet = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("tablet-0003", "Tablette")),
            )
            .await,
        );
        let r = client
            .request("POST", "/api/logout", Some(&tablet), None)
            .await;
        assert_eq!(r.status, 204);
        let now = entries(&dir);
        let cuts = of(&now, Action::ConnectionCut);
        assert_eq!(cuts.len(), 2);
        assert_eq!(cuts[1].target.as_deref(), Some("iPad du salon"));
        assert_eq!(cuts[0].detail.as_deref(), Some("logout"));
    });

    // Les comptes, par la ligne de commande : créé, mot de passe changé, désactivé, réactivé.
    assert!(
        run(
            &dir,
            &["user", "passwd", "--username", "sam"],
            Some("un-autre-mot-de-passe-de-sam")
        )
        .0
    );
    assert!(run(&dir, &["user", "disable", "--username", "sam"], None).0);
    assert!(run(&dir, &["user", "enable", "--username", "sam"], None).0);
    let all = entries(&dir);
    assert_eq!(of(&all, Action::UserCreated).len(), 2);
    for (action, shown) in [
        (Action::PasswordChanged, "Sam"),
        (Action::UserDisabled, "Sam"),
        (Action::UserEnabled, "Sam"),
    ] {
        let found = of(&all, action);
        assert_eq!(found.len(), 1, "{action:?}");
        assert_eq!(found[0].target.as_deref(), Some(shown));
        assert_eq!(
            found[0].connection.as_ref().unwrap().name,
            "Ligne de commande"
        );
    }

    // Jamais de secret dans les fichiers du journal.
    let text = raw(&dir);
    for secret in [
        PASSWORD,
        SAM_PASSWORD,
        "un-autre-mot-de-passe-de-sam",
        WRONG_PASSWORD,
        TYPED_IN_THE_USERNAME_BOX,
        "34970112332",
        "Bearer",
    ] {
        assert!(!text.contains(secret), "« {secret} » dans le journal");
    }
    // Les droits : dossier 0700, fichiers 0600.
    {
        use std::os::unix::fs::PermissionsExt;
        let audit = dir.join("serveur/audit");
        assert_eq!(
            std::fs::metadata(&audit).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for file in std::fs::read_dir(&audit).unwrap().flatten() {
            assert_eq!(
                std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    // La ligne de commande relit le journal, la plus récente d'abord, et filtre par personne.
    let (ok, out) = run(&dir, &["audit", "--days", "1"], None);
    assert!(ok);
    assert!(out.contains("Connexion à l'app web, sur place"), "{out}");
    assert!(out.contains("Mode Nuit, choisi dans l'app web"), "{out}");
    assert!(out.contains("Compte de Sam désactivé"), "{out}");
    assert!(!out.contains("34970112332"));
    let (ok, only_sam) = run(&dir, &["audit", "--user", "sam"], None);
    assert!(ok);
    assert!(!only_sam.contains("Mode Nuit"), "{only_sam}");
    let (ok, only_hamdi) = run(&dir, &["audit", "--user", USER, "--days", "30"], None);
    assert!(ok);
    assert!(only_hamdi.contains("Mode Nuit"), "{only_hamdi}");
    assert!(!run(&dir, &["audit", "--days", "zéro"], None).0);
    drop(server);
}

#[test]
fn a_shortcut_says_where_it_comes_from_and_presence_changes_the_mode_automatically() {
    let dir = temp_dir("audit-shortcut");
    common::enable_auto_mode(&dir, USER);
    let server = Server::start_with(&dir, &[("OSTAL_TEST_UNSIGNED_SHORTCUTS", "1")]);
    let client = server.client();
    runtime().block_on(async {
        let session = token_of(
            &login_with(
                &client,
                USER,
                PASSWORD,
                Some(installation("iphone-0001", "iPhone de Hamdi")),
            )
            .await,
        );
        let token = install_shortcuts(&client, &session).await;

        // Un raccourci qui choisit le mode : « raccourci », depuis le téléphone qui l'a préparé.
        let r = put_mode(&client, &token, json!({"mode": "nuit"})).await;
        assert_eq!(r.status, 200, "{}", r.text());
        // « Je rentre » depuis la nuit ne change rien ; « Je pars » met la maison en Absent (le mode
        // automatique est allumé et un téléphone est relié : sans cela, rien ne change).
        let r = client
            .send_json("PUT", "/api/presence", &token, &json!({"state": "home"}))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let r = put_mode(&client, &token, json!({"mode": "present"})).await;
        assert_eq!(r.status, 200);
        let r = client
            .send_json("PUT", "/api/presence", &token, &json!({"state": "away"}))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.json()["mode"], "absent");

        let now = entries(&dir);
        let modes = of(&now, Action::ModeChanged);
        // La plus récente d'abord : absent (automatique), présent, nuit (raccourcis).
        assert_eq!(modes.len(), 3, "{modes:?}");
        assert_eq!(modes[0].target.as_deref(), Some("absent"));
        assert_eq!(modes[0].source, Some(Origin::Automatic));
        assert_eq!(
            ostal_server::audit::describe(modes[0]).1,
            "La maison passe en Absent (automatique)"
        );
        assert_eq!(modes[1].source, Some(Origin::Shortcut));
        assert_eq!(modes[2].source, Some(Origin::Shortcut));
        for m in &modes {
            assert!(m.user.is_some(), "attribué à celui qui a parlé");
            assert_eq!(m.connection.as_ref().unwrap().name, "iPhone de Hamdi");
        }
        let presence = of(&now, Action::Presence);
        assert_eq!(presence.len(), 2);
        assert_eq!(presence[0].target.as_deref(), Some("away"));
    });
    // Le jeton n'est jamais écrit.
    let text = raw(&dir);
    assert!(!text.contains("Bearer"));
    drop(server);
}

#[test]
fn a_2_0_0_folder_keeps_working_and_gets_its_journal_next_to_the_old_files() {
    // Un dossier à la forme de la 2.0.0 : `compte.json`, sans journal. Le premier démarrage crée
    // `serveur/audit/` à côté, sans rien modifier d'ancien.
    let dir = temp_dir("audit-legacy");
    let scratch = temp_dir("audit-legacy-scratch");
    ostal_server::security::account::create(&scratch, "Hamdi", PASSWORD, false).unwrap();
    let file: Value =
        serde_json::from_slice(&std::fs::read(scratch.join("comptes.json")).unwrap()).unwrap();
    let hash = file["users"][0]["password_hash"].as_str().unwrap();
    let serveur = dir.join("serveur");
    std::fs::create_dir_all(&serveur).unwrap();
    let legacy =
        json!({"version": 1, "id": "00112233445566778899aabbccddeeff", "username": "Hamdi",
        "password_hash": hash, "created": 1_700_000_000.0})
        .to_string();
    std::fs::write(serveur.join("compte.json"), &legacy).unwrap();
    assert!(!serveur.join("audit").exists());
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // Un ancien client : pas d'identifiant de connexion.
        let old = token_of(&login_with(&client, "Hamdi", PASSWORD, None).await);
        assert_eq!(client.get("/api/state", &old).await.status, 200);
    });
    assert_eq!(
        std::fs::read_to_string(serveur.join("compte.json")).unwrap(),
        legacy
    );
    let all = entries(&dir);
    assert_eq!(of(&all, Action::Login).len(), 1);
    assert!(of(&all, Action::Login)[0].connection.is_none());
    drop(server);
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
fn a_lockout_is_written_once_per_minute_not_once_per_request() {
    let dir = temp_dir("audit-lockout");
    let server = Server::start(&dir);
    let client = server.client();
    runtime().block_on(async {
        // Cinq échecs sont tolérés, le sixième met l'adresse en attente ; les suivants sont refusés
        // sans calcul, et le journal ne s'en remplit pas.
        for _ in 0..5 {
            assert_eq!(
                login_with(&client, USER, WRONG_PASSWORD, None).await.status,
                401
            );
        }
        for _ in 0..6 {
            assert_eq!(
                login_with(&client, USER, WRONG_PASSWORD, None).await.status,
                429
            );
        }
    });
    let all = entries(&dir);
    let locked: Vec<&Entry> = all
        .iter()
        .filter(|e| e.detail.as_deref() == Some("lockout"))
        .collect();
    assert_eq!(locked.len(), 1, "{locked:?}");
    assert_eq!(locked[0].result, Outcome::Refused);
    // Les échecs de mot de passe : une ligne par fenêtre de 5 secondes, pas une par essai.
    let wrong = all
        .iter()
        .filter(|e| e.action == Action::Login && e.detail.is_none() && e.result == Outcome::Refused)
        .count();
    assert!((1..=2).contains(&wrong), "{wrong} lignes pour 5 essais");
    assert_eq!(
        ostal_server::audit::describe(locked[0]).1,
        "Connexion bloquée, trop d'essais, sur place"
    );
    drop(server);
}
