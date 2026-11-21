//! Renommage « Matter Home » → Ostal, de bout en bout avec le vrai binaire : le dossier de
//! données de l'ancien nom est copié au premier lancement, l'ancien reste intact, et l'ancienne
//! application (qui tient l'ancien verrou) empêche toute écriture.
//!
//! Sécurité : HOME factice dans le dossier temporaire, **sans** `--data-dir` (c'est le chemin
//! standard, sous ce HOME, qui est en jeu). Seule `create-account` est lancée : elle ne démarre
//! ni Matter, ni le réseau.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{temp_dir, PASSWORD, USER};
use ostal_server::persistence;

const BIN: &str = env!("CARGO_BIN_EXE_ostal-server");

fn support(home: &Path) -> PathBuf {
    home.join("Library/Application Support")
}

fn create_account_in_home(home: &Path) -> (bool, String) {
    let mut child = Command::new(BIN)
        .args(["create-account", "--username", USER, "--password-stdin"])
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{PASSWORD}").unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Un ancien dossier de données : identité factice (non lue par `create-account`), plan,
/// historique.
fn old_install(home: &Path) -> PathBuf {
    let old = support(home).join("dev.matter-home-desktop.MatterHomeDesktop");
    std::fs::create_dir_all(old.join("history")).unwrap();
    std::fs::write(old.join(persistence::FILE_NAME), "identité factice").unwrap();
    std::fs::write(old.join("maison.json"), "{\"plan\":1}").unwrap();
    std::fs::write(old.join("history/42.jsonl"), "{}\n{}\n").unwrap();
    old
}

fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.clone());
                out.push((p.strip_prefix(dir).unwrap().display().to_string(), vec![]));
            } else {
                let bytes = std::fs::read(&p).unwrap();
                out.push((p.strip_prefix(dir).unwrap().display().to_string(), bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn first_launch_copies_the_old_folder_and_leaves_it_intact() {
    let base = temp_dir("rename-copy");
    let home = base.join("home");
    let old = old_install(&home);
    let before = tree(&old);

    let (ok, text) = create_account_in_home(&home);
    assert!(ok, "{text}");
    let new = support(&home).join("dev.ostal.OstalServer");
    // Les données sont dans le nouveau dossier, le compte aussi ; l'ancien n'a pas bougé.
    assert_eq!(
        std::fs::read(new.join(persistence::FILE_NAME)).unwrap(),
        "identité factice".as_bytes()
    );
    assert_eq!(
        std::fs::read(new.join("history/42.jsonl")).unwrap(),
        b"{}\n{}\n"
    );
    assert!(new.join(persistence::SERVER_DIR).is_dir());
    assert_eq!(tree(&old), before);
    assert!(!old.join(persistence::SERVER_DIR).exists());

    // Deuxième lancement : plus de copie, le nouveau dossier est celui qu'on utilise.
    std::fs::write(old.join("maison.json"), "{\"modifie\":1}").unwrap();
    let (ok, text) = create_account_in_home(&home);
    assert!(
        !ok || text.contains("remplacé") || text.contains("existe"),
        "{text}"
    );
    assert_eq!(
        std::fs::read(new.join("maison.json")).unwrap(),
        b"{\"plan\":1}"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn the_old_app_holding_its_lock_blocks_everything() {
    let base = temp_dir("rename-held");
    let home = base.join("home");
    let old = old_install(&home);
    let before = tree(&old);
    // L'ancienne application tourne : elle tient son verrou.
    let lock = support(&home).join("dev.matter-home-desktop.MatterHomeDesktop.lock");
    let held = persistence::acquire_instance_lock(&lock).unwrap();

    let (ok, text) = create_account_in_home(&home);
    assert!(!ok, "{text}");
    assert!(text.contains("Matter Home Server"), "{text}");
    assert!(!support(&home).join("dev.ostal.OstalServer").exists());
    assert_eq!(tree(&old), before);

    // Une fois quittée, la copie se fait.
    drop(held);
    let (ok, text) = create_account_in_home(&home);
    assert!(ok, "{text}");
    assert!(support(&home)
        .join("dev.ostal.OstalServer")
        .join(persistence::FILE_NAME)
        .is_file());
    assert_eq!(tree(&old), before);
    let _ = std::fs::remove_dir_all(base);
}
