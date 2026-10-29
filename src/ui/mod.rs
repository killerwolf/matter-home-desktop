//! Interface egui. Ne fait jamais d'entrée/sortie réseau : elle envoie des
//! `Command` et applique les `Event` reçus à chaque image.

mod customize;
mod plan;
mod rules;
mod sensors;
mod thermometer;

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc;

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc::UnboundedSender;

use crate::events::{
    now_unix, Command, CoverMotion, Event, MeasureKind, NodeInfo, NodeStatus, ReportingMode,
    StateKind,
};
use crate::history::{RingBuffer, Sample, TimeWindow};
use crate::matter::convert::{format_position, format_power, format_temperature};

pub struct Device {
    pub info: NodeInfo,
    pub status: NodeStatus,
    pub last_seen: Option<f64>,
    pub mode: Option<ReportingMode>,
    pub temperature: RingBuffer,
    pub humidity: RingBuffer,
    pub power: RingBuffer,
    /// Énergie cumulée (kWh) et instant de la mesure.
    pub energy: Option<Sample>,
    pub battery: Option<f64>,
    pub battery_low: bool,
    /// `Some(true)` = ouvert. `open_since` : instant du dernier changement observé.
    pub open: Option<bool>,
    pub open_since: Option<f64>,
    /// Changements d'état de la porte (instant, ouvert ?), du plus ancien au plus récent.
    pub door_events: VecDeque<(f64, bool)>,
    pub on_off: Option<bool>,
    /// Ouverture d'un volet (%, 100 = ouvert) au fil du temps.
    pub position: RingBuffer,
    pub cover_motion: Option<CoverMotion>,
    /// Position choisie sur le curseur de la fiche, pas encore envoyée.
    pub cover_slider: Option<u8>,
}

impl Device {
    fn new(info: NodeInfo) -> Self {
        Self {
            info,
            status: NodeStatus::Connecting,
            last_seen: None,
            mode: None,
            temperature: RingBuffer::default(),
            humidity: RingBuffer::default(),
            power: RingBuffer::default(),
            energy: None,
            battery: None,
            battery_low: false,
            open: None,
            open_since: None,
            door_events: VecDeque::new(),
            on_off: None,
            position: RingBuffer::default(),
            cover_motion: None,
            cover_slider: None,
        }
    }

    /// Oublie les mesures en mémoire (les dernières valeurs connues aussi).
    fn clear_history(&mut self) {
        self.temperature = RingBuffer::default();
        self.humidity = RingBuffer::default();
        self.power = RingBuffer::default();
        self.position = RingBuffer::default();
        self.door_events.clear();
    }

    /// Résumé d'une ligne pour la liste : mesure principale et pile.
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.temperature.last() {
            parts.push(format_temperature(s.value));
        }
        if let Some(open) = self.open {
            parts.push(if open {
                "Ouvert".into()
            } else {
                "Fermé".into()
            });
        }
        if let Some(s) = self.power.last() {
            parts.push(format_power(s.value));
        }
        if let Some(on) = self.on_off {
            parts.push(if on {
                "Allumé".into()
            } else {
                "Éteint".into()
            });
        }
        if self.info.caps.cover {
            let mut text = match self.position.last() {
                Some(s) => format_position(s.value),
                None if self.info.caps.cover_position => "Position inconnue".into(),
                None => String::new(),
            };
            match self.cover_motion {
                Some(CoverMotion::Opening) => text.push_str(" ▲ ouverture"),
                Some(CoverMotion::Closing) => text.push_str(" ▼ fermeture"),
                _ => {}
            }
            let text = text.trim().to_string();
            if !text.is_empty() {
                parts.push(text);
            }
        }
        if let Some(b) = self.battery {
            parts.push(format!(
                "pile {b:.0} %{}",
                if self.battery_low { " (faible)" } else { "" }
            ));
        }
        parts.join(" · ")
    }
}

/// Nombre maximal de changements d'état de porte gardés en mémoire.
const DOOR_EVENTS_CAPACITY: usize = 1000;

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

