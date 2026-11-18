//! Les événements sur disque : `<données>/events/`, trois fichiers par passage, droits 0600 :
//!
//! - `<id>.json` : l'événement (`DetectionEventView`) ;
//! - `<id>.jpg` : la meilleure image du passage ;
//! - `<id>.mp4` : le clip (avant + passage + après), sans réencodage.
//!
//! Rétention comme l'historique (`history_store.rs`) : 7 jours, et un plafond de taille **total**
//! (2 Go par défaut, `DetectionConfig::max_total_bytes`) au-delà duquel les plus anciens partent :
//! au démarrage, toutes les heures, et dès qu'un enregistrement fait passer le total au-dessus.
//! Aucun clip n'est écrit quand le disque est presque plein ([`MIN_FREE_BYTES`]).
//!
//! Un **index en mémoire** (lu une fois sur le disque) sert les listes sans relire les fiches.
//! Une fiche illisible n'est jamais supprimée : elle est ignorée (et ses fichiers gardés).
//! L'identifiant (`<début en ms>-<caméra>`) est le seul texte qui entre dans un nom de fichier, et
//! il est vérifié (`wire::is_event_id`).

use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use protocol::wire::{is_event_id, DetectionEventView};

/// Âge maximal des événements gardés.
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
/// Plafond par défaut de la taille totale des événements.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);
pub const DIR: &str = "events";
/// Pas de clip écrit avec moins d'espace libre que ça (2 Go), ou 5 % d'un petit disque.
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MIN_FREE_RATIO: f64 = 0.05;

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

#[derive(Default)]
struct Index {
    loaded: bool,
    /// Fiche et taille de ses trois fichiers.
    events: BTreeMap<String, (DetectionEventView, u64)>,
    /// Fiches illisibles (gardées sur le disque, jamais listées ni supprimées).
    unreadable: HashSet<String>,
}

impl Index {
    fn total(&self) -> u64 {
        self.events.values().map(|(_, b)| *b).sum()
    }
}

