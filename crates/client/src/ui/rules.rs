//! Onglet « Automatisations » : liste des règles, éditeur « Quand / Si / Alors », journal.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use eframe::egui::{self, Color32, RichText};

use super::plan::{glyph, icon_of};
use super::sync::{Failure, Incoming, SyncState};
use super::theme;
use super::{ago, Device};
use protocol::automation::{
    is_webhook_url, Action, Condition, CoverCommand, OnOffCommand, Rule, RuleSet, Trigger,
};
use protocol::events::{now_unix, MeasureKind};
use protocol::wire::RuleRunView;

const DAYS: [&str; 7] = ["L", "M", "M", "J", "V", "S", "D"];
const MEASURES: [MeasureKind; 5] = [
    MeasureKind::Temperature,
    MeasureKind::Humidity,
    MeasureKind::Power,
    MeasureKind::Battery,
    MeasureKind::Position,
];

pub enum RulesAction {
    None,
    Test(u64),
}

pub struct RulesView {
    set: RuleSet,
    selected: Option<u64>,
    confirm_delete: bool,
    /// L'historique des exécutions est ouvert (depuis la barre d'état).
    history_open: bool,
    /// Rectangle du premier interrupteur de la liste (tests : où cliquer).
    #[cfg(test)]
    first_switch: egui::Rect,
    /// Suivi de la synchronisation avec le serveur (dernière modification gagnante).
    sync: SyncState<RuleSet>,
}

/// Ce qui empêche d'envoyer ces règles : le serveur refuse (400) une action « Appeler une adresse web »
/// dont l'adresse n'est pas une adresse http(s) sans espace. Tant qu'elle est vide ou à moitié
/// saisie, l'ensemble reste un brouillon visible, et rien ne part (ni ne revient en arrière).
///
/// Une règle **désactivée** peut garder une adresse incomplète (le serveur l'accepte, c'est ce que
/// l'import d'une sauvegarde de la v0.2.1 produit) : elle ne bloque pas l'envoi des autres.
fn rules_problem(set: &RuleSet) -> Option<String> {
    set.rules
        .iter()
        .filter(|rule| rule.enabled)
        .find_map(|rule| {
            rule.actions.iter().find_map(|action| match action {
                Action::Webhook { url, .. } if !is_webhook_url(url) => Some(format!(
                    "l'action « Appeler une adresse web » de la règle « {} » n'a pas d'adresse valable \
                 (http:// ou https://, sans espace)",
                    rule.name
                )),
                _ => None,
            })
        })
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

/// « 22:00 », « 7:05 » : une heure et des minutes valides, sinon `None`.
pub fn parse_hhmm(text: &str) -> Option<(u8, u8)> {
    let (h, m) = text.trim().split_once(':')?;
    let digits = |t: &str| (1..=2).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || !digits(m) {
        return None;
    }
    let (h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// Un seul champ « hh:mm ». Tant qu'on écrit, le texte est gardé tel quel ; une valeur invalide
/// est entourée en rouge et ne change rien à la règle ; en quittant le champ, le texte reprend
/// la valeur de la règle.
fn hhmm(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    h: &mut u8,
    m: &mut u8,
) -> bool {
    let id = ui.make_persistent_id(salt);
    let editing = ui.memory(|mem| mem.has_focus(id));
    let mut text: String = if editing {
        ui.data_mut(|d| d.get_temp(id))
            .unwrap_or_else(|| format!("{h:02}:{m:02}"))
    } else {
        format!("{h:02}:{m:02}")
    };
    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .hint_text("hh:mm")
            .desired_width(52.0),
    );
    let parsed = parse_hhmm(&text);
    let mut changed = false;
    match parsed {
        Some((nh, nm)) => {
            if (nh, nm) != (*h, *m) {
                *h = nh;
                *m = nm;
                changed = true;
            }
        }
        None => {
            ui.painter().rect_stroke(
                response.rect,
                theme::tokens().radius.sm,
                egui::Stroke::new(1.5, theme::palette(ui).error),
                egui::StrokeKind::Outside,
            );
            response.on_hover_text("Heure invalide : écrivez hh:mm, par exemple 22:00.");
        }
    }
    ui.data_mut(|d| d.insert_temp(id, text));
    changed
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
            Action::Notify { .. } => "envoyer une notification".to_string(),
            Action::SetOnOff { node_id, command } => {
                format!("{} {}", command.label().to_lowercase(), name(*node_id))
            }
            Action::SetCover { node_id, command } => {
                format!("{} {}", command.label().to_lowercase(), name(*node_id))
            }
            Action::Webhook { .. } => "appeler une adresse web".to_string(),
            Action::Delay { seconds } => format!("attendre {seconds} s"),
        })
        .collect();
    let conditions: Vec<String> = rule
        .conditions
        .iter()
        .map(|c| match c {
            Condition::TimeBetween { from, to } => format!(
                "il est entre {:02}:{:02} et {:02}:{:02}",
                from.0, from.1, to.0, to.1
            ),
            Condition::Contact { node_id, open } => format!(
                "{} est {}",
                name(*node_id),
                super::devices::open_label(*open).to_lowercase()
            ),
            Condition::Switch { node_id, on } => format!(
                "{} est {}",
                name(*node_id),
                if *on { "allumée" } else { "éteinte" }
            ),
            Condition::Threshold {
                node_id,
                measure,
                above,
                value,
            } => format!(
                "{} : {} {} {value}",
                name(*node_id),
                measure_label(*measure),
                if *above { ">" } else { "<" }
            ),
        })
        .collect();
    // Une phrase : « Quand Porte s'ouvre, seulement si il est entre 22:00 et 07:00, allumer
    // Lampe. » ; une règle à heure fixe commence par l'heure : « À 23:00, éteindre Prise. »
    let lead = match &rule.trigger {
        Trigger::Time { .. } => when.replace("à ", "À "),
        _ => format!("Quand {when}"),
    };
    let only = if conditions.is_empty() {
        String::new()
    } else {
        format!(", seulement si {}", conditions.join(" et "))
    };
    format!(
        "{lead}{only}, {}.",
        if then.is_empty() {
            "ne rien faire".into()
        } else {
            then.join(", ")
        }
    )
}

