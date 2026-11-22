//! Mes connexions (maquette Bureau-Mes-Connexions) : les téléphones et ordinateurs où le compte
//! est ouvert, lus sur `GET /api/me/installations`, avec « Couper » (`DELETE`). La connexion de
//! cette app porte « Cette app », sans bouton. Tout ce qui est « quoi dire » est pur et testé.
//!
//! La page n'est pas un onglet de la maison : on y va par Fichier › « Mes connexions… », et la barre
//! d'onglets devient « ‹ Maison · Mes connexions » tant qu'on y est.

use eframe::egui::{self, RichText};
use protocol::wire::{
    connection_kind_label, last_seen_label, via_label, ConnectionView, ConnectionsResponse,
};

use ostal_client::net::discovery::{Finder, Found};

use super::theme;

/// Comment ce Mac joint la maison, pour la carte « Comment ce Mac joint la maison ».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Route {
    /// L'adresse principale (celle de la maison), gardée dans le fichier de connexion.
    pub primary: Option<String>,
    /// Les adresses Tailscale gardées (IP du tailnet ou nom `….ts.net`).
    pub tailscale: Vec<String>,
    /// `Some(true)` : la session passe en ce moment par une adresse à distance.
    pub via_remote: Option<bool>,
}

/// Où en est la liste.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// Pas encore demandée (ou à redemander).
    Idle,
    Loading,
    Ready(Vec<ConnectionView>),
    Failed(String),
}

/// Ce que la page demande à l'application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Relire la liste.
    Reload,
    /// Couper la connexion `id` (déjà confirmée).
    Cut {
        id: String,
        name: String,
    },
}

pub struct Page {
    pub load: Load,
    /// La connexion dont « Couper » attend une confirmation : (id, nom).
    confirming: Option<(String, String)>,
    /// Un message sous la liste (« Connexion coupée. »).
    pub notice: Option<(bool, String)>,
    /// « Détails » de la carte « Comment ce Mac joint la maison » est ouvert.
    details: bool,
    /// « Chercher à nouveau » : la recherche Bonjour lancée depuis la carte.
    finder: Option<Finder>,
}

impl Page {
    pub fn new() -> Self {
        Self {
            load: Load::Idle,
            confirming: None,
            notice: None,
            details: false,
            finder: None,
        }
    }

    /// La réponse du serveur à la liste.
    pub fn loaded(&mut self, result: Result<ConnectionsResponse, String>) {
        self.load = match result {
            Ok(r) => Load::Ready(sorted(r.connections)),
            Err(e) => Load::Failed(e),
        };
    }

    /// Une phrase sous la liste après une demande.
    pub fn say(&mut self, ok: bool, text: String) {
        self.notice = Some((ok, text));
    }