/// Le dossier des événements. Les écritures sont sérialisées (un seul fil écrit à la fois).
pub struct EventStore {
    dir: PathBuf,
    retention: Duration,
    max_total_bytes: Mutex<u64>,
    index: Mutex<Index>,
    /// Seuil d'espace libre imposé (tests) ; sinon [`free_threshold`].
    min_free: Mutex<Option<u64>>,
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

/// Espace libre et taille du disque qui porte `dir` (`statvfs`).
#[cfg(unix)]
pub fn disk_space(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY : `statvfs` remplit la structure fournie ; le chemin est une chaîne C valable.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    Some((stat.f_bavail as u64 * block, stat.f_blocks as u64 * block))
}

#[cfg(not(unix))]
pub fn disk_space(_dir: &Path) -> Option<(u64, u64)> {
    None
}

/// Le seuil effectif : 2 Go, ou 5 % d'un disque plus petit que 40 Go.
pub fn free_threshold(total: u64, min_free: u64) -> u64 {
    min_free.min((total as f64 * MIN_FREE_RATIO) as u64)
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
            index: Mutex::new(Index::default()),
            min_free: Mutex::new(None),
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

    #[cfg(test)]
    pub fn set_min_free(&self, bytes: u64) {
        *self.min_free.lock().unwrap_or_else(|e| e.into_inner()) = Some(bytes);
    }

    fn path(&self, id: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{id}.{extension}"))
    }

    fn size(&self, id: &str) -> u64 {
        ["json", "jpg", "mp4"]
            .iter()
            .filter_map(|ext| std::fs::metadata(self.path(id, ext)).ok())
            .map(|m| m.len())
            .sum()
    }

    /// L'index, lu sur le disque la première fois.
    fn index(&self) -> MutexGuard<'_, Index> {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if !index.loaded {
            index.loaded = true;
            self.load(&mut index);
        }
        index
    }

    fn load(&self, index: &mut Index) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let parsed = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<DetectionEventView>(&b).ok())
                .filter(|e| is_event_id(&e.id) && e.id == stem);
            match parsed {
                Some(mut event) => {
                    if event.in_progress {
                        // La fiche est lue une seule fois, avant que cette exécution n'en écrive
                        // une : un événement encore « en cours » vient d'une exécution précédente
                        // (serveur arrêté pendant le passage), quel que soit son âge : clos tel quel.
                        event.in_progress = false;
                        if let Ok(json) = serde_json::to_vec_pretty(&event) {
                            let _ = write_private(&path, &json);
                        }
                    }
                    let bytes = self.size(&stem);
                    index.events.insert(stem, (event, bytes));
                }
                None => {
                    log::warn!("événement illisible ignoré : events/{stem}.json");
                    index.unreadable.insert(stem);
                }
            }
        }
    }

    /// Enregistre un événement (en cours ou terminé) : capture et clip d'abord, la fiche en
    /// dernier (une fiche présente a ses fichiers). Le clip n'est pas écrit si le disque est presque
    /// plein (`has_clip` devient faux). Rend la fiche enregistrée. Le plafond total est appliqué.
    pub fn save(
        &self,
        mut event: DetectionEventView,
        jpeg: Option<&[u8]>,
        clip: Option<&[u8]>,
    ) -> std::io::Result<DetectionEventView> {
        if !is_event_id(&event.id) {
            return Err(std::io::Error::other("identifiant d'événement invalide"));
        }
        let mut index = self.index();
        crate::persistence::ensure_private_dir(&self.dir).map_err(std::io::Error::other)?;
        if let Some(jpeg) = jpeg {
            write_private(&self.path(&event.id, "jpg"), jpeg)?;
        }
        match clip {
            Some(clip) => {
                let forced = *self.min_free.lock().unwrap_or_else(|e| e.into_inner());
                let room = disk_space(&self.dir).is_none_or(|(free, total)| {
                    let threshold = forced.unwrap_or_else(|| free_threshold(total, MIN_FREE_BYTES));
                    free.saturating_sub(clip.len() as u64) >= threshold
                });
                if room {
                    write_private(&self.path(&event.id, "mp4"), clip)?;
                } else {
                    log::warn!(
                        "disque presque plein : clip de l'événement {} non enregistré",
                        event.id
                    );
                    event.has_clip = false;
                    event.clip_s = 0.0;
                }
            }
            None => {
                if !self.path(&event.id, "mp4").is_file() {
                    event.has_clip = false;
                }
            }
        }
        // La description (ticket n° 19) arrive à part et peut précéder la fin du passage : une
        // fiche qui n'en porte pas garde celle déjà enregistrée.
        if event.description.is_none() {
            if let Some((old, _)) = index.events.get(&event.id) {
                event.description.clone_from(&old.description);
                event.niveau = old.niveau;
            }
        }
        let json = serde_json::to_vec_pretty(&event).map_err(std::io::Error::other)?;
        write_private(&self.path(&event.id, "json"), &json)?;
        let bytes = self.size(&event.id);
        index
            .events
            .insert(event.id.clone(), (event.clone(), bytes));
        if index.total() > self.max_total_bytes() {
            let now = chrono::Utc::now().timestamp() as f64;
            self.prune_locked(&mut index, now, Some(&event.id));
        }
        Ok(event)
    }

    /// Enregistre la description d'un événement (ticket n° 19) et rend la fiche à jour (`None` :
    /// l'événement n'existe plus). Seule la fiche est réécrite.
    pub fn set_description(
        &self,
        id: &str,
        description: String,
        niveau: Option<u8>,
    ) -> Option<DetectionEventView> {
        if !is_event_id(id) {
            return None;
        }
        let mut index = self.index();
        let entry = index.events.get_mut(id)?;
        entry.0.description = Some(description);
        entry.0.niveau = niveau;
        let event = entry.0.clone();
        let json = serde_json::to_vec_pretty(&event).ok()?;
        if let Err(e) = write_private(&self.path(id, "json"), &json) {
            log::warn!("description de l'événement {id} non enregistrée : {e}");
        }
        Some(event)
    }

    /// Toutes les fiches, les plus récentes d'abord.
    pub fn all(&self) -> Vec<DetectionEventView> {
        self.list(f64::NEG_INFINITY, None, usize::MAX)
    }

    /// Les événements terminés après `since`, de cette caméra, au plus `limit`.
    pub fn list(&self, since: f64, camera: Option<u64>, limit: usize) -> Vec<DetectionEventView> {
        let index = self.index();
        let mut events: Vec<&DetectionEventView> = index
            .events
            .values()
            .map(|(e, _)| e)
            .filter(|e| e.end >= since)
            .filter(|e| camera.is_none_or(|c| c == e.camera_id))
            .collect();
        events.sort_by(|a, b| b.start.total_cmp(&a.start).then(b.id.cmp(&a.id)));
        events.into_iter().take(limit).cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<DetectionEventView> {
        if !is_event_id(id) {
            return None;
        }
        self.index().events.get(id).map(|(e, _)| e.clone())
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
        let mut index = self.index();
        let existed = index.events.remove(id).is_some();
        if existed {
            self.remove_files(id);
        }
        existed
    }

    fn remove_files(&self, id: &str) {
        // La fiche d'abord : un événement sans fiche n'est plus listé.
        let _ = std::fs::remove_file(self.path(id, "json"));
        for ext in ["jpg", "mp4", "tmp"] {
            let _ = std::fs::remove_file(self.path(id, ext));
        }
    }

    /// Rétention par âge, puis plafond de taille : les plus anciens partent d'abord. Les fichiers
    /// sans fiche (écriture interrompue) partent aussi ; ceux d'une fiche illisible restent.
    pub fn prune(&self, now: f64) -> PruneReport {
        let mut index = self.index();
        self.prune_locked(&mut index, now, None)
    }

    /// `keep` : l'événement qu'on vient d'écrire, jamais retiré par le plafond.
    fn prune_locked(&self, index: &mut Index, now: f64, keep: Option<&str>) -> PruneReport {
        let since = now - self.retention.as_secs_f64();
        let max = self.max_total_bytes();
        let mut report = PruneReport::default();
        let mut order: Vec<(String, f64, f64, u64)> = index
            .events
            .iter()
            .map(|(id, (e, b))| (id.clone(), e.start, e.end, *b))
            .collect();
        // Les plus récents d'abord : on garde tant que le plafond le permet.
        order.sort_by(|a, b| b.1.total_cmp(&a.1));
        if let Some(k) = keep {
            if let Some(pos) = order.iter().position(|o| o.0 == k) {
                let item = order.remove(pos);
                order.insert(0, item);
            }
        }
        let mut total = 0u64;
        for (id, _, end, bytes) in order {
            let pinned = keep == Some(id.as_str());
            if (end < since || total + bytes > max) && !pinned {
                index.events.remove(&id);
                self.remove_files(&id);
                report.removed += 1;
            } else {
                total += bytes;
                report.kept += 1;
            }
        }
        // Fichiers orphelins : sans fiche du tout (ni lisible, ni illisible).
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for path in entries.filter_map(Result::ok).map(|e| e.path()) {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !index.events.contains_key(stem)
                    && !index.unreadable.contains(stem)
                    && !self.path(stem, "json").is_file()
                {
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
            in_progress: false,
            description: None,
            niveau: None,
        }
    }

    #[test]
    fn save_list_get_delete() {
        let dir = temp();
        let store = EventStore::new(dir.join("events"), DEFAULT_MAX_TOTAL_BYTES);
        store.set_min_free(0);
        for (i, cam) in [(0, 1u64), (1, 2), (2, 1)] {
            let e = event(1000.0 + f64::from(i) * 100.0, cam);
            store.save(e, Some(b"jpeg"), Some(b"mp4")).unwrap();
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
        // Relu par un autre magasin (redémarrage) : la même liste.
        let again = EventStore::new(dir.join("events"), DEFAULT_MAX_TOTAL_BYTES);
        assert_eq!(again.all(), store.all());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_by_age_then_by_size_and_on_save() {
        let dir = temp();
        let store = EventStore::with_retention(dir.clone(), Duration::from_secs(1000), u64::MAX);
        store.set_min_free(0);
        let clip = vec![0u8; 10_000];
        for i in 0..5 {
            store
                .save(event(f64::from(i) * 400.0, 7), None, Some(&clip))
                .unwrap();
        }
        std::fs::write(dir.join("123-4.mp4"), b"orphelin").unwrap();
        // Une fiche illisible : ni listée, ni supprimée, ses fichiers restent.
        std::fs::write(dir.join("999-4.json"), b"{pas du json").unwrap();
        std::fs::write(dir.join("999-4.mp4"), b"clip").unwrap();
        let store = EventStore::with_retention(dir.clone(), Duration::from_secs(1000), u64::MAX);
        store.set_min_free(0);
        // now = 2000 : garde ceux finis après 1000 (débuts 1200 et 1600).
        let report = store.prune(2000.0);
        assert_eq!((report.kept, report.removed), (2, 3));
        assert!(!dir.join("123-4.mp4").exists());
        assert!(dir.join("999-4.json").exists() && dir.join("999-4.mp4").exists());
        // Plafond : un seul tient.
        store.set_max_total_bytes(15_000);
        let report = store.prune(2000.0);
        assert_eq!(report.kept, 1);
        assert_eq!(store.all()[0].start, 1600.0, "le plus récent reste");
        // Un nouvel enregistrement qui dépasse le plafond fait partir le plus ancien aussitôt.
        let now = chrono::Utc::now().timestamp() as f64;
        store.set_max_total_bytes(25_000);
        store.save(event(now, 7), None, Some(&clip)).unwrap();
        store.save(event(now + 1.0, 7), None, Some(&clip)).unwrap();
        let ids: Vec<f64> = store.all().iter().map(|e| e.start).collect();
        assert_eq!(ids, [now + 1.0, now], "{ids:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_clip_on_a_full_disk() {
        let dir = temp();
        let store = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        store.set_min_free(u64::MAX);
        let saved = store
            .save(event(1000.0, 3), Some(b"jpeg"), Some(b"mp4"))
            .unwrap();
        assert!(!saved.has_clip, "pas de place : pas de clip");
        assert!(store.part(&saved.id, Part::Clip).is_none());
        assert!(disk_space(&dir).is_some());
        assert_eq!(free_threshold(1000, u64::MAX), 50);
        assert_eq!(free_threshold(1 << 40, 2 << 30), 2 << 30);
        assert!(store.part(&saved.id, Part::Snapshot).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_description_survives_the_closing_of_the_passage() {
        let dir = temp();
        let store = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        let mut e = event(1000.0, 3);
        e.in_progress = true;
        e.has_clip = false;
        let id = e.id.clone();
        store.save(e.clone(), Some(b"jpeg"), None).unwrap();
        let described = store
            .set_description(&id, "Une personne devant la porte.".into(), Some(1))
            .unwrap();
        assert_eq!(described.niveau, Some(1));
        // La fin du passage arrive sans description : celle d'avant est gardée.
        e.in_progress = false;
        let closed = store.save(e, Some(b"jpeg"), None).unwrap();
        assert_eq!(
            closed.description.as_deref(),
            Some("Une personne devant la porte.")
        );
        assert_eq!(closed.niveau, Some(1));
        // Relue sur le disque, et un événement supprimé n'en reçoit plus.
        let again = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        assert_eq!(again.get(&id).unwrap().niveau, Some(1));
        assert!(store.delete(&id));
        assert!(store.set_description(&id, "x".into(), None).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_in_progress_event_is_also_closed_at_startup() {
        let dir = temp();
        let store = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        let now = chrono::Utc::now().timestamp() as f64;
        let mut e = event(now - 1.0, 3);
        e.in_progress = true;
        e.has_clip = false;
        store.save(e, Some(b"jpeg"), None).unwrap();
        // Une nouvelle exécution : le passage récent ne peut pas être celui de cette exécution.
        let again = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        assert!(!again.all()[0].in_progress);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_in_progress_event_is_closed_on_reload() {
        let dir = temp();
        let store = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        let mut e = event(1000.0, 3);
        e.in_progress = true;
        e.has_clip = false;
        store.save(e, Some(b"jpeg"), None).unwrap();
        let again = EventStore::new(dir.clone(), DEFAULT_MAX_TOTAL_BYTES);
        assert!(!again.all()[0].in_progress);
        let _ = std::fs::remove_dir_all(dir);
    }
}