impl RulesView {
    /// Aucune règle : les vraies viennent du serveur (état complet à la connexion).
    pub fn new() -> Self {
        Self {
            set: RuleSet::default(),
            selected: None,
            confirm_delete: false,
            history_open: false,
            #[cfg(test)]
            first_switch: egui::Rect::NOTHING,
            sync: SyncState::with_check(rules_problem),
        }
    }

    /// Simule « + Nouvelle règle » puis une action « Appeler une adresse web » (tests).
    #[cfg(test)]
    pub fn add_webhook_rule(&mut self, url: &str) {
        let id = self.set.next_id();
        let mut rule = Rule::new(id);
        rule.actions.push(Action::Webhook {
            url: url.into(),
            body: String::new(),
        });
        self.set.rules.push(rule);
        self.sync.mark_changed(Instant::now());
    }

    /// Simule la saisie de l'adresse du webhook de la dernière règle (tests).
    #[cfg(test)]
    pub fn set_webhook_url(&mut self, url: &str) {
        if let Some(Action::Webhook { url: u, .. }) =
            self.set.rules.last_mut().and_then(|r| r.actions.last_mut())
        {
            *u = url.into();
        }
        self.sync.mark_changed(Instant::now());
    }

    #[cfg(test)]
    pub fn rule_count(&self) -> usize {
        self.set.rules.len()
    }

    /// Une modification locale n'est pas encore arrivée au serveur.
    pub fn editing(&self) -> bool {
        self.sync.editing()
    }

    /// Déconnexion : les règles affichées ne sont plus celles du serveur, on les oublie.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Règles reçues du serveur (état complet ou `rules_changed`). Une édition en cours n'est
    /// jamais écrasée : voir `SyncState`.
    pub fn on_server(&mut self, incoming: RuleSet) {
        if self.sync.on_server(incoming, &mut self.set) == Incoming::Adopted {
            self.fix_selection();
        }
    }

