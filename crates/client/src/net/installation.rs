//! Cette app est une connexion : un identifiant tiré au hasard à la première ouverture et gardé
//! (`installation.json`, à côté du fichier de connexion), et un nom, celui du Mac. Le serveur les
//! reçoit au login (`LoginRequest::installation`) : « Mac du salon » apparaît dans « Mes
//! connexions », et se reconnecter depuis ce Mac retrouve la même connexion.
//!
//! L'identifiant reste après « Se déconnecter » ou « Oublier ce serveur » : c'est le même Mac.
//! Il ne donne aucun accès (le serveur ne l'utilise que pour s'y retrouver).

use std::io::{Read, Write};
use std::path::Path;

use protocol::wire::{ConnectionKind, DeviceClass, Installation};

use super::store::Store;

const FILE_NAME: &str = "installation.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct File {
    id: String,
}

/// Un identifiant neuf : 16 octets du générateur du système, en hexadécimal.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !ok {
        // Sans générateur : heure et processus. Ce n'est qu'un repère, jamais un secret.
        use sha2::{Digest, Sha256};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let digest = Sha256::digest(format!("{now}-{}", std::process::id()));
        bytes.copy_from_slice(&digest[..16]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// L'identifiant de cette app : lu, ou créé (0600) à la première demande. Si le fichier ne peut
/// pas être écrit, l'identifiant ne tient que pour cette ouverture (une connexion « de plus »
/// au prochain lancement, que le serveur remplace dès qu'on se reconnecte avec le même).
pub fn id_for(dir: &Path) -> String {
    let path = dir.join(FILE_NAME);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(file) = serde_json::from_str::<File>(&text) {
            if (8..=64).contains(&file.id.len())
                && file
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                return file.id;
            }
        }
    }
    let id = new_id();
    let _ = write_private(&path, &File { id: id.clone() });
    id
}

fn write_private(path: &Path, file: &File) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut out = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?
    };
    out.write_all(serde_json::to_string(file).unwrap_or_default().as_bytes())?;
    out.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Le nom du Mac (« Mac du salon ») : `scutil --get ComputerName`, une lecture seule du système ;
/// à défaut « Mac ».
pub fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".to_string())
}

/// Ce que cette app dit d'elle-même au login.
pub fn describe(store: &Store) -> Installation {
    let dir = store
        .path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Installation {
        id: id_for(&dir),
        kind: ConnectionKind::Mac,
        device: DeviceClass::Computer,
        name: computer_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_created_once_kept_and_private() {
        let dir = std::env::temp_dir().join(format!("ostal-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = id_for(&dir);
        assert_eq!(first.len(), 32);
        assert_eq!(id_for(&dir), first, "le même à chaque ouverture");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Fichier abîmé : un nouvel identifiant, jamais une erreur.
        std::fs::write(dir.join(FILE_NAME), "pas du json").unwrap();
        let again = id_for(&dir);
        assert_eq!(again.len(), 32);
        assert_ne!(again, first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
