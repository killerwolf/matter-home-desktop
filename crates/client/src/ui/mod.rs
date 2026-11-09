//! Interface egui. Ne fait jamais d'entrée/sortie réseau : elle envoie des demandes à
//! `net::Backend` (qui rend la main aussitôt) et lit les `net::Event` à chaque image.
//!
//! Elle n'affiche que ce que le serveur lui a envoyé (état complet, puis événements) : pas de
//! cache, pas de mode local. Déconnectée, elle affiche « déconnecté » et n'autorise aucune
//! commande.

mod connect;
mod customize;
mod plan;
mod rules;
mod sensors;
mod sync;
mod thermometer;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

pub use matter_home_client::model::Device;
use matter_home_client::model::{Change, Model};
use matter_home_client::net::error::api_message;
use matter_home_client::net::{Backend, Body, Event, Link, NetError, Request, Response};
use matter_home_client::series::{metrics_for, needs_fetch, DeviceSeries, TimeWindow};
use protocol::automation::CoverCommand;
use protocol::events::now_unix;
use protocol::home::{HomePlan, Symbol};
use protocol::wire::{
    CoverRequest, CustomizeRequest, HistoryResponse, ImportResponse, ImportSummary, PairRequest,
    Snapshot, TextIds, WireEvent, WireStatus, HISTORY_MAX_LIMIT,
};

/// Ce que les vues d'un appareil demandent à l'application (elles ne parlent pas au réseau).
#[derive(Default)]
pub struct ViewRequests {
    /// Relire l'historique après un échec.
    pub retry: bool,
    /// Exporter la température en CSV (fichier produit par le serveur).
    pub export_csv: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Devices,
    Plan,
    Rules,
}

enum Pairing {
    Idle,
    InProgress(String),
    Success(String),
    Failed(String),
}

/// À quoi sert une requête en vol : la réponse est aiguillée d'après son numéro.
enum Pending {
    History {
        node_id: u64,
    },
    /// Commande dont seul l'échec compte (son effet arrive en événements).
    Command {
        what: &'static str,
    },
    Pair,
    ClearHistory(u64),
    PlanSave,
    RulesSave,
    Csv {
        path: PathBuf,
    },
    Export {
        path: PathBuf,
    },
    ImportCheck {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    ImportApply,
}

/// Fenêtre ouverte par le menu Fichier → Exporter / Importer.
enum DataDialog {
    /// Téléchargement ou vérification en cours.
    Busy(String),
    Message {
        ok: bool,
        text: String,
    },
    /// Sauvegarde choisie et validée par le serveur (`dry_run`), en attente de confirmation.
    ConfirmImport {
        path: PathBuf,
        summary: String,
        bytes: Vec<u8>,
    },
    /// Import accepté : le serveur redémarre pour l'appliquer.
    ImportStarted,
}

/// Notification d'une règle, affichée tant que le client est ouvert.
struct Toast {
    title: String,
    message: String,
    until: Instant,
}

const TOAST_LIFETIME: Duration = Duration::from_secs(20);

pub struct MatterApp {
    backend: Backend,
    events: mpsc::Receiver<Event>,
    link: Link,
    connect: connect::ConnectScreen,
    model: Model,
    /// Historique lu à la demande, par appareil ; vidé à la déconnexion.
    series: HashMap<u64, DeviceSeries>,
    pending: HashMap<u64, Pending>,
    selected: Option<u64>,
    code: String,
    pairing: Pairing,
    window: TimeWindow,
    export_message: Option<(bool, String)>,
    /// Dernière erreur d'une commande, affichée sous la barre du haut.
    error_banner: Option<(Instant, String)>,
    log: VecDeque<String>,
    toasts: Vec<Toast>,
    view: View,
    plan: plan::PlanView,
    rules: rules::RulesView,
    /// Appareil dont l'effacement d'historique attend confirmation.
    confirm_clear: Option<u64>,
    /// Nom et icône en cours de modification sur la fiche.
    device_edit: Option<customize::DeviceEdit>,
    /// Option `--device` : appareil à sélectionner quand il apparaîtra.
    select_on_arrival: Option<String>,
    data_dialog: Option<DataDialog>,
}

impl MatterApp {
    pub fn new(backend: Backend, events: mpsc::Receiver<Event>) -> Self {
        Self {
            backend,
            events,
            link: Link::Connecting,
            connect: connect::ConnectScreen::new(),
            model: Model::default(),
            series: HashMap::new(),
            pending: HashMap::new(),
            selected: None,
            code: String::new(),
            pairing: Pairing::Idle,
            window: TimeWindow::Min15,
            export_message: None,
            error_banner: None,
            log: VecDeque::new(),
            toasts: Vec::new(),
            view: View::Devices,
            plan: plan::PlanView::new(),
            rules: rules::RulesView::new(),
            confirm_clear: None,
            device_edit: None,
            select_on_arrival: None,
            data_dialog: None,
        }
    }

    /// Sélectionne, dès qu'il est connu, le premier appareil dont le nom contient `filter`.
    pub fn select_device_named(&mut self, filter: String) {
        self.select_on_arrival = Some(filter);
        self.view = View::Devices;
    }