    fn fix_selection(&mut self) {
        self.confirm_delete = false;
        if !self
            .selected
            .is_some_and(|id| self.set.rules.iter().any(|r| r.id == id))
        {
            self.selected = self.set.rules.first().map(|r| r.id);
        }
    }

    /// Modification à envoyer maintenant, s'il y en a une. `holding` : le pointeur est
    /// enfoncé (glisser en cours), on n'envoie rien avant le relâchement.
    pub fn poll_save(&self, now: Instant, holding: bool) -> Option<RuleSet> {
        self.sync.due(now, holding, &self.set)
    }

    /// Modification à envoyer sans attendre le délai de calme (fermeture de la fenêtre).
    pub fn poll_save_now(&self) -> Option<RuleSet> {
        self.sync.due_now(&self.set)
    }

    pub fn sent(&mut self, request_id: u64, content: RuleSet) {
        self.sync.sent(request_id, content);
    }

    pub fn saved(&mut self, request_id: u64, stored: Option<RuleSet>) {
        self.sync.on_saved(request_id, stored, &mut self.set);
    }

    /// Envoi échoué. `true` si c'était un envoi des règles. Selon la panne, le brouillon reste
    /// (refus du contenu, panne passagère) ou les règles reviennent à la version du serveur.
    pub fn failed(&mut self, request_id: u64, failure: Failure, now: Instant) -> bool {
        let was_ours = self.sync.on_failed(request_id, failure, now, &mut self.set);
        if was_ours {
            self.fix_selection();
        }
        was_ours
    }

    /// La liaison est coupée : un brouillon est gardé pour la reprise, sinon tout est oublié.
    pub fn suspend(&mut self, now: Instant) {
        if !self.sync.suspend(now) {
            self.reset();
        }
    }

    /// Un envoi est à faire ou en vol (réveil de l'interface).
    pub fn needs_tick(&self) -> bool {
        self.sync.needs_tick(&self.set)
    }

    /// Ce qu'il faut dire de règles qui ne sont pas (encore) parties.
    pub fn notice(&self) -> Option<String> {
        self.sync.notice(&self.set)
    }

    /// Pourquoi les règles affichées ne peuvent pas être enregistrées telles quelles.
    pub fn blocked(&self) -> Option<String> {
        self.sync.blocked(&self.set)
    }

