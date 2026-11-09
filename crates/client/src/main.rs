//! Client de bureau Matter Home : l'interface, branchée sur `matter-home-server`.
//!
//! Le client ne pilote rien lui-même et ne garde aucune donnée : tout vient du serveur (HTTPS
//! et WebSocket, certificat épinglé). Sans serveur, il affiche « déconnecté ». Seuls l'adresse,
//! l'empreinte du certificat et le jeton de session sont conservés (fichier en 0600).
//!
//! - `matter-home-desktop`           : lance l'interface
//! - `matter-home-desktop --plan`    : ouvre directement le plan de la maison
//! - `matter-home-desktop --rules`   : ouvre directement les automatisations
//! - `matter-home-desktop --device X`: ouvre la fiche du premier appareil dont le nom contient X

mod ui;

use std::sync::Arc;

use eframe::egui;
use matter_home_client::net::{Backend, Store};

const HELP: &str = "\
matter-home-desktop — client Matter Home (interface)

Usage :
  matter-home-desktop             lance l'interface et se connecte au serveur
  matter-home-desktop --plan      ouvre directement le plan de la maison
  matter-home-desktop --rules     ouvre directement les automatisations
  matter-home-desktop --device X  ouvre la fiche du premier appareil dont le nom contient X
  matter-home-desktop --help      affiche cette aide

Les appareils simulés (--demo) et le diagnostic mDNS (--discover) sont des options du
serveur : matter-home-server --demo.

Variables : RUST_LOG=debug pour des journaux détaillés.";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    // `--device` attend une valeur : on la met de côté avant de vérifier les options.
    let mut device_filter = None;
    let mut flags = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--device" {
            match it.next() {
                Some(v) => device_filter = Some(v.clone()),
                None => {
                    eprintln!("--device attend un nom d'appareil\n\n{HELP}");
                    std::process::exit(2);
                }
            }
        } else {
            flags.push(a.clone());
        }
    }
    let args = flags;
    if let Some(unknown) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--plan" | "--rules"))
    {
        eprintln!("Option inconnue : {unknown}\n\n{HELP}");
        std::process::exit(2);
    }
    let open_plan = args.iter().any(|a| a == "--plan");
    let open_rules = args.iter().any(|a| a == "--rules");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Matter Home Desktop")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Matter Home Desktop",
        options,
        Box::new(move |cc| {
            // Le réseau tourne dans ses propres fils : chaque événement réveille l'interface.
            let ctx = cc.egui_ctx.clone();
            let notify: matter_home_client::net::Notifier = Arc::new(move || ctx.request_repaint());
            let store = Store::default_location()
                .ok_or("dossier de données de l'utilisateur introuvable")?;
            let (backend, events) = Backend::start(store, notify);
            let mut app = ui::MatterApp::new(backend, events);
            if open_plan {
                app.show_plan();
            }
            if open_rules {
                app.show_rules();
            }
            if let Some(filter) = device_filter {
                app.select_device_named(filter);
            }
            Ok(Box::new(app))
        }),
    )
}
