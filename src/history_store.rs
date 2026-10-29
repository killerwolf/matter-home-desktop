//! Historique des mesures sur disque : un fichier JSON Lines par appareil dans
//! `<données>/history/`, complété au fil de l'eau et relu au démarrage.
//!
//! Garde-fous contre le grossissement : rétention par âge (7 jours) et taille
//! maximale par appareil (5 Mo), appliquées au démarrage puis toutes les 6 h,
//! plus un effacement manuel par appareil. L'écriture se fait sur un thread
//! dédié : ni l'interface ni le runtime réseau n'attendent le disque.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::events::{Event, MeasureKind, StateKind};

/// Âge maximal des mesures conservées.
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
/// Taille maximale du fichier d'un appareil ; au-delà, les plus anciennes lignes sont retirées.
pub const MAX_BYTES_PER_NODE: u64 = 5 * 1024 * 1024;
/// Intervalle entre deux nettoyages automatiques.
const PRUNE_EVERY: Duration = Duration::from_secs(6 * 3600);

/// Une ligne du fichier : instant (s Unix), grandeur, valeur (0/1 pour un état).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub t: f64,
    pub k: String,
    pub v: f64,
}

impl Record {
    pub fn from_event(event: &Event) -> Option<(u64, Record)> {
        let (node_id, t, k, v) = match event {
            Event::Measurement {
                node_id,
                kind,
                value,
                at,
            } => (*node_id, *at, measure_key(*kind), *value),
            Event::State {
                node_id,
                kind,
                value,
                at,
            } => (*node_id, *at, state_key(*kind), f64::from(u8::from(*value))),
            _ => return None,
        };
        Some((
            node_id,
            Record {
                t,
                k: k.to_string(),
                v,
            },
        ))
    }

    pub fn to_event(&self, node_id: u64) -> Option<Event> {
        let at = self.t;
        let measure = |kind| {
            Some(Event::Measurement {
                node_id,
                kind,
                value: self.v,
                at,
            })
        };
        let state = |kind| {
            Some(Event::State {
                node_id,
                kind,
                value: self.v != 0.0,
                at,
            })
        };
        match self.k.as_str() {
            "temperature" => measure(MeasureKind::Temperature),
            "humidity" => measure(MeasureKind::Humidity),
            "power" => measure(MeasureKind::Power),
            "energy" => measure(MeasureKind::Energy),
            "battery" => measure(MeasureKind::Battery),
            "position" => measure(MeasureKind::Position),
            "open" => state(StateKind::Open),
            "on_off" => state(StateKind::OnOff),
            "battery_low" => state(StateKind::BatteryLow),
            _ => None,
        }
    }
}

fn measure_key(kind: MeasureKind) -> &'static str {
    match kind {
        MeasureKind::Temperature => "temperature",
        MeasureKind::Humidity => "humidity",
        MeasureKind::Power => "power",
        MeasureKind::Energy => "energy",
        MeasureKind::Battery => "battery",
        MeasureKind::Position => "position",
    }
}

fn state_key(kind: StateKind) -> &'static str {
    match kind {
        StateKind::Open => "open",
        StateKind::OnOff => "on_off",
        StateKind::BatteryLow => "battery_low",
    }
}

/// Bilan d'un nettoyage.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub removed_lines: usize,
    pub kept_lines: usize,
}

pub struct HistoryStore {
    dir: PathBuf,
    retention: Duration,
    max_bytes: u64,
    open: HashMap<u64, File>,
}

