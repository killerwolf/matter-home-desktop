//! Les événements sur disque : `<données>/events/`, trois fichiers par passage, droits 0600 :
//!
//! - `<id>.json` : l'événement (`DetectionEventView`) ;
//! - `<id>.jpg` : la meilleure image du passage ;
//! - `<id>.mp4` : le clip (avant + passage + après), sans réencodage.
//!
//! Rétention comme l'historique (`history_store.rs`) : 7 jours, et un plafond de taille **total**
//! (2 Go par défaut, [`DetectionConfig::max_total_bytes`]) au-delà duquel les plus anciens partent,
//! appliqués au démarrage puis toutes les heures. L'identifiant (`<début en ms>-<caméra>`) est le
//! seul texte qui entre dans un nom de fichier, et il est vérifié (`wire::is_event_id`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use protocol::wire::{is_event_id, DetectionEventView};

/// Âge maximal des événements gardés.
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
/// Plafond par défaut de la taille totale des événements.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);
pub const DIR: &str = "events";

/// Ce qu'on peut lire d'un événement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Snapshot,
    Clip,
}

impl Part {
    fn extension(self) -> &'static str {
        match self {
            Part::Snapshot => "jpg",
            Part::Clip => "mp4",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    pub kept: usize,
    pub removed: usize,
    pub bytes: u64,
}

/// Le dossier des événements. Les écritures sont sérialisées (un seul fil écrit à la fois).
pub struct EventStore {
    dir: PathBuf,
    retention: Duration,
    max_total_bytes: Mutex<u64>,
    write: Mutex<()>,
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(&tmp)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    std::fs::rename(tmp, path)
}

impl EventStore {
    pub fn new(dir: PathBuf, max_total_bytes: u64) -> Self {
        Self::with_retention(dir, RETENTION, max_total_bytes)
    }

