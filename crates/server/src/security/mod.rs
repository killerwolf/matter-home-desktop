//! Tout ce qui protège l'accès réseau : certificat TLS, compte unique, sessions, limitation
//! des tentatives. Les fichiers vivent dans `<données>/serveur/` (droits 0700, fichiers 0600),
//! un dossier que ni l'export ni l'import de sauvegarde ne touchent.

pub mod account;
pub mod limiter;
pub mod sessions;
pub mod tls;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::persistence::SERVER_DIR;

/// Dossier des fichiers du serveur réseau, créé en 0700 s'il manque.
pub fn server_dir(data_dir: &Path) -> Result<PathBuf> {
    let dir = data_dir.join(SERVER_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("droits de {}", dir.display()))?;
    }
    Ok(dir)
}

/// Écrit un fichier secret : créé directement en 0600 (jamais lisible par d'autres, même
/// un instant), écriture atomique par renommage.
pub fn write_secret(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("création de {}", tmp.display()))?;
    file.write_all(contents)
        .and_then(|_| file.sync_all())
        .with_context(|| format!("écriture de {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renommage vers {}", path.display()))?;
    Ok(())
}

/// Secondes Unix.
pub fn now() -> f64 {
    crate::events::now_unix()
}
