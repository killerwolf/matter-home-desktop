//! Onglet « Automatisations » : liste des règles, éditeur « Quand / Si / Alors », journal.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};

use super::plan::{glyph, icon_of};
use super::{ago, Device};
use crate::automation::{
    self, Action, Condition, CoverCommand, OnOffCommand, Rule, RuleSet, Trigger,
};
use crate::events::{now_unix, MeasureKind};
use crate::persistence;

const RED: Color32 = Color32::from_rgb(210, 60, 50);
const GREEN: Color32 = Color32::from_rgb(40, 160, 70);
const DAYS: [&str; 7] = ["L", "M", "M", "J", "V", "S", "D"];
const MEASURES: [MeasureKind; 5] = [
    MeasureKind::Temperature,
    MeasureKind::Humidity,
    MeasureKind::Power,
    MeasureKind::Battery,
    MeasureKind::Position,
];

pub struct RunEntry {
    pub rule_id: u64,
    pub name: String,
    pub at: f64,
    pub ok: bool,
    pub detail: String,
}

pub enum RulesAction {
    None,
    Changed(Vec<Rule>),
    Test(u64),
}

pub struct RulesView {
    set: RuleSet,
    path: PathBuf,
    selected: Option<u64>,
    save_error: Option<String>,
    confirm_delete: bool,
    runs: VecDeque<RunEntry>,
}

/// Quel genre d'appareil un sélecteur doit proposer.
#[derive(Clone, Copy)]
enum Need {
    Contact,
    Switch,
    Cover,
    Measure(MeasureKind),
    Any,
}

fn measure_label(m: MeasureKind) -> &'static str {
    match m {
        MeasureKind::Temperature => "température (°C)",
        MeasureKind::Humidity => "humidité (%)",
        MeasureKind::Power => "puissance (W)",
        MeasureKind::Energy => "énergie (kWh)",
        MeasureKind::Battery => "pile (%)",
        MeasureKind::Position => "ouverture du volet (%)",
    }
}

fn suits(d: &Device, need: Need) -> bool {
    let c = d.info.caps;
    match need {
        Need::Contact => c.contact,
        Need::Switch => c.on_off,
        Need::Cover => c.cover,
        Need::Measure(MeasureKind::Temperature) => c.temperature,
        Need::Measure(MeasureKind::Humidity) => c.humidity,
        Need::Measure(MeasureKind::Power) => c.power,
        Need::Measure(MeasureKind::Energy) => c.energy,
        Need::Measure(MeasureKind::Battery) => c.battery,
        Need::Measure(MeasureKind::Position) => c.cover_position,
        Need::Any => true,
    }
}

fn device_label(devices: &BTreeMap<u64, Device>, id: u64) -> String {
    devices
        .get(&id)
        .map(|d| format!("{} {}", glyph(icon_of(d)), d.info.name))
        .unwrap_or_else(|| {
            if id == 0 {
                "— choisir —".into()
            } else {
                format!("Appareil inconnu ({id:016X})")
            }
        })
}

/// Sélecteur d'appareil filtré par capacité. Retourne `true` si la valeur a changé.
fn device_picker(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    id: &mut u64,
    need: Need,
    devices: &BTreeMap<u64, Device>,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(device_label(devices, *id))
        .width(220.0)
        .show_ui(ui, |ui| {
            for (node_id, d) in devices.iter().filter(|(_, d)| suits(d, need)) {
                changed |= ui
                    .selectable_value(
                        id,
                        *node_id,
                        format!("{} {}", glyph(icon_of(d)), d.info.name),
                    )
                    .changed();
            }
        });
    changed
}

fn first_device(devices: &BTreeMap<u64, Device>, need: Need) -> u64 {
    devices
        .iter()
        .find(|(_, d)| suits(d, need))
        .map_or(0, |(id, _)| *id)
}

fn measure_picker(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    m: &mut MeasureKind,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(measure_label(*m))
        .show_ui(ui, |ui| {
            for k in MEASURES {
                changed |= ui.selectable_value(m, k, measure_label(k)).changed();
            }
        });
    changed
}

fn above_picker(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    above: &mut bool,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(if *above { "dépasse" } else { "descend sous" })
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(above, true, "dépasse").changed();
            changed |= ui.selectable_value(above, false, "descend sous").changed();
        });
    changed
}

