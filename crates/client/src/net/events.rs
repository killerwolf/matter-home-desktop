//! Événements de détection (ticket n° 18) : la liste (`GET /api/events`), les captures, et le clip.
//!
//! Comme les images fixes ([`super::snapshot`]) : des fils réseau, jamais celui de l'interface. Le
//! clip est lu dans le panneau (`crate::clip`, ticket n° 22) ; « Ouvrir dans QuickTime » le
//! télécharge dans un dossier temporaire privé (0600) puis le fait ouvrir par le lecteur du Mac
//! (`open`, QuickTime Player par défaut).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use protocol::wire::{is_event_id, DetectionEventView, DetectionStatusView, EventsResponse};

use super::error::NetError;
use super::http::{self, Request};
use super::snapshot::decode_jpeg;
use super::stream::current;
use super::tls::Trust;
use ostal_video::Image;

/// Événements lus à chaque rafraîchissement, et captures chargées.
pub const LIST_LIMIT: usize = 50;
pub const THUMBS: usize = 24;
const TIMEOUT: Duration = Duration::from_secs(20);

/// Un événement et sa capture (si elle est arrivée).
#[derive(Debug, Clone)]
pub struct Item {
    pub event: DetectionEventView,
    pub thumb: Option<Arc<Image>>,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub items: Vec<Item>,
    pub status: Option<DetectionStatusView>,
    /// Dernier échec, ou message du clip (« ouvert dans QuickTime Player »).
    pub notice: Option<String>,
    pub loaded: bool,
    /// Le serveur ne connaît pas la route : plus ancien que ce client.
    pub server_too_old: bool,
}

