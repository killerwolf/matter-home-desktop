//! Seul fichier que le client écrit : l'adresse du serveur, l'empreinte de son certificat et le
//! jeton de session. Jamais le mot de passe. Aucune mesure, aucun plan, aucune règle.
//!
//! Le fichier est créé en 0600 dans un dossier propre au client, **distinct** du dossier de
//! données de la v0.2.1 (`dev.matter-home-desktop.MatterHomeDesktop`) : le client ne lit,
//! n'écrit ni ne supprime rien dans ce dernier.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::NetError;

/// Nom du dossier du client (la v0.2.1 utilisait `MatterHomeDesktop`).
const APP_NAME: &str = "MatterHomeClient";
const FILE_NAME: &str = "connexion.json";

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Saved {
    /// Forme canonique, `https://hôte:port`.
    pub url: String,
    /// Empreinte SHA-256 du certificat approuvé, `AA:BB:…`.
    pub fingerprint: String,
    /// Absent après une déconnexion ou un jeton refusé : l'adresse et l'empreinte restent, il
    /// suffit alors de se reconnecter.
    #[serde(default)]
    pub token: Option<String>,
    /// Autres adresses du **même** serveur (à distance : nom Tailscale, IP 100.x…), essayées dans
    /// l'ordre après `url`. Même certificat, donc même empreinte. Absent des anciens fichiers
    /// (lus sans changement) ; non écrit quand il est vide, pour qu'un ancien client relise le
    /// fichier tel quel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alt_urls: Vec<String>,
}

// Pas de `Debug` dérivé : le jeton ne doit jamais finir dans un journal.
impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("url", &self.url)
            .field("fingerprint", &self.fingerprint)
            .field("token", &self.token.as_ref().map(|_| "…"))
            .field("alt_urls", &self.alt_urls)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// Fichier à un emplacement précis (tests).
    pub fn at(path: PathBuf) -> Store {
        Store { path }
    }

    /// Emplacement habituel : `~/Library/Application Support/dev.matter-home-desktop.MatterHomeClient/`
    /// sous macOS (suit `$HOME`).
    pub fn default_location() -> Option<Store> {
        let dirs = directories::ProjectDirs::from("dev", "matter-home-desktop", APP_NAME)?;
        Some(Store::at(dirs.data_dir().join(FILE_NAME)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Ok(None)` si le fichier n'existe pas. Un fichier illisible est une erreur (jamais
    /// supprimée en silence).
    pub fn load(&self) -> Result<Option<Saved>, NetError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(NetError::Local(format!(
                    "Le fichier de connexion est illisible ({}).",
                    e.kind()
                )))
            }
        };
        #[cfg(unix)]
        tighten_permissions(&self.path);
        let saved: Saved = serde_json::from_str(&text).map_err(|_| {
            NetError::Local(
                "Le fichier de connexion est abîmé : reconnectez-vous au serveur.".into(),
            )
        })?;
        Ok(Some(saved))
    }

    /// Écriture atomique, en 0600 dès la création.
    pub fn save(&self, saved: &Saved) -> Result<(), NetError> {
        let local = |what: &str, e: std::io::Error| {
            NetError::Local(format!(
                "Impossible d'enregistrer la connexion ({what} : {}).",
                e.kind()
            ))
        };
        let dir = self.path.parent().ok_or_else(|| {
            NetError::Local("Emplacement du fichier de connexion invalide.".into())
        })?;
        create_private_dir(dir).map_err(|e| local("dossier", e))?;
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(saved)
            .map_err(|_| NetError::Local("Connexion impossible à enregistrer.".into()))?;
        let result = (|| {
            let _ = std::fs::remove_file(&tmp);
            let mut file = private_file(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(local("fichier", e));
        }
        Ok(())
    }

    /// Supprime le fichier (déconnexion). Absent : rien à faire.
    pub fn delete(&self) -> Result<(), NetError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(NetError::Local(format!(
                "Impossible de supprimer le fichier de connexion ({}).",
                e.kind()
            ))),
        }
    }
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Un fichier copié ou restauré avec des droits trop larges est resserré à 0600.
#[cfg(unix)]
fn tighten_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mhd-store-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn saved() -> Saved {
        Saved {
            url: "https://127.0.0.1:8443".into(),
            fingerprint: "AA:BB".into(),
            token: Some("jeton-secret".into()),
            alt_urls: Vec::new(),
        }
    }

    #[test]
    fn round_trip_with_private_permissions_and_no_password_field() {
        let dir = temp("rt");
        let store = Store::at(dir.join("sous/dossier/connexion.json"));
        assert_eq!(store.load().unwrap(), None);
        store.save(&saved()).unwrap();
        assert_eq!(store.load().unwrap(), Some(saved()));
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("password") && !text.contains("mot_de_passe"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        // Remplacement : toujours en 0600, pas de fichier temporaire oublié.
        store
            .save(&Saved {
                token: None,
                ..saved()
            })
            .unwrap();
        assert_eq!(store.load().unwrap().unwrap().token, None);
        assert!(!store.path().with_extension("json.tmp").exists());
        store.delete().unwrap();
        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_file_without_alternate_addresses_still_loads_and_is_not_rewritten_with_them() {
        let dir = temp("old");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connexion.json");
        std::fs::write(
            &path,
            r#"{"url":"https://192.168.1.20:8443","fingerprint":"AA:BB","token":"t"}"#,
        )
        .unwrap();
        let store = Store::at(path);
        let loaded = store.load().unwrap().unwrap();
        assert!(loaded.alt_urls.is_empty());
        store.save(&loaded).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("alt_urls"), "{text}");
        let with = Saved {
            alt_urls: vec!["https://mac.ts.net:8443".into()],
            ..loaded
        };
        store.save(&with).unwrap();
        assert_eq!(store.load().unwrap(), Some(with));
        // Un ancien client (sans ce champ) relit un fichier récent : le champ inconnu est ignoré.
        #[derive(serde::Deserialize)]
        struct Old {
            url: String,
        }
        let old: Old =
            serde_json::from_str(&std::fs::read_to_string(store.path()).unwrap()).unwrap();
        assert_eq!(old.url, "https://192.168.1.20:8443");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_is_an_error_and_is_kept() {
        let dir = temp("broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connexion.json");
        std::fs::write(&path, "{pas du json").unwrap();
        let store = Store::at(path.clone());
        assert!(matches!(store.load(), Err(NetError::Local(_))));
        assert!(
            path.exists(),
            "un fichier abîmé n'est pas supprimé en silence"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_hides_the_token() {
        let text = format!("{:?}", saved());
        assert!(!text.contains("jeton-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("loose");
        let store = Store::at(dir.join("c.json"));
        store.save(&saved()).unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        store.load().unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_location_is_not_the_v0_2_1_data_dir() {
        if let Some(store) = Store::default_location() {
            let path = store.path().to_string_lossy().to_string();
            assert!(!path.contains("MatterHomeDesktop"), "{path}");
        }
    }
}