    pub fn with_retention(dir: PathBuf, retention: Duration, max_total_bytes: u64) -> Self {
        Self {
            dir,
            retention,
            max_total_bytes: Mutex::new(max_total_bytes),
            write: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn max_total_bytes(&self) -> u64 {
        *self
            .max_total_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_max_total_bytes(&self, bytes: u64) {
        *self
            .max_total_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = bytes;
    }

    fn path(&self, id: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{id}.{extension}"))
    }

    /// Enregistre un événement : capture et clip d'abord, la fiche en dernier (une fiche présente
    /// a ses fichiers).
    pub fn save(
        &self,
        event: &DetectionEventView,
        jpeg: Option<&[u8]>,
        clip: Option<&[u8]>,
    ) -> std::io::Result<()> {
        if !is_event_id(&event.id) {
            return Err(std::io::Error::other("identifiant d'événement invalide"));
        }
        let _write = self.write.lock().unwrap_or_else(|e| e.into_inner());
        crate::persistence::ensure_private_dir(&self.dir).map_err(std::io::Error::other)?;
        if let Some(jpeg) = jpeg {
            write_private(&self.path(&event.id, "jpg"), jpeg)?;
        }
        if let Some(clip) = clip {
            write_private(&self.path(&event.id, "mp4"), clip)?;
        }
        let json = serde_json::to_vec_pretty(event).map_err(std::io::Error::other)?;
        write_private(&self.path(&event.id, "json"), &json)
    }

    /// Toutes les fiches lisibles, les plus récentes d'abord.
    pub fn all(&self) -> Vec<DetectionEventView> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut events: Vec<DetectionEventView> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| std::fs::read(&p).ok())
            .filter_map(|b| serde_json::from_slice::<DetectionEventView>(&b).ok())
            .filter(|e| is_event_id(&e.id))
            .collect();
        events.sort_by(|a, b| b.start.total_cmp(&a.start).then(b.id.cmp(&a.id)));
        events
    }

    /// Les événements terminés après `since`, de cette caméra, au plus `limit`.
    pub fn list(&self, since: f64, camera: Option<u64>, limit: usize) -> Vec<DetectionEventView> {
        self.all()
            .into_iter()
            .filter(|e| e.end >= since)
            .filter(|e| camera.is_none_or(|c| c == e.camera_id))
            .take(limit)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<DetectionEventView> {
        if !is_event_id(id) {
            return None;
        }
        let bytes = std::fs::read(self.path(id, "json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Le chemin de la capture ou du clip, s'il existe.
    pub fn part(&self, id: &str, part: Part) -> Option<PathBuf> {
        if !is_event_id(id) {
            return None;
        }
        let path = self.path(id, part.extension());
        path.is_file().then_some(path)
    }

    /// Supprime un événement. `false` : il n'existait pas.
    pub fn delete(&self, id: &str) -> bool {
        if !is_event_id(id) {
            return false;
        }
        let _write = self.write.lock().unwrap_or_else(|e| e.into_inner());
        self.remove_files(id)
    }

    fn remove_files(&self, id: &str) -> bool {
        // La fiche d'abord : un événement sans fiche n'est plus listé.
        let existed = std::fs::remove_file(self.path(id, "json")).is_ok();
        for ext in ["jpg", "mp4", "tmp"] {
            let _ = std::fs::remove_file(self.path(id, ext));
        }
        existed
    }

    /// Rétention par âge, puis plafond de taille : les plus anciens partent d'abord. Les fichiers
    /// orphelins (sans fiche, écriture interrompue) partent aussi.
    pub fn prune(&self, now: f64) -> PruneReport {
        let _write = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let since = now - self.retention.as_secs_f64();
        let mut events = self.all();
        let mut report = PruneReport::default();
        let size = |id: &str| -> u64 {
            ["json", "jpg", "mp4"]
                .iter()
                .filter_map(|ext| std::fs::metadata(self.path(id, ext)).ok())
                .map(|m| m.len())
                .sum()
        };
        // Les plus récents d'abord : on garde tant que le plafond le permet.
        let max = self.max_total_bytes();
        let mut kept_ids = std::collections::HashSet::new();
        let mut total = 0u64;
        for e in events.drain(..) {
            let bytes = size(&e.id);
            if e.end < since || total + bytes > max {
                self.remove_files(&e.id);
                report.removed += 1;
            } else {
                total += bytes;
                report.kept += 1;
                kept_ids.insert(e.id);
            }
        }
        // Orphelins.
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for path in entries.filter_map(Result::ok).map(|e| e.path()) {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !kept_ids.contains(stem) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        report.bytes = total;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::DetectedClass;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mh-events-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn event(start: f64, camera: u64) -> DetectionEventView {
        DetectionEventView {
            id: format!("{}-{camera}", (start * 1000.0) as u64),
            camera_id: camera,
            camera_name: "Entrée".into(),
            classes: vec![DetectedClass::Personne],
            score: 0.8,
            start,
            end: start + 5.0,
            has_clip: true,
            clip_s: 20.0,
        }
    }

    #[test]
    fn save_list_get_delete() {
        let dir = temp();
        let store = EventStore::new(dir.join("events"), DEFAULT_MAX_TOTAL_BYTES);
        for (i, cam) in [(0, 1u64), (1, 2), (2, 1)] {
            let e = event(1000.0 + f64::from(i) * 100.0, cam);
            store.save(&e, Some(b"jpeg"), Some(b"mp4")).unwrap();
        }
        let all = store.list(0.0, None, 10);
        assert_eq!(all.len(), 3);
        assert!(all[0].start > all[1].start, "les plus récents d'abord");
        assert_eq!(store.list(0.0, Some(1), 10).len(), 2);
        assert_eq!(store.list(1150.0, None, 10).len(), 1);
        assert_eq!(store.list(0.0, None, 1).len(), 1);
        let id = all[0].id.clone();
        assert_eq!(store.get(&id).unwrap(), all[0]);
        assert!(store.part(&id, Part::Clip).is_some());
        assert!(store.part("../../x", Part::Clip).is_none());
        assert!(store.delete(&id));
        assert!(!store.delete(&id));
        assert!(store.part(&id, Part::Snapshot).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let other = &all[1].id;
            let mode = std::fs::metadata(store.part(other, Part::Clip).unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_by_age_then_by_size() {
        let dir = temp();
        let store = EventStore::with_retention(dir.clone(), Duration::from_secs(1000), u64::MAX);
        let clip = vec![0u8; 10_000];
        for i in 0..5 {
            store
                .save(&event(f64::from(i) * 400.0, 7), None, Some(&clip))
                .unwrap();
        }
        std::fs::write(dir.join("123-4.mp4"), b"orphelin").unwrap();
        // now = 2000 : garde ceux finis après 1000 (débuts 1200 et 1600).
        let report = store.prune(2000.0);
        assert_eq!((report.kept, report.removed), (2, 3));
        assert!(!dir.join("123-4.mp4").exists());
        // Plafond : un seul tient.
        store.set_max_total_bytes(15_000);
        let report = store.prune(2000.0);
        assert_eq!(report.kept, 1);
        assert_eq!(store.all()[0].start, 1600.0, "le plus récent reste");
        let _ = std::fs::remove_dir_all(dir);
    }
}