    /// Dessine la page : la liste, puis la note sur « Couper ».
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        now: f64,
        utc_offset_s: i64,
        route: &Route,
    ) -> Action {
        let p = *theme::palette(ui);
        let mut action = Action::None;
        if self.load == Load::Idle {
            action = Action::Reload;
        }
        theme::card_frame(ui)
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Mes connexions")
                        .size(theme::tokens().font.size.heading)
                        .strong(),
                );
                ui.label(
                    RichText::new("Les téléphones et ordinateurs où votre compte est ouvert. Couper ferme la connexion tout de suite.")
                        .color(p.text_secondary),
                );
                ui.add_space(10.0);
                match &self.load {
                    Load::Idle | Load::Loading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Chargement…");
                        });
                    }
                    Load::Failed(text) => {
                        ui.colored_label(p.error, text);
                        if theme::secondary_button(ui, "Réessayer").clicked() {
                            action = Action::Reload;
                        }
                    }
                    Load::Ready(list) if list.is_empty() => {
                        ui.label(
                            RichText::new("Aucune connexion ouverte pour l'instant.")
                                .color(p.text_secondary),
                        );
                    }
                    Load::Ready(list) => {
                        let list = list.clone();
                        self.table(ui, &list, now, utc_offset_s, &mut action);
                    }
                }
                if let Some((ok, text)) = &self.notice {
                    ui.add_space(8.0);
                    ui.colored_label(if *ok { p.ok } else { p.error }, text);
                }
            });
        ui.add_space(12.0);
        self.route_card(ui, route);
        action
    }

    /// « Comment ce Mac joint la maison » : le réseau de la maison, Tailscale, et en « Détails »
    /// les adresses et « Chercher à nouveau ».
    fn route_card(&mut self, ui: &mut egui::Ui, route: &Route) {
        let p = *theme::palette(ui);
        theme::card_frame(ui)
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Comment ce Mac joint la maison")
                        .size(theme::tokens().font.size.heading)
                        .strong(),
                );
                ui.label(
                    RichText::new("Ostal Server se trouve tout seul sur le réseau de la maison. L'adresse Tailscale ne sert que hors de chez vous.")
                        .color(p.text_secondary),
                );
                ui.add_space(8.0);
                let at_home = route.via_remote == Some(false);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        theme::state_word(
                            ui,
                            if at_home { theme::StateWord::Ok } else { theme::StateWord::Neutral },
                            if at_home { "joignable" } else { "pas en ce moment" },
                        );
                        ui.label(RichText::new("Sur le réseau de la maison").strong());
                        ui.label(
                            RichText::new("Trouvé tout seul sur le réseau de la maison. Rien à taper.")
                                .small()
                                .color(p.text_secondary),
                        );
                    });
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        let on = !route.tailscale.is_empty();
                        theme::state_word(
                            ui,
                            if on { theme::StateWord::Ok } else { theme::StateWord::Neutral },
                            if on { "Activé" } else { "Pas encore" },
                        );
                        ui.label(RichText::new("À distance : par Tailscale").strong());
                        ui.label(
                            RichText::new(if on {
                                "Hors de la maison, Ostal passe par Tailscale."
                            } else {
                                "Le serveur donne son adresse Tailscale à la prochaine connexion."
                            })
                            .small()
                            .color(p.text_secondary),
                        );
                    });
                });
                ui.add_space(6.0);
                let label = if self.details { "Détails ▾" } else { "Détails ▸" };
                if ui
                    .add(egui::Button::new(RichText::new(label).color(p.accent_text)).frame(false))
                    .clicked()
                {
                    self.details = !self.details;
                }
                if self.details {
                    let line = |ui: &mut egui::Ui, what: &str, value: &str| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(what).color(p.text_secondary));
                            ui.label(RichText::new(value).monospace());
                        });
                    };
                    line(
                        ui,
                        "Adresse locale",
                        route.primary.as_deref().unwrap_or("—"),
                    );
                    for url in &route.tailscale {
                        line(ui, "Adresse Tailscale", url);
                    }
                    let searching = self.finder.as_ref().is_some_and(Finder::searching);
                    if let Some(finder) = &self.finder {
                        let found: Vec<Found> = finder.found();
                        for f in &found {
                            line(ui, "Trouvé", &format!("{} · {}", f.name, f.url()));
                        }
                        if found.is_empty() && !searching {
                            ui.label(
                                RichText::new("Aucun Ostal Server trouvé sur ce réseau.")
                                    .small()
                                    .color(p.text_secondary),
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!searching, egui::Button::new("Chercher à nouveau"))
                            .clicked()
                        {
                            let ctx = ui.ctx().clone();
                            self.finder = Some(Finder::start(std::sync::Arc::new(move || {
                                ctx.request_repaint()
                            })));
                        }
                        if searching {
                            ui.spinner();
                        }
                    });
                }
            });
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        list: &[ConnectionView],
        now: f64,
        utc_offset_s: i64,
        action: &mut Action,
    ) {
        let p = *theme::palette(ui);
        egui::Grid::new("my-connections")
            .num_columns(4)
            .spacing([28.0, 10.0])
            .show(ui, |ui| {
                for head in ["Téléphone ou ordinateur", "Dernier accès", "Où", ""] {
                    ui.label(RichText::new(head).small().color(p.text_secondary));
                }
                ui.end_row();
                for c in list {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&c.name).strong());
                        ui.label(
                            RichText::new(connection_kind_label(c.kind, c.device, c.unknown))
                                .small()
                                .color(p.text_secondary),
                        );
                    });
                    ui.label(last_seen_label(c.last_seen, now, utc_offset_s));
                    ui.label(via_label(c.via));
                    if c.current {
                        ui.label(RichText::new("Cette app").small().color(p.text_secondary));
                    } else if self.confirming.as_ref().is_some_and(|(id, _)| *id == c.id) {
                        // Confirmation sur place : [Couper] rouge, [Annuler] par défaut.
                        ui.horizontal(|ui| {
                            if theme::destructive_button(ui, "Couper").clicked() {
                                *action = Action::Cut {
                                    id: c.id.clone(),
                                    name: c.name.clone(),
                                };
                                self.confirming = None;
                            }
                            if theme::secondary_button(ui, "Annuler").clicked() {
                                self.confirming = None;
                            }
                        });
                    } else if theme::secondary_button(ui, "Couper").clicked() {
                        self.confirming = Some((c.id.clone(), c.name.clone()));
                    }
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        if let Some((_, name)) = &self.confirming {
            ui.colored_label(
                p.error,
                format!("Couper « {name} » ? Il devra se reconnecter avec son identifiant et son mot de passe."),
            );
        } else {
            ui.label(
                RichText::new("Une connexion coupée ne peut plus rien faire : ses raccourcis cessent de marcher. Ce téléphone ou cet ordinateur devra se reconnecter avec son identifiant et son mot de passe.")
                    .small()
                    .color(p.text_secondary),
            );
        }
    }
}

/// Cette app d'abord, puis la plus récente.
fn sorted(mut list: Vec<ConnectionView>) -> Vec<ConnectionView> {
    list.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(b.last_seen.total_cmp(&a.last_seen))
    });
    list
}

/// L'heure et le décalage locaux d'aujourd'hui, pour écrire « Hier, 22:10 ».
pub fn clock() -> (f64, i64) {
    use chrono::Offset;
    let local = chrono::Local::now();
    (
        local.timestamp() as f64,
        i64::from(local.offset().fix().local_minus_utc()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::{ConnectionKind, DeviceClass, Via};

    fn view(id: &str, current: bool, seen: f64) -> ConnectionView {
        ConnectionView {
            id: id.into(),
            name: id.into(),
            kind: ConnectionKind::Web,
            device: DeviceClass::Iphone,
            last_seen: seen,
            via: Via::Local,
            current,
            unknown: false,
        }
    }

    #[test]
    fn this_app_comes_first_then_the_most_recent() {
        let out = sorted(vec![
            view("a", false, 5.0),
            view("b", false, 9.0),
            view("c", true, 1.0),
        ]);
        let ids: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
    }

    #[test]
    fn a_failed_load_keeps_its_sentence_and_a_new_one_replaces_it() {
        let mut page = Page::new();
        page.loaded(Err("Plus de connexion.".into()));
        assert_eq!(page.load, Load::Failed("Plus de connexion.".into()));
        page.loaded(Ok(ConnectionsResponse {
            connections: vec![view("a", true, 1.0)],
        }));
        assert!(matches!(&page.load, Load::Ready(l) if l.len() == 1));
    }
}