pub struct MatterApp {
    commands: UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    demo: bool,
    devices: BTreeMap<u64, Device>,
    selected: Option<u64>,
    code: String,
    pairing: Pairing,
    window: TimeWindow,
    export_message: Option<(bool, String)>,
    fatal: Option<String>,
    log: VecDeque<String>,
    view: View,
    plan: plan::PlanView,
    rules: rules::RulesView,
    /// Appareil dont l'effacement d'historique attend confirmation.
    confirm_clear: Option<u64>,
    /// Nom et icône en cours de modification sur la fiche.
    device_edit: Option<customize::DeviceEdit>,
    /// Option `--device` : appareil à sélectionner quand il apparaîtra.
    select_on_arrival: Option<String>,
}

impl MatterApp {
    pub fn new(
        commands: UnboundedSender<Command>,
        events: mpsc::Receiver<Event>,
        demo: bool,
    ) -> Self {
        Self {
            commands,
            events,
            demo,
            devices: BTreeMap::new(),
            selected: None,
            code: String::new(),
            pairing: Pairing::Idle,
            window: TimeWindow::Min15,
            export_message: None,
            fatal: None,
            log: VecDeque::new(),
            view: View::Devices,
            rules: rules::RulesView::load(
                demo,
                if demo {
                    crate::demo::demo_rules
                } else {
                    crate::automation::RuleSet::default
                },
            ),
            confirm_clear: None,
            device_edit: None,
            select_on_arrival: None,
            plan: plan::PlanView::load(
                demo,
                if demo {
                    crate::demo::demo_plan
                } else {
                    crate::home::HomePlan::default
                },
            ),
        }
    }

    /// Sélectionne, dès qu'il est connu, le premier appareil dont le nom contient `filter`.
    pub fn select_device_named(&mut self, filter: String) {
        self.select_on_arrival = Some(filter);
        self.view = View::Devices;
    }

    /// Ajoute une ligne au journal de l'interface.
    pub fn note(&mut self, line: String) {
        self.push_log(line);
    }

    /// Ouvre l'onglet « Automatisations ».
    pub fn show_rules(&mut self) {
        self.view = View::Rules;
    }

    /// Ouvre l'onglet « Plan de la maison ».
    pub fn show_plan(&mut self) {
        self.view = View::Plan;
    }

    fn send(&mut self, cmd: Command) {
        if self.commands.send(cmd).is_err() {
            self.fatal = Some("La tâche de fond s'est arrêtée.".into());
        }
    }

    fn push_log(&mut self, line: String) {
        let time = chrono::Local::now().format("%H:%M:%S");
        self.log.push_back(format!("{time}  {line}"));
        while self.log.len() > 50 {
            self.log.pop_front();
        }
    }

