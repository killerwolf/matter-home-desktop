//! Client de bureau Matter Home : l'interface, branchée sur `matter-home-server`.
//!
//! Le client ne pilote rien lui-même et ne garde aucune donnée : tout vient du serveur (HTTPS
//! et WebSocket, certificat épinglé). Sans serveur, il affiche « déconnecté ». Seuls l'adresse,
//! l'empreinte du certificat et le jeton de session sont conservés (fichier en 0600).
//!
//! - `matter-home-desktop`           : lance l'interface
//! - `matter-home-desktop --plan`    : ouvre directement le plan de la maison
//! - `matter-home-desktop --rules`   : ouvre directement les automatisations
//! - `matter-home-desktop --temperatures` : ouvre directement la page Températures
//! - `matter-home-desktop --device X`: ouvre la fiche du premier appareil dont le nom contient X

mod ui;

use std::sync::Arc;

use eframe::egui;
use matter_home_client::net::{Backend, Store};
use matter_home_client::series::TimeWindow;

const HELP: &str = "\
matter-home-desktop — client Matter Home (interface)

Usage :
  matter-home-desktop             lance l'interface et se connecte au serveur
  matter-home-desktop --plan      ouvre directement le plan de la maison
  matter-home-desktop --rules     ouvre directement les automatisations
  matter-home-desktop --temperatures  ouvre directement la page Températures
  matter-home-desktop --period P  avec --temperatures : période affichée (1h, 24h ou 1w)
  matter-home-desktop --edit-plan ouvre le plan en mode Modifier
  matter-home-desktop --cameras   ouvre directement la mosaïque des caméras
  matter-home-desktop --camera X  ouvre en grand la première caméra dont le nom contient X
  matter-home-desktop --add      ouvre directement la fenêtre « Ajouter un appareil »
  matter-home-desktop --device X  ouvre la fiche du premier appareil dont le nom contient X
  matter-home-desktop --version   affiche la version
  matter-home-desktop --help      affiche cette aide

Les appareils simulés (--demo) et le diagnostic mDNS (--discover) sont des options du
serveur : matter-home-server --demo.

Variables : RUST_LOG=debug pour des journaux détaillés ; MATTER_HOME_THEME=light ou dark
impose le thème (essais), sinon celui du système.";

fn main() -> eframe::Result {
    // Journal filtré : jamais d'en-tête `Authorization`, même avec RUST_LOG=trace.
    matter_home_client::logging::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("matter-home-desktop {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // `--device` attend une valeur : on la met de côté avant de vérifier les options.
    let mut device_filter = None;
    let mut camera_filter: Option<String> = None;
    let mut period: Option<TimeWindow> = None;
    let mut flags = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--period" {
            match it.next().map(String::as_str) {
                Some("1h") => period = Some(TimeWindow::Hour1),
                Some("24h") => period = Some(TimeWindow::Hour24),
                Some("1w") => period = Some(TimeWindow::All),
                _ => {
                    eprintln!("--period attend 1h, 24h ou 1w\n\n{HELP}");
                    std::process::exit(2);
                }
            }
        } else if a == "--camera" {
            match it.next() {
                Some(v) => camera_filter = Some(v.clone()),
                None => {
                    eprintln!("--camera attend un nom de caméra\n\n{HELP}");
                    std::process::exit(2);
                }
            }
        } else if a == "--device" {
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
    if let Some(unknown) = args.iter().find(|a| {
        !matches!(
            a.as_str(),
            "--plan" | "--rules" | "--temperatures" | "--add" | "--edit-plan" | "--cameras"
        )
    }) {
        eprintln!("Option inconnue : {unknown}\n\n{HELP}");
        std::process::exit(2);
    }
    let open_add = args.iter().any(|a| a == "--add");
    let open_edit_plan = args.iter().any(|a| a == "--edit-plan");
    let open_cameras = args.iter().any(|a| a == "--cameras");
    let open_plan = args.iter().any(|a| a == "--plan");
    let open_rules = args.iter().any(|a| a == "--rules");
    let open_temperatures = args.iter().any(|a| a == "--temperatures");

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
            ui::apply_theme(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let notify: matter_home_client::net::Notifier = Arc::new(move || ctx.request_repaint());
            let store = Store::default_location()
                .ok_or("dossier de données de l'utilisateur introuvable")?;
            let (backend, events) = Backend::start(store, notify);
            let mut app = ui::MatterApp::new(backend, events);
            // `--device` d'abord : les options d'onglet qui suivent gardent la main sur l'onglet,
            // l'appareil s'ouvre alors dans le panneau de cet onglet (plan ou appareils).
            if let Some(filter) = device_filter {
                app.select_device_named(filter);
            }
            if open_add {
                app.show_add();
            }
            if open_plan {
                app.show_plan();
            }
            if open_cameras {
                app.show_cameras();
            }
            if let Some(filter) = camera_filter {
                app.open_camera_named(filter);
            }
            if open_edit_plan {
                app.show_plan_editing();
            }
            if open_rules {
                app.show_rules();
            }
            if open_temperatures {
                app.show_temperatures(period);
            }
            Ok(Box::new(app))
        }),
    )
}
