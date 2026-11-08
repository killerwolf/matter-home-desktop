//! Export et import de toutes les données de l'application dans un seul fichier
//! (`.matterhome`), pour changer de Mac sans réappairer les appareils.
//!
//! Le fichier est un JSON qui embarque les fichiers du dossier de données :
//! `controller.json`, `maison.json`, `regles.json` et `history/*.jsonl`.
//!
//! L'import n'écrase rien tant que l'application tourne (elle réécrit ses fichiers) :
//! il dépose la sauvegarde validée dans le dossier de données, et c'est le lancement
//! suivant qui l'applique ([`apply_pending`]), avant tout chargement. Les données
//! remplacées sont conservées dans un dossier voisin « …avant-import-<date> ».
//!
//! Le sous-dossier du serveur réseau (`serveur/` : compte, sessions, certificat TLS et sa
//! clé) n'est jamais exporté, jamais accepté dans une sauvegarde, et passe de l'ancien
//! dossier au nouveau pendant l'import : l'empreinte du certificat et le compte ne changent pas.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::persistence::{StoredState, FILE_NAME, SERVER_DIR};

/// Extension proposée dans les boîtes de dialogue.
pub const EXTENSION: &str = "matterhome";

const FORMAT: &str = "matter-home-desktop-sauvegarde";
const VERSION: u32 = 1;
const PENDING: &str = "import-en-attente.matterhome";
const PLAN: &str = "maison.json";
const RULES: &str = "regles.json";

#[derive(Debug, Serialize, Deserialize)]
struct Bundle {
    format: String,
    version: u32,
    /// Date de création, ISO 8601.
    created: String,
    /// Chemin relatif (séparateur `/`) → contenu texte du fichier.
    files: BTreeMap<String, String>,
}

/// Ce que contient une sauvegarde, pour l'afficher à l'utilisateur.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub created: String,
    pub devices: usize,
    pub history_files: usize,
    pub has_plan: bool,
    pub has_rules: bool,
}

impl Summary {
    pub fn describe(&self) -> String {
        let date = self.created.get(..10).unwrap_or(&self.created);
        let mut parts = vec![format!("{} appareil(s)", self.devices)];
        if self.has_plan {
            parts.push("plan de la maison".into());
        }
        if self.has_rules {
            parts.push("automatisations".into());
        }
        if self.history_files > 0 {
            parts.push(format!("historique de {} appareil(s)", self.history_files));
        }
        format!("sauvegarde du {date} : {}", parts.join(", "))
    }
}

/// Seuls ces fichiers sont exportés et acceptés à l'import : jamais de chemin libre.
fn is_allowed(name: &str) -> bool {
    if matches!(name, FILE_NAME | PLAN | RULES) {
        return true;
    }
    name.strip_prefix("history/")
        .and_then(|f| f.strip_suffix(".jsonl"))
        .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()))
}

fn summarize(bundle: &Bundle) -> Result<Summary> {
    let controller = bundle
        .files
        .get(FILE_NAME)
        .with_context(|| format!("{FILE_NAME} manque : ce n'est pas une sauvegarde complète"))?;
    let state: StoredState =
        serde_json::from_str(controller).with_context(|| format!("{FILE_NAME} illisible"))?;
    Ok(Summary {
        created: bundle.created.clone(),
        devices: state.device_ids().len(),
        history_files: bundle
            .files
            .keys()
            .filter(|k| k.starts_with("history/"))
            .count(),
        has_plan: bundle.files.contains_key(PLAN),
        has_rules: bundle.files.contains_key(RULES),
    })
}

