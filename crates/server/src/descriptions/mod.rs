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

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

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

/// Le compte du jour (heure locale).
struct Day {
    date: chrono::NaiveDate,
    count: u32,
    warned: bool,
}

pub struct Descriptions {
    /// Le dossier de données (`None` en démonstration : rien n'est lu ni écrit).
    data_dir: Option<PathBuf>,
    stored: Mutex<Stored>,
    shared: Arc<Shared>,
    /// Événements dont les règles attendent la description.
    awaiting: Mutex<HashSet<String>>,
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
            awaiting: Mutex::new(HashSet::new()),
            worker_started: Mutex::new(false),
            endpoint: Mutex::new(Endpoint::default()),
            day: Mutex::new(Day {
                date: chrono::Local::now().date_naive(),
                count: 0,
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
        lock(&self.awaiting).insert(event.id.clone());
        true
    }

    /// Les règles de cet événement attendent-elles encore sa description ?
    pub(crate) fn is_awaiting(&self, id: &str) -> bool {
        lock(&self.awaiting).contains(id)
    }

    /// Après la publication de l'ouverture : la description de la première capture, si le réglage
    /// « dès l'ouverture » est actif (sinon, une seule description, à la fin du passage).
    pub fn describe_opening(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) {
        if self.settings().on_open && lock(&self.awaiting).contains(&event.id) {
            self.enqueue(hub, event, jpeg);
        }
    }

    /// Le passage est fini (avec sa meilleure capture) : sa description.
    pub fn on_close(&self, hub: &Arc<Hub>, event: &DetectionEventView, jpeg: &[u8]) {
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
    fn resolve(&self, hub: &Hub, id: &str) {
        if lock(&self.awaiting).remove(id) {
            if let Some(event) = hub.detection().store().get(id) {
                hub.detection_described(&event);
            }
        }
    }

    fn ensure_worker(&self, hub: &Arc<Hub>) {
        let mut started = lock(&self.worker_started);
        if *started {
            return;
        }
        let (shared, hub) = (self.shared.clone(), Arc::downgrade(hub));
        match std::thread::Builder::new()
            .name("descriptions".into())
            .spawn(move || worker(&shared, &hub))
        {
            Ok(_) => *started = true,
            Err(e) => log::error!("fil des descriptions impossible : {e}"),
        }
    }

    /// Arrêt du serveur : le fil finit l'appel en cours (au plus ~1 minute) et s'arrête ; les
    /// tâches en attente sont abandonnées.
    pub fn shutdown(&self) {
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