    fn apply_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Node(info) => {
                    let id = info.node_id;
                    match self.devices.get_mut(&id) {
                        Some(d) => d.info = info,
                        None => {
                            self.devices.insert(id, Device::new(info));
                        }
                    }
                    if self.selected.is_none() {
                        self.selected = Some(id);
                    }
                    let wanted = self.select_on_arrival.as_deref().map(str::to_lowercase);
                    if let Some(w) = wanted {
                        if self.devices[&id].info.name.to_lowercase().contains(&w) {
                            self.selected = Some(id);
                            self.select_on_arrival = None;
                        }
                    }
                }
                Event::NodeRemoved(id) => {
                    self.devices.remove(&id);
                    self.plan.remove_device(id);
                    if self.selected == Some(id) {
                        self.selected = self.devices.keys().next().copied();
                    }
                }
                Event::Status { node_id, status } => {
                    let mut line = None;
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        let was_unreachable = matches!(d.status, NodeStatus::Unreachable { .. });
                        match &status {
                            NodeStatus::Unreachable { error, .. } if !was_unreachable => {
                                line = Some(format!("{} injoignable : {error}", d.info.name));
                            }
                            NodeStatus::Online if was_unreachable => {
                                line = Some(format!("{} de nouveau en ligne", d.info.name));
                            }
                            _ => {}
                        }
                        d.status = status;
                    }
                    if let Some(line) = line {
                        self.push_log(line);
                    }
                }
                Event::Seen { node_id, at } => {
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        d.last_seen = Some(d.last_seen.map_or(at, |s| s.max(at)));
                    }
                }
                Event::Mode { node_id, mode } => {
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        d.mode = Some(mode);
                    }
                }
                Event::Measurement {
                    node_id,
                    kind,
                    value,
                    at,
                } => {
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        let sample = Sample { t: at, value };
                        match kind {
                            MeasureKind::Temperature => d.temperature.push(sample),
                            MeasureKind::Humidity => d.humidity.push(sample),
                            MeasureKind::Power => d.power.push(sample),
                            MeasureKind::Energy => d.energy = Some(sample),
                            MeasureKind::Battery => d.battery = Some(value),
                            MeasureKind::Position => d.position.push(sample),
                        }
                    }
                }
                Event::CoverMotion { node_id, motion } => {
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        d.cover_motion = Some(motion);
                    }
                }
                Event::State {
                    node_id,
                    kind,
                    value,
                    at,
                } => {
                    if let Some(d) = self.devices.get_mut(&node_id) {
                        match kind {
                            StateKind::Open => {
                                if d.open != Some(value) {
                                    d.open = Some(value);
                                    d.open_since = Some(at);
                                    d.door_events.push_back((at, value));
                                    if d.door_events.len() > DOOR_EVENTS_CAPACITY {
                                        d.door_events.pop_front();
                                    }
                                }
                            }
                            StateKind::OnOff => d.on_off = Some(value),
                            StateKind::BatteryLow => d.battery_low = value,
                        }
                    }
                }
                Event::PairingProgress(m) => self.pairing = Pairing::InProgress(m),
                Event::PairingDone { node_id, name } => {
                    self.push_log(format!("Appairé : {name}"));
                    self.pairing = Pairing::Success(format!(
                        "« {name} » est appairé. Lecture de sa description en cours…"
                    ));
                    self.selected = Some(node_id);
                    self.code.clear();
                }
                Event::PairingFailed(m) => {
                    self.push_log(format!(
                        "Échec d'appairage : {}",
                        m.lines().next().unwrap_or("")
                    ));
                    self.pairing = Pairing::Failed(m);
                }
                Event::Fatal(m) => {
                    self.push_log(format!("Erreur : {m}"));
                    self.fatal = Some(m);
                }
                Event::Info(m) => self.push_log(m),
                Event::RuleRun {
                    rule_id,
                    name,
                    at,
                    ok,
                    detail,
                } => {
                    self.push_log(format!(
                        "Règle « {name} » {} : {detail}",
                        if ok { "exécutée" } else { "en échec" }
                    ));
                    self.rules.record_run(rules::RunEntry {
                        rule_id,
                        name,
                        at,
                        ok,
                        detail,
                    });
                }
            }
        }
    }

    fn pairing_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Appairer un appareil");
        ui.label("Code de l'app Maison : 11 chiffres, ou code QR « MT:… ».");
        let busy = matches!(self.pairing, Pairing::InProgress(_));
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
            self.pairing = Pairing::InProgress("Démarrage…".into());
            self.send(Command::Pair(code));
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
        if self.devices.is_empty() {
            ui.label("Aucun appareil appairé pour l'instant.");
            return;
        }
        let now = now_unix();
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (id, d) in &self.devices {
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
                                self.devices.get(&link.node_id).map_or_else(
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
            let link = self.devices.get(&id)?.info.bridge?;
            Some(
                self.devices
                    .get(&link.node_id)
                    .map_or_else(|| format!("{:016X}", link.node_id), |b| b.info.name.clone()),
            )
        });
        let Some(device) = self.devices.get_mut(&id) else {
            return;
        };
        let fresh_edit = customize::DeviceEdit::from_info(&device.info);

        let mut retry = false;
        let mut forget = false;
        let mut clear = false;
        let mut customize = false;
        let mut open_device = None;
        let mut cover_command = None;
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
                        "Supprime les mesures enregistrées (mémoire et disque) de cet appareil.",
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
        if let Some(b) = device.battery {
            let color = if device.battery_low || b < 20.0 {
                Color32::from_rgb(210, 60, 50)
            } else {
                Color32::from_rgb(40, 160, 70)
            };
            let warn = if device.battery_low {
                " — pile faible, à remplacer"
            } else {
                ""
            };
            ui.colored_label(color, format!("Pile : {b:.0} %{warn}"));
        }
        let (color, text) = status_badge(&device.status);
        ui.horizontal(|ui| {
            ui.colored_label(color, format!("● {text}"));
            if let NodeStatus::Unreachable { error, retry_in_s } = &device.status {
                if *retry_in_s > 0 {
                    ui.label(format!("— nouvel essai automatique sous {retry_in_s} s"))
                        .on_hover_text(error);
                }
            }
        });
        if let NodeStatus::Unreachable { error, .. } = &device.status {
            ui.small(RichText::new(error).color(Color32::GRAY));
        }
        ui.separator();

        let caps = device.info.caps;
        let window = &mut self.window;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if caps.temperature {
                    thermometer::show(ui, device, window, &mut self.export_message);
                    ui.separator();
                }
                if caps.contact {
                    sensors::door(ui, device, window);
                    ui.separator();
                }
                if caps.power || caps.energy || caps.on_off {
                    sensors::power(ui, device, window);
                    ui.separator();
                }
                if caps.cover {
                    cover_command = sensors::cover(ui, device, window);
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

        if retry {
            self.send(Command::Retry(id));
        }
        if let Some(command) = cover_command {
            self.send(Command::SetCover {
                node_id: id,
                command,
            });
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
            self.send(Command::Customize {
                node_id: id,
                name: Some(edit.name),
                icon: edit.icon,
            });
        }
        if forget {
            self.send(Command::Forget(id));
        }
        if clear {
            self.confirm_clear = None;
            if let Some(d) = self.devices.get_mut(&id) {
                d.clear_history();
            }
            self.push_log("Historique effacé.".into());
            self.send(Command::ClearHistory(id));
        }
    }
}