fn parse(text: &str) -> Result<(Bundle, Summary)> {
    let bundle: Bundle =
        serde_json::from_str(text).context("ce fichier n'est pas une sauvegarde Matter Home")?;
    if bundle.format != FORMAT {
        bail!("ce fichier n'est pas une sauvegarde Matter Home");
    }
    if bundle.version > VERSION {
        bail!(
            "sauvegarde créée par une version plus récente de l'application (format {})",
            bundle.version
        );
    }
    if let Some(bad) = bundle.files.keys().find(|k| !is_allowed(k)) {
        bail!("fichier inattendu dans la sauvegarde : {bad}");
    }
    let summary = summarize(&bundle)?;
    Ok((bundle, summary))
}

fn read_bundle(path: &Path) -> Result<(Bundle, Summary)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    parse(&text)
}

/// Écrit `contents` dans `path`, droits 0600 sous Unix : le fichier contient des clés privées.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("écriture de {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Nom de fichier proposé à l'export.
pub fn default_file_name() -> String {
    format!(
        "matter-home-{}.{EXTENSION}",
        chrono::Local::now().format("%Y-%m-%d")
    )
}

/// Exporte le dossier de données `dir` vers le fichier `dest`.
pub fn export(dir: &Path, dest: &Path) -> Result<Summary> {
    let (text, summary) = export_to_string(dir)?;
    write_private(dest, &text)?;
    Ok(summary)
}

/// Sauvegarde de `dir` sous forme de texte (le contenu d'un fichier `.matterhome`).
/// Elle contient les clés privées en clair.
pub fn export_to_string(dir: &Path) -> Result<(String, Summary)> {
    let mut files = BTreeMap::new();
    for name in [FILE_NAME, PLAN, RULES] {
        let path = dir.join(name);
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("lecture de {}", path.display()))?;
            files.insert(name.to_string(), text);
        }
    }
    if !files.contains_key(FILE_NAME) {
        bail!("aucun appareil appairé pour l'instant : il n'y a rien à exporter");
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("history")) {
        for entry in entries.flatten() {
            let name = format!("history/{}", entry.file_name().to_string_lossy());
            if is_allowed(&name) {
                // Un historique illisible ne doit pas empêcher d'exporter le reste.
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    files.insert(name, text);
                }
            }
        }
    }
    let bundle = Bundle {
        format: FORMAT.into(),
        version: VERSION,
        created: chrono::Local::now().to_rfc3339(),
        files,
    };
    let summary = summarize(&bundle)?;
    Ok((serde_json::to_string(&bundle)?, summary))
}

/// Lit et valide une sauvegarde sans rien modifier.
pub fn inspect(src: &Path) -> Result<Summary> {
    read_bundle(src).map(|(_, s)| s)
}

/// Valide `src` puis le met de côté dans `dir` : il sera appliqué au prochain lancement.
pub fn stage_import(dir: &Path, src: &Path) -> Result<Summary> {
    let text =
        std::fs::read_to_string(src).with_context(|| format!("lecture de {}", src.display()))?;
    stage_import_text(dir, &text)
}

/// Comme [`stage_import`], pour une sauvegarde reçue en mémoire (import par l'API : aucune
/// copie intermédiaire des clés sur le disque).
pub fn stage_import_text(dir: &Path, text: &str) -> Result<Summary> {
    let (bundle, summary) = parse(text)?;
    write_private(&dir.join(PENDING), &serde_json::to_string(&bundle)?)?;
    Ok(summary)
}

/// À appeler au démarrage, avant tout chargement : applique l'import en attente.
/// Retourne un message à afficher si quelque chose s'est passé.
pub fn apply_pending(dir: &Path) -> Option<String> {
    let pending = dir.join(PENDING);
    if !pending.exists() {
        return None;
    }
    match apply(dir, &pending) {
        Ok((summary, backup)) => Some(format!(
            "Import terminé ({}). Anciennes données conservées dans {}.",
            summary.describe(),
            backup.display()
        )),
        Err(e) => {
            // Une sauvegarde invalide ne doit pas être retentée à chaque lancement.
            let _ = std::fs::remove_file(&pending);
            Some(format!(
                "Import impossible, données actuelles conservées : {e:#}"
            ))
        }
    }
}