    /// Ouvre l'onglet « Automatisations ».
    pub fn show_rules(&mut self) {
        self.view = View::Rules;
    }

    /// Ouvre l'onglet « Plan de la maison ».
    pub fn show_plan(&mut self) {
        self.view = View::Plan;
    }

    /// Les commandes ne sont possibles que liés au serveur.
    fn connected(&self) -> bool {
        matches!(self.link, Link::Connected { .. } | Link::Resyncing)
    }

    fn push_log(&mut self, line: String) {
        let time = chrono::Local::now().format("%H:%M:%S");
        self.log.push_back(format!("{time}  {line}"));
        while self.log.len() > 50 {
            self.log.pop_front();
        }
    }

    fn show_error(&mut self, text: String) {
        self.push_log(text.clone());
        self.error_banner = Some((Instant::now(), text));
    }

    // -----------------------------------------------------------------------
    // Requêtes
    // -----------------------------------------------------------------------

    /// Envoie une requête au serveur, sauf si l'on est déconnecté : aucune commande n'est
    /// acceptée sur des données qui ne sont plus à jour.
    fn request(&mut self, pending: Pending, request: Request) -> Option<u64> {
        if !self.connected() {
            self.push_log("Déconnecté : commande non envoyée.".into());
            return None;
        }
        let id = self.backend.call(request);
        self.pending.insert(id, pending);
        Some(id)
    }

    fn command(&mut self, what: &'static str, request: Request) {
        self.request(Pending::Command { what }, request);
    }

    fn pair(&mut self, code: String) {
        let body = PairRequest { code };
        if self
            .request(Pending::Pair, Request::json("POST", "/api/pairing", &body))
            .is_some()
        {
            self.pairing = Pairing::InProgress("Démarrage…".into());
        }
    }

    fn retry_device(&mut self, id: u64) {
        self.command(
            "Reconnexion de l'appareil",
            Request::new("POST", format!("/api/devices/{id}/retry")),
        );
    }

    fn forget_device(&mut self, id: u64) {
        self.command(
            "Oubli de l'appareil",
            Request::new("DELETE", format!("/api/devices/{id}")),
        );
    }

    fn clear_history(&mut self, id: u64) {
        self.request(
            Pending::ClearHistory(id),
            Request::new("DELETE", format!("/api/devices/{id}/history")),
        );
    }

    fn customize(&mut self, id: u64, name: Option<String>, icon: Option<Symbol>) {
        let body = CustomizeRequest { name, icon };
        self.command(
            "Personnalisation de l'appareil",
            Request::json("PUT", format!("/api/devices/{id}/customization"), &body),
        );
    }

    fn set_cover(&mut self, id: u64, command: CoverCommand) {
        self.command(
            "Commande du volet",
            Request::json(
                "POST",
                format!("/api/devices/{id}/cover"),
                &CoverRequest { command },
            ),
        );
    }

    fn test_rule(&mut self, id: u64) {
        self.command(
            "Test de la règle",
            Request::new("POST", format!("/api/rules/{id}/test")),
        );
    }

    /// Envoie le plan et les règles modifiés, au rythme décidé par `SyncState` (un envoi à la
    /// fois, modifications regroupées).
    fn save_documents(&mut self, now: Instant) {
        if let Some(plan) = self.plan.poll_save(now) {
            let body = TextIds(plan.clone());
            if let Some(id) =
                self.request(Pending::PlanSave, Request::json("PUT", "/api/plan", &body))
            {
                self.plan.sent(id, plan);
            }
        }
        if let Some(rules) = self.rules.poll_save(now) {
            let body = TextIds(rules.clone());
            if let Some(id) = self.request(
                Pending::RulesSave,
                Request::json("PUT", "/api/rules", &body),
            ) {
                self.rules.sent(id, rules);
            }
        }
    }