    /// Enregistrer sans attendre le délai de calme (avant de tester une règle).
    pub fn save_asap(&mut self, now: Instant) {
        self.sync.save_asap(now);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        devices: &BTreeMap<u64, Device>,
        runs: &VecDeque<RuleRunView>,
    ) -> RulesAction {
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
            // Une ligne discrète, au lieu d'un paragraphe sous le titre.
            ui.label(
                RichText::new("Les règles s'exécutent sur le serveur, tant qu'il tourne.")
                    .small()
                    .color(theme::palette(ui).text_secondary),
            );
        });
        self.remote_banner(ui);
        self.draft_banner(ui);
        ui.separator();

        // Barre d'état : la dernière exécution et un bouton qui ouvre l'historique.
        let history_open = &mut self.history_open;
        egui::Panel::bottom("rules_status")
            .resizable(false)
            .show(ui, |ui| {
                let p = theme::palette(ui);
                ui.horizontal(|ui| {
                    let now = now_unix();
                    match runs.back() {
                        Some(r) => {
                            let color = if r.ok { p.ok } else { p.error };
                            ui.colored_label(color, "●");
                            ui.label(
                                RichText::new(format!(
                                    "Dernière exécution : {} ({})",
                                    r.name,
                                    ago(now - r.at)
                                ))
                                .small()
                                .color(p.text_secondary),
                            );
                        }
                        None => {
                            ui.label(
                                RichText::new("Aucune exécution depuis le démarrage du serveur.")
                                    .small()
                                    .color(p.text_secondary),
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if *history_open {
                            "Historique ▾"
                        } else {
                            "Historique ▸"
                        };
                        if ui.small_button(text).clicked() {
                            *history_open = !*history_open;
                        }
                    });
                });
                if *history_open {
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let now = now_unix();
                            if runs.is_empty() {
                                ui.small("Aucune exécution depuis le démarrage du serveur.");
                            }
                            for r in runs.iter().rev() {
                                let color = if r.ok { p.ok } else { p.error };
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
                }
            });

        egui::Panel::left("rules_list")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.set.rules.is_empty() {
                        ui.label("Aucune règle. Cliquez sur « + Nouvelle règle ».");
                    }
                    for (row, rule) in self.set.rules.iter_mut().enumerate() {
                        let _ = row; // (utilisé seulement par les tests)
                        let selected = self.selected == Some(rule.id);
                        let p = theme::palette(ui);
                        let fill = if selected {
                            theme::tokens().selected(p)
                        } else {
                            Color32::TRANSPARENT
                        };
                        // Le clic de la ligne est posé AVANT son contenu (`sense` du scope) : un
                        // clic sur l'interrupteur, posé après, l'emporte sur lui.
                        let row_scope = ui.scope_builder(
                            egui::UiBuilder::new().sense(egui::Sense::click()),
                            |ui| {
                                egui::Frame::new()
                                    .fill(fill)
                                    .corner_radius(theme::tokens().radius.md)
                                    .inner_margin(egui::Margin::same(
                                        theme::tokens().space.sm as i8,
                                    ))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let switch = theme::toggle_switch(
                                                ui,
                                                &mut rule.enabled,
                                                "Activée",
                                            )
                                            .on_hover_text("Activée");
                                            #[cfg(test)]
                                            if row == 0 {
                                                self.first_switch = switch.rect;
                                            }
                                            changed |= switch.changed();
                                            let name_color = if rule.enabled {
                                                p.text
                                            } else {
                                                p.text_secondary
                                            };
                                            ui.label(
                                                RichText::new(&rule.name)
                                                    .strong()
                                                    .color(name_color),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if let Some(last) = runs
                                                        .iter()
                                                        .rev()
                                                        .find(|r| r.rule_id == rule.id)
                                                    {
                                                        // Une réussite reste discrète ; un échec le dit avec le mot.
                                                        let (text, color) = if last.ok {
                                                            (
                                                                ago(now_unix() - last.at),
                                                                p.text_secondary,
                                                            )
                                                        } else {
                                                            (
                                                                format!(
                                                                    "échec {}",
                                                                    ago(now_unix() - last.at)
                                                                ),
                                                                p.warn,
                                                            )
                                                        };
                                                        ui.label(
                                                            RichText::new(text)
                                                                .small()
                                                                .color(color),
                                                        );
                                                    }
                                                },
                                            );
                                        });
                                        ui.label(
                                            RichText::new(summary(rule, devices))
                                                .small()
                                                .color(p.text_secondary),
                                        );
                                    })
                            },
                        );
                        if row_scope.response.clicked() {
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
            self.sync.mark_changed(Instant::now());
        }
        action
    }

    /// Règles non envoyées (brouillon incomplet, refus du serveur, panne passagère), avec de quoi
    /// en sortir : revenir à la version du serveur.
    fn draft_banner(&mut self, ui: &mut egui::Ui) {
        let Some(text) = self.notice() else { return };
        let mut revert = false;
        theme::banner_frame(ui, theme::Banner::Warn).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(text);
                revert = ui
                    .add_enabled(
                        self.sync.can_revert(),
                        egui::Button::new("Revenir à la version du serveur"),
                    )
                    .on_hover_text("Abandonne vos modifications non enregistrées des règles.")
                    .on_disabled_hover_text("Un envoi est en cours : réessayez dans un instant.")
                    .clicked();
            });
        });
        if revert {
            self.revert_to_server();
        }
    }

    /// Abandonne le brouillon : les règles affichées redeviennent celles du serveur. `false` si
    /// c'est impossible pour l'instant (envoi en cours, liaison coupée).
    pub fn revert_to_server(&mut self) -> bool {
        if !self.sync.revert(&mut self.set) {
            return false;
        }
        self.fix_selection();
        true
    }

    /// Un autre client a modifié les règles pendant l'édition : un choix attend.
    pub fn conflict_pending(&self) -> bool {
        self.sync.remote().is_some()
    }

    /// Un autre client a modifié les règles pendant que nous les éditions.
    fn remote_banner(&mut self, ui: &mut egui::Ui) {
        if self.sync.remote().is_none() {
            return;
        }
        theme::banner_frame(ui, theme::Banner::Info).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Les règles ont été modifiées par un autre client pendant votre édition.");
                if ui.button("Afficher sa version").clicked() {
                    self.sync.accept_remote(&mut self.set);
                    self.fix_selection();
                }
                if ui.button("Garder la mienne").clicked() {
                    self.sync.keep_mine(Instant::now());
                }
            });
        });
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
            changed |= theme::toggle_switch(ui, &mut rule.enabled, "Activée").changed();
            ui.label("Activée");
            if ui
                .button("Essayer maintenant")
                .on_hover_text(
                    "Exécute les actions tout de suite, sans vérifier « Quand » ni « Seulement si ».",
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
                    .button(RichText::new("Oui, supprimer").color(theme::palette(ui).error))
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
                changed |= hhmm(ui, ("t_time", rid), hour, minute);
                for (i, d) in DAYS.iter().enumerate() {
                    changed |= ui.toggle_value(&mut days[i], *d).changed();
                }
            }
        });
        ui.add_space(6.0);

        // SI
        ui.heading("Seulement si");
        let mut remove_cond = None;
        for (i, cond) in rule.conditions.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                match cond {
                    Condition::TimeBetween { from, to } => {
                        ui.label("entre");
                        changed |= hhmm(ui, ("c_from", rid, i), &mut from.0, &mut from.1);
                        ui.label("et");
                        changed |= hhmm(ui, ("c_to", rid, i), &mut to.0, &mut to.1);
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
                            ui.label("Appeler une adresse web :");
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
            if ui.button("Appeler une adresse web").clicked() {
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
    use protocol::events::Capabilities;
    use protocol::wire::WireNode;

    /// Un clic sur l'interrupteur de la liste bascule la règle, et ne la sélectionne pas : le clic
    /// de la ligne, posé après celui de l'interrupteur, ne doit pas l'emporter.
    #[test]
    fn clicking_the_switch_in_the_list_toggles_the_rule() {
        use eframe::egui::{Context, Event, PointerButton, Pos2, RawInput, Rect};
        let ctx = Context::default();
        let mut view = RulesView::new();
        let mut rule = Rule::new(1);
        rule.enabled = false;
        view.set.rules.push(rule);
        let devices = BTreeMap::new();
        let runs = VecDeque::new();
        let frame = |view: &mut RulesView, events: Vec<Event>| {
            let mut input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 600.0))),
                ..Default::default()
            };
            input.events = events;
            let mut out = ctx.run_ui(input, |ui| {
                view.show(ui, &devices, &runs);
            });
            out.textures_delta.clear();
        };
        frame(&mut view, vec![]);
        frame(&mut view, vec![]);
        let at = view.first_switch.center();
        assert!(view.first_switch != Rect::NOTHING, "interrupteur dessiné");
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(&mut view, vec![Event::PointerMoved(at)]);
        frame(&mut view, vec![button(true)]);
        frame(&mut view, vec![button(false)]);
        frame(&mut view, vec![]);
        assert!(view.set.rules[0].enabled, "la règle s'active");
        assert_eq!(view.selected, None, "et n'est pas sélectionnée");
        // Un clic ailleurs sur la ligne la sélectionne toujours.
        let elsewhere = at + egui::vec2(140.0, 0.0);
        let press = |pressed| Event::PointerButton {
            pos: elsewhere,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(&mut view, vec![Event::PointerMoved(elsewhere)]);
        frame(&mut view, vec![press(true)]);
        frame(&mut view, vec![press(false)]);
        frame(&mut view, vec![]);
        assert_eq!(view.selected, Some(1));
        assert!(view.set.rules[0].enabled, "sans la basculer");
    }

    #[test]
    fn a_time_is_one_valid_hh_mm_field() {
        assert_eq!(parse_hhmm("22:00"), Some((22, 0)));
        assert_eq!(parse_hhmm("7:05"), Some((7, 5)));
        assert_eq!(parse_hhmm(" 07:5 "), Some((7, 5)));
        for bad in [
            "", "24:00", "12:60", "12", "12:", ":30", "ab:cd", "1:2:3", "123:00",
        ] {
            assert_eq!(parse_hhmm(bad), None, "{bad:?}");
        }
    }

    fn webhook_rule(id: u64, enabled: bool, url: &str) -> Rule {
        let mut rule = Rule::new(id);
        rule.enabled = enabled;
        rule.actions.push(Action::Webhook {
            url: url.into(),
            body: String::new(),
        });
        rule
    }

    /// Un brouillon de webhook dans une règle désactivée (ce que produit l'import d'une
    /// sauvegarde de la v0.2.1) n'empêche pas d'enregistrer les autres règles ; activé, il
    /// bloque comme avant.
    #[test]
    fn a_disabled_rule_may_keep_an_incomplete_webhook() {
        let mut set = RuleSet::default();
        set.rules
            .push(webhook_rule(1, true, "https://exemple.fr/ok"));
        set.rules.push(webhook_rule(2, false, ""));
        set.rules
            .push(webhook_rule(3, false, "exemple.fr/sans-http"));
        set.rules.push(webhook_rule(4, false, "http://a b"));
        assert_eq!(rules_problem(&set), None);
        set.rules[1].enabled = true;
        let why = rules_problem(&set).expect("une règle activée doit être complète");
        assert!(why.contains(&set.rules[1].name), "{why}");
    }

    #[test]
    fn summary_reads_naturally() {
        let mut devices = BTreeMap::new();
        devices.insert(
            7,
            Device::new(WireNode {
                node_id: 7,
                name: "Porte".into(),
                device_name: "Porte".into(),
                custom_name: None,
                icon: None,
                vendor: None,
                product: None,
                device_types: vec![],
                caps: Capabilities {
                    contact: true,
                    ..Default::default()
                },
                bridge: None,
            }),
        );
        devices.insert(
            8,
            Device::new(WireNode {
                node_id: 8,
                name: "Lampe".into(),
                device_name: "Lampe".into(),
                custom_name: None,
                icon: None,
                vendor: None,
                product: None,
                device_types: vec![],
                caps: Capabilities {
                    on_off: true,
                    ..Default::default()
                },
                bridge: None,
            }),
        );
        devices.insert(
            9,
            Device::new(WireNode {
                node_id: 9,
                name: "Volet".into(),
                device_name: "Volet".into(),
                custom_name: None,
                icon: None,
                vendor: None,
                product: None,
                device_types: vec![],
                caps: Capabilities {
                    cover: true,
                    ..Default::default()
                },
                bridge: None,
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
        // Une règle à heure fixe commence par l'heure ; les conditions sont dans la phrase.
        let mut timed = Rule::new(2);
        timed.trigger = Trigger::Time {
            hour: 23,
            minute: 0,
            days: [true; 7],
        };
        timed.actions = vec![Action::SetOnOff {
            node_id: 8,
            command: OnOffCommand::Off,
        }];
        assert_eq!(summary(&timed, &devices), "À 23:00, éteindre Lampe.");
        timed.conditions = vec![
            Condition::Contact {
                node_id: 7,
                open: false,
            },
            Condition::TimeBetween {
                from: (22, 0),
                to: (7, 0),
            },
        ];
        assert_eq!(
            summary(&timed, &devices),
            "À 23:00, seulement si Porte est fermée et il est entre 22:00 et 07:00, éteindre Lampe."
        );
        assert_eq!(
            summary(&r, &devices),
            "Quand Porte s'ouvre, allumer Lampe, attendre 30 s, ouvrir à 30 % Volet."
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
