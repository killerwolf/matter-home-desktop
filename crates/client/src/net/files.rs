//! Lecture et écriture des fichiers que l'utilisateur choisit (export, CSV, sauvegarde à
//! importer). Ces fonctions bloquent : elles ne tournent que dans les fils réseau, jamais dans
//! le fil de l'interface.

use std::path::Path;

/// Taille maximale d'une sauvegarde acceptée à l'import (le serveur a sa propre limite).
pub const MAX_BACKUP_BYTES: u64 = 128 * 1024 * 1024;

/// Écrit un fichier reçu du serveur. `private` : en 0600 (la sauvegarde contient des clés).
pub fn write_file(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        if private {
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        if private {
            // Un fichier écrasé garde ses anciens droits : on les resserre.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = private;
        let mut file = std::fs::File::create(path)?;
        file.write_all(bytes)
    }
}

/// Lit une sauvegarde à importer, en refusant un fichier plus gros que `max`.
pub fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("fichier illisible ({})", e.kind()))?;
    if meta.len() > max {
        return Err("fichier trop volumineux pour une sauvegarde Matter Home".into());
    }
    std::fs::read(path).map_err(|e| format!("fichier illisible ({})", e.kind()))
}
