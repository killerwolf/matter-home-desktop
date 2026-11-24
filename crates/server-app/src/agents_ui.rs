//! Le dessin de Réglages › Agents IA : l'avertissement, l'interrupteur général, « Tout couper »,
//! la liste de tous les agents (avec « Révoquer »), et les listes « jamais par un agent » et
//! « sensible » avec leurs propositions par défaut. La logique et les phrases sont dans
//! `ostal_server_app::agents`.
//!
//! La page se relit toutes les deux secondes tant qu'elle est ouverte : un agent créé depuis l'app
//! web ou révoqué en ligne de commande apparaît tout seul, et « dernier appel » avance. Les listes
//! en cours de choix ne sont **jamais écrasées** par cette relecture, et rien n'est écrit tant que
//! « Enregistrer » n'est pas cliqué (les propositions sont cochées d'avance, pas appliquées).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use ostal_server::hub::Hub;
use ostal_server::net::Auth;
use ostal_server_app::accounts;
use ostal_server_app::agents::{self, Row};
use ostal_server_app::confirm::{self, Confirmation};
use ostal_server_app::controller::Notice;
use ostal_theme as theme;

use crate::ui::widgets;

const RELOAD_EVERY: Duration = Duration::from_secs(2);

/// Les deux listes pendant qu'on les choisit.
struct ListsEdit {
    never: BTreeSet<u64>,
    sensitive: BTreeSet<u64>,
    /// Les cases viennent des propositions (d'après les noms), pas d'un choix déjà enregistré.
    from_suggestion: bool,
}

#[derive(Default)]
pub struct AgentsUi {
    open: bool,
    owners: Vec<(String, String)>,
    /// Les prises, lampes et volets de la maison : (identifiant, nom).
    devices: Vec<(u64, String)>,
    rows: Vec<Row>,
    enabled: bool,
    never: Vec<u64>,
    sensitive: Vec<u64>,
    /// Une entrée écrite à la main est illisible : tout est bloqué ou « sensible ».
    broken: bool,
    loaded_at: Option<Instant>,
    editing: Option<ListsEdit>,
    notice: Option<Notice>,
}

