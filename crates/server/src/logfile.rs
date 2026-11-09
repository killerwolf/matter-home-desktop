//! Journal dans un fichier à droits restreints, avec une rotation simple par taille.
//!
//! Utilisé par `--log-file` (service launchd) : le fichier est créé en 0600 dans un dossier en
//! 0700, et quand il dépasse la taille maximale il devient `.1` (l'ancien `.1` devient `.2`,
//! etc.), les plus anciens étant supprimés. Aucun outil externe : launchd garde ouvert le
//! descripteur d'un fichier de sortie qu'on lui donne, ce qui rendrait une rotation par
//! renommage externe inopérante.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Taille à partir de laquelle le journal est tourné.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Nombre de fichiers anciens conservés (`.1` à `.3`).
pub const DEFAULT_KEEP: usize = 3;

pub struct RotatingLog {
    path: PathBuf,
    file: File,
    size: u64,
    max_bytes: u64,
    keep: usize,
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    // Un fichier créé avant par autre chose (launchd, un essai) : on resserre les droits.
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(file)
}

fn numbered(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

impl RotatingLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::with_limits(path, DEFAULT_MAX_BYTES, DEFAULT_KEEP)
    }

    pub fn with_limits(path: &Path, max_bytes: u64, keep: usize) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            let existed = dir.exists();
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            if !existed {
                std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
            }
        }
        let mut log = Self {
            path: path.to_path_buf(),
            file: open_append(path)?,
            size: 0,
            max_bytes,
            keep,
        };
        log.size = log.file.metadata()?.len();
        if log.size >= log.max_bytes {
            log.rotate()?;
        }
        Ok(log)
    }

    fn rotate(&mut self) -> io::Result<()> {
        if self.keep == 0 {
            // Rien à garder : on repart d'un fichier vide.
            self.file.set_len(0)?;
            self.size = 0;
            return Ok(());
        }
        let _ = std::fs::remove_file(numbered(&self.path, self.keep));
        for n in (1..self.keep).rev() {
            let from = numbered(&self.path, n);
            if from.exists() {
                std::fs::rename(&from, numbered(&self.path, n + 1))?;
            }
        }
        std::fs::rename(&self.path, numbered(&self.path, 1))?;
        self.file = open_append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            // Une rotation ratée (disque plein, droits) ne doit pas faire perdre la ligne.
            let _ = self.rotate();
        }
        let written = self.file.write(buf)?;
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mhd-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn rotates_by_size_keeps_a_few_files_and_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("rotate");
        let path = dir.join("logs").join("server.log");
        let mut log = RotatingLog::with_limits(&path, 100, 2).unwrap();
        for i in 0..12 {
            writeln!(log, "ligne {i:02} {}", "x".repeat(20)).unwrap();
        }
        drop(log);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert!(path.exists() && numbered(&path, 1).exists() && numbered(&path, 2).exists());
        assert!(!numbered(&path, 3).exists(), "au plus 2 anciens fichiers");
        for p in [path.clone(), numbered(&path, 1), numbered(&path, 2)] {
            assert_eq!(mode(&p), 0o600, "{}", p.display());
            assert!(std::fs::metadata(&p).unwrap().len() <= 100);
        }
        // Le plus récent est dans le fichier courant.
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("ligne 11"), "{current}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_oversized_existing_file_is_rotated_at_open_and_perms_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("open");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.log");
        std::fs::write(&path, vec![b'a'; 500]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut log = RotatingLog::with_limits(&path, 100, 3).unwrap();
        writeln!(log, "neuf").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "neuf\n");
        assert_eq!(std::fs::metadata(numbered(&path, 1)).unwrap().len(), 500);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
