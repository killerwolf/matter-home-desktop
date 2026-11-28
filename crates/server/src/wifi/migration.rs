//! La reprise des anciens réglages de la box dans l'extension « Bbox ».
//!
//! Avant les extensions, `serveur/box.json` gardait l'adresse de la box et **son mot de passe** à
//! côté de l'interrupteur du mode automatique et du délai d'absence. Aujourd'hui l'adresse et le
//! mot de passe sont ceux de l'extension (`extensions.json`) ; `box.json` ne garde que ce qui est
//! propre à la présence. [`run`] fait la reprise **au premier démarrage**, avant l'ouverture de
//! `Wifi` (voir `Hub::new`), dans cet ordre :
//!
//! 1. une copie de `box.json` est gardée à côté (`box.json.avant-extension`, 0600, jamais exportée) ;
//! 2. s'il n'y a pas encore d'entrée « bbox » dans `extensions.json` : l'adresse, le mot de passe
//!    et l'activation (activée si la box était reliée, c'est-à-dire adresse **et** mot de passe) y
//!    sont écrits par le chemin normal des réglages d'extension (`Extensions::update` : un seul
//!    fichier, écrit en une fois par renommage) ;
//! 3. seulement alors, `url` et `password` sont retirés de `box.json` (tout le reste du fichier est
//!    gardé tel quel : interrupteur, délai, champs inconnus). `telephones.json` n'est jamais touché.
//!
//! La copie sert à revenir à la version précédente d'Ostal (la remettre sous le nom `box.json`). Elle
//! porte l'ancien mot de passe : elle est **supprimée** (toutes les copies, [`remove_copies`]) dès
//! qu'un premier relevé réussi confirme la reprise, et quand la box est déliée ou son mot de passe
//! effacé.
//!
//! - **Idempotente** : une fois `url` et `password` retirés, plus rien à faire, et rien n'est écrit.
//! - **Reprise après interruption** : si l'entrée « bbox » existe déjà avec les mêmes réglages
//!   (arrêt entre l'étape 2 et l'étape 3), l'extension n'est pas réécrite ; on termine seulement
//!   le retrait.
//! - **Retour à une ancienne version puis nouvelle montée** : si l'entrée existe mais que `box.json`
//!   porte de nouveau un mot de passe (ou une adresse) **différent**, il est plus récent : il est
//!   copié puis repris dans l'extension, et seulement ensuite retiré.
//! - **Échec** (extensions illisibles, disque plein, adresse invalide, trop de copies…) : `box.json`
//!   n'est pas modifié, l'erreur est journalisée, et la box reste lue comme avant
//!   (`Bbox::use_legacy`) en attendant : une maison ne perd jamais sa présence en silence.
//!
//! Aucune valeur secrète n'est écrite dans le journal.

use std::path::Path;

use serde_json::{json, Value};

use super::CONFIG_FILE;
use crate::extensions::bbox::{check_url, DEFAULT_URL, ID, PASSWORD};
use crate::extensions::{Extensions, Update};
use crate::security::write_secret;

/// La copie de `box.json` d'avant la reprise (puis `.1`, `.2`… si `box.json` a encore changé).
pub const BACKUP_FILE: &str = "box.json.avant-extension";
/// Nombre maximal de copies gardées côte à côte.
const MAX_COPIES: usize = 10;

/// Ce que la reprise a fait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Rien à reprendre.
    Nothing,
    /// Réglages repris dans l'extension (`enabled` : activée).
    Migrated { enabled: bool },
    /// L'extension avait déjà son entrée (reprise interrompue) : seuls les restes de `box.json`
    /// ont été retirés.
    Cleaned,
    /// Échec : `box.json` est intact, la box reste lue comme avant. La phrase dit pourquoi.
    Failed(String),
}

/// Lit `box.json` de `dir` (le dossier `serveur/`) et reprend l'ancienne box dans l'extension.
pub fn run(dir: &Path, extensions: &Extensions) -> Outcome {
    let outcome = run_inner(dir, extensions);
    match &outcome {
        Outcome::Nothing => {}
        Outcome::Migrated { enabled } => log::info!(
            "extension Bbox : les réglages de la box ont été repris depuis box.json ({}) ; copie gardée sous {BACKUP_FILE} (supprimée dès que la box répond)",
            if *enabled { "extension activée" } else { "extension laissée éteinte" }
        ),
        Outcome::Cleaned => log::info!(
            "extension Bbox : reprise terminée (restes de l'ancienne box retirés de box.json)"
        ),
        Outcome::Failed(why) => log::error!(
            "extension Bbox : reprise des anciens réglages de la box IMPOSSIBLE ({why}) ; box.json n'est pas modifié et la box reste lue comme avant"
        ),
    }
    outcome
}

