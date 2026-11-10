//! Démonstration du lecteur de caméra (ticket n° 5) : une vraie liaison avec un serveur (par
//! exemple `matter-home-server`'s `--example camera_demo`), le vrai lecteur `ui/player.rs`, et une
//! capture d'écran de ce qu'il affiche une fois la vidéo décodée.
//!
//! ```text
//! MH_DEMO_PASSWORD=… cargo run -p matter-home-desktop --example camera_player -- \
//!     --store FICHIER --url https://127.0.0.1:PORT --fingerprint AA:BB:… \
//!     [--camera NOM] [--kind main|sub] [--shot IMAGE.ppm] [--after-frames N]
//! ```
//!
//! Le fichier de connexion (`--store`) est celui qu'on veut (jetable). Avec `--shot`, la capture
//! est faite après N images décodées (30 par défaut), écrite en PPM (P6), et l'exemple se ferme.

#[path = "../src/ui/player.rs"]
mod player;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use eframe::egui;
use matter_home_client::model::Model;
use matter_home_client::net::{Backend, Event, Link, Store};
use protocol::wire::StreamKind;

struct App {
    _backend: Backend,
    events: Receiver<Event>,
    model: Model,
    link: Link,
    camera: Option<String>,
    kind: StreamKind,
    shot: Option<std::path::PathBuf>,
    after_frames: u64,
    requested: bool,
    done: Arc<AtomicBool>,
    last_log: std::time::Instant,
}

impl App {
    fn pump(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Link(link) => {
                    println!("liaison : {link:?}");
                    self.link = link;
                }
                Event::Snapshot(snapshot) => {
                    let _ = self.model.apply_snapshot(*snapshot);
                }
                Event::Live(event) => {
                    let _ = self.model.apply_event(*event);
                }
                Event::LoginFailed(e) => println!("connexion refusée : {}", e.message()),
                _ => {}
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let id = self
            .model
            .cameras
            .iter()
            .find(|c| self.camera.as_deref().is_none_or(|n| c.name == n))
            .map(|c| c.id);
        egui::CentralPanel::default().show(ui, |ui| match id {
            Some(id) => {
                player::player(ui, &self.model, id, self.kind);
            }
            None => {
                ui.label(format!(
                    "En attente de la liste des caméras ({:?})…",
                    self.link
                ));
            }
        });
        if self.last_log.elapsed().as_secs() >= 2 {
            self.last_log = std::time::Instant::now();
            println!("caméras connues : {}", self.model.cameras.len());
        }
        if self.done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if let Some(path) = &self.shot {
            let frames = id.map_or(0, |id| player::frames_decoded(&ctx, id));
            if !self.requested && frames >= self.after_frames {
                self.requested = true;
                let (path, done, repaint) = (path.clone(), self.done.clone(), ctx.clone());
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |image| {
                        write_ppm(&path, &image);
                        println!("capture écrite : {}", path.display());
                        done.store(true, Ordering::SeqCst);
                        repaint.request_repaint();
                    }),
                ));
            }
        }
    }
}

fn write_ppm(path: &std::path::Path, image: &egui::ColorImage) {
    let mut out = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
    for pixel in &image.pixels {
        out.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
    }
    std::fs::write(path, out).expect("écriture de la capture");
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let store = arg(&args, "--store").expect("--store FICHIER");
    let url = arg(&args, "--url").expect("--url https://127.0.0.1:PORT");
    let fingerprint = arg(&args, "--fingerprint").expect("--fingerprint AA:BB:…");
    let password = std::env::var("MH_DEMO_PASSWORD").expect("MH_DEMO_PASSWORD");
    let kind = match arg(&args, "--kind").as_deref() {
        Some("main") => StreamKind::Main,
        _ => StreamKind::Sub,
    };
    let camera = arg(&args, "--camera");
    let shot = arg(&args, "--shot").map(std::path::PathBuf::from);
    let after_frames = arg(&args, "--after-frames")
        .and_then(|n| n.parse().ok())
        .unwrap_or(30);
    println!("fichier de connexion : {store}");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lecteur de caméra")
            .with_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Lecteur de caméra",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let notify: matter_home_client::net::Notifier = Arc::new(move || ctx.request_repaint());
            let (backend, events) = Backend::start(Store::at(store.into()), notify);
            backend.login(&url, &fingerprint, "demo", &password);
            Ok(Box::new(App {
                _backend: backend,
                events,
                model: Model::default(),
                link: Link::Connecting,
                camera,
                kind,
                shot,
                after_frames,
                requested: false,
                done: Arc::new(AtomicBool::new(false)),
                last_log: std::time::Instant::now(),
            }))
        }),
    )
}
