//! Persistance minimale : identifiants du contrôleur et liste des nœuds appairés,
//! dans un seul fichier JSON du dossier de données de l'application.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::home::Symbol;

pub const FILE_NAME: &str = "controller.json";

/// Identité du contrôleur sur sa fabric (clés en clair, comme les fichiers PEM de matc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerIdentity {
    pub fabric_id: u64,
    pub controller_node_id: u64,
    /// Clé privée P-256 de l'autorité racine (RCAC), hexadécimal.
    pub ca_private_key: String,
    /// Certificat X.509 DER de la racine, hexadécimal.
    pub ca_cert_der: String,
    /// Clé privée P-256 du contrôleur, hexadécimal.
    pub controller_private_key: String,
    /// Certificat opérationnel X.509 DER du contrôleur, hexadécimal.
    pub controller_cert_der: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredNode {
    pub node_id: u64,
    pub name: String,
    /// Dernière adresse connue, `[ipv6]:port`.
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub device_types: Vec<String>,
    /// Endpoint portant le cluster Temperature Measurement (0x0402), s'il existe.
    #[serde(default)]
    pub temperature_endpoint: Option<u16>,
    /// Endpoint portant le cluster Relative Humidity Measurement (0x0405), s'il existe.
    #[serde(default)]
    pub humidity_endpoint: Option<u16>,
    /// Endpoint du cluster Boolean State (0x0045) : détecteur d'ouverture.
    #[serde(default)]
    pub contact_endpoint: Option<u16>,
    /// Endpoint du cluster Power Source (0x002F) alimenté par pile.
    #[serde(default)]
    pub battery_endpoint: Option<u16>,
    /// Endpoint du cluster Electrical Power Measurement (0x0090).
    #[serde(default)]
    pub power_endpoint: Option<u16>,
    /// Endpoint du cluster Electrical Energy Measurement (0x0091).
    #[serde(default)]
    pub energy_endpoint: Option<u16>,
    /// Endpoint du cluster On/Off (0x0006).
    #[serde(default)]
    pub on_off_endpoint: Option<u16>,
    /// La structure (endpoints, clusters) a été lue au moins une fois.
    #[serde(default)]
    pub interviewed: bool,
    /// Nom choisi par l'utilisateur (« Porte d'entrée ») ; prioritaire sur `name`,
    /// qui reste le nom annoncé par l'appareil et peut être relu.
    #[serde(default)]
    pub custom_name: Option<String>,
    /// Icône choisie par l'utilisateur (ex. une prise qui alimente une lampe).
    #[serde(default)]
    pub icon: Option<Symbol>,
    /// Version de la lecture de structure ; une version plus ancienne que
    /// `INTERVIEW_VERSION` déclenche une nouvelle lecture (nouveaux clusters suivis).
    #[serde(default)]
    pub interview_version: u32,
}

/// Version courante de la lecture de structure des nœuds.
pub const INTERVIEW_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredState {
    pub version: u32,
    pub identity: ControllerIdentity,
    #[serde(default)]
    pub nodes: Vec<StoredNode>,
}

fn project_dir(organization: &str, application: &str) -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", organization, application)
        .map(|d| d.data_dir().to_path_buf())
}

fn current_dir() -> PathBuf {
    project_dir("matter-home-desktop", "MatterHomeDesktop")
        .unwrap_or_else(|| PathBuf::from("./matter-home-desktop-data"))
}

/// Dossier des versions nommées « matter-demo ».
fn legacy_dir() -> Option<PathBuf> {
    project_dir("matter-demo", "MatterDemo")
}

/// Dossier à utiliser : le nouveau, sauf s'il n'existe pas encore alors que
/// l'ancien existe (migration pas encore faite ou ratée) : jamais de perte de données.
fn resolve_dir(current: PathBuf, legacy: Option<PathBuf>) -> PathBuf {
    match legacy {
        Some(old) if !current.exists() && old.exists() => old,
        _ => current,
    }
}

/// Dossier de données de l'application
/// (macOS : `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop`).
pub fn data_dir() -> PathBuf {
    resolve_dir(current_dir(), legacy_dir())
}

/// Déplace l'ancien dossier vers le nouveau s'il n'y a que l'ancien. `Ok(true)` si déplacé.
fn migrate(old: &Path, new: &Path) -> std::io::Result<bool> {
    if new.exists() || !old.exists() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(old, new)?;
    Ok(true)
}

/// À appeler au démarrage : renomme le dossier de données des versions « matter-demo ».
/// Retourne un message pour le journal si quelque chose s'est passé.
pub fn migrate_legacy_data_dir() -> Option<String> {
    let old = legacy_dir()?;
    let new = current_dir();
    match migrate(&old, &new) {
        Ok(true) => Some(format!(
            "Données déplacées de {} vers {}.",
            old.display(),
            new.display()
        )),
        Ok(false) => None,
        Err(e) => Some(format!(
            "Impossible de déplacer {} vers {} ({e}) : l'ancien dossier reste utilisé.",
            old.display(),
            new.display()
        )),
    }
}

pub fn state_path() -> PathBuf {
    data_dir().join(FILE_NAME)
}

pub fn load(path: &Path) -> Result<Option<StoredState>> {
    load_json(path)
}

/// Écriture atomique (fichier temporaire puis renommage), droits 0600 sous Unix.
pub fn save(path: &Path, state: &StoredState) -> Result<()> {
    save_json(path, state)
}

/// Lit un fichier JSON ; `None` s'il n'existe pas encore.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    let value = serde_json::from_str(&text)
        .with_context(|| format!("fichier {} illisible", path.display()))?;
    Ok(Some(value))
}

/// Écrit un fichier JSON de façon atomique (fichier temporaire puis renommage), droits 0600.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, text).with_context(|| format!("écriture de {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renommage vers {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredState {
        StoredState {
            version: 1,
            identity: ControllerIdentity {
                fabric_id: 42,
                controller_node_id: 112233,
                ca_private_key: "aa".into(),
                ca_cert_der: "bb".into(),
                controller_private_key: "cc".into(),
                controller_cert_der: "dd".into(),
            },
            nodes: vec![StoredNode {
                node_id: 7,
                name: "Capteur".into(),
                temperature_endpoint: Some(1),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn round_trip() {
        let dir =
            std::env::temp_dir().join(format!("matter-home-desktop-test-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        assert!(load(&path).unwrap().is_none());
        save(&path, &sample()).unwrap();
        let back = load(&path).unwrap().unwrap();
        assert_eq!(back.identity.fabric_id, 42);
        assert_eq!(back.nodes, sample().nodes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_dir_migration() {
        let base = std::env::temp_dir().join(format!("mhd-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("dev.matter-demo.MatterDemo");
        let new = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(old.join("history")).unwrap();
        std::fs::write(old.join(FILE_NAME), "{}").unwrap();

        // Tant que rien n'est déplacé, l'ancien dossier est utilisé.
        assert_eq!(resolve_dir(new.clone(), Some(old.clone())), old);
        assert!(migrate(&old, &new).unwrap());
        assert!(new.join(FILE_NAME).exists() && new.join("history").is_dir() && !old.exists());
        assert_eq!(resolve_dir(new.clone(), Some(old.clone())), new);
        // Deuxième lancement : rien à faire.
        assert!(!migrate(&old, &new).unwrap());
        // Les deux existent : le nouveau gagne, l'ancien n'est pas touché.
        std::fs::create_dir_all(&old).unwrap();
        assert!(!migrate(&old, &new).unwrap());
        assert_eq!(resolve_dir(new.clone(), Some(old.clone())), new);
        std::fs::remove_dir_all(base).unwrap();
    }
}
