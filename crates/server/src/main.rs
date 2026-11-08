//! Serveur Matter Home, sans fenêtre.
//!
//! - `matter-home-server`            : mode normal (réseau réel)
//! - `matter-home-server --demo`     : données simulées, sans réseau ni écriture sur disque
//! - `matter-home-server --discover` : diagnostic en ligne de commande (mDNS)
//!
//! Le serveur quitte avec un code non nul sur une erreur fatale, pour qu'un superviseur
//! (launchd, systemd) le relance. Ctrl-C ou SIGTERM l'arrête proprement (code 0).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use matter_home_server::events::{Event, EventSink};
use matter_home_server::{backup, demo, matter, persistence};

const HELP: &str = "\
matter-home-server — serveur Matter Home (sans fenêtre)

Usage :
  matter-home-server             lance le serveur (réseau réel)
  matter-home-server --demo      lance le serveur avec des appareils simulés
  matter-home-server --discover  liste les nœuds Matter vus en mDNS puis quitte
  matter-home-server --help      affiche cette aide

Variables : RUST_LOG=debug pour des journaux détaillés.";

/// Délai laissé au backend et à l'écriture de l'historique pour s'arrêter.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Intervalle entre deux lignes de bilan des événements reçus.
const SUMMARY_EVERY: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,matc=warn"))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if let Some(unknown) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--demo" | "--discover"))
    {
        eprintln!("Option inconnue : {unknown}\n\n{HELP}");
        return ExitCode::from(2);
    }
    let demo = args.iter().any(|a| a == "--demo");
    let discover_only = args.iter().any(|a| a == "--discover");

    // Une panique dans une sous-tâche ou dans le thread d'historique ne doit pas laisser un
    // serveur à moitié vivant : on journalise puis on quitte, le superviseur relancera.
    install_panic_hook();

    // Le mode démo ne touche à aucun fichier (ni verrou, ni migration de dossier, ni import
    // en attente). Le verrou est pris avant tout le reste : l'import renomme le dossier.
    let _instance_lock = if !demo && !discover_only {
        match persistence::acquire_instance_lock(&persistence::instance_lock_path()) {
            Ok(lock) => Some(lock),
            Err(e) => {
                log::error!("{e:#}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    if !demo && !discover_only {
        // Ancien nom « matter-demo » : son dossier de données est déplacé au premier lancement.
        let mut notes: Vec<String> = Vec::new();
        notes.extend(persistence::migrate_legacy_data_dir());
        // Import demandé avant l'arrêt précédent : à appliquer avant tout chargement.
        notes.extend(backup::apply_pending(&persistence::data_dir()));
        for m in &notes {
            log::warn!("{m}");
        }
    }
    if discover_only {
        return discover();
    }

    let (tx, rx) = mpsc::channel();
    let sink = EventSink::new(tx);
    // Garder le backend en vie : fermer ses commandes demande l'arrêt de la tâche de fond.
    let backend = if demo {
        log::info!("Mode démo : appareils simulés, rien n'est écrit sur disque.");
        demo::spawn(sink)
    } else {
        matter::spawn(sink)
    };

    let stop = Arc::new(AtomicBool::new(false));
    watch_signals(stop.clone());
    let code = serve(&rx, &stop);
    // Arrêt ordonné, sur signal comme sur erreur fatale : le backend s'arrête, l'écriture de
    // l'historique vide sa file, puis on journalise ce qui restait dans le canal.
    let clean = backend.shutdown(SHUTDOWN_TIMEOUT);
    for event in rx.try_iter() {
        log_event(&event);
    }
    if clean {
        code
    } else {
        log::error!("arrêt incomplet : certains threads n'ont pas fini à temps.");
        ExitCode::FAILURE
    }
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        log::error!("panique : {info}");
        std::process::exit(1);
    }));
}

/// Boucle principale : journalise les événements jusqu'à une erreur fatale ou un arrêt demandé.
fn serve(events: &mpsc::Receiver<Event>, stop: &AtomicBool) -> ExitCode {
    let mut total = 0u64;
    let mut measures = 0u64;
    let mut last_summary = Instant::now();
    loop {
        match events.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::Fatal(message)) => {
                log::error!("Erreur fatale : {message}");
                return ExitCode::FAILURE;
            }
            Ok(event) => {
                total += 1;
                if matches!(event, Event::Measurement { .. }) {
                    measures += 1;
                }
                log_event(&event);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::error!("La tâche de fond s'est arrêtée sans signaler d'erreur.");
                return ExitCode::FAILURE;
            }
        }
        if stop.load(Ordering::Relaxed) {
            log::info!("Arrêt demandé ({total} événements reçus). Au revoir.");
            return ExitCode::SUCCESS;
        }
        if last_summary.elapsed() >= SUMMARY_EVERY {
            log::info!("{total} événements reçus ({measures} mesures).");
            last_summary = Instant::now();
        }
    }
}

/// Une ligne de journal par événement ; les mesures et états (très nombreux au démarrage,
/// quand l'historique est rejoué) restent au niveau debug.
fn log_event(event: &Event) {
    match event {
        Event::Node(info) => log::info!("appareil {:#x} : {}", info.node_id, info.name),
        Event::NodeRemoved(id) => log::info!("appareil {id:#x} oublié"),
        Event::Status { node_id, status } => log::info!("appareil {node_id:#x} : {status:?}"),
        Event::RuleRun { .. } => log::info!("{event:?}"),
        Event::PairingProgress(m) | Event::Info(m) => log::info!("{m}"),
        Event::PairingDone { node_id, name } => log::info!("appairage : {name} ({node_id:#x})"),
        Event::PairingFailed(m) => log::warn!("appairage échoué : {m}"),
        Event::Fatal(m) => log::error!("{m}"),
        _ => log::debug!("{event:?}"),
    }
}

/// Positionne `stop` à la réception de Ctrl-C ou de SIGTERM.
fn watch_signals(stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("signaux".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::warn!("signaux non surveillés : {e}");
                    return;
                }
            };
            rt.block_on(async {
                use tokio::signal::unix::{signal, SignalKind};
                let mut term = match signal(SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("SIGTERM non surveillé : {e}");
                        return;
                    }
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                stop.store(true, Ordering::Relaxed);
            });
        })
        .expect("thread signaux");
}

/// Diagnostic : vérifie que la pile mDNS de matc voit les nœuds Matter du réseau.
fn discover() -> ExitCode {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");
    rt.block_on(async {
        let discovery = match matter::discovery::Discovery::start().await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Démarrage mDNS impossible : {e:#}");
                return ExitCode::FAILURE;
            }
        };
        println!("Recherche des nœuds Matter (_matter._tcp) pendant 10 s…");
        for _ in 0..5 {
            discovery.browse_operational().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let nodes = discovery.operational_snapshot().await;
        println!("{} nœud(s) opérationnel(s) vu(s) :", nodes.len());
        for (instance, found) in nodes {
            println!("  {instance}  {}", found.addresses.join(", "));
        }
        ExitCode::SUCCESS
    })
}