    /// Lit l'historique de l'appareil affiché quand la fiche s'ouvre ou que la fenêtre change.
    fn maintain_series(&mut self) {
        if !self.connected() || self.view != View::Devices {
            return;
        }
        let Some(id) = self.selected else { return };
        let Some(device) = self.model.devices.get(&id) else {
            return;
        };
        let metrics = metrics_for(&device.info.caps);
        if metrics.is_empty() {
            return;
        }
        let now = now_unix();
        let entry = self.series.entry(id).or_default();
        if entry.loading.is_some()
            || entry.error.is_some()
            || !needs_fetch(entry.fetched_since, self.window, now)
        {
            return;
        }
        let since = self.window.fetch_start(now);
        let list: Vec<&str> = metrics.iter().map(|m| m.key()).collect();
        let path = format!(
            "/api/devices/{id}/history?since={since:.0}&until={now:.0}&metrics={}&limit={HISTORY_MAX_LIMIT}",
            list.join(",")
        );
        if let Some(req_id) = self.request(Pending::History { node_id: id }, Request::get(path)) {
            if let Some(entry) = self.series.get_mut(&id) {
                entry.loading = Some(req_id);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Ce que le réseau remonte
    // -----------------------------------------------------------------------

    fn pump_network(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Link(link) => self.on_link(link),
                Event::Snapshot(snapshot) => self.on_snapshot(*snapshot),
                Event::Live(event) => self.on_live(*event),
                Event::Reply { id, result } => self.on_reply(id, result),
                Event::Probed { url, fingerprint } => self.connect.probed(url, fingerprint),
                Event::ProbeFailed(e) | Event::LoginFailed(e) => {
                    self.connect.failed(e.message());
                }
                Event::Notice(text) => self.push_log(text),
            }
        }
    }

    /// Oublie tout ce qui venait du serveur.
    fn clear_data(&mut self) {
        self.model.clear();
        self.series.clear();
        self.pending.clear();
        self.plan.reset();
        self.rules.reset();
        self.pairing = Pairing::Idle;
        self.confirm_clear = None;
        self.device_edit = None;
        self.export_message = None;
    }

    fn on_link(&mut self, link: Link) {
        match &link {
            // Resynchronisation : on garde l'affichage, un nouvel état complet arrive aussitôt.
            Link::Resyncing => {}
            Link::Connected { server_version } => {
                self.push_log(format!("Connecté au serveur (version {server_version})."));
                self.connect.connected();
            }
            Link::Reconnecting {
                reason, retry_in_s, ..
            } => {
                self.push_log(format!(
                    "Liaison perdue : {reason} Nouvel essai dans {retry_in_s} s."
                ));
                self.clear_data();
            }
            Link::LoggedOut {
                url,
                pinned,
                reason,
            } => {
                self.clear_data();
                self.connect
                    .logged_out(url.clone(), *pinned, reason.clone());
            }
            Link::Connecting | Link::CertificateChanged { .. } | Link::Incompatible { .. } => {
                self.clear_data();
            }
        }
        self.link = link;
    }

    fn on_snapshot(&mut self, snapshot: Snapshot) {
        let busy = snapshot.pairing_busy;
        let (plan, rules) = self.model.apply_snapshot(snapshot);
        self.plan.on_server(plan);
        self.rules.on_server(rules);
        if busy && matches!(self.pairing, Pairing::Idle) {
            self.pairing = Pairing::InProgress("Appairage en cours…".into());
        }
        // Les séries lues avant ne tiennent plus compte de ce qui a pu se passer : relecture.
        for s in self.series.values_mut() {
            s.fetched_since = None;
        }
        if !self
            .selected
            .is_some_and(|id| self.model.devices.contains_key(&id))
        {
            self.selected = self.model.devices.keys().next().copied();
        }
        self.try_select_on_arrival();
    }

    fn try_select_on_arrival(&mut self) {
        let Some(wanted) = self.select_on_arrival.as_deref().map(str::to_lowercase) else {
            return;
        };
        if let Some(id) = self
            .model
            .devices
            .iter()
            .find(|(_, d)| d.info.name.to_lowercase().contains(&wanted))
            .map(|(id, _)| *id)
        {
            self.selected = Some(id);
            self.select_on_arrival = None;
        }
    }

    fn on_live(&mut self, event: WireEvent) {
        match self.model.apply_event(event) {
            Change::Nothing => {}
            Change::DeviceUpdated(id) => {
                if self.selected.is_none() {
                    self.selected = Some(id);
                }
                self.try_select_on_arrival();
            }
            Change::DeviceRemoved(id) => {
                self.series.remove(&id);
                if self.selected == Some(id) {
                    self.selected = self.model.devices.keys().next().copied();
                }
            }
            Change::Reading {
                node_id,
                metric,
                value,
                at,
            } => {
                if let Some(series) = self.series.get_mut(&node_id) {
                    series.push_live(metric, matter_home_client::series::Sample { t: at, value });
                }
            }
            Change::Journal(line) => self.push_log(line),
            Change::PairingProgress(m) => self.pairing = Pairing::InProgress(m),
            Change::PairingDone { node_id, name } => {
                self.push_log(format!("Appairé : {name}"));
                self.pairing = Pairing::Success(format!(
                    "« {name} » est appairé. Lecture de sa description en cours…"
                ));
                self.selected = Some(node_id);
                self.code.clear();
            }
            Change::PairingFailed { code, detail } => {
                let text = api_message(code, detail.as_deref(), None, 0);
                self.push_log(format!("Échec d'appairage : {text}"));
                self.pairing = Pairing::Failed(text);
            }
            Change::Notification { title, message } => {
                self.push_log(format!("Notification : {title} — {message}"));
                self.toasts.push(Toast {
                    title,
                    message,
                    until: Instant::now() + TOAST_LIFETIME,
                });
                if self.toasts.len() > 5 {
                    self.toasts.remove(0);
                }
            }
            Change::RuleRun(run) => {
                self.push_log(format!(
                    "Règle « {} » {} : {}",
                    run.name,
                    if run.ok { "exécutée" } else { "en échec" },
                    run.detail
                ));
            }
            Change::PlanChanged(plan) => self.plan.on_server(*plan),
            Change::RulesChanged(rules) => self.rules.on_server(*rules),
        }
    }

    fn on_reply(&mut self, id: u64, result: Result<Response, NetError>) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        match pending {
            Pending::History { node_id } => {
                let Some(entry) = self.series.get_mut(&node_id) else {
                    return;
                };
                if entry.loading != Some(id) {
                    return;
                }
                entry.loading = None;
                match result.and_then(|r| r.json::<HistoryResponse>()) {
                    Ok(h) => entry.load(h.since, h.truncated, &h.points),
                    Err(e) => entry.error = Some(e.message()),
                }
            }
            Pending::Command { what } => {
                if let Err(e) = result {
                    self.show_error(format!("{what} : {}", e.message()));
                }
            }
            Pending::Pair => {
                if let Err(e) = result {
                    self.pairing = Pairing::Failed(e.message());
                }
            }
            Pending::ClearHistory(node_id) => match result {
                Ok(_) => {
                    self.series.remove(&node_id);
                    self.push_log("Historique effacé.".into());
                }
                Err(e) => self.show_error(format!("Effacement de l'historique : {}", e.message())),
            },
            Pending::PlanSave => match result {
                Ok(r) => {
                    let stored = r.json::<TextIds<HomePlan>>().ok().map(|p| p.0);
                    self.plan.saved(id, stored);
                }
                Err(e) => {
                    if self.plan.rejected(id) {
                        self.show_error(format!(
                            "Le plan n'a pas été enregistré : {}",
                            e.message()
                        ));
                    }
                }
            },
            Pending::RulesSave => match result {
                Ok(r) => {
                    let stored = r
                        .json::<TextIds<protocol::automation::RuleSet>>()
                        .ok()
                        .map(|p| p.0);
                    self.rules.saved(id, stored);
                }
                Err(e) => {
                    if self.rules.rejected(id) {
                        self.show_error(format!(
                            "Les règles n'ont pas été enregistrées : {}",
                            e.message()
                        ));
                    }
                }
            },
            Pending::Csv { path } => {
                self.export_message = Some(match result {
                    Ok(r) => match write_file(&path, &r.body, false) {
                        Ok(()) => (true, format!("CSV enregistré dans {}", path.display())),
                        Err(e) => (false, format!("Export impossible : {e}")),
                    },
                    Err(e) => (false, format!("Export impossible : {}", e.message())),
                });
            }
            Pending::Export { path } => {
                self.data_dialog = Some(match result {
                    Ok(r) => match write_file(&path, &r.body, true) {
                        Ok(()) => {
                            self.push_log(format!("Export enregistré dans {}", path.display()));
                            DataDialog::Message {
                                ok: true,
                                text: format!(
                                    "Exporté dans {}\n\nCe fichier contient les clés privées de \
                                     votre maison et les heures d'ouverture des portes : \
                                     transférez-le par AirDrop ou clé USB, ne le partagez pas, et \
                                     supprimez-le une fois l'import fait.",
                                    path.display()
                                ),
                            }
                        }
                        Err(e) => DataDialog::Message {
                            ok: false,
                            text: format!("Export impossible : {e}"),
                        },
                    },
                    Err(e) => DataDialog::Message {
                        ok: false,
                        text: format!("Export impossible : {}", e.message()),
                    },
                });
            }
            Pending::ImportCheck { path, bytes } => {
                self.data_dialog = Some(match result.and_then(|r| r.json::<ImportResponse>()) {
                    Ok(r) => DataDialog::ConfirmImport {
                        path,
                        summary: describe_backup(&r.summary),
                        bytes,
                    },
                    Err(e) => DataDialog::Message {
                        ok: false,
                        text: format!("Import impossible : {}", e.message()),
                    },
                });
            }
            Pending::ImportApply => {
                self.data_dialog = Some(match result {
                    Ok(_) => DataDialog::ImportStarted,
                    Err(e) => DataDialog::Message {
                        ok: false,
                        text: format!("Import impossible : {}", e.message()),
                    },
                });
            }
        }
    }