fn bool_picker(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    v: &mut bool,
    yes: &str,
    no: &str,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(if *v { yes } else { no })
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(v, true, yes).changed();
            changed |= ui.selectable_value(v, false, no).changed();
        });
    changed
}

fn hhmm(ui: &mut egui::Ui, h: &mut u8, m: &mut u8) -> bool {
    let a = ui
        .add(egui::DragValue::new(h).range(0..=23).suffix(" h"))
        .changed();
    let b = ui
        .add(egui::DragValue::new(m).range(0..=59).suffix(" min"))
        .changed();
    a || b
}

fn trigger_kind(t: &Trigger) -> usize {
    match t {
        Trigger::Contact { .. } => 0,
        Trigger::Threshold { .. } => 1,
        Trigger::Switch { .. } => 2,
        Trigger::Unreachable { .. } => 3,
        Trigger::Time { .. } => 4,
    }
}

const TRIGGER_KINDS: [&str; 5] = [
    "Une porte / fenêtre change",
    "Une mesure franchit un seuil",
    "Une prise / lampe change",
    "Un appareil reste injoignable",
    "À une heure donnée",
];

fn default_trigger(kind: usize, devices: &BTreeMap<u64, Device>) -> Trigger {
    match kind {
        0 => Trigger::Contact {
            node_id: first_device(devices, Need::Contact),
            open: true,
        },
        1 => Trigger::Threshold {
            node_id: first_device(devices, Need::Measure(MeasureKind::Temperature)),
            measure: MeasureKind::Temperature,
            above: true,
            value: 26.0,
            for_s: 0,
        },
        2 => Trigger::Switch {
            node_id: first_device(devices, Need::Switch),
            on: true,
        },
        3 => Trigger::Unreachable {
            node_id: first_device(devices, Need::Any),
            for_s: 3600,
        },
        _ => Trigger::Time {
            hour: 7,
            minute: 0,
            days: [true; 7],
        },
    }
}

/// Résumé d'une ligne d'une règle, pour la liste.
fn summary(rule: &Rule, devices: &BTreeMap<u64, Device>) -> String {
    let name = |id: u64| {
        devices
            .get(&id)
            .map_or_else(|| "?".to_string(), |d| d.info.name.clone())
    };
    let when = match &rule.trigger {
        Trigger::Contact { node_id, open } => format!(
            "{} {}",
            name(*node_id),
            if *open { "s'ouvre" } else { "se ferme" }
        ),
        Trigger::Threshold {
            node_id,
            measure,
            above,
            value,
            for_s,
        } => {
            let dur = if *for_s > 0 {
                format!(" pendant {} min", for_s / 60)
            } else {
                String::new()
            };
            format!(
                "{} : {} {} {value}{dur}",
                name(*node_id),
                measure_label(*measure),
                if *above { ">" } else { "<" }
            )
        }
        Trigger::Switch { node_id, on } => format!(
            "{} {}",
            name(*node_id),
            if *on { "s'allume" } else { "s'éteint" }
        ),
        Trigger::Unreachable { node_id, for_s } => {
            format!("{} injoignable {} min", name(*node_id), for_s / 60)
        }
        Trigger::Time { hour, minute, .. } => format!("à {hour:02}:{minute:02}"),
    };
    let then: Vec<String> = rule
        .actions
        .iter()
        .map(|a| match a {
            Action::Notify { .. } => "notifier".to_string(),
            Action::SetOnOff { node_id, command } => {
                format!("{} {}", command.label().to_lowercase(), name(*node_id))
            }
            Action::SetCover { node_id, command } => {
                format!("{} {}", command.label().to_lowercase(), name(*node_id))
            }
            Action::Webhook { .. } => "webhook".to_string(),
            Action::Delay { seconds } => format!("attendre {seconds} s"),
        })
        .collect();
    format!(
        "Quand {when} → {}",
        if then.is_empty() {
            "rien".into()
        } else {
            then.join(", ")
        }
    )
}

impl RulesView {
    pub fn load(demo: bool, default: impl FnOnce() -> RuleSet) -> Self {
        let (set, save_error) = automation::load(demo, default);
        Self {
            selected: set.rules.first().map(|r| r.id),
            set,
            path: automation::path(demo),
            save_error,
            confirm_delete: false,
            runs: VecDeque::new(),
        }
    }