fn status_badge(status: &NodeStatus) -> (Color32, &'static str) {
    match status {
        NodeStatus::Online => (Color32::from_rgb(40, 170, 70), "En ligne"),
        NodeStatus::Connecting => (Color32::from_rgb(230, 160, 30), "Connexion…"),
        NodeStatus::Unreachable { .. } => (Color32::from_rgb(210, 60, 50), "Injoignable"),
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
    /// Appelé avant chaque `ui`, et aussi fenêtre masquée dès que le backend appelle
    /// `request_repaint` : les événements sont donc appliqués même quand macOS ne
    /// redessine pas la fenêtre (recouverte ou réduite).
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_events();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Rafraîchit les « il y a X s » même sans événement.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Matter over Thread — contrôleur");
                if self.demo {
                    ui.label(
                        RichText::new(" MODE DÉMO ")
                            .strong()
                            .color(Color32::BLACK)
                            .background_color(Color32::from_rgb(250, 200, 60)),
                    );
                }
                ui.separator();
                ui.selectable_value(&mut self.view, View::Devices, "Appareils");
                ui.selectable_value(&mut self.view, View::Plan, "Plan de la maison");
                ui.selectable_value(&mut self.view, View::Rules, "Automatisations");
            });
            if let Some(f) = &self.fatal {
                ui.colored_label(Color32::from_rgb(210, 60, 50), format!("Erreur : {f}"));
            }
        });

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
            View::Plan => match self.plan.show(ui, &self.devices) {
                plan::PlanAction::OpenDevice(id) => {
                    self.selected = Some(id);
                    self.export_message = None;
                    self.view = View::Devices;
                }
                plan::PlanAction::Customize {
                    node_id,
                    name,
                    icon,
                } => {
                    self.send(Command::Customize {
                        node_id,
                        name,
                        icon,
                    });
                }
                plan::PlanAction::None => {}
            },
            View::Rules => match self.rules.show(ui, &self.devices) {
                rules::RulesAction::Changed(rules) => self.send(Command::SetRules(rules)),
                rules::RulesAction::Test(id) => self.send(Command::TestRule(id)),
                rules::RulesAction::None => {}
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::CoverCommand;

    /// Mode démo de bout en bout, sans fenêtre : backend simulé → événements → état de l'interface.
    #[test]
    fn demo_backend_end_to_end() {
        let (tx, rx) = mpsc::channel();
        let sink = crate::events::EventSink::new(tx, egui::Context::default());
        let commands = crate::demo::spawn(sink);
        let mut app = MatterApp::new(commands, rx, true);

        let wait_up_to = |app: &mut MatterApp, cond: &dyn Fn(&MatterApp) -> bool, n: u32| {
            for _ in 0..n {
                app.apply_events();
                if cond(app) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };
        let wait_for =
            |app: &mut MatterApp, cond: &dyn Fn(&MatterApp) -> bool| wait_up_to(app, cond, 100);

        // La démo envoie les statuts « en ligne » après tout l'historique pré-rempli :
        // 4 appareils, un pont et ses 3 volets.
        assert!(wait_for(&mut app, &|a| {
            a.devices.len() == 8 && a.devices.values().all(|d| d.status == NodeStatus::Online)
        }));
        let living = app.devices.values().find(|d| d.info.caps.humidity).unwrap();
        assert!(living.temperature.len() > 150, "historique pré-rempli");
        assert!(!living.humidity.is_empty());
        assert_eq!(living.status, NodeStatus::Online);
        assert_eq!(living.battery, Some(87.0));

        // Porte : transitions rejouées et état courant connu.
        let door = app.devices.values().find(|d| d.info.caps.contact).unwrap();
        assert!(door.open.is_some());
        assert!(door.door_events.len() > 10, "historique des ouvertures");
        assert!(door
            .door_events
            .iter()
            .zip(door.door_events.iter().skip(1))
            .all(|(a, b)| a.1 != b.1));

        // Prise : puissance, énergie, marche/arrêt.
        let plug = app.devices.values().find(|d| d.info.caps.power).unwrap();
        assert!(plug.power.len() > 150);
        assert!(plug.energy.is_some_and(|e| e.value > 12.0));
        assert_eq!(plug.on_off, Some(true));
        assert!(plug.summary().contains(" W"));
        let plug_id = plug.info.node_id;

        // Pile faible signalée.
        assert!(app.devices.values().any(|d| d.battery_low));

        // Volets derrière le pont : position connue pour deux, inconnue pour le moteur RTS.
        let bridge = app
            .devices
            .values()
            .find(|d| d.info.device_types.iter().any(|t| t == "Passerelle"))
            .unwrap();
        let bridge_id = bridge.info.node_id;
        let shutters: Vec<&Device> = app
            .devices
            .values()
            .filter(|d| d.info.bridge.is_some_and(|b| b.node_id == bridge_id))
            .collect();
        assert_eq!(shutters.len(), 3);
        assert!(shutters.iter().all(|d| d.info.caps.cover));
        assert_eq!(
            shutters
                .iter()
                .filter(|d| d.info.caps.cover_position)
                .count(),
            2
        );
        let open_shutter = shutters
            .iter()
            .find(|d| d.position.last().is_some_and(|p| p.value >= 100.0))
            .expect("un volet tout ouvert");
        assert!(open_shutter.summary().contains("Ouvert"));
        let shutter_id = open_shutter.info.node_id;
        let rts = shutters
            .iter()
            .find(|d| !d.info.caps.cover_position)
            .unwrap();
        assert!(rts.position.is_empty());
        let rts_id = rts.info.node_id;

        // Fermer un volet : il descend par paliers puis s'arrête fermé.
        app.send(Command::SetCover {
            node_id: shutter_id,
            command: CoverCommand::Close,
        });
        assert!(wait_for(&mut app, &|a| a.devices[&shutter_id].cover_motion
            == Some(CoverMotion::Closing)));
        // 100 % → 0 % par pas de 25 % toutes les 2 s : une dizaine de secondes.
        assert!(wait_up_to(
            &mut app,
            &|a| {
                let d = &a.devices[&shutter_id];
                d.cover_motion == Some(CoverMotion::Stopped)
                    && d.position.last().is_some_and(|p| p.value <= 0.0)
            },
            300
        ));
        assert_eq!(app.devices[&shutter_id].summary(), "Fermé");
        // Puis à mi-hauteur.
        app.send(Command::SetCover {
            node_id: shutter_id,
            command: CoverCommand::Position(50),
        });
        assert!(wait_up_to(
            &mut app,
            &|a| {
                a.devices[&shutter_id]
                    .position
                    .last()
                    .is_some_and(|p| p.value == 50.0)
            },
            300
        ));
        // Le moteur RTS bouge sans jamais donner de position.
        app.send(Command::SetCover {
            node_id: rts_id,
            command: CoverCommand::Open,
        });
        assert!(wait_for(&mut app, &|a| a.devices[&rts_id].cover_motion
            == Some(CoverMotion::Opening)));
        assert!(app.devices[&rts_id].position.is_empty());

        // Nom et icône choisis, puis retour à l'automatique.
        app.send(Command::Customize {
            node_id: plug_id,
            name: Some("  Lampe du salon ".into()),
            icon: Some(crate::home::Symbol::Light),
        });
        assert!(wait_for(&mut app, &|a| a.devices[&plug_id].info.name
            == "Lampe du salon"));
        let p = &app.devices[&plug_id];
        assert_eq!(p.info.icon, Some(crate::home::Symbol::Light));
        assert_eq!(p.info.device_name, "GRILLPLATS Plug");
        assert_eq!(plan::icon_of(p), Some(crate::home::Symbol::Light));
        app.send(Command::Customize {
            node_id: plug_id,
            name: Some("   ".into()),
            icon: None,
        });
        assert!(wait_for(&mut app, &|a| a.devices[&plug_id]
            .info
            .custom_name
            .is_none()));
        assert_eq!(app.devices[&plug_id].info.name, "GRILLPLATS Plug");
        assert_eq!(
            plan::icon_of(&app.devices[&plug_id]),
            Some(crate::home::Symbol::Plug)
        );

        // Code invalide : message d'erreur clair.
        app.send(Command::Pair("12345".into()));
        assert!(wait_for(&mut app, &|a| matches!(
            a.pairing,
            Pairing::Failed(_)
        )));

        // Code valide : un appareil de plus.
        app.send(Command::Pair("3497-011-2332".into()));
        assert!(wait_for(&mut app, &|a| matches!(
            a.pairing,
            Pairing::Success(_)
        )));
        assert_eq!(app.devices.len(), 9);
        let new_id = app.selected.unwrap();

        // Oublier.
        app.send(Command::Forget(new_id));
        assert!(wait_for(&mut app, &|a| a.devices.len() == 8));

        // Export CSV du salon.
        let living = app.devices.values().find(|d| d.info.caps.humidity).unwrap();
        let csv = crate::history::to_csv(&living.temperature, Some(&living.humidity));
        assert_eq!(csv.lines().count(), living.temperature.len() + 1);
    }

    #[test]
    fn ago_formatting() {
        assert_eq!(ago(5.4), "il y a 5 s");
        assert_eq!(ago(125.0), "il y a 2 min");
        assert_eq!(ago(7300.0), "il y a 2 h");
        assert_eq!(ago(-3.0), "il y a 0 s");
    }
}