    // -----------------------------------------------------------------------
    // Menu Fichier
    // -----------------------------------------------------------------------

    /// Menu Fichier → Exporter : choisit un fichier, le serveur produit la sauvegarde.
    fn export_data(&mut self) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Exporter mes données")
            .set_file_name(format!(
                "matter-home-{}.matterhome",
                chrono::Local::now().format("%Y%m%d")
            ))
            .add_filter("Sauvegarde Matter Home", &["matterhome"])
            .save_file()
        else {
            return;
        };
        let request = Request::get("/api/export").with_timeout(Duration::from_secs(180));
        if self
            .request(Pending::Export { path: dest }, request)
            .is_some()
        {
            self.data_dialog = Some(DataDialog::Busy("Téléchargement de la sauvegarde…".into()));
        }
    }

    /// Menu Fichier → Importer : choisit une sauvegarde, le serveur la valide, on confirme.
    fn import_data(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Importer des données")
            .add_filter("Sauvegarde Matter Home", &["matterhome"])
            .pick_file()
        else {
            return;
        };
        let bytes = match read_backup(&path) {
            Ok(b) => b,
            Err(e) => {
                self.data_dialog = Some(DataDialog::Message {
                    ok: false,
                    text: format!("Import impossible : {e}"),
                });
                return;
            }
        };
        let request = Request {
            body: Body::Bytes(bytes.clone()),
            timeout: Duration::from_secs(180),
            ..Request::new("POST", "/api/import?dry_run=1")
        };
        if self
            .request(Pending::ImportCheck { path, bytes }, request)
            .is_some()
        {
            self.data_dialog = Some(DataDialog::Busy("Vérification de la sauvegarde…".into()));
        }
    }