/// La liste, partagée entre les fils réseau et l'interface.
#[derive(Clone)]
pub struct EventsFeed {
    shared: Arc<Mutex<State>>,
    busy: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

fn get(path: &str) -> Result<Vec<u8>, NetError> {
    let Some((_, creds)) = current() else {
        return Err(NetError::Unreachable("pas de connexion au serveur".into()));
    };
    let request = Request::get(path.to_string()).with_timeout(TIMEOUT);
    let trust = Trust::Pinned(creds.fingerprint.clone());
    let response = http::exec(&creds.addr, &trust, Some(&creds.token), &request)?;
    response.into_result().map(|r| r.body)
}

impl EventsFeed {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(Mutex::new(State::default())),
            busy: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(notify),
        }
    }

    pub fn state(&self) -> State {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Oublie tout (déconnexion).
    pub fn clear(&self) {
        *self.lock() = State::default();
    }

    /// Relit la liste puis les captures, dans un fil (un seul à la fois).
    pub fn refresh(&self) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let feed = self.clone();
        let spawned = std::thread::Builder::new()
            .name("evenements".into())
            .spawn(move || {
                feed.load();
                feed.busy.store(false, Ordering::SeqCst);
                (feed.notify)();
            });
        if spawned.is_err() {
            self.busy.store(false, Ordering::SeqCst);
        }
    }

    fn load(&self) {
        let list = get(&format!(
            "{}?limit={LIST_LIMIT}",
            protocol::wire::EVENTS_PATH
        ))
        .and_then(|b| {
            serde_json::from_slice::<EventsResponse>(&b)
                .map_err(|_| NetError::Unreachable("réponse illisible".into()))
        });
        let response = match list {
            Ok(r) => r,
            Err(NetError::Api {
                status: 404,
                detail: None,
                ..
            }) => {
                let mut st = self.lock();
                st.server_too_old = true;
                st.loaded = true;
                return;
            }
            Err(e) => {
                let mut st = self.lock();
                st.notice = Some(format!("Événements indisponibles : {}", e.message()));
                st.loaded = true;
                return;
            }
        };
        {
            let mut st = self.lock();
            let old: Vec<Item> = std::mem::take(&mut st.items);
            st.items = response
                .events
                .into_iter()
                .map(|event| {
                    let thumb = old
                        .iter()
                        // La capture d'un passage en cours est remplacée par la finale, et par
                        // celle de chaque mise à jour du passage (sa fin avance) : elle n'est
                        // gardée que si rien de tout cela n'a changé (la description, elle,
                        // n'y change rien).
                        .find(|i| {
                            i.event.id == event.id
                                && i.event.in_progress == event.in_progress
                                && i.event.end == event.end
                        })
                        .and_then(|i| i.thumb.clone());
                    Item { event, thumb }
                })
                .collect();
            st.status = Some(response.detection);
            st.loaded = true;
            st.server_too_old = false;
        }
        (self.notify)();
        let wanted: Vec<String> = self
            .lock()
            .items
            .iter()
            .take(THUMBS)
            .filter(|i| i.thumb.is_none())
            .map(|i| i.event.id.clone())
            .collect();
        for id in wanted {
            if !is_event_id(&id) {
                continue;
            }
            let Ok(bytes) = get(&format!("/api/events/{id}/snapshot")) else {
                continue;
            };
            let Some(image) = decode_jpeg(&bytes) else {
                continue;
            };
            let mut st = self.lock();
            if let Some(item) = st.items.iter_mut().find(|i| i.event.id == id) {
                item.thumb = Some(Arc::new(image));
            }
            drop(st);
            (self.notify)();
        }
    }

    /// Télécharge le clip et l'ouvre avec le lecteur du Mac.
    pub fn open_clip(&self, id: &str) {
        if !is_event_id(id) {
            return;
        }
        let (feed, id) = (self.clone(), id.to_string());
        let _ = std::thread::Builder::new()
            .name("clip".into())
            .spawn(move || {
                let outcome = download_clip(&id).and_then(|path| {
                    std::process::Command::new("/usr/bin/open")
                        .arg(&path)
                        .status()
                        .map_err(|e| format!("ouverture impossible ({})", e.kind()))
                        .and_then(|s| {
                            s.success()
                                .then_some(())
                                .ok_or_else(|| "aucun lecteur n'a ouvert le clip".to_string())
                        })
                });
                feed.lock().notice = Some(match outcome {
                    Ok(()) => "Clip ouvert dans le lecteur du Mac.".into(),
                    Err(why) => format!("Clip indisponible : {why}"),
                });
                (feed.notify)();
            });
    }
}

/// Les clips téléchargés de plus d'un jour : effacés au lancement du client.
pub const CLIPS_KEPT: Duration = Duration::from_secs(24 * 3600);

/// Efface les clips téléchargés depuis plus de [`CLIPS_KEPT`].
pub fn purge_old_clips() {
    let Ok(entries) = std::fs::read_dir(clips_dir()) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > CLIPS_KEPT);
        let mp4 = entry.path().extension().is_some_and(|x| x == "mp4");
        if old && mp4 {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Le dossier des clips téléchargés : privé (0700), dans le dossier temporaire du compte.
pub fn clips_dir() -> PathBuf {
    std::env::temp_dir().join("ostal-clips")
}

/// Télécharge le clip en mémoire (lecteur intégré, ticket n° 22).
pub fn fetch_clip(id: &str) -> Result<Vec<u8>, String> {
    if !is_event_id(id) {
        return Err("Clip indisponible : identifiant invalide.".into());
    }
    get(&format!("/api/events/{id}/clip"))
        .map_err(|e| format!("Clip indisponible : {}.", e.message()))
}

fn download_clip(id: &str) -> Result<PathBuf, String> {
    let bytes = get(&format!("/api/events/{id}/clip")).map_err(|e| e.message())?;
    let dir = clips_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("dossier temporaire ({})", e.kind()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(format!("{id}.mp4"));
    super::files::write_file(&path, &bytes, true)
        .map_err(|e| format!("écriture du clip ({})", e.kind()))?;
    Ok(path)
}
