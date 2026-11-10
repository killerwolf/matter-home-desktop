//! Surveillance : vérifie la joignabilité des caméras, une à la fois, sans garder de connexion.
//!
//! - Au démarrage (après [`FIRST_PROBE_DELAY`]) puis toutes les [`MONITOR_EVERY`] : un balayage de
//!   toutes les caméras, dans l'ordre.
//! - À l'ajout ou à la modification d'une caméra : elle passe **devant** le balayage en cours
//!   ([`Queue`]). Les demandes arrivées pendant une vérification sont lues entre deux caméras, pas
//!   seulement à la fin du balayage.
//! - Une caméra **regardée** (un spectateur, voir `Hub::acquire_camera_viewer`) est sautée : on ne
//!   lui ouvre pas une session de plus (3 flux simultanés suffisent à faire échouer une Aqara G100),
//!   et l'état est connu de toute façon. Une caméra en cours de test (« Tester ») est sautée
//!   aussi : même verrou (`Hub::camera_probe_lock`).
//! - Premier échec d'une caméra joignable : revérifiée aussitôt (voir `CameraBook::record_probe`).

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};

use crate::hub::Hub;

use super::probe::{probe, PROBE_TIMEOUT};

/// Intervalle entre deux vérifications de toutes les caméras.
pub const MONITOR_EVERY: Duration = Duration::from_secs(60);
/// Délai avant la première vérification au démarrage du serveur.
const FIRST_PROBE_DELAY: Duration = Duration::from_secs(2);

/// Demande au surveillant de vérifier tout de suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Camera(u64),
    All,
}

/// L'ordre des vérifications : les caméras réveillées d'abord (sans doublon), puis le balayage.
#[derive(Debug, Default)]
pub struct Queue {
    urgent: VecDeque<u64>,
    sweep: VecDeque<u64>,
}

impl Queue {
    /// Une caméra à vérifier en premier. Plusieurs demandes pour la même caméra n'en font qu'une,
    /// et elle sort du balayage (elle ne sera pas vérifiée deux fois).
    pub fn wake_camera(&mut self, id: u64) {
        self.sweep.retain(|x| *x != id);
        if !self.urgent.contains(&id) {
            self.urgent.push_back(id);
        }
    }

    /// Un balayage de `ids` : sans ceux qui attendent déjà.
    pub fn wake_all(&mut self, ids: impl IntoIterator<Item = u64>) {
        for id in ids {
            if !self.urgent.contains(&id) && !self.sweep.contains(&id) {
                self.sweep.push_back(id);
            }
        }
    }

    pub fn pop(&mut self) -> Option<u64> {
        self.urgent.pop_front().or_else(|| self.sweep.pop_front())
    }

    pub fn is_empty(&self) -> bool {
        self.urgent.is_empty() && self.sweep.is_empty()
    }
}

/// Le fil de surveillance. [`CameraMonitor::stop`] l'arrête (une vérification en cours est
/// abandonnée sur-le-champ).
pub struct CameraMonitor {
    stop: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
}

impl CameraMonitor {
    /// Arrête et attend le fil. `true` s'il s'est arrêté sans panique.
    pub fn stop(mut self) -> bool {
        self.stop.notify_one();
        self.thread.take().is_none_or(|t| t.join().is_ok())
    }
}

/// Lance la surveillance : vérifie chaque caméra peu après le démarrage puis toutes les
/// [`MONITOR_EVERY`], et sur demande (ajout, modification). Un seul `DESCRIBE` à la fois.
pub fn start_monitor(hub: Arc<Hub>) -> std::io::Result<CameraMonitor> {
    let (tx, rx) = mpsc::unbounded_channel();
    hub.set_camera_wake(Some(tx));
    let stop = Arc::new(Notify::new());
    let thread = {
        let (hub, stop) = (hub.clone(), stop.clone());
        std::thread::Builder::new()
            .name("cameras".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("surveillance des caméras impossible : {e}");
                        return;
                    }
                };
                runtime.block_on(monitor_loop(
                    &hub,
                    rx,
                    &stop,
                    MONITOR_EVERY,
                    FIRST_PROBE_DELAY,
                ));
                // Pas de `drop` ordinaire : un fil de résolution DNS bloqué ferait attendre l'arrêt
                // du serveur (`spawn_blocking` ne s'interrompt pas).
                runtime.shutdown_background();
                hub.set_camera_wake(None);
            })?
    };
    Ok(CameraMonitor {
        stop,
        thread: Some(thread),
    })
}

fn absorb(queue: &mut Queue, hub: &Hub, wake: Wake) {
    match wake {
        Wake::Camera(id) => queue.wake_camera(id),
        Wake::All => queue.wake_all(hub.camera_ids()),
    }
}

/// La boucle de surveillance (publique à la crate pour les tests, qui règlent les délais).
///
/// Le verrou d'une caméra est tenu pendant sa vérification, donc à travers un `await` : voulu. Cette
/// boucle tourne seule sur son fil (runtime à un seul fil) et tous les autres preneurs de ce verrou
/// l'essaient (`try_lock`) ou l'attendent en rendant la main (`CameraTest`) : aucun blocage possible.
#[allow(clippy::await_holding_lock)]
pub(crate) async fn monitor_loop(
    hub: &Hub,
    mut rx: mpsc::UnboundedReceiver<Wake>,
    stop: &Notify,
    every: Duration,
    first: Duration,
) {
    let mut queue = Queue::default();
    let mut next_full = tokio::time::Instant::now() + first;
    loop {
        tokio::select! {
            () = stop.notified() => return,
            message = rx.recv() => match message {
                Some(wake) => absorb(&mut queue, hub, wake),
                None => return,
            },
            () = tokio::time::sleep_until(next_full) => {
                absorb(&mut queue, hub, Wake::All);
                next_full = tokio::time::Instant::now() + every;
            }
        }
        loop {
            // Les demandes arrivées pendant la vérification précédente passent avant la suite.
            while let Ok(wake) = rx.try_recv() {
                absorb(&mut queue, hub, wake);
            }
            let Some(id) = queue.pop() else { break };
            if hub.camera_viewers(id) > 0 {
                continue;
            }
            let lock = hub.camera_probe_lock(id);
            let Ok(_guard) = lock.try_lock() else {
                continue;
            };
            let Some((kind, target)) = hub.camera_probe_target(id) else {
                continue;
            };
            let outcome = tokio::select! {
                () = stop.notified() => return,
                outcome = probe(&target, PROBE_TIMEOUT) => outcome,
            };
            if hub.record_camera_probe(id, kind, &target, outcome) {
                queue.wake_camera(id);
            }
        }
    }
}