    /// Fenêtre d'information ou de confirmation du menu Fichier.
    fn data_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &self.data_dialog else {
            return;
        };
        let mut close = false;
        let mut confirmed = false;
        egui::Window::new("Données")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                match dialog {
                    DataDialog::Busy(text) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(text);
                        });
                        close = ui.button("Fermer").clicked();
                    }
                    DataDialog::Message { ok, text } => {
                        let color = if *ok {
                            Color32::from_rgb(60, 160, 90)
                        } else {
                            Color32::from_rgb(210, 60, 50)
                        };
                        ui.colored_label(color, text);
                        ui.add_space(6.0);
                        close = ui.button("OK").clicked();
                    }
                    DataDialog::ConfirmImport { path, summary, .. } => {
                        ui.label(format!("Importer {} ?", path.display()));
                        ui.label(summary);
                        ui.add_space(4.0);
                        ui.label(
                            "Les données du serveur seront remplacées. Le serveur redémarre pour \
                             appliquer l'import (une copie de secours de ses données est gardée \
                             à côté) ; l'application se reconnecte toute seule.",
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            confirmed = ui.button("Importer").clicked();
                            close = ui.button("Annuler").clicked();
                        });
                    }
                    DataDialog::ImportStarted => {
                        ui.label(
                            "Import accepté. Le serveur redémarre pour l'appliquer : la \
                             connexion va se couper puis revenir d'elle-même.",
                        );
                        ui.add_space(6.0);
                        close = ui.button("OK").clicked();
                    }
                }
            });
        if confirmed {
            if let Some(DataDialog::ConfirmImport { bytes, .. }) = self.data_dialog.take() {
                let request = Request {
                    body: Body::Bytes(bytes),
                    timeout: Duration::from_secs(180),
                    ..Request::new("POST", "/api/import")
                };
                self.data_dialog = if self.request(Pending::ImportApply, request).is_some() {
                    Some(DataDialog::Busy("Envoi de la sauvegarde…".into()))
                } else {
                    None
                };
            }
        } else if close {
            self.data_dialog = None;
        }
    }

    /// Export CSV d'un appareil : le serveur produit le fichier pour la fenêtre affichée.
    fn export_csv(&mut self, id: u64) {
        let Some(device) = self.model.devices.get(&id) else {
            return;
        };
        let safe_name: String = device
            .info
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Exporter en CSV")
            .set_file_name(format!(
                "matter-{}-{}.csv",
                safe_name,
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
            .add_filter("Fichier CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let now = now_unix();
        let path = format!(
            "/api/devices/{id}/history.csv?since={:.0}&until={now:.0}",
            self.window.start(now)
        );
        if self
            .request(
                Pending::Csv { path: dest },
                Request::get(path).with_timeout(Duration::from_secs(120)),
            )
            .is_some()
        {
            self.export_message = Some((true, "Export en cours…".into()));
        }
    }

    // -----------------------------------------------------------------------
    // Dessin
    // -----------------------------------------------------------------------

    fn pairing_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appairer un appareil");
        ui.label("Code de l'app Maison : 11 chiffres, ou code QR « MT:… ».");
        let busy = matches!(self.pairing, Pairing::InProgress(_)) || self.model.pairing_busy;
        let response = ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.code)
                .hint_text("1234-567-8901")
                .desired_width(f32::INFINITY),
        );
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let clicked = ui
            .add_enabled(
                !busy && !self.code.trim().is_empty(),
                egui::Button::new("Appairer"),
            )
            .clicked();
        if (clicked || enter) && !busy && !self.code.trim().is_empty() {
            let code = self.code.trim().to_string();
            self.pair(code);
        }
        match &self.pairing {
            Pairing::Idle => {}
            Pairing::InProgress(m) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(m);
                });
            }
            Pairing::Success(m) => {
                ui.colored_label(Color32::from_rgb(40, 160, 70), format!("✔ {m}"));
            }
            Pairing::Failed(m) => {
                ui.colored_label(Color32::from_rgb(210, 60, 50), format!("✖ {m}"));
            }
        }
    }

    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appareils");
        if self.model.devices.is_empty() {
            ui.label("Aucun appareil appairé pour l'instant.");
            return;
        }
        let now = now_unix();
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (id, d) in &self.model.devices {
                let selected = self.selected == Some(*id);
                let frame = egui::Frame::group(ui.style()).fill(if selected {
                    ui.visuals().selection.bg_fill.linear_multiply(0.35)
                } else {
                    Color32::TRANSPARENT
                });
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (color, text) = status_badge(&d.status);
                            ui.colored_label(color, "●");
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    plan::glyph(plan::icon_of(d)),
                                    d.info.name
                                ))
                                .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.small(text);
                                },
                            );
                        });
                        let kind = if d.info.device_types.is_empty() {
                            "Type inconnu".to_string()
                        } else {
                            d.info.device_types.join(", ")
                        };
                        ui.small(kind);
                        let summary = d.summary();
                        if !summary.is_empty() {
                            ui.small(RichText::new(summary).strong());
                        }
                        let origin = match d.info.bridge {
                            Some(link) => format!(
                                "Via le pont {}",
                                self.model.devices.get(&link.node_id).map_or_else(
                                    || format!("{:016X}", link.node_id),
                                    |b| b.info.name.clone()
                                )
                            ),
                            None => format!("Nœud {id:016X}"),
                        };
                        ui.small(format!(
                            "{origin} · dernière communication : {}",
                            d.last_seen.map_or("jamais".into(), |t| ago(now - t))
                        ));
                    })
                    .response;
                if resp.interact(egui::Sense::click()).clicked() {
                    clicked = Some(*id);
                }
            }
        });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.view = View::Devices;
            self.export_message = None;
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected else {
            ui.centered_and_justified(|ui| {
                ui.label("Appairez un appareil ou sélectionnez-en un dans la liste.");
            });
            return;
        };
        // Appareils exposés par ce nœud, s'il est un pont (calculé avant d'emprunter la fiche).
        let children: Vec<(u64, String, String)> = self
            .model
            .devices
            .values()
            .filter(|d| d.info.bridge.is_some_and(|b| b.node_id == id))
            .map(|d| {
                (
                    d.info.node_id,
                    format!("{} {}", plan::glyph(plan::icon_of(d)), d.info.name),
                    d.summary(),
                )
            })
            .collect();
        let bridge_name = self.selected.and_then(|id| {
            let link = self.model.devices.get(&id)?.info.bridge?;
            Some(
                self.model
                    .devices
                    .get(&link.node_id)
                    .map_or_else(|| format!("{:016X}", link.node_id), |b| b.info.name.clone()),
            )
        });
        let Some(device) = self.model.devices.get_mut(&id) else {
            return;
        };
        let fresh_edit = customize::DeviceEdit::from_info(&device.info);

        let mut retry = false;
        let mut forget = false;
        let mut clear = false;
        let mut customize = false;
        let mut open_device = None;
        let mut cover_command = None;
        let mut requests = ViewRequests::default();
        ui.horizontal(|ui| {
            ui.heading(format!(
                "{} {}",
                plan::glyph(plan::icon_of(device)),
                device.info.name
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if device.info.bridge.is_none() {
                    forget = ui
                        .button("Oublier")
                        .on_hover_text(
                            "Retire l'appareil de cette application (il reste dans Apple Home).",
                        )
                        .clicked();
                }
                retry = ui.button("Reconnecter").clicked();
                if self.confirm_clear == Some(id) {
                    if ui.button("Annuler").clicked() {
                        self.confirm_clear = None;
                    }
                    clear = ui
                        .button(RichText::new("Oui, effacer").color(Color32::from_rgb(210, 60, 50)))
                        .clicked();
                    ui.label("Effacer tout l'historique de cet appareil ?");
                } else if ui
                    .button("Effacer l'historique")
                    .on_hover_text(
                        "Supprime les mesures enregistrées sur le serveur pour cet appareil.",
                    )
                    .clicked()
                {
                    self.confirm_clear = Some(id);
                }
            });
        });
        let editing = self.device_edit.as_ref().is_some_and(|e| e.node_id == id);
        if !editing
            && ui
                .small_button("✏ Personnaliser le nom et l'icône")
                .clicked()
        {
            customize = true;
        }
        if device.info.custom_name.is_some() {
            ui.small(format!("Nom de l'appareil : {}", device.info.device_name));
        }
        if let (Some(link), Some(bridge_name)) = (device.info.bridge, &bridge_name) {
            ui.horizontal(|ui| {
                ui.small(format!(
                    "Exposé par le pont « {bridge_name} » (endpoint {})",
                    link.endpoint
                ));
                if ui.small_button("Voir le pont").clicked() {
                    open_device = Some(link.node_id);
                }
            });
        }
        let mut save = None;
        if let Some(edit) = self.device_edit.as_mut().filter(|e| e.node_id == id) {
            match customize::editor(ui, edit, &device.info.device_name) {
                Some(customize::EditResult::Save) => save = Some(edit.clone()),
                Some(customize::EditResult::Cancel) => self.device_edit = None,
                None => {}
            }
        }
        let meta: Vec<String> = [device.info.vendor.clone(), device.info.product.clone()]
            .into_iter()
            .flatten()
            .collect();
        if !meta.is_empty() {
            ui.label(meta.join(" — "));
        }
        if let Some(b) = device.battery() {
            let low = device.battery_low();
            let color = if low || b < 20.0 {
                Color32::from_rgb(210, 60, 50)
            } else {
                Color32::from_rgb(40, 160, 70)
            };
            let warn = if low {
                " — pile faible, à remplacer"
            } else {
                ""
            };
            ui.colored_label(color, format!("Pile : {b:.0} %{warn}"));
        }
        let (color, text) = status_badge(&device.status);
        ui.horizontal(|ui| {
            ui.colored_label(color, format!("● {text}"));
            if let WireStatus::Unreachable { retry_in_s, .. } = &device.status {
                if *retry_in_s > 0 {
                    ui.label(format!("— nouvel essai automatique sous {retry_in_s} s"));
                }
            }
        });
        if let Some(why) = unreachable_text(&device.status) {
            ui.small(RichText::new(why).color(Color32::GRAY));
        }
        ui.separator();

        let caps = device.info.caps;
        let window = &mut self.window;
        let empty_series = DeviceSeries::default();
        let series = self.series.get(&id).unwrap_or(&empty_series);
        let export_message = &self.export_message;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if caps.temperature {
                    thermometer::show(ui, device, series, window, export_message, &mut requests);
                    ui.separator();
                }
                if caps.contact {
                    sensors::door(ui, device, series, window, &mut requests);
                    ui.separator();
                }
                if caps.power || caps.energy || caps.on_off {
                    sensors::power(ui, device, series, window, &mut requests);
                    ui.separator();
                }
                if caps.cover {
                    cover_command = sensors::cover(ui, device, series, window, &mut requests);
                    ui.separator();
                }
                if !children.is_empty() {
                    ui.heading("Appareils exposés par ce pont");
                    for (child_id, label, summary) in &children {
                        ui.horizontal(|ui| {
                            if ui.link(label).clicked() {
                                open_device = Some(*child_id);
                            }
                            if !summary.is_empty() {
                                ui.small(summary);
                            }
                        });
                    }
                    ui.separator();
                }
            });
        if !caps.any()
            && children.is_empty()
            && (!device.info.device_types.is_empty() || device.last_seen.is_some())
        {
            ui.label("Cet appareil n'expose aucune mesure suivie par cette application.");
            ui.label("Une prise ou une lampe se commande depuis l'onglet « Automatisations ».");
        } else if !caps.any() && children.is_empty() {
            ui.label("Description de l'appareil pas encore lue (il est peut-être endormi).");
        }

        if requests.retry {
            if let Some(s) = self.series.get_mut(&id) {
                s.error = None;
            }
        }
        if requests.export_csv {
            self.export_csv(id);
        }
        if retry {
            self.retry_device(id);
        }
        if let Some(command) = cover_command {
            self.set_cover(id, command);
        }
        if let Some(other) = open_device {
            self.selected = Some(other);
            self.export_message = None;
        }
        if customize {
            self.device_edit = Some(fresh_edit);
        }
        if let Some(edit) = save {
            self.device_edit = None;
            self.customize(id, Some(edit.name), edit.icon);
        }
        if forget {
            self.forget_device(id);
        }
        if clear {
            self.confirm_clear = None;
            self.clear_history(id);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let mut menu_export = false;
        let mut menu_import = false;
        let mut logout = false;
        egui::Panel::top("top").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("Fichier", |ui| {
                    let connected = self.connected();
                    if ui
                        .add_enabled(connected, egui::Button::new("Exporter mes données…"))
                        .on_disabled_hover_text("Indisponible : pas de connexion au serveur")
                        .clicked()
                    {
                        menu_export = true;
                        ui.close();
                    }
                    if ui
                        .add_enabled(connected, egui::Button::new("Importer des données…"))
                        .on_disabled_hover_text("Indisponible : pas de connexion au serveur")
                        .clicked()
                    {
                        menu_import = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Se déconnecter").clicked() {
                        logout = true;
                        ui.close();
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.heading("Matter over Thread — contrôleur");
                match &self.link {
                    Link::Connected { server_version } => {
                        ui.label(
                            RichText::new(format!("● connecté · serveur {server_version}"))
                                .small()
                                .color(Color32::from_rgb(40, 160, 70)),
                        );
                    }
                    Link::Resyncing => {
                        ui.label(RichText::new("● resynchronisation…").small());
                    }
                    _ => {
                        ui.label(
                            RichText::new(" DÉCONNECTÉ ")
                                .strong()
                                .color(Color32::BLACK)
                                .background_color(Color32::from_rgb(250, 200, 60)),
                        )
                        .on_hover_text("Pas de connexion au serveur Matter Home.");
                    }
                }
                ui.separator();
                if self.connected() {
                    ui.selectable_value(&mut self.view, View::Devices, "Appareils");
                    ui.selectable_value(&mut self.view, View::Plan, "Plan de la maison");
                    ui.selectable_value(&mut self.view, View::Rules, "Automatisations");
                }
            });
            if let Some((at, text)) = &self.error_banner {
                if at.elapsed() < Duration::from_secs(20) {
                    ui.colored_label(Color32::from_rgb(210, 60, 50), text);
                }
            }
        });
        if menu_export {
            self.export_data();
        }
        if menu_import {
            self.import_data();
        }
        if logout {
            self.backend.logout();
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        let mut dismissed = None;
        for (i, t) in self.toasts.iter().enumerate() {
            egui::Area::new(egui::Id::unique(("toast", i)))
                .anchor(egui::Align2::RIGHT_TOP, [-12.0, 56.0 + i as f32 * 78.0])
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(320.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("🔔 {}", t.title)).strong());
                            if ui.small_button("✕").clicked() {
                                dismissed = Some(i);
                            }
                        });
                        ui.label(&t.message);
                    });
                });
        }
        if let Some(i) = dismissed {
            self.toasts.remove(i);
        }
    }

    fn disconnected_panel(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("Déconnecté");
            ui.add_space(8.0);
            match &self.link {
                Link::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connexion au serveur…");
                    });
                }
                Link::Reconnecting {
                    reason, retry_in_s, ..
                } => {
                    ui.label(format!("Liaison perdue : {reason}"));
                    ui.label(format!(
                        "Nouvel essai automatique dans {retry_in_s} s au plus."
                    ));
                    if ui.button("Réessayer maintenant").clicked() {
                        self.backend.reconnect();
                    }
                }
                _ => {}
            }
            ui.add_space(8.0);
            ui.small(
                "Aucune donnée n'est affichée ni modifiable tant que le serveur n'est pas joint.",
            );
            if ui.button("Changer de serveur").clicked() {
                self.backend.logout();
            }
        });
    }
}