fn run_inner(dir: &Path, extensions: &Extensions) -> Outcome {
    let path = dir.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::Nothing,
        Err(e) => return Outcome::Failed(format!("box.json illisible ({})", e.kind())),
    };
    // Un fichier qui n'est pas du JSON : `Wifi::open` le met de côté, comme avant.
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&bytes) else {
        return Outcome::Nothing;
    };
    let text = |key: &str| {
        doc.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let (url, password) = (text("url"), text("password"));
    if url.is_empty() && password.is_empty() {
        return Outcome::Nothing;
    }

    let existing = extensions.has_entry(ID);
    // Une entrée déjà là (reprise interrompue, ou retour à une ancienne version puis nouvelle montée) :
    // `box.json` ne l'emporte que s'il porte un mot de passe **différent** (il est alors plus récent,
    // écrit par une version plus ancienne qui ne connaît pas l'extension).
    let wants_update = if existing {
        let ext_url = extensions
            .config(ID)
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_URL)
            .to_string();
        !password.is_empty()
            && (extensions.secret(ID, PASSWORD).as_deref() != Some(&password)
                || (!url.is_empty() && check_url(&url).ok().as_deref() != Some(&ext_url)))
    } else {
        true
    };

    // La copie d'abord, l'écriture dans l'extension ensuite, le retrait de box.json en dernier : si
    // la copie échoue, rien n'est changé (et la prochaine fois on réessaie).
    if wants_update && extensions.is_read_only() {
        return Outcome::Failed("extensions.json illisible et impossible à mettre de côté".into());
    }
    if let Err(why) = keep_a_copy(dir, &bytes) {
        return Outcome::Failed(why);
    }
    if wants_update {
        let linked = !url.is_empty() && !password.is_empty();
        let mut update = Update {
            enabled: Some(linked),
            config: if url.is_empty() && existing {
                None
            } else if url.is_empty() {
                Some(json!({}))
            } else {
                Some(json!({ "url": url }))
            },
            ..Update::default()
        };
        if !password.is_empty() {
            update
                .secrets
                .insert(PASSWORD.to_string(), Some(password.clone()));
        }
        if let Err(e) = extensions.update(ID, update) {
            return Outcome::Failed(format!("l'extension a refusé les réglages ({e:?})"));
        }
        // Relu avant de toucher à box.json : le mot de passe est bien dans l'extension.
        if !password.is_empty() && extensions.secret(ID, PASSWORD).as_deref() != Some(&password) {
            return Outcome::Failed("le mot de passe n'a pas été enregistré".into());
        }
    }
    let migrated = wants_update;
    doc.remove("url");
    doc.remove("password");
    let stripped = serde_json::to_vec_pretty(&Value::Object(doc))
        .map_err(|e| e.to_string())
        .and_then(|b| write_secret(&path, &b).map_err(|e| format!("{e:#}")));
    if let Err(why) = stripped {
        log::warn!(
            "extension Bbox : box.json non nettoyé ({why}) ; nouvel essai au prochain démarrage"
        );
    }
    if migrated {
        Outcome::Migrated {
            enabled: !url.is_empty() && !password.is_empty(),
        }
    } else {
        Outcome::Cleaned
    }
}

/// Garde une copie privée de `box.json` (sauf si une copie identique existe déjà).
fn keep_a_copy(dir: &Path, bytes: &[u8]) -> Result<(), String> {
    for n in 0..MAX_COPIES {
        let name = if n == 0 {
            BACKUP_FILE.to_string()
        } else {
            format!("{BACKUP_FILE}.{n}")
        };
        let target = dir.join(name);
        match std::fs::read(&target) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return write_secret(&target, bytes).map_err(|e| format!("{e:#}"));
            }
            Err(e) => return Err(format!("{}", e.kind())),
        }
    }
    Err(format!(
        "{MAX_COPIES} copies de box.json ({BACKUP_FILE}*) existent déjà dans le dossier serveur/ : vérifiez-les, supprimez-les, puis relancez Ostal Server"
    ))
}