fn apply(dir: &Path, pending: &Path) -> Result<(Summary, PathBuf)> {
    let (bundle, summary) = read_bundle(pending)?;
    let name = dir
        .file_name()
        .context("dossier de données sans nom")?
        .to_string_lossy();
    let backup = dir.with_file_name(format!(
        "{name}.avant-import-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::rename(dir, &backup).with_context(|| format!("mise de côté de {}", dir.display()))?;
    // Le dossier du serveur réseau (compte, sessions, certificat) ne fait pas partie des
    // données importées : il est remis tel quel dans le nouveau dossier.
    let kept_old = backup.join(SERVER_DIR);
    let kept_new = dir.join(SERVER_DIR);
    let restored = (|| -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
        if kept_old.exists() {
            std::fs::rename(&kept_old, &kept_new)
                .with_context(|| format!("conservation de {}", kept_new.display()))?;
        }
        bundle
            .files
            .iter()
            .try_for_each(|(rel, text)| write_private(&dir.join(rel), text))
    })();
    if let Err(e) = restored {
        // Retour en arrière : on remet les anciennes données en place, dossier du serveur compris.
        if kept_new.exists() {
            let _ = std::fs::rename(&kept_new, &kept_old);
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::rename(&backup, dir);
        return Err(e);
    }
    let _ = std::fs::remove_file(backup.join(PENDING));
    Ok((summary, backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROLLER: &str = r#"{"version":1,"identity":{"fabric_id":1,"controller_node_id":2,
        "ca_private_key":"aa","ca_cert_der":"bb","controller_private_key":"cc","controller_cert_der":"dd"},
        "nodes":[{"node_id":7,"name":"Capteur"}]}"#;

    fn temp(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("mhd-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn sample_dir(base: &Path) -> PathBuf {
        let dir = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(dir.join("history")).unwrap();
        std::fs::write(dir.join(FILE_NAME), CONTROLLER).unwrap();
        std::fs::write(dir.join(PLAN), "{\"rooms\":[]}").unwrap();
        std::fs::write(dir.join("history/B992B180DAC7AD3B.jsonl"), "{\"t\":1}\n").unwrap();
        std::fs::write(dir.join("history/notes.txt"), "ignoré").unwrap();
        dir
    }

    #[test]
    fn allowed_names() {
        assert!(is_allowed("controller.json") && is_allowed("history/3D76.jsonl"));
        for bad in [
            "../controller.json",
            "history/../x.jsonl",
            "history/x.jsonl",
            "history/.jsonl",
            "autre.json",
            "/etc/passwd",
        ] {
            assert!(!is_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn export_import_round_trip() {
        let base = temp("rt");
        let old = sample_dir(&base);
        let file = base.join("sauvegarde.matterhome");
        let summary = export(&old, &file).unwrap();
        assert_eq!(summary.devices, 1);
        assert_eq!(summary.history_files, 1);
        assert!(summary.has_plan && !summary.has_rules);
        assert_eq!(inspect(&file).unwrap(), summary);

        // « Nouveau Mac » : dossier vide, avec une ancienne identité à remplacer.
        let new = base
            .join("neuf")
            .join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join(FILE_NAME), "ancien").unwrap();
        assert_eq!(stage_import(&new, &file).unwrap(), summary);
        // Rien n'est touché avant le lancement suivant.
        assert_eq!(
            std::fs::read_to_string(new.join(FILE_NAME)).unwrap(),
            "ancien"
        );

        let msg = apply_pending(&new).unwrap();
        assert!(msg.contains("Import terminé"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(new.join(FILE_NAME)).unwrap(),
            CONTROLLER
        );
        assert!(new.join("history/B992B180DAC7AD3B.jsonl").exists());
        assert!(!new.join("history/notes.txt").exists());
        assert!(!new.join(PENDING).exists());
        // Les anciennes données sont gardées à côté.
        let kept: Vec<_> = std::fs::read_dir(new.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("avant-import"))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read_to_string(kept[0].path().join(FILE_NAME)).unwrap(),
            "ancien"
        );
        // Deuxième lancement : rien à faire.
        assert!(apply_pending(&new).is_none());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_bad_files() {
        let base = temp("bad");
        let file = base.join("x.matterhome");
        std::fs::write(&file, "pas du json").unwrap();
        assert!(inspect(&file).is_err());
        std::fs::write(
            &file,
            r#"{"format":"autre","version":1,"created":"","files":{}}"#,
        )
        .unwrap();
        assert!(inspect(&file).is_err());
        let evil = format!(
            r#"{{"format":"{FORMAT}","version":1,"created":"2026-01-01","files":{{"controller.json":{},"../../evil":"x"}}}}"#,
            serde_json::to_string(CONTROLLER).unwrap()
        );
        std::fs::write(&file, evil).unwrap();
        assert!(inspect(&file)
            .unwrap_err()
            .to_string()
            .contains("inattendu"));
        let future =
            format!(r#"{{"format":"{FORMAT}","version":99,"created":"2026-01-01","files":{{}}}}"#);
        std::fs::write(&file, future).unwrap();
        assert!(inspect(&file).is_err());
        // Pas de controller.json : refusé, et un export sans appareil aussi.
        let empty = base.join("vide");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(export(&empty, &base.join("o.matterhome")).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn invalid_pending_is_dropped_and_data_kept() {
        let base = temp("pend");
        let dir = sample_dir(&base);
        std::fs::write(dir.join(PENDING), "cassé").unwrap();
        let msg = apply_pending(&dir).unwrap();
        assert!(msg.contains("Import impossible"), "{msg}");
        assert!(!dir.join(PENDING).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            CONTROLLER
        );
        std::fs::remove_dir_all(base).unwrap();
    }
    /// Le dossier du serveur réseau (compte, sessions, certificat) ne part pas dans un export,
    /// n'est pas accepté dans une sauvegarde, et survit à l'import.
    #[test]
    fn server_dir_is_neither_exported_nor_replaced_by_import() {
        let base = temp("serveur");
        let dir = sample_dir(&base);
        let secret = dir.join(SERVER_DIR);
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("tls-key.pem"), "CLE PRIVEE DU SERVEUR").unwrap();
        std::fs::write(secret.join("compte.json"), "{\"username\":\"hamdi\"}").unwrap();

        // Export : le dossier du serveur n'y figure pas.
        let (text, _) = export_to_string(&dir).unwrap();
        assert!(!text.contains("CLE PRIVEE DU SERVEUR") && !text.contains("compte.json"));
        assert!(!text.contains(SERVER_DIR));

        // Une sauvegarde qui prétend en fournir un est refusée.
        let evil = format!(
            r#"{{"format":"{FORMAT}","version":1,"created":"2026-01-01","files":{{"controller.json":{},"serveur/tls-key.pem":"x"}}}}"#,
            serde_json::to_string(CONTROLLER).unwrap()
        );
        assert!(stage_import_text(&dir, &evil).is_err());
        assert!(!dir.join(PENDING).exists());

        // Import : les données changent, le dossier du serveur reste identique.
        let mut other = text.clone();
        other = other.replace("Capteur", "Capteur importé");
        stage_import_text(&dir, &other).unwrap();
        let msg = apply_pending(&dir).unwrap();
        assert!(msg.contains("Import terminé"), "{msg}");
        assert!(std::fs::read_to_string(dir.join(FILE_NAME))
            .unwrap()
            .contains("Capteur importé"));
        assert_eq!(
            std::fs::read_to_string(dir.join(SERVER_DIR).join("tls-key.pem")).unwrap(),
            "CLE PRIVEE DU SERVEUR"
        );
        assert!(dir.join(SERVER_DIR).join("compte.json").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