/// Écrit un fichier reçu du serveur. `private` : en 0600 (la sauvegarde contient des clés).
fn write_file(path: &std::path::Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        if private {
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        if private {
            // Un fichier écrasé garde ses anciens droits : on les resserre.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = private;
        let mut file = std::fs::File::create(path)?;
        file.write_all(bytes)
    }
}

/// Taille maximale d'une sauvegarde acceptée à l'import (le serveur a sa propre limite).
const MAX_BACKUP_BYTES: u64 = 128 * 1024 * 1024;

fn read_backup(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("fichier illisible ({})", e.kind()))?;
    if meta.len() > MAX_BACKUP_BYTES {
        return Err("fichier trop volumineux pour une sauvegarde Matter Home".into());
    }
    std::fs::read(path).map_err(|e| format!("fichier illisible ({})", e.kind()))
}

/// « 12 appareils, 7 pièces, 4 règles, sauvegarde du … ».
fn describe_backup(s: &ImportSummary) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n > 1 { many } else { one });
    let when = chrono::DateTime::parse_from_rfc3339(&s.created)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y à %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| s.created.clone());
    format!(
        "{}, {}, {}, sauvegarde du {when}.",
        plural(s.devices, "appareil", "appareils"),
        plural(s.rooms, "pièce", "pièces"),
        plural(s.rules, "règle", "règles"),
    )
}