impl HistoryStore {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_limits(dir, RETENTION, MAX_BYTES_PER_NODE)
    }

    pub fn with_limits(dir: PathBuf, retention: Duration, max_bytes: u64) -> Self {
        Self {
            dir,
            retention,
            max_bytes,
            open: HashMap::new(),
        }
    }

    fn path(&self, node_id: u64) -> PathBuf {
        self.dir.join(format!("{node_id:016X}.jsonl"))
    }

    /// Appareils ayant un fichier d'historique.
    pub fn nodes(&self) -> Vec<u64> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                u64::from_str_radix(name.strip_suffix(".jsonl")?, 16).ok()
            })
            .collect()
    }

    /// Mesures d'un appareil postérieures à `since`, dans l'ordre du fichier.
    /// Les lignes illisibles (fichier tronqué par une coupure) sont ignorées.
    pub fn load(&self, node_id: u64, since: f64) -> Vec<Record> {
        let Ok(file) = File::open(self.path(node_id)) else {
            return Vec::new();
        };
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Record>(&line).ok())
            .filter(|r| r.t >= since)
            .collect()
    }

    pub fn append(&mut self, node_id: u64, record: &Record) -> std::io::Result<()> {
        if !self.open.contains_key(&node_id) {
            std::fs::create_dir_all(&self.dir)?;
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let file = options.open(self.path(node_id))?;
            self.open.insert(node_id, file);
        }
        let file = self.open.get_mut(&node_id).expect("fichier ouvert");
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        file.write_all(line.as_bytes())
    }

    /// Efface l'historique d'un appareil.
    pub fn remove(&mut self, node_id: u64) -> std::io::Result<()> {
        self.open.remove(&node_id);
        match std::fs::remove_file(self.path(node_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Applique la rétention par âge puis la taille maximale, à chaque fichier.
    pub fn prune(&mut self, now: f64) -> PruneReport {
        let mut report = PruneReport::default();
        let since = now - self.retention.as_secs_f64();
        for node_id in self.nodes() {
            self.open.remove(&node_id); // fermé avant réécriture
            let path = self.path(node_id);
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let lines: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
            let total = lines.len();
            let mut kept: Vec<&String> = lines
                .iter()
                .filter(|l| serde_json::from_str::<Record>(l).is_ok_and(|r| r.t >= since))
                .collect();
            // Taille maximale : on garde les lignes les plus récentes qui tiennent.
            let mut size: u64 = kept.iter().map(|l| l.len() as u64 + 1).sum();
            let mut drop_front = 0;
            while size > self.max_bytes && drop_front < kept.len() {
                size -= kept[drop_front].len() as u64 + 1;
                drop_front += 1;
            }
            kept.drain(..drop_front);
            report.kept_lines += kept.len();
            report.removed_lines += total - kept.len();
            if kept.len() == total {
                continue;
            }
            if let Err(e) = rewrite(&path, &kept) {
                log::error!("nettoyage de {} impossible : {e}", path.display());
            }
        }
        report
    }
}

fn rewrite(path: &Path, lines: &[&String]) -> std::io::Result<()> {
    if lines.is_empty() {
        return std::fs::remove_file(path);
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(&tmp)?;
    for l in lines {
        out.write_all(l.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.sync_all()?;
    std::fs::rename(tmp, path)
}

enum Msg {
    Append(u64, Record),
    Remove(u64),
}

/// Côté émetteur de l'historique, utilisable depuis n'importe quelle tâche.
#[derive(Clone)]
pub struct HistorySink(mpsc::Sender<Msg>);

impl HistorySink {
    /// Enregistre l'événement s'il s'agit d'une mesure ou d'un état.
    pub fn record(&self, event: &Event) {
        if let Some((node_id, record)) = Record::from_event(event) {
            let _ = self.0.send(Msg::Append(node_id, record));
        }
    }

    pub fn remove(&self, node_id: u64) {
        let _ = self.0.send(Msg::Remove(node_id));
    }
}

/// Lance le thread d'écriture ; le premier nettoyage a déjà été fait par l'appelant.
pub fn start(mut store: HistoryStore) -> HistorySink {
    let (tx, rx) = mpsc::channel::<Msg>();
    std::thread::Builder::new()
        .name("history-writer".into())
        .spawn(move || {
            let mut last_prune = Instant::now();
            loop {
                match rx.recv_timeout(PRUNE_EVERY) {
                    Ok(Msg::Append(node_id, record)) => {
                        if let Err(e) = store.append(node_id, &record) {
                            log::error!("historique {node_id:016X} : écriture impossible : {e}");
                        }
                    }
                    Ok(Msg::Remove(node_id)) => {
                        if let Err(e) = store.remove(node_id) {
                            log::error!("historique {node_id:016X} : effacement impossible : {e}");
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if last_prune.elapsed() >= PRUNE_EVERY {
                    let report = store.prune(crate::events::now_unix());
                    log::info!("historique nettoyé : {report:?}");
                    last_prune = Instant::now();
                }
            }
        })
        .expect("thread historique");
    HistorySink(tx)
}

pub fn dir() -> PathBuf {
    crate::persistence::data_dir().join("history")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "matter-home-desktop-hist-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn rec(t: f64, k: &str, v: f64) -> Record {
        Record { t, k: k.into(), v }
    }

    #[test]
    fn event_round_trip() {
        let e = Event::State {
            node_id: 7,
            kind: StateKind::Open,
            value: true,
            at: 10.0,
        };
        let (id, r) = Record::from_event(&e).unwrap();
        assert_eq!((id, r.clone()), (7, rec(10.0, "open", 1.0)));
        assert!(matches!(
            r.to_event(7),
            Some(Event::State {
                kind: StateKind::Open,
                value: true,
                ..
            })
        ));
        let e = Event::Measurement {
            node_id: 7,
            kind: MeasureKind::Power,
            value: 7.1,
            at: 11.0,
        };
        assert_eq!(Record::from_event(&e).unwrap().1, rec(11.0, "power", 7.1));
        assert!(Record::from_event(&Event::Info("x".into())).is_none());
        assert!(rec(0.0, "inconnu", 1.0).to_event(7).is_none());
    }

    #[test]
    fn append_load_remove() {
        let dir = temp_dir("alr");
        let mut store = HistoryStore::new(dir.clone());
        store.append(1, &rec(100.0, "temperature", 21.5)).unwrap();
        store.append(1, &rec(200.0, "temperature", 22.0)).unwrap();
        store.append(2, &rec(150.0, "power", 3.0)).unwrap();
        let mut nodes = store.nodes();
        nodes.sort();
        assert_eq!(nodes, vec![1, 2]);
        assert_eq!(store.load(1, 0.0).len(), 2);
        assert_eq!(store.load(1, 150.0), vec![rec(200.0, "temperature", 22.0)]);
        store.remove(1).unwrap();
        store.remove(1).unwrap(); // déjà absent : pas d'erreur
        assert!(store.load(1, 0.0).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupted_lines_are_skipped() {
        let dir = temp_dir("corrupt");
        let mut store = HistoryStore::new(dir.clone());
        store.append(3, &rec(1.0, "temperature", 20.0)).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(format!("{:016X}.jsonl", 3)))
            .unwrap()
            .write_all(b"{\"t\":2.0,\"k\":\"temp")
            .unwrap();
        assert_eq!(store.load(3, 0.0).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prune_by_age_and_size() {
        let dir = temp_dir("prune");
        // Rétention 100 s, 200 octets maximum par appareil.
        let mut store = HistoryStore::with_limits(dir.clone(), Duration::from_secs(100), 200);
        for t in 0..20 {
            store
                .append(1, &rec(1000.0 + t as f64 * 10.0, "temperature", 20.0))
                .unwrap();
        }
        store.append(2, &rec(500.0, "power", 1.0)).unwrap(); // trop vieux : fichier supprimé
        let report = store.prune(1200.0);
        let kept = store.load(1, 0.0);
        // Âge : t ≥ 1100 → 10 lignes ; taille : seules les plus récentes tiennent dans 200 octets.
        assert!(
            kept.len() < 10 && !kept.is_empty(),
            "{} lignes gardées",
            kept.len()
        );
        assert_eq!(kept.last().unwrap().t, 1190.0);
        assert!(kept.iter().all(|r| r.t >= 1100.0));
        assert_eq!(report.kept_lines, kept.len());
        assert_eq!(report.removed_lines, 21 - kept.len());
        assert!(store.nodes() == vec![1]);
        // L'écriture continue après réécriture.
        store.append(1, &rec(1200.0, "temperature", 21.0)).unwrap();
        assert_eq!(store.load(1, 0.0).last().unwrap().t, 1200.0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