/// Supprime **toutes** les copies de l'ancien `box.json` (elles portent l'ancien mot de passe) : une
/// fois la reprise confirmée par un relevé réussi, ou quand la box est déliée ou son mot de passe
/// effacé. Rend le nombre de copies supprimées.
pub fn remove_copies(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(BACKUP_FILE)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{bbox, Apis};
    use crate::wifi::Wifi;
    use std::path::PathBuf;

    /// Un mot de passe avec des caractères qu'un formulaire ou un JSON doit protéger.
    const PASSWORD: &str = "p@ss w/ %&=ord+é\"\\";

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ostal-box-migration-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn old_box(url: &str, password: &str, auto: bool, delay: u32) -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "version": 1, "url": url, "password": password,
            "auto_mode": auto, "away_delay_min": delay, "champ_futur": {"x": 1}
        }))
        .unwrap()
    }

    const PHONES: &[u8] = br#"{"version":1,"phones":[{"user_id":"u1","host":"iPhone-de-Sam"}]}"#;

    fn open(dir: &Path) -> Extensions {
        Extensions::open(dir, Apis::demo())
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn a_linked_box_is_taken_over_by_the_extension_and_nothing_else_changes() {
        let dir = temp("linked");
        let original = old_box("https://mabbox.bytel.fr", PASSWORD, true, 25);
        std::fs::write(dir.join("box.json"), &original).unwrap();
        std::fs::write(dir.join("telephones.json"), PHONES).unwrap();
        // Des extensions déjà réglées : leurs secrets ne bougent pas.
        std::fs::write(
            dir.join("extensions.json"),
            br#"{"version":1,"extensions":{"somfy":{"enabled":true,"config":{"email":"a@b.fr","exposed":null},"secrets":{"password":"SECRET-SOMFY"}},"weather":{"enabled":true,"config":{"place":null},"secrets":{}}}}"#,
        )
        .unwrap();

        let ext = open(&dir);
        assert_eq!(run(&dir, &ext), Outcome::Migrated { enabled: true });

        // Activée, adresse et mot de passe repris à l'identique (secret relu côté serveur).
        assert!(ext.is_enabled(bbox::ID));
        assert_eq!(
            ext.secret(bbox::ID, bbox::PASSWORD).as_deref(),
            Some(PASSWORD)
        );
        assert_eq!(
            ext.config(bbox::ID),
            json!({"url": "https://mabbox.bytel.fr"})
        );
        // Les autres extensions n'ont pas bougé.
        assert!(ext.is_enabled("somfy"));
        assert_eq!(
            ext.secret("somfy", "password").as_deref(),
            Some("SECRET-SOMFY")
        );
        assert!(ext.is_enabled("weather"));
        // Tout survit à un redémarrage.
        let again = open(&dir);
        assert!(again.is_enabled(bbox::ID));
        assert_eq!(
            again.secret(bbox::ID, bbox::PASSWORD).as_deref(),
            Some(PASSWORD)
        );
        // Le fichier des extensions est privé.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("extensions.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // box.json : plus d'adresse ni de mot de passe, le reste est gardé (interrupteur, délai,
        // champs futurs).
        let after: Value = serde_json::from_str(&read(&dir, "box.json")).unwrap();
        assert!(after.get("url").is_none() && after.get("password").is_none());
        assert_eq!(after["auto_mode"], true);
        assert_eq!(after["away_delay_min"], 25);
        assert_eq!(after["champ_futur"], json!({"x": 1}));
        assert!(!read(&dir, "box.json").contains("ord"));
        // Une copie intacte de l'ancien fichier est gardée à côté.
        assert_eq!(std::fs::read(dir.join(BACKUP_FILE)).unwrap(), original);
        // Les téléphones ne changent pas d'un octet.
        assert_eq!(std::fs::read(dir.join("telephones.json")).unwrap(), PHONES);
        // La présence reprend tel quel : interrupteur allumé, délai de 25 minutes, une box reliée.
        let wifi = Wifi::open(&dir, again.box_connectors());
        assert!(wifi.auto_mode());
        assert_eq!(wifi.away_delay_s(), 1500.0);
        assert_eq!(wifi.phone_of("u1").as_deref(), Some("iPhone-de-Sam"));
        let status = wifi.status();
        assert!(status.linked && status.connector == Some("Bbox"));
        // Le mot de passe ne se montre nulle part.
        let shown = format!(
            "{:?} {:?} {:?} {:?}",
            ext.list(true),
            ext.get(bbox::ID, true),
            status,
            run(&dir, &ext)
        );
        assert!(
            !shown.contains("p@ss") && !shown.contains("ord+"),
            "{shown}"
        );
        assert!(!serde_json::to_string(&ext.list(true))
            .unwrap()
            .contains("ord+"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_run_writes_nothing() {
        let dir = temp("twice");
        std::fs::write(
            dir.join("box.json"),
            old_box("http://192.168.1.254", "mdp", false, 10),
        )
        .unwrap();
        let ext = open(&dir);
        assert!(matches!(run(&dir, &ext), Outcome::Migrated { .. }));
        let names = ["box.json", "extensions.json", BACKUP_FILE];
        let snapshot = |dir: &Path| -> Vec<Vec<u8>> {
            names
                .iter()
                .map(|n| std::fs::read(dir.join(n)).unwrap())
                .collect()
        };
        let times = |dir: &Path| -> Vec<_> {
            names
                .iter()
                .map(|n| std::fs::metadata(dir.join(n)).unwrap().modified().unwrap())
                .collect()
        };
        let (bytes, mtimes) = (snapshot(&dir), times(&dir));
        assert_eq!(run(&dir, &ext), Outcome::Nothing);
        assert_eq!(run(&dir, &open(&dir)), Outcome::Nothing);
        assert_eq!(snapshot(&dir), bytes);
        assert_eq!(times(&dir), mtimes, "aucun fichier réécrit");
        assert!(!dir.join(format!("{BACKUP_FILE}.1")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unlinked_box_leaves_every_file_alone() {
        let dir = temp("unlinked");
        let original = br#"{"version":1,"auto_mode":true,"away_delay_min":10}"#;
        std::fs::write(dir.join("box.json"), original).unwrap();
        let ext = open(&dir);
        assert_eq!(run(&dir, &ext), Outcome::Nothing);
        assert_eq!(std::fs::read(dir.join("box.json")).unwrap(), original);
        assert!(!dir.join("extensions.json").exists() && !dir.join(BACKUP_FILE).exists());
        assert!(!ext.is_enabled(bbox::ID));
        // Ni fichier du tout.
        let empty = temp("nofile");
        assert_eq!(run(&empty, &open(&empty)), Outcome::Nothing);
        assert!(!empty.join("extensions.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn an_address_without_a_password_is_kept_but_the_extension_stays_off() {
        let dir = temp("no-password");
        std::fs::write(
            dir.join("box.json"),
            old_box("http://192.168.1.254", "", true, 10),
        )
        .unwrap();
        let ext = open(&dir);
        assert_eq!(run(&dir, &ext), Outcome::Migrated { enabled: false });
        assert!(!ext.is_enabled(bbox::ID));
        assert_eq!(ext.config(bbox::ID), json!({"url": "http://192.168.1.254"}));
        assert!(ext.secret(bbox::ID, bbox::PASSWORD).is_none());
        assert!(!Wifi::open(&dir, ext.box_connectors()).status().linked);
        let after: Value = serde_json::from_str(&read(&dir, "box.json")).unwrap();
        assert_eq!(after["auto_mode"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_migration_is_finished_without_overwriting_the_extension() {
        // Arrêt entre l'écriture de l'extension et le nettoyage de box.json.
        let dir = temp("crash");
        std::fs::write(
            dir.join("box.json"),
            old_box("https://mabbox.bytel.fr", "ancien", true, 10),
        )
        .unwrap();
        std::fs::write(
            dir.join("extensions.json"),
            br#"{"version":1,"extensions":{"bbox":{"enabled":true,"config":{"url":"https://mabbox.bytel.fr"},"secrets":{"password":"ancien"}}}}"#,
        )
        .unwrap();
        let ext = open(&dir);
        assert_eq!(run(&dir, &ext), Outcome::Cleaned);
        // L'extension garde ce qu'elle avait.
        assert_eq!(
            ext.secret(bbox::ID, bbox::PASSWORD).as_deref(),
            Some("ancien")
        );
        assert!(ext.is_enabled(bbox::ID));
        // box.json est nettoyé, l'ancien est copié, rien n'est perdu.
        let after = read(&dir, "box.json");
        assert!(!after.contains("ancien") && after.contains("auto_mode"));
        assert!(read(&dir, BACKUP_FILE).contains("ancien"));
        assert_eq!(run(&dir, &ext), Outcome::Nothing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_migration_leaves_box_json_intact_and_the_box_is_still_read() {
        use crate::wifi::connector::BoxConnector;
        // extensions.json est illisible et impossible à mettre de côté : réglages en lecture seule.
        let dir = temp("readonly");
        let original = old_box("https://mabbox.bytel.fr", PASSWORD, true, 10);
        std::fs::write(dir.join("box.json"), &original).unwrap();
        std::fs::create_dir(dir.join("extensions.json")).unwrap();
        let ext = open(&dir);
        assert!(ext.is_read_only());
        let outcome = run(&dir, &ext);
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        assert!(!format!("{outcome:?}").contains("ord+"));
        assert_eq!(std::fs::read(dir.join("box.json")).unwrap(), original);
        assert!(!dir.join(BACKUP_FILE).exists());
        // La box reste lue comme avant : l'ancien réglage sert de secours (jamais écrit ailleurs).
        let wifi = Wifi::open(&dir, ext.box_connectors());
        assert!(!wifi.status().linked);
        let (url, password) = wifi.legacy_box().unwrap();
        ext.bbox().use_legacy(&url, &password);
        assert!(ext.bbox().ready());
        assert!(wifi.status().linked);
        // Un réglage de présence ne fait pas perdre le mot de passe de box.json.
        wifi.set_auto_mode(false).unwrap();
        assert!(read(&dir, "box.json").contains("ord"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_address_the_extension_refuses_is_a_clear_failure_not_a_silent_loss() {
        let dir = temp("bad-url");
        let original = old_box("ftp://pas-une-box", "mdp", false, 10);
        std::fs::write(dir.join("box.json"), &original).unwrap();
        let ext = open(&dir);
        assert!(matches!(run(&dir, &ext), Outcome::Failed(_)));
        assert_eq!(std::fs::read(dir.join("box.json")).unwrap(), original);
        assert!(!ext.has_entry(bbox::ID));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_password_in_box_json_after_going_back_a_version_is_taken_over() {
        // Reprise, retour à l'ancienne version (nouveau mot de passe dans box.json), nouvelle version :
        // l'extension a le nouveau mot de passe, l'ancien est copié, rien n'est perdu.
        let dir = temp("recopy");
        std::fs::write(
            dir.join("box.json"),
            old_box("http://192.168.1.254", "premier", false, 10),
        )
        .unwrap();
        let ext = open(&dir);
        assert!(matches!(run(&dir, &ext), Outcome::Migrated { .. }));
        std::fs::write(
            dir.join("box.json"),
            old_box("http://192.168.1.99", "second", true, 15),
        )
        .unwrap();
        let ext = open(&dir);
        assert_eq!(run(&dir, &ext), Outcome::Migrated { enabled: true });
        assert_eq!(
            ext.secret(bbox::ID, bbox::PASSWORD).as_deref(),
            Some("second")
        );
        assert_eq!(ext.config(bbox::ID), json!({"url": "http://192.168.1.99"}));
        assert!(ext.is_enabled(bbox::ID));
        let after = read(&dir, "box.json");
        assert!(!after.contains("second") && after.contains("\"away_delay_min\": 15"));
        assert!(read(&dir, BACKUP_FILE).contains("premier"));
        assert!(read(&dir, &format!("{BACKUP_FILE}.1")).contains("second"));
        assert_eq!(run(&dir, &ext), Outcome::Nothing);
        // Les copies partent toutes ensemble.
        assert_eq!(remove_copies(&dir), 2);
        assert!(!dir.join(BACKUP_FILE).exists());
        assert_eq!(remove_copies(&dir), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn too_many_copies_say_what_to_do_and_change_nothing() {
        let dir = temp("too-many");
        let original = old_box("http://192.168.1.254", "mdp", false, 10);
        std::fs::write(dir.join("box.json"), &original).unwrap();
        std::fs::write(dir.join(BACKUP_FILE), b"x").unwrap();
        for n in 1..MAX_COPIES {
            std::fs::write(dir.join(format!("{BACKUP_FILE}.{n}")), format!("y{n}")).unwrap();
        }
        let ext = open(&dir);
        let Outcome::Failed(why) = run(&dir, &ext) else {
            panic!("la reprise aurait dû échouer");
        };
        assert!(
            why.contains("supprimez-les") && why.contains(BACKUP_FILE),
            "{why}"
        );
        assert_eq!(std::fs::read(dir.join("box.json")).unwrap(), original);
        assert!(!ext.has_entry(bbox::ID));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