impl AgentsUi {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Un choix de listes est en cours : on ne le jette pas sans prévenir.
    pub fn editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn open(&mut self) {
        self.reset();
        self.open = true;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn reload(&mut self, data_dir: &Path, auth: &Auth, hub: Option<&Arc<Hub>>) {
        self.owners = accounts::list(data_dir)
            .map(|users| users.into_iter().map(|u| (u.id, u.display_name)).collect())
            .unwrap_or_default();
        self.devices = hub.map(|h| agents::controllable(h)).unwrap_or_default();
        let now = ostal_server::events::now_unix();
        self.rows = agents::rows(&auth.agents, &self.owners, &self.devices, now);
        self.enabled = auth.agents.enabled();
        (self.never, self.sensitive) = auth.agents.lists();
        self.broken = auth.agents.lists_are_broken();
        self.loaded_at = Some(Instant::now());
    }

    fn name_of(&self, id: u64) -> String {
        self.devices
            .iter()
            .find(|(i, _)| *i == id)
            .map_or_else(|| format!("appareil retiré ({id})"), |(_, n)| n.clone())
    }

    fn names(&self, ids: &[u64]) -> String {
        if ids.is_empty() {
            return "aucun appareil".into();
        }
        ids.iter()
            .map(|id| self.name_of(*id))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Dessine la page. Rend `true` si la personne veut revenir aux Réglages.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        data_dir: &Path,
        auth: Option<&Arc<Auth>>,
        hub: Option<&Arc<Hub>>,
    ) -> bool {
        let p = theme::palette(ui);
        let mut leave = false;
        ui.horizontal(|ui| {
            leave = ui.link("‹ Réglages").clicked();
            ui.label(RichText::new("›").color(p.text_secondary));
            ui.label(RichText::new("Agents IA").color(p.text_secondary));
        });
        ui.add_space(4.0);
        ui.heading("Agents IA");
        let Some(auth) = auth else {
            ui.label("Le serveur ne tourne pas : les agents se règlent quand il tourne.");
            return leave;
        };
        if self.loaded_at.is_none_or(|at| at.elapsed() > RELOAD_EVERY) {
            self.reload(data_dir, auth, hub);
        }
        ctx.request_repaint_after(RELOAD_EVERY);

        ui.label("Un agent d'IA (Claude Code, Cursor…) lit la maison par un jeton, et la commande si vous le lui permettez. Chaque personne crée ses agents dans l'app web, « Mes agents » ; ici on décide s'ils existent, on les voit tous et on les coupe.");
        ui.add_space(8.0);

        // L'avertissement : ce qui part, et l'agent sur ce Mac.
        widgets::card(ui, |ui| {
            ui.label(RichText::new("À savoir avant d'allumer").strong());
            ui.add(egui::Label::new(agents::PROVIDER_WARNING).wrap());
            ui.add_space(4.0);
            ui.add(egui::Label::new(agents::SAME_MAC_WARNING).wrap());
        });
        ui.add_space(8.0);

        self.switch_card(ui, data_dir, auth);
        ui.add_space(8.0);
        self.lists_card(ui, data_dir);
        ui.add_space(8.0);
        self.agents_card(ui, data_dir);
        if let Some(notice) = &self.notice {
            ui.add_space(6.0);
            match notice {
                Notice::Ok(t) => ui.colored_label(p.ok, t),
                Notice::Err(t) => ui.colored_label(p.error, t),
            };
        }
        ui.add_space(6.0);
        widgets::hint(
            ui,
            "Ce que font les agents se lit dans l'onglet Activité, filtre « Agents d'IA ».",
        );
        leave
    }

    fn switch_card(&mut self, ui: &mut egui::Ui, data_dir: &Path, auth: &Arc<Auth>) {
        widgets::section(ui, "Accès des agents", |ui| {
            let mut on = self.enabled;
            if ui.checkbox(&mut on, "Autoriser les agents IA").changed() {
                self.notice = match agents::set_switch(data_dir, on, Some(&auth.confirmations)) {
                    Ok(_) => None,
                    Err(e) => Some(Notice::Err(e)),
                };
                self.enabled = auth.agents.enabled();
                self.loaded_at = None;
                // On allume : si les listes n'ont jamais été choisies, on les propose tout de suite.
                if on && self.never.is_empty() && self.sensitive.is_empty() {
                    self.start_lists_edit();
                }
            }
            widgets::hint(ui, agents::switch_text(self.enabled));
            ui.add_space(6.0);
            let count = self.rows.len();
            ui.horizontal(|ui| {
                if theme::destructive_button(ui, "Tout couper").clicked()
                    && confirm::ask(&Confirmation::CutAllAgents { count })
                {
                    self.notice =
                        Some(match agents::cut_all(data_dir, Some(&auth.confirmations)) {
                            Ok(n) => Notice::Ok(agents::cut_notice(n)),
                            Err(e) => Notice::Err(e),
                        });
                    self.editing = None;
                    self.loaded_at = None;
                }
                widgets::hint(
                    ui,
                    "Éteint l'accès, révoque tous les agents et oublie leurs demandes en attente.",
                );
            });
        });
    }

    fn start_lists_edit(&mut self) {
        let empty = self.never.is_empty() && self.sensitive.is_empty();
        let (never, sensitive) = if empty {
            agents::suggestion(&self.devices)
        } else {
            (self.never.clone(), self.sensitive.clone())
        };
        self.editing = Some(ListsEdit {
            never: never.into_iter().collect(),
            sensitive: sensitive.into_iter().collect(),
            from_suggestion: empty,
        });
    }

    fn lists_card(&mut self, ui: &mut egui::Ui, data_dir: &Path) {
        widgets::section(ui, "Ce que les agents ne touchent pas", |ui| {
            if self.broken {
                ui.colored_label(
                    theme::palette(ui).error,
                    "Une des listes a une entrée illisible (écrite à la main). Par prudence, aucune commande d'agent n'est permise et chaque commande demande votre accord. Enregistrez les listes ci-dessous pour réparer.",
                );
            }
            let Some(edit) = &mut self.editing else {
                ui.label(RichText::new("Jamais par un agent").strong());
                widgets::hint(ui, "Aucun agent ne les commande, même avec le droit de commander : congélateur, box, prise du Mac du serveur (la couper supprimerait aussi le journal).");
                ui.add(egui::Label::new(self.names(&self.never)).wrap());
                ui.add_space(6.0);
                ui.label(RichText::new("Sensibles").strong());
                widgets::hint(ui, "Chaque commande d'un agent attend votre accord dans l'app : porte de garage, baie vitrée.");
                ui.add(egui::Label::new(self.names(&self.sensitive)).wrap());
                if self.never.is_empty() && self.sensitive.is_empty() && !self.devices.is_empty() {
                    ui.add_space(6.0);
                    widgets::hint(ui, "Les deux listes sont vides. Choisissez-les avant de donner à un agent le droit de commander.");
                }
                ui.add_space(6.0);
                if theme::secondary_button(ui, "Régler les listes…").clicked() {
                    self.start_lists_edit();
                }
                return;
            };
            if edit.from_suggestion {
                widgets::hint(ui, "Propositions d'après le nom de vos appareils (congélateur, box, Mac du serveur ; porte de garage, baie vitrée). Vérifiez, corrigez, puis Enregistrer : rien n'est écrit avant.");
            }
            let mut ids: Vec<u64> = self.devices.iter().map(|(id, _)| *id).collect();
            // Un appareil retiré qui figure encore dans une liste reste visible, pour pouvoir l'ôter.
            for id in edit.never.iter().chain(edit.sensitive.iter()) {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
            if ids.is_empty() {
                widgets::hint(ui, "Aucune prise, lampe ou volet à classer pour l'instant.");
            } else {
                egui::Grid::new("agent_lists")
                    .num_columns(3)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Appareil").strong().small());
                        ui.label(RichText::new("Jamais").strong().small());
                        ui.label(RichText::new("Sensible").strong().small());
                        ui.end_row();
                        for id in ids {
                            let name = self.devices.iter().find(|(i, _)| *i == id).map_or_else(
                                || format!("appareil retiré ({id})"),
                                |(_, n)| n.clone(),
                            );
                            ui.label(name);
                            let mut never = edit.never.contains(&id);
                            if ui.checkbox(&mut never, "").changed() {
                                if never {
                                    edit.never.insert(id);
                                    edit.sensitive.remove(&id);
                                } else {
                                    edit.never.remove(&id);
                                }
                            }
                            let mut sensitive = edit.sensitive.contains(&id);
                            if ui
                                .add_enabled(
                                    !edit.never.contains(&id),
                                    egui::Checkbox::new(&mut sensitive, ""),
                                )
                                .changed()
                            {
                                if sensitive {
                                    edit.sensitive.insert(id);
                                } else {
                                    edit.sensitive.remove(&id);
                                }
                            }
                            ui.end_row();
                        }
                    });
            }
            let (mut save, mut cancel, mut propose) = (false, false, false);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Annuler").clicked();
                propose = ui.button("Cocher les propositions").clicked();
                save = theme::primary_button(ui, "Enregistrer").clicked();
            });
            if propose {
                let (never, sensitive) = agents::suggestion(&self.devices);
                edit.never = never.into_iter().collect();
                edit.sensitive = sensitive.into_iter().collect();
                edit.from_suggestion = true;
            }
            if cancel {
                self.editing = None;
            } else if save {
                let never: Vec<u64> = edit.never.iter().copied().collect();
                let sensitive: Vec<u64> = edit.sensitive.iter().copied().collect();
                match agents::save_lists(data_dir, &never, &sensitive) {
                    Ok(_) => {
                        self.notice = Some(Notice::Ok("Listes enregistrées.".into()));
                        self.editing = None;
                        self.loaded_at = None;
                    }
                    Err(e) => self.notice = Some(Notice::Err(e)),
                }
            }
        });
    }

    fn agents_card(&mut self, ui: &mut egui::Ui, data_dir: &Path) {
        let p = theme::palette(ui);
        let mut revoke: Option<(String, String)> = None;
        widgets::section(ui, "Tous les agents", |ui| {
            if self.rows.is_empty() {
                ui.label("Aucun agent. Une personne en crée un depuis l'app web, « Mes agents ».");
                return;
            }
            for (i, row) in self.rows.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&row.label).strong());
                            if row.expired {
                                theme::state_word(ui, theme::StateWord::Neutral, "expiré");
                            }
                        });
                        ui.label(
                            RichText::new(format!("{} · {}", row.owner, row.rights))
                                .small()
                                .color(p.text_secondary),
                        );
                        if !row.devices.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&row.devices).small().color(p.text_secondary),
                                )
                                .wrap(),
                            );
                        }
                        ui.label(
                            RichText::new(format!("{} · {}", row.last_use, row.expires))
                                .small()
                                .color(p.text_secondary),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::secondary_button(ui, "Révoquer").clicked() {
                            revoke = Some((row.id.clone(), row.label.clone()));
                        }
                    });
                });
            }
            ui.add_space(4.0);
            widgets::hint(
                ui,
                "Le dernier appel et les appels du jour comptent depuis le démarrage du serveur.",
            );
        });
        if let Some((id, label)) = revoke {
            if confirm::ask(&Confirmation::RevokeAgent { label }) {
                self.notice = Some(match agents::revoke(data_dir, &id) {
                    Ok(Some(a)) => Notice::Ok(format!("Agent « {} » révoqué.", a.label)),
                    Ok(None) => Notice::Ok("Cet agent n'existe plus.".into()),
                    Err(e) => Notice::Err(e),
                });
                self.loaded_at = None;
            }
        }
    }
}