    pub fn record_run(&mut self, entry: RunEntry) {
        self.runs.push_front(entry);
        self.runs.truncate(50);
    }

    fn save(&mut self) -> RulesAction {
        match persistence::save_json(&self.path, &self.set) {
            Ok(()) => self.save_error = None,
            Err(e) => self.save_error = Some(format!("Sauvegarde des règles impossible : {e:#}")),
        }
        RulesAction::Changed(self.set.rules.clone())
    }

    pub fn show(&mut self, ui: &mut egui::Ui, devices: &BTreeMap<u64, Device>) -> RulesAction {
        let mut action = RulesAction::None;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.heading("Automatisations");
            if ui.button("+ Nouvelle règle").clicked() {
                let id = self.set.next_id();
                self.set.rules.push(Rule::new(id));
                self.selected = Some(id);
                changed = true;
            }
        });
        ui.small("Les règles s'exécutent tant que l'application est ouverte.");
        if let Some(e) = &self.save_error {
            ui.colored_label(RED, e);
        }
        ui.separator();

        egui::Panel::bottom("rules_journal")
            .resizable(true)
            .default_size(140.0)
            .show(ui, |ui| {
                ui.strong("Dernières exécutions");
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        if self.runs.is_empty() {
                            ui.small("Aucune exécution depuis le lancement.");
                        }
                        let now = now_unix();
                        for r in &self.runs {
                            let color = if r.ok { GREEN } else { RED };
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(color, if r.ok { "✔" } else { "✖" });
                                ui.small(format!(
                                    "{} — {} ({})",
                                    r.name,
                                    r.detail,
                                    ago(now - r.at)
                                ));
                            });
                        }
                    });
            });

        egui::Panel::left("rules_list")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.set.rules.is_empty() {
                        ui.label("Aucune règle. Cliquez sur « + Nouvelle règle ».");
                    }
                    for rule in &mut self.set.rules {
                        let selected = self.selected == Some(rule.id);
                        let resp = egui::Frame::group(ui.style())
                            .fill(if selected {
                                ui.visuals().selection.bg_fill.linear_multiply(0.35)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    changed |= ui
                                        .checkbox(&mut rule.enabled, "")
                                        .on_hover_text("Activée")
                                        .changed();
                                    ui.label(RichText::new(&rule.name).strong());
                                });
                                ui.small(summary(rule, devices));
                                if let Some(last) = self.runs.iter().find(|r| r.rule_id == rule.id)
                                {
                                    let color = if last.ok { GREEN } else { RED };
                                    ui.colored_label(
                                        color,
                                        format!("Dernière exécution {}", ago(now_unix() - last.at)),
                                    );
                                }
                            })
                            .response;
                        if resp.interact(egui::Sense::click()).clicked() {
                            self.selected = Some(rule.id);
                            self.confirm_delete = false;
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let Some(idx) = self
                .selected
                .and_then(|id| self.set.rules.iter().position(|r| r.id == id))
            else {
                ui.label("Sélectionnez ou créez une règle.");
                return;
            };
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    let (c, a) = self.editor(ui, idx, devices);
                    changed |= c;
                    if let Some(a) = a {
                        action = a;
                    }
                });
        });

        if changed {
            if let RulesAction::None = action {
                action = self.save();
            } else {
                self.save();
            }
        }
        action
    }

    /// Éditeur d'une règle. Retourne (modifiée ?, action demandée).
    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        devices: &BTreeMap<u64, Device>,
    ) -> (bool, Option<RulesAction>) {
        let mut changed = false;
        let mut request = None;
        let mut delete = false;
        let rule = &mut self.set.rules[idx];
        let rid = rule.id;

        ui.horizontal_wrapped(|ui| {
            ui.label("Nom :");
            changed |= ui
                .add(egui::TextEdit::singleline(&mut rule.name).desired_width(260.0))
                .changed();
            changed |= ui.checkbox(&mut rule.enabled, "Activée").changed();
            if ui
                .button("▶ Tester")
                .on_hover_text(
                    "Exécute les actions tout de suite, sans vérifier « Quand » ni « Si ».",
                )
                .clicked()
            {
                request = Some(RulesAction::Test(rid));
            }
            if !self.confirm_delete {
                if ui.button("Supprimer").clicked() {
                    self.confirm_delete = true;
                }
            } else {
                if ui
                    .button(RichText::new("Oui, supprimer").color(RED))
                    .clicked()
                {
                    delete = true;
                }
                if ui.button("Annuler").clicked() {
                    self.confirm_delete = false;
                }
            }
        });
        ui.add_space(6.0);

        // QUAND
        ui.heading("Quand");
        let mut kind = trigger_kind(&rule.trigger);
        egui::ComboBox::from_id_salt(("trig_kind", rid))
            .selected_text(TRIGGER_KINDS[kind])
            .width(260.0)
            .show_ui(ui, |ui| {
                for (i, label) in TRIGGER_KINDS.iter().enumerate() {
                    ui.selectable_value(&mut kind, i, *label);
                }
            });
        if kind != trigger_kind(&rule.trigger) {
            rule.trigger = default_trigger(kind, devices);
            changed = true;
        }
        ui.horizontal_wrapped(|ui| match &mut rule.trigger {
            Trigger::Contact { node_id, open } => {
                changed |= device_picker(ui, ("t_dev", rid), node_id, Need::Contact, devices);
                changed |= bool_picker(ui, ("t_open", rid), open, "s'ouvre", "se ferme");
            }
            Trigger::Threshold {
                node_id,
                measure,
                above,
                value,
                for_s,
            } => {
                let mut m = *measure;
                if measure_picker(ui, ("t_meas", rid), &mut m) {
                    *measure = m;
                    *node_id = first_device(devices, Need::Measure(m));
                    changed = true;
                }
                ui.label("de");
                changed |= device_picker(
                    ui,
                    ("t_dev", rid),
                    node_id,
                    Need::Measure(*measure),
                    devices,
                );
                changed |= above_picker(ui, ("t_above", rid), above);
                changed |= ui.add(egui::DragValue::new(value).speed(0.5)).changed();
                ui.label("pendant au moins");
                let mut minutes = *for_s / 60;
                if ui
                    .add(
                        egui::DragValue::new(&mut minutes)
                            .range(0..=1440)
                            .suffix(" min"),
                    )
                    .changed()
                {
                    *for_s = minutes * 60;
                    changed = true;
                }
            }
            Trigger::Switch { node_id, on } => {
                changed |= device_picker(ui, ("t_dev", rid), node_id, Need::Switch, devices);
                changed |= bool_picker(ui, ("t_on", rid), on, "s'allume", "s'éteint");
            }
            Trigger::Unreachable { node_id, for_s } => {
                changed |= device_picker(ui, ("t_dev", rid), node_id, Need::Any, devices);
                ui.label("depuis au moins");
                let mut minutes = *for_s / 60;
                if ui
                    .add(
                        egui::DragValue::new(&mut minutes)
                            .range(1..=10_080)
                            .suffix(" min"),
                    )
                    .changed()
                {
                    *for_s = minutes * 60;
                    changed = true;
                }
            }
            Trigger::Time { hour, minute, days } => {
                changed |= hhmm(ui, hour, minute);
                for (i, d) in DAYS.iter().enumerate() {
                    changed |= ui.toggle_value(&mut days[i], *d).changed();
                }
            }
        });
        ui.add_space(6.0);

        // SI
        ui.heading("Si (toutes les conditions)");
        let mut remove_cond = None;
        for (i, cond) in rule.conditions.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                match cond {
                    Condition::TimeBetween { from, to } => {
                        ui.label("entre");
                        changed |= hhmm(ui, &mut from.0, &mut from.1);
                        ui.label("et");
                        changed |= hhmm(ui, &mut to.0, &mut to.1);
                    }
                    Condition::Contact { node_id, open } => {
                        changed |=
                            device_picker(ui, ("c_dev", rid, i), node_id, Need::Contact, devices);
                        changed |=
                            bool_picker(ui, ("c_open", rid, i), open, "est ouverte", "est fermée");
                    }
                    Condition::Switch { node_id, on } => {
                        changed |=
                            device_picker(ui, ("c_dev", rid, i), node_id, Need::Switch, devices);
                        changed |=
                            bool_picker(ui, ("c_on", rid, i), on, "est allumée", "est éteinte");
                    }
                    Condition::Threshold {
                        node_id,
                        measure,
                        above,
                        value,
                    } => {
                        let mut m = *measure;
                        if measure_picker(ui, ("c_meas", rid, i), &mut m) {
                            *measure = m;
                            *node_id = first_device(devices, Need::Measure(m));
                            changed = true;
                        }
                        ui.label("de");
                        changed |= device_picker(
                            ui,
                            ("c_dev", rid, i),
                            node_id,
                            Need::Measure(*measure),
                            devices,
                        );
                        changed |= bool_picker(
                            ui,
                            ("c_above", rid, i),
                            above,
                            "est au-dessus de",
                            "est sous",
                        );
                        changed |= ui.add(egui::DragValue::new(value).speed(0.5)).changed();
                    }
                }
                if ui
                    .small_button("✖")
                    .on_hover_text("Retirer la condition")
                    .clicked()
                {
                    remove_cond = Some(i);
                }
            });
        }
        if let Some(i) = remove_cond {
            rule.conditions.remove(i);
            changed = true;
        }
        ui.horizontal(|ui| {
            ui.menu_button("+ Condition", |ui| {
                if ui.button("Plage horaire").clicked() {
                    rule.conditions.push(Condition::TimeBetween {
                        from: (22, 0),
                        to: (7, 0),
                    });
                    changed = true;
                    ui.close();
                }
                if ui.button("Porte / fenêtre ouverte ou fermée").clicked() {
                    rule.conditions.push(Condition::Contact {
                        node_id: first_device(devices, Need::Contact),
                        open: true,
                    });
                    changed = true;
                    ui.close();
                }
                if ui.button("Prise / lampe allumée ou éteinte").clicked() {
                    rule.conditions.push(Condition::Switch {
                        node_id: first_device(devices, Need::Switch),
                        on: true,
                    });
                    changed = true;
                    ui.close();
                }
                if ui.button("Mesure au-dessus / en dessous").clicked() {
                    rule.conditions.push(Condition::Threshold {
                        node_id: first_device(devices, Need::Measure(MeasureKind::Temperature)),
                        measure: MeasureKind::Temperature,
                        above: true,
                        value: 20.0,
                    });
                    changed = true;
                    ui.close();
                }
            });
        });
        ui.add_space(6.0);

        // ALORS
        ui.heading("Alors");
        let mut remove_act = None;
        let mut move_up = None;
        let n_actions = rule.actions.len();
        for (i, act) in rule.actions.iter_mut().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    match act {
                        Action::Notify { title, message } => {
                            ui.label("Notifier :");
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(title)
                                        .hint_text("titre")
                                        .desired_width(160.0),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(message)
                                        .hint_text("message")
                                        .desired_width(260.0),
                                )
                                .changed();
                        }
                        Action::SetOnOff { node_id, command } => {
                            egui::ComboBox::from_id_salt(("a_cmd", rid, i))
                                .selected_text(command.label())
                                .show_ui(ui, |ui| {
                                    for c in OnOffCommand::ALL {
                                        changed |=
                                            ui.selectable_value(command, c, c.label()).changed();
                                    }
                                });
                            changed |= device_picker(
                                ui,
                                ("a_dev", rid, i),
                                node_id,
                                Need::Switch,
                                devices,
                            );
                        }
                        Action::SetCover { node_id, command } => {
                            egui::ComboBox::from_id_salt(("a_cover", rid, i))
                                .selected_text(command.kind_label())
                                .show_ui(ui, |ui| {
                                    for c in CoverCommand::KINDS {
                                        if ui
                                            .selectable_label(command.same_kind(c), c.kind_label())
                                            .clicked()
                                            && !command.same_kind(c)
                                        {
                                            *command = c;
                                            changed = true;
                                        }
                                    }
                                });
                            if let CoverCommand::Position(percent) = command {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(percent)
                                            .range(0..=100)
                                            .suffix(" % d'ouverture"),
                                    )
                                    .changed();
                            }
                            changed |=
                                device_picker(ui, ("a_dev", rid, i), node_id, Need::Cover, devices);
                        }
                        Action::Webhook { url, body } => {
                            ui.label("Appeler :");
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(url)
                                        .hint_text("https://…")
                                        .desired_width(260.0),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    egui::TextEdit::singleline(body)
                                        .hint_text("corps (vide = GET)")
                                        .desired_width(200.0),
                                )
                                .changed();
                        }
                        Action::Delay { seconds } => {
                            ui.label("Attendre");
                            changed |= ui
                                .add(egui::DragValue::new(seconds).range(1..=3600).suffix(" s"))
                                .changed();
                        }
                    }
                    if i > 0 && ui.small_button("↑").on_hover_text("Monter").clicked() {
                        move_up = Some(i);
                    }
                    if ui
                        .small_button("✖")
                        .on_hover_text("Retirer l'action")
                        .clicked()
                    {
                        remove_act = Some(i);
                    }
                });
            });
        }
        if n_actions == 0 {
            ui.small("Aucune action.");
        }
        if let Some(i) = move_up {
            rule.actions.swap(i - 1, i);
            changed = true;
        }
        if let Some(i) = remove_act {
            rule.actions.remove(i);
            changed = true;
        }
        ui.menu_button("+ Action", |ui| {
            if ui.button("Notification macOS").clicked() {
                rule.actions.push(Action::Notify {
                    title: "{regle}".into(),
                    message: "{appareil} : {valeur} à {heure}".into(),
                });
                changed = true;
                ui.close();
            }
            if ui.button("Allumer / éteindre une prise ou lampe").clicked() {
                rule.actions.push(Action::SetOnOff {
                    node_id: first_device(devices, Need::Switch),
                    command: OnOffCommand::On,
                });
                changed = true;
                ui.close();
            }
            if ui.button("Ouvrir / fermer un volet").clicked() {
                rule.actions.push(Action::SetCover {
                    node_id: first_device(devices, Need::Cover),
                    command: CoverCommand::Close,
                });
                changed = true;
                ui.close();
            }
            if ui.button("Appeler une URL (webhook)").clicked() {
                rule.actions.push(Action::Webhook {
                    url: String::new(),
                    body: String::new(),
                });
                changed = true;
                ui.close();
            }
            if ui.button("Attendre").clicked() {
                rule.actions.push(Action::Delay { seconds: 60 });
                changed = true;
                ui.close();
            }
        });
        ui.small("Variables dans les textes : {appareil}, {valeur}, {heure}, {regle}.");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Pas plus d'une exécution toutes les");
            let mut minutes = rule.cooldown_s / 60;
            if ui
                .add(
                    egui::DragValue::new(&mut minutes)
                        .range(0..=1440)
                        .suffix(" min"),
                )
                .changed()
            {
                rule.cooldown_s = minutes * 60;
                changed = true;
            }
        });

        if delete {
            self.set.rules.remove(idx);
            self.selected = self.set.rules.first().map(|r| r.id);
            self.confirm_delete = false;
            changed = true;
        }
        (changed, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Capabilities, NodeInfo};

    #[test]
    fn summary_reads_naturally() {
        let mut devices = BTreeMap::new();
        devices.insert(
            7,
            Device::new(NodeInfo {
                node_id: 7,
                name: "Porte".into(),
                caps: Capabilities {
                    contact: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        devices.insert(
            8,
            Device::new(NodeInfo {
                node_id: 8,
                name: "Lampe".into(),
                caps: Capabilities {
                    on_off: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        devices.insert(
            9,
            Device::new(NodeInfo {
                node_id: 9,
                name: "Volet".into(),
                caps: Capabilities {
                    cover: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        let mut r = Rule::new(1);
        r.trigger = Trigger::Contact {
            node_id: 7,
            open: true,
        };
        r.actions = vec![
            Action::SetOnOff {
                node_id: 8,
                command: OnOffCommand::On,
            },
            Action::Delay { seconds: 30 },
            Action::SetCover {
                node_id: 9,
                command: CoverCommand::Position(30),
            },
        ];
        assert_eq!(
            summary(&r, &devices),
            "Quand Porte s'ouvre → allumer Lampe, attendre 30 s, ouvrir à 30 % Volet"
        );
        assert_eq!(first_device(&devices, Need::Switch), 8);
        assert_eq!(first_device(&devices, Need::Cover), 9);
        assert_eq!(
            first_device(&devices, Need::Measure(MeasureKind::Position)),
            0
        );
        assert_eq!(first_device(&devices, Need::Measure(MeasureKind::Power)), 0);
        assert!(matches!(
            default_trigger(0, &devices),
            Trigger::Contact {
                node_id: 7,
                open: true
            }
        ));
    }
}
