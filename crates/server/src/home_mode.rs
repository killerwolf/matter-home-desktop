//! Le fichier du mode global de la maison (`mode.json`) : un seul mot, `present` par défaut.
//!
//! Écrit par le hub (atomique, 0600, comme `regles.json`), exporté et importé avec les
//! sauvegardes. Un fichier absent vaut « Présent ». Un fichier illisible est mis de côté sous
//! `mode.json.illisible` et le mode repart de « Présent ».

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use protocol::mode::HomeMode;

use crate::persistence;

pub const FILE_NAME: &str = "mode.json";
const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeFile {
    pub version: u32,
    #[serde(default)]
    pub mode: HomeMode,
}

impl Default for ModeFile {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            mode: HomeMode::default(),
        }
    }
}

impl ModeFile {
    /// Contrôle d'une sauvegarde à importer : un seul mode connu, la bonne version.
    pub fn parse(text: &str) -> Result<ModeFile, String> {
        let file: ModeFile =
            serde_json::from_str(text).map_err(|_| format!("{FILE_NAME} illisible"))?;
        if file.version != FILE_VERSION {
            return Err(format!("{FILE_NAME} : version non prise en charge"));
        }
        Ok(file)
    }
}

/// Lit le mode enregistré dans `dir`.
pub fn load(dir: &Path) -> HomeMode {
    let path = dir.join(FILE_NAME);
    match persistence::load_json::<ModeFile>(&path) {
        Ok(Some(file)) => file.mode,
        Ok(None) => HomeMode::default(),
        Err(e) => {
            let aside = path.with_extension("json.illisible");
            log::error!(
                "mode illisible, « Présent » utilisé ({e:#}) ; fichier conservé sous {}",
                aside.display()
            );
            let _ = std::fs::rename(&path, aside);
            HomeMode::default()
        }
    }
}

/// Enregistre le mode dans `dir` (atomique, 0600).
pub fn save(dir: &Path, mode: HomeMode) -> Result<()> {
    persistence::save_json(
        &dir.join(FILE_NAME),
        &ModeFile {
            version: FILE_VERSION,
            mode,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-mode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn absent_file_is_present_and_a_saved_mode_comes_back() {
        let dir = temp("roundtrip");
        assert_eq!(load(&dir), HomeMode::Present);
        save(&dir, HomeMode::Vacances).unwrap();
        assert_eq!(load(&dir), HomeMode::Vacances);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_file_is_kept_aside_and_resets_to_present() {
        let dir = temp("broken");
        std::fs::write(dir.join(FILE_NAME), "{pas du json").unwrap();
        assert_eq!(load(&dir), HomeMode::Present);
        assert!(!dir.join(FILE_NAME).exists());
        assert!(dir.join("mode.json.illisible").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backups_are_checked() {
        assert!(ModeFile::parse(r#"{"version":1,"mode":"nuit"}"#).is_ok());
        assert!(ModeFile::parse(r#"{"version":1,"mode":"dehors"}"#).is_err());
        assert!(ModeFile::parse(r#"{"version":9,"mode":"nuit"}"#).is_err());
        assert!(ModeFile::parse("[]").is_err());
    }
}
