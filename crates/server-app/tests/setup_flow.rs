//! La première configuration de bout en bout, par le chemin de l'application (sans fenêtre) :
//! dossier vide → « nouvelle maison » → compte → serveur en marche (appareils simulés, boucle
//! locale). Un seul test : le dossier de données est un réglage du processus entier.

use std::time::{Duration, Instant};

use matter_home_server_app::controller::{self, Cmd, Config, Handle, Notice};
use matter_home_server_app::startup::SetupNeed;
use matter_home_server_app::status::Phase;

fn wait_for(handle: &Handle, what: &str, ok: impl Fn(&controller::State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let st = handle.snapshot();
        if ok(&st) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "attente de « {what} » : {:?}",
            st.phase
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn empty_folder_goes_through_identity_then_account_then_runs() {
    let dir = std::env::temp_dir().join(format!("mh-app-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matter_home_server::persistence::set_data_dir(dir.clone()));
    let lock = controller::acquire_lock();
    let handle = controller::spawn(
        Config {
            data_dir: dir.clone(),
            listen: "127.0.0.1:0".parse().unwrap(),
            demo: true,
            demo_setup: true,
            temp_data_dir: false,
            log_path: dir.join("journal.log"),
            args: vec![],
        },
        lock,
    );

    // Aucune identité : jamais créée toute seule, la configuration la demande.
    wait_for(&handle, "choix de l'identité", |s| {
        s.phase == Phase::NeedsSetup && s.setup == Some(SetupNeed::Identity { unreadable: None })
    });
    assert!(!dir.join("controller.json").exists());

    // Choix explicite : nouvelle maison.
    handle.tx.send(Cmd::NewHome).unwrap();
    wait_for(&handle, "compte à créer", |s| {
        s.phase == Phase::NeedsSetup && s.setup == Some(SetupNeed::Account)
    });
    assert!(dir.join("controller.json").exists());

    // Un mot de passe trop court est refusé, sans rien créer.
    handle
        .tx
        .send(Cmd::CreateAccount {
            username: "hamdi".into(),
            password: "court".into(),
        })
        .unwrap();
    wait_for(
        &handle,
        "refus du mot de passe",
        |s| matches!(&s.notice, Some(Notice::Err(m)) if m.contains("10 caractères")),
    );

    handle
        .tx
        .send(Cmd::CreateAccount {
            username: "hamdi".into(),
            password: "un-mot-de-passe-long".into(),
        })
        .unwrap();
    wait_for(&handle, "serveur en marche", |s| s.phase == Phase::Running);
    let st = handle.snapshot();
    assert!(st.url.is_some_and(|u| u.starts_with("https://127.0.0.1:")));
    assert!(st.fingerprint.is_some_and(|f| f.len() == 95));

    // On ne passe pas par Quit : il termine le processus de test.
    let _ = std::fs::remove_dir_all(&dir);
}
