//! Description des événements de détection par l'API Anthropic (ticket n° 19) : une phrase en
//! français et un niveau de menace (0 normal, 1 suspect, 2 critique), comme la revue GenAI de
//! Frigate. Voir `docs/descriptions.md`.
//!
//! # La chaîne
//!
//! ```text
//! fil d'écriture des événements (detection/watcher.rs) ── capture JPEG ≤ 1024 px ──▶ file bornée
//!      (ouverture : si « dès l'ouverture » ; fin : toujours)        (8 tâches, la plus ancienne
//!                                                                     part si elle est pleine)
//!                    fil « descriptions » : un appel à la fois (api.rs, curl, 30 s, 1 reprise)
//!                                   │
//!                                   ▼
//!     fiche de l'événement (store.rs) ──▶ diffusée aux clients (même `id`) ──▶ règles en attente
//! ```
//!
//! - **Rien ne bloque l'analyse** : ni le fil d'analyse des caméras, ni le fil d'écriture des
//!   événements ne font d'appel réseau ; tout passe par la file et le fil des descriptions.
//! - **Rien ne part sans choix** : désactivé par défaut, sans clé rien n'est envoyé, et seules les
//!   caméras et classes choisies sont décrites (voir [`settings`]).
//! - **Plafond quotidien** : au-delà, l'événement reste sans description, avec une ligne au
//!   journal par jour. Le compte repart de zéro au redémarrage du serveur.
//! - **Règles** : une règle dont le texte emploie `{description}` ou `{niveau}` attend la
//!   description (ou son absence : refus, plafond, erreur) pour partir (`awaiting`).

pub mod api;
pub mod settings;

#[cfg(test)]
pub(crate) mod mock;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use protocol::wire::DetectionEventView;

use crate::hub::Hub;
use api::{Context, Endpoint, Outcome};
use settings::{Settings, Stored};

/// Tâches en attente au plus ; au-delà, la plus ancienne part (avec une ligne au journal).
pub const QUEUE_MAX: usize = 8;

struct Job {
    event: DetectionEventView,
    jpeg: Vec<u8>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// Le compte du jour (heure locale), gardé dans `serveur/descriptions-usage.json` (0600) : un
/// redémarrage ne remet pas le plafond à zéro.
struct Day {
    date: chrono::NaiveDate,
    count: u32,
    warned: bool,
}

const USAGE_FILE: &str = "descriptions-usage.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Usage {
    date: String,
    count: u32,
}

/// Une règle attend la description d'un événement. Elle n'attend pas indéfiniment : [`Limits`].
struct Waiting {
    /// Dernière fiche connue (si l'événement est supprimé ou nettoyé avant la description, les
    /// règles partent quand même avec elle).
    event: DetectionEventView,
    opened: Instant,
    closed: Option<Instant>,
}

/// Combien de temps une règle attend sa description avant de partir sans elle.
#[derive(Clone, Copy)]
struct Limits {
    /// Après la fin du passage.
    after_close: Duration,
    /// Depuis l'ouverture, au plus (un passage dure 2 minutes au plus).
    after_open: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            after_close: Duration::from_secs(45),
            after_open: Duration::from_secs(180),
        }
    }
}

