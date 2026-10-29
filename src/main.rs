//! Contrôleur Matter over Thread de bureau — preuve de concept.
//!
//! - `matter-demo`            : mode normal (réseau réel)
//! - `matter-demo --demo`     : données simulées, sans réseau
//! - `matter-demo --discover` : diagnostic en ligne de commande (mDNS)

mod automation;
mod demo;
mod events;
mod history;
mod history_store;
mod home;
mod matter;
mod persistence;
mod ui;

use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;

const HELP: &str = "\
matter-demo — contrôleur Matter over Thread (preuve de concept)

Usage :
  matter-demo             lance l'application (réseau réel)
  matter-demo --demo      lance l'application avec des appareils simulés
  matter-demo --discover  liste les nœuds Matter vus en mDNS puis quitte
  matter-demo --plan      ouvre directement le plan de la maison (combinable avec --demo)
  matter-demo --rules     ouvre directement les automatisations (combinable avec --demo)
  matter-demo --help      affiche cette aide

Variables : RUST_LOG=debug pour des journaux détaillés.";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,matc=warn"))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if let Some(unknown) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--demo" | "--discover" | "--plan" | "--rules"))
    {
        eprintln!("Option inconnue : {unknown}\n\n{HELP}");
        std::process::exit(2);
    }
    if args.iter().any(|a| a == "--discover") {
        discover();
        return Ok(());
    }
    let demo = args.iter().any(|a| a == "--demo");
    let open_plan = args.iter().any(|a| a == "--plan");
    let open_rules = args.iter().any(|a| a == "--rules");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if demo { "Matter Demo (démo)" } else { "Matter Demo" })
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Matter Demo",
        options,
        Box::new(move |cc| {
            let (tx, rx) = mpsc::channel();
            let sink = events::EventSink::new(tx, cc.egui_ctx.clone());
            let commands = if demo { demo::spawn(sink) } else { matter::spawn(sink) };
            let mut app = ui::MatterApp::new(commands, rx, demo);
            if open_plan {
                app.show_plan();
            }
            if open_rules {
                app.show_rules();
            }
            Ok(Box::new(app))
        }),
    )
}

/// Diagnostic : vérifie que la pile mDNS de matc voit les nœuds Matter du réseau.
fn discover() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");
    rt.block_on(async {
        let discovery = match matter::discovery::Discovery::start().await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Démarrage mDNS impossible : {e:#}");
                std::process::exit(1);
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
    });
}
