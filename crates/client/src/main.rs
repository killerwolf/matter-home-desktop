//! Client de bureau Matter Home : l'interface seule.
//!
//! Étape 1 du découpage client-serveur : le client n'a plus de lien Matter ni de données
//! locales. Il se lance sans serveur et affiche « déconnecté » ; le branchement sur le
//! serveur (`matter-home-server`) est l'étape 4.
//!
//! - `matter-home-desktop`           : lance l'interface
//! - `matter-home-desktop --plan`    : ouvre directement le plan de la maison
//! - `matter-home-desktop --rules`   : ouvre directement les automatisations
//! - `matter-home-desktop --device X`: ouvre la fiche du premier appareil dont le nom contient X

mod history;
mod ui;

use std::sync::mpsc;

use eframe::egui;

const HELP: &str = "\
matter-home-desktop — client Matter Home (interface)

Usage :
  matter-home-desktop             lance l'interface (déconnectée tant que le serveur n'est pas branché)
  matter-home-desktop --plan      ouvre directement le plan de la maison
  matter-home-desktop --rules     ouvre directement les automatisations
  matter-home-desktop --device X  ouvre la fiche du premier appareil dont le nom contient X
  matter-home-desktop --help      affiche cette aide

Les appareils simulés (--demo) et le diagnostic mDNS (--discover) sont maintenant
des options du serveur : matter-home-server --demo.

Variables : RUST_LOG=debug pour des journaux détaillés.";

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();

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
        Box::new(move |_cc| {
            // TODO étape 4 : ces canaux seront alimentés par la connexion au serveur.
            // Pour l'instant personne ne lit les commandes ni n'écrit les événements :
            // l'interface reste « déconnectée ».
            let (commands, _unconnected_commands) = mpsc::channel();
            let (_unconnected_events, events) = mpsc::channel();
            let mut app = ui::MatterApp::new(commands, events);
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