/// Les corps de requête laissés dans le dossier temporaire par un arrêt brutal (descriptions,
/// et notifications web push avec leur propre préfixe).
pub(crate) fn clean_leftover_bodies(prefix: &str) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(prefix) {
            continue;
        }
        // Moins de 10 minutes : peut-être celui d'un appel en cours (autre processus).
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(600));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub struct Descriptions {
    /// Le dossier de données (`None` en démonstration : rien n'est lu ni écrit).
    data_dir: Option<PathBuf>,
    stored: Mutex<Stored>,
    shared: Arc<Shared>,
    /// Événements dont les règles attendent la description.
    awaiting: Mutex<HashMap<String, Waiting>>,
    limits: Mutex<Limits>,
    worker_started: Mutex<bool>,
    endpoint: Mutex<Endpoint>,
    day: Mutex<Day>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Descriptions {
    /// Lit `serveur/descriptions.json`. En démonstration, rien n'est lu ni écrit, et rien ne part.
    pub fn new(data_dir: &Path, demo: bool) -> Descriptions {
        let data_dir = (!demo).then(|| data_dir.to_path_buf());
        if !demo {
            clean_leftover_bodies(api::BODY_PREFIX);
        }
        let usage = data_dir.as_ref().and_then(|d| {
            let path = d.join(crate::persistence::SERVER_DIR).join(USAGE_FILE);
            crate::persistence::load_json::<Usage>(&path).ok().flatten()
        });
        let today = chrono::Local::now().date_naive();
        let count = usage
            .filter(|u| u.date == today.to_string())
            .map_or(0, |u| u.count);
        let file = data_dir
            .as_ref()
            .map(|d| d.join(crate::persistence::SERVER_DIR).join(settings::FILE));
        let stored = match file.as_deref().map(crate::persistence::load_json::<Stored>) {
            Some(Ok(Some(stored))) => Stored {
                settings: stored.settings.sanitized(),
                ..stored
            },
            Some(Err(e)) => {
                // Le message d'erreur ne cite que le chemin et la position du défaut.
                log::warn!("descriptions : réglages illisibles, désactivées : {e:#}");
                Stored::default()
            }
            _ => Stored::default(),
        };
        Descriptions {
            data_dir,
            stored: Mutex::new(stored),
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
            }),
            awaiting: Mutex::new(HashMap::new()),
            limits: Mutex::new(Limits::default()),
            worker_started: Mutex::new(false),
            endpoint: Mutex::new(Endpoint::default()),
            day: Mutex::new(Day {
                date: today,
                count,
                warned: false,
            }),
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.stored).settings.clone()
    }

    /// Une clé est enregistrée (jamais sa valeur).
    pub fn has_key(&self) -> bool {
        lock(&self.stored).api_key.is_some()
    }

    fn save(&self, stored: &Stored) -> Result<()> {
        if let Some(dir) = &self.data_dir {
            let server = crate::security::server_dir(dir)?;
            crate::persistence::save_json(&server.join(settings::FILE), stored)?;
        }
        Ok(())
    }

    /// Enregistre de nouveaux réglages (fenêtre du serveur).
    pub fn update(&self, new: Settings) -> Result<()> {
        let mut stored = lock(&self.stored);
        let next = Stored {
            settings: new.sanitized(),
            api_key: stored.api_key.clone(),
        };
        self.save(&next)?;
        *stored = next;
        Ok(())
    }

    /// Enregistre (`Some`) ou retire (`None`) la clé d'API. Elle n'est jamais relue par l'interface.
    pub fn set_api_key(&self, key: Option<&str>) -> Result<()> {
        if let Some(key) = key.map(str::trim) {
            if !settings::valid_key(key) {
                bail!("clé refusée : 8 à 512 caractères imprimables, sans espace ni guillemet");
            }
        }
        let mut stored = lock(&self.stored);
        let next = Stored {
            settings: stored.settings.clone(),
            api_key: key.map(|k| k.trim().to_string()),
        };
        self.save(&next)?;
        *stored = next;
        Ok(())
    }

    /// Les tests seuls adressent un faux serveur local.
    #[cfg(test)]
    pub(crate) fn set_endpoint(&self, endpoint: Endpoint) {
        *lock(&self.endpoint) = endpoint;
    }

    /// L'événement est-il à décrire maintenant (réglages, clé, capture présente) ?
    fn eligible(&self, hub: &Hub, event: &DetectionEventView, jpeg: &[u8]) -> bool {
        let stored = lock(&self.stored);
        !jpeg.is_empty()
            && stored.api_key.is_some()
            && stored.settings.wants(
                event.camera_id,
                hub.camera_placement(event.camera_id),
                &event.classes,
            )
    }

    /// Un passage s'ouvre (avec sa première capture). Rend `true` si une description va arriver
    /// (ici ou à la fin du passage) : les règles qui l'emploient attendent alors
    /// (`Command::Detection { awaiting }`).
    pub fn on_open(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) -> bool {
        if !self.eligible(hub, event, jpeg) {
            return false;
        }
        self.wait_for(hub, event);
        true
    }

    fn wait_for(&self, hub: &Arc<Hub>, event: &DetectionEventView) {
        self.ensure_worker(hub);
        lock(&self.awaiting)
            .entry(event.id.clone())
            .or_insert_with(|| Waiting {
                event: event.clone(),
                opened: Instant::now(),
                closed: None,
            });
    }

    /// Une classe nouvelle en cours de passage (`event` porte toutes les classes) : les règles qui
    /// emploient la description attendent-elles ? Oui si l'événement est déjà attendu, ou si cette
    /// classe le rend éligible (il ne l'était pas à l'ouverture).
    pub(crate) fn on_update(
        &self,
        hub: &Arc<Hub>,
        event: &DetectionEventView,
        jpeg: &[u8],
    ) -> bool {
        if let Some(w) = lock(&self.awaiting).get_mut(&event.id) {
            w.event = event.clone();
            return true;
        }
        if self.eligible(hub, event, jpeg) {
            self.wait_for(hub, event);
            return true;
        }
        false
    }

    /// Après la publication de l'ouverture : la description de la première capture, si le réglage
    /// « dès l'ouverture » est actif (sinon, une seule description, à la fin du passage).
    pub fn describe_opening(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) {
        if self.settings().on_open && lock(&self.awaiting).contains_key(&event.id) {
            self.enqueue(hub, event, jpeg);
        }
    }

    /// Le passage est fini (avec sa meilleure capture) : sa description.
    pub fn on_close(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) {
        if let Some(w) = lock(&self.awaiting).get_mut(&event.id) {
            w.event = event.clone();
            w.closed = Some(Instant::now());
        }
        if self.eligible(hub, event, jpeg) {
            self.enqueue(hub, event, jpeg);
        } else {
            // Réglages changés en cours de passage : les règles en attente partent quand même.
            self.resolve(hub, &event.id);
        }
    }

    fn enqueue(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) {
        self.ensure_worker(hub);
        let dropped = {
            let mut queue = lock(&self.shared.queue);
            if queue.closed {
                return;
            }
            let dropped = if queue.jobs.len() >= QUEUE_MAX {
                queue.jobs.pop_front()
            } else {
                None
            };
            queue.jobs.push_back(Job {
                event: event.clone(),
                jpeg: jpeg.to_vec(),
            });
            dropped
        };
        self.shared.wake.notify_one();
        if let Some(old) = dropped {
            log::warn!(
                "descriptions : file pleine, l'événement {} reste sans description",
                old.event.id
            );
            self.resolve(hub, &old.event.id);
        }
    }

    /// Les règles qui attendaient la description de cet événement partent (avec ou sans elle).
    pub(crate) fn resolve(&self, hub: &Hub, id: &str) {
        let waiting = lock(&self.awaiting).remove(id);
        if let Some(waiting) = waiting {
            let event = hub.detection().store().get(id).unwrap_or_else(|| {
                log::info!("descriptions : l'événement {id} a disparu avant sa description");
                waiting.event
            });
            hub.detection_described(&event);
        }
    }

    /// Les règles dont l'attente est dépassée partent sans description.
    fn expire(&self, hub: &Hub) {
        let limits = *lock(&self.limits);
        let now = Instant::now();
        let late: Vec<String> = lock(&self.awaiting)
            .iter()
            .filter(|(_, w)| {
                now.duration_since(w.opened) >= limits.after_open
                    || w.closed
                        .is_some_and(|c| now.duration_since(c) >= limits.after_close)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in late {
            log::warn!(
                "descriptions : pas de description de l'événement {id} à temps, la règle part sans"
            );
            self.resolve(hub, &id);
        }
    }

    /// Une description de cet événement est-elle attendue ? (Notifications web push : le passage
    /// est notifié à sa description plutôt qu'à sa fin.)
    pub(crate) fn is_awaiting(&self, id: &str) -> bool {
        lock(&self.awaiting).contains_key(id)
    }

    #[cfg(test)]
    pub(crate) fn is_awaiting_for_test(&self, id: &str) -> bool {
        lock(&self.awaiting).contains_key(id)
    }

    #[cfg(test)]
    pub(crate) fn set_limits(&self, after_close: Duration, after_open: Duration) {
        *lock(&self.limits) = Limits {
            after_close,
            after_open,
        };
    }

    fn ensure_worker(&self, hub: &Arc<Hub>) {
        let mut started = lock(&self.worker_started);
        if *started {
            return;
        }
        let (shared, weak) = (self.shared.clone(), Arc::downgrade(hub));
        match std::thread::Builder::new()
            .name("descriptions".into())
            .spawn(move || worker(&shared, &weak))
        {
            Ok(_) => *started = true,
            Err(e) => log::error!("fil des descriptions impossible : {e}"),
        }
        // Le fil des échéances : un appel lent à l'API ne retient pas les règles plus longtemps.
        let (shared, weak) = (self.shared.clone(), Arc::downgrade(hub));
        let reaper = std::thread::Builder::new()
            .name("descriptions-delais".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                if lock(&shared.queue).closed {
                    return;
                }
                let Some(hub) = weak.upgrade() else { return };
                hub.detection().descriptions().expire(&hub);
            });
        if let Err(e) = reaper {
            log::error!("fil des délais des descriptions impossible : {e}");
        }
    }

    /// Arrêt du serveur : le fil finit l'appel en cours (au plus ~1 minute) et s'arrête ; les
    /// tâches en attente sont abandonnées.
    ///
    /// Les règles qui attendaient une description partent sans elle.
    pub fn shutdown(&self, hub: &Hub) {
        let pending = {
            let mut queue = lock(&self.shared.queue);
            queue.closed = true;
            let n = queue.jobs.len();
            queue.jobs.clear();
            n
        };
        if pending > 0 {
            log::info!("descriptions : {pending} tâche(s) abandonnée(s) à l'arrêt");
        }
        self.shared.wake.notify_all();
        let ids: Vec<String> = lock(&self.awaiting).keys().cloned().collect();
        for id in ids {
            self.resolve(hub, &id);
        }
    }

    /// Prend une place du plafond quotidien. `false` : plafond atteint (une ligne par jour).
    fn take_slot(&self, cap: u32) -> bool {
        let mut day = lock(&self.day);
        let today = chrono::Local::now().date_naive();
        if day.date != today {
            *day = Day {
                date: today,
                count: 0,
                warned: false,
            };
        }
        if day.count >= cap {
            if !day.warned {
                day.warned = true;
                log::warn!(
                    "descriptions : plafond de {cap} descriptions par jour atteint, les \
                     événements suivants d'aujourd'hui restent sans description"
                );
            }
            return false;
        }
        day.count += 1;
        if let Some(dir) = &self.data_dir {
            let usage = Usage {
                date: today.to_string(),
                count: day.count,
            };
            let saved = crate::security::server_dir(dir)
                .and_then(|d| crate::persistence::save_json(&d.join(USAGE_FILE), &usage));
            if let Err(e) = saved {
                log::warn!("descriptions : compte du jour non enregistré : {e:#}");
            }
        }
        true
    }

    /// Décrit un événement (fil des descriptions) : appelle l'API, enregistre, diffuse.
    fn process(&self, hub: &Hub, job: &Job) {
        let id = &job.event.id;
        let (settings, key) = {
            let stored = lock(&self.stored);
            (stored.settings.clone(), stored.api_key.clone())
        };
        let Some(key) = key.filter(|_| settings.enabled) else {
            return self.resolve(hub, id);
        };
        if !self.take_slot(settings.daily_cap) {
            return self.resolve(hub, id);
        }
        let context = Context {
            camera_name: job.event.camera_name.clone(),
            classes: job.event.classes.iter().map(|c| c.lower()).collect(),
        };
        let endpoint = lock(&self.endpoint).clone();
        let store = hub.detection().store();
        let saved = match api::describe(&endpoint, &key, settings.model, &context, &job.jpeg) {
            Ok(Outcome::Described {
                description,
                niveau,
            }) => {
                log::info!("description de l'événement {id} : niveau {niveau}");
                store.set_description(id, description, Some(niveau))
            }
            Ok(Outcome::Refused) => {
                log::info!("description de l'événement {id} : refusée par le modèle");
                store.set_description(id, api::UNAVAILABLE.into(), None)
            }
            Err(why) => {
                log::warn!("description de l'événement {id} impossible : {why}");
                None
            }
        };
        if let Some(event) = saved {
            hub.publish_detection(&event, None, false);
        }
        self.resolve(hub, id);
    }
}

/// Le fil des descriptions : une tâche à la fois, jusqu'à l'arrêt ou la disparition du hub.
fn worker(shared: &Shared, hub: &Weak<Hub>) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(hub) = hub.upgrade() else { return };
        hub.detection().descriptions().process(&hub, &job);
    }
}
