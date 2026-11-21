//! Quand le prologue échoue (reprise d'import impossible, copie des données de l'ancien nom
//! impossible…), l'application relâche ses verrous d'instance : l'ancienne application, ou la
//! commande en ligne, peut alors terminer ce qui bloque. Sinon, impasse : chacune attendrait
//! l'autre. Un seul test : le dossier de données est un réglage du processus entier.
//!
//! L'échec est provoqué sans rien toucher hors du dossier temporaire : un import interrompu
//! attend à côté du dossier de données, et le dossier parent est en lecture seule, donc la
//! reprise (un renommage) échoue.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use ostal_server::persistence;
use ostal_server_app::controller::{self, Config};
use ostal_server_app::status::Phase;

#[test]
fn a_failed_prologue_releases_the_locks_and_says_to_quit() {
    let base = std::env::temp_dir().join(format!("ostal-app-prologue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let parent = base.join("parent");
    std::fs::create_dir_all(parent.join("donnees.import-nouveau")).unwrap();
    std::fs::write(
        parent.join("donnees.import-nouveau/controller.json"),
        "identité factice",
    )
    .unwrap();
    let dir = parent.join("donnees");
    assert!(persistence::set_data_dir(dir.clone()));
    let lock = controller::acquire_lock();
    assert!(lock.1.is_some());
    // Le verrou est pris ; la reprise de l'import (renommer dans `parent`) va échouer.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::File::create(parent.join("essai")).is_ok();

    let handle = controller::spawn(
        Config {
            data_dir: dir.clone(),
            listen: "127.0.0.1:0".parse().unwrap(),
            demo: true,
            demo_setup: true,
            temp_data_dir: false,
            log_path: base.join("journal.log"),
            args: vec![],
        },
        lock,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let problem = loop {
        let st = handle.snapshot();
        if let Phase::Problem(text) = &st.phase {
            break text.clone();
        }
        assert!(
            Instant::now() < deadline,
            "attente du problème : {:?}",
            st.phase
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let released = persistence::acquire_instance_lock(&persistence::instance_lock_path());
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !writable {
        assert!(
            problem.starts_with("Quittez Ostal Server, puis"),
            "{problem}"
        );
        assert!(problem.contains("import interrompu"), "{problem}");
        // Le verrou d'instance est libre : quelqu'un d'autre peut terminer l'import.
        assert!(released.is_ok(), "{:?}", released.err());
    }
    drop(released);
    let _ = std::fs::remove_dir_all(&base);
}