fn status_badge(status: &WireStatus) -> (Color32, &'static str) {
    match status {
        WireStatus::Online => (Color32::from_rgb(40, 170, 70), "En ligne"),
        WireStatus::Connecting => (Color32::from_rgb(230, 160, 30), "Connexion…"),
        WireStatus::Unreachable { .. } => (Color32::from_rgb(210, 60, 50), "Injoignable"),
    }
}

/// Raison affichée pour un appareil injoignable.
fn unreachable_text(status: &WireStatus) -> Option<String> {
    match status {
        WireStatus::Unreachable { code, detail, .. } => {
            Some(api_message(*code, detail.as_deref(), None, 0))
        }
        _ => None,
    }
}

/// « il y a 12 s », « il y a 3 min », « il y a 2 h ».
pub fn ago(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..=59 => format!("il y a {s} s"),
        60..=3599 => format!("il y a {} min", s / 60),
        _ => format!("il y a {} h", s / 3600),
    }
}

impl eframe::App for MatterApp {
    /// Appelé avant chaque `ui`, et aussi fenêtre masquée dès que le réseau appelle
    /// `request_repaint` : les événements sont donc appliqués même quand macOS ne redessine
    /// pas la fenêtre (recouverte ou réduite).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_network();
        self.save_documents(Instant::now());
        self.maintain_series();
        if self.plan_or_rules_pending() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Rafraîchit les « il y a X s » même sans événement.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));

        // Pas de session : l'écran de connexion prend toute la fenêtre.
        if matches!(
            self.link,
            Link::LoggedOut { .. } | Link::CertificateChanged { .. } | Link::Incompatible { .. }
        ) {
            self.connect.show(ui, &self.link, &self.backend);
            return;
        }

        self.top_bar(ui);
        self.toasts_ui(&ui.ctx().clone());
        self.data_dialog_window(&ui.ctx().clone());

        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(90.0)
            .show(ui, |ui| {
                egui::CollapsingHeader::new("Journal")
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for line in &self.log {
                                    ui.small(line);
                                }
                            });
                    });
            });

        if !self.connected() {
            egui::CentralPanel::default().show(ui, |ui| self.disconnected_panel(ui));
            return;
        }

        egui::Panel::left("left")
            .resizable(true)
            .default_size(340.0)
            // Sans borne, un long message (adresse IPv6…) élargit le panneau et masque la vue thermomètre.
            .size_range(260.0..=460.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                self.pairing_panel(ui);
                ui.separator();
                self.device_list(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Devices => self.details(ui),
            View::Plan => match self.plan.show(ui, &self.model.devices) {
                plan::PlanAction::OpenDevice(id) => {
                    self.selected = Some(id);
                    self.export_message = None;
                    self.view = View::Devices;
                }
                plan::PlanAction::Customize {
                    node_id,
                    name,
                    icon,
                } => self.customize(node_id, name, icon),
                plan::PlanAction::None => {}
            },
            View::Rules => {
                if let rules::RulesAction::Test(id) =
                    self.rules
                        .show(ui, &self.model.devices, &self.model.rule_runs)
                {
                    self.test_rule(id);
                }
            }
        });
    }
}

impl MatterApp {
    fn plan_or_rules_pending(&self) -> bool {
        self.plan.editing() || self.rules.editing()
    }
}

#[cfg(test)]
mod tests;
