//! Le cadre de la fenêtre, de haut en bas : la barre du haut (52 px), la phrase d'état (36 px), les
//! onglets (42 px), le bandeau d'erreur éventuel, puis, tout en bas, le journal replié (28 px).
//! Le contenu de la vue s'insère entre les deux. Maquettes `Bureau-*.dc.html`, kit §7.
//!
//! Tout ce qui est « quoi dire » (la phrase d'état, les noms des injoignables) est pur et testé ;
//! le dessin n'en dépend que par ces résultats.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Vec2,
};
use protocol::status::{self, Point};
use protocol::wire::CameraStatus;

use super::*;

/// Hauteurs fixes du cadre (kit §7).
const TOP_H: f32 = 52.0;
const STATUS_H: f32 = 36.0;
const TABS_H: f32 = 42.0;
const LOG_H: f32 = 28.0;
/// Hauteur du journal déplié (la ligne d'en-tête comprise).
const LOG_OPEN_H: f32 = 180.0;
/// Marge de la fenêtre, à gauche et à droite.
const SIDE: i8 = 24;

/// Ce que la phrase d'état dit, et sur quel ton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Phrase {
    pub tone: Tone,
    /// Le titre : « La maison va bien. », « Une chose à regarder », « Plus de connexion à la
    /// maison »…
    pub lead: String,
    /// La suite, vide quand le titre suffit.
    pub detail: String,
    /// Ce que « Voir l'appareil » ouvre.
    pub target: Option<Target>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    /// Tout va bien : coche verte.
    Fine,
    /// À regarder : triangle orange sur fond `banner_warn`.
    Watch,
    /// La liaison est perdue : même ton que « à regarder ».
    Lost,
    /// Première connexion en cours : information.
    Connecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Device(u64),
    Cameras,
}

impl MatterApp {
    /// La phrase d'état de la maison : la même que sur le Mac, le web et le widget
    /// (`protocol::status`), avec, pour un seul appareil injoignable, l'heure depuis laquelle.
    pub(super) fn status_phrase(&self) -> Phrase {
        match &self.link {
            Link::Reconnecting { .. } => {
                return Phrase {
                    tone: Tone::Lost,
                    lead: "Plus de connexion à la maison".into(),
                    detail: String::new(),
                    target: None,
                }
            }
            Link::Connected { .. } | Link::Resyncing => {}
            _ => {
                return Phrase {
                    tone: Tone::Connecting,
                    lead: "Connexion au serveur…".into(),
                    detail: String::new(),
                    target: None,
                }
            }
        }
        let unreachable: Vec<&Device> = self
            .model
            .devices
            .values()
            .filter(|d| matches!(d.status, WireStatus::Unreachable { .. }))
            .filter(|d| d.info.bridge.is_none() || d.info.caps.any())
            .collect();
        let cameras: Vec<String> = self
            .model
            .cameras
            .iter()
            .filter(|c| matches!(c.status, CameraStatus::Unreachable { .. }))
            .map(|c| c.name.clone())
            .collect();
        let points = [
            Point::Devices(unreachable.iter().map(|d| d.info.name.clone()).collect()),
            Point::Cameras(cameras.clone()),
        ];
        let n = status::count(&points);
        if n == 0 {
            return Phrase {
                tone: Tone::Fine,
                lead: status::headline(0),
                detail: String::new(),
                target: None,
            };
        }
        let mut detail = status::detail(&points).unwrap_or_default();
        // Un seul appareil : « … est injoignable depuis 18:02. »
        if let [only] = unreachable.as_slice() {
            if cameras.is_empty() {
                if let (Some(seen), Some(stem)) = (only.last_seen, detail.strip_suffix('.')) {
                    detail = format!("{stem} depuis {}.", devices::clock(seen));
                }
            }
        }
        let target = unreachable
            .first()
            .map(|d| Target::Device(d.info.node_id))
            .or((!cameras.is_empty()).then_some(Target::Cameras));
        Phrase {
            tone: Tone::Watch,
            lead: format!("{} :", status::headline(n)),
            detail,
            target,
        }
    }

    /// Le cadre complet, au-dessus du contenu : menu intégré (hors macOS), barre du haut, phrase
    /// d'état, onglets, bandeau d'erreur.
    pub(super) fn chrome(&mut self, ui: &mut egui::Ui) {
        let mut chosen_mode = None;
        let mut logout = false;
        let mut menu_export = false;
        let mut menu_import = false;
        // Sur macOS, Fichier est dans la barre de menus du Mac : pas de rangée dans la fenêtre.
        if self.native_menu.is_none() {
            egui::Panel::top("menu").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("Fichier", |ui| {
                        let connected = self.connected();
                        if ui
                            .add_enabled(connected, egui::Button::new("Exporter mes données…"))
                            .on_disabled_hover_text("Indisponible : plus de connexion au serveur")
                            .clicked()
                        {
                            menu_export = true;
                            ui.close();
                        }
                        if ui
                            .add_enabled(connected, egui::Button::new("Importer des données…"))
                            .on_disabled_hover_text("Indisponible : plus de connexion au serveur")
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
            });
        }
        self.top_bar(ui, &mut chosen_mode, &mut logout);
        self.status_bar(ui);
        self.tab_bar(ui);
        self.error_bar(ui);
        if let Some(mode) = chosen_mode.filter(|m| *m != self.model.mode) {
            self.set_mode(mode);
        }
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

    /// Barre du haut, 52 px : le logotype, la pastille Mode, puis à droite l'appairage en cours,
    /// l'adresse du serveur et « + Ajouter ».
    fn top_bar(
        &mut self,
        ui: &mut egui::Ui,
        chosen_mode: &mut Option<HomeMode>,
        logout: &mut bool,
    ) {
        let p = *theme::palette(ui);
        egui::Panel::top("top")
            .exact_size(TOP_H)
            .frame(bar_frame(&p))
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    theme::logotype(ui);
                    self.mode_button(ui, chosen_mode);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;
                        if self.connected() && theme::primary_button(ui, "+ Ajouter").clicked() {
                            self.add_open = true;
                            // Un appairage fini (réussi ou non) ne s'affiche pas à la réouverture.
                            if matches!(self.pairing, Pairing::Success(_) | Pairing::Failed(_)) {
                                self.pairing = Pairing::Idle;
                            }
                        }
                        self.address_button(ui, logout);
                        if matches!(self.pairing, Pairing::InProgress(_)) || self.model.pairing_busy
                        {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.spinner();
                                ui.label(
                                    RichText::new("Appairage en cours…").color(p.text_secondary),
                                );
                            });
                        }
                    });
                });
            });
    }

    /// La pastille Mode : un clic ouvre les quatre choix, chacun avec son effet. Grisée quand la
    /// liaison est perdue.
    fn mode_button(&mut self, ui: &mut egui::Ui, chosen_mode: &mut Option<HomeMode>) {
        let p = *theme::palette(ui);
        let label = format!("Mode : {} ▾", self.model.mode.label());
        let hint = "Mode de la maison : commande les caméras et les automatisations";
        // Un serveur sans modes : pas de pastille du tout.
        if self.connected() && !self.model.has_mode {
            return;
        }
        if !self.connected() {
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new(label).strong().color(p.text_faint))
                    .fill(p.surface_alt)
                    .corner_radius(CornerRadius::same(255)),
            )
            .on_disabled_hover_text("Mode de la maison : indisponible sans connexion");
            return;
        }
        let pill =
            theme::mode_pill(ui, &label, self.model.mode == HomeMode::Present).on_hover_text(hint);
        egui::Popup::menu(&pill)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(300.0);
                for mode in HomeMode::ALL {
                    let current = mode == self.model.mode;
                    let clicked = ui
                        .vertical(|ui| {
                            let r =
                                ui.selectable_label(current, RichText::new(mode.label()).strong());
                            ui.label(
                                RichText::new(mode.effect())
                                    .size(theme::tokens().font.size.caption)
                                    .color(p.text_secondary),
                            );
                            r.clicked()
                        })
                        .inner;
                    if clicked {
                        *chosen_mode = Some(mode);
                        ui.close();
                    }
                }
            });
    }

    /// L'adresse du serveur : « ● adresse · vX » en vert ; quand la liaison est perdue, le point
    /// et le mot « injoignable » passent en orange.
    fn address_button(&mut self, ui: &mut egui::Ui, logout: &mut bool) {
        let p = *theme::palette(ui);
        let size = theme::tokens().font.size.body - 1.0;
        match &self.link {
            Link::Connected {
                server_version,
                server,
            } => {
                let full = server_address(server);
                // À distance (autre adresse, ou Tailscale) : l'adresse réellement utilisée est dans
                // l'infobulle.
                let used = ostal_client::net::stream::active_address()
                    .filter(|_| ostal_client::net::stream::is_remote())
                    .map(|a| a.authority());
                let shown = shorten(&full, MAX_ADDRESS_CHARS);
                let suffix = if used.is_some() {
                    " · à distance"
                } else {
                    ""
                };
                let text = RichText::new(format!("● {shown} · {server_version}{suffix}"))
                    .size(size)
                    .color(p.ok);
                let (reply, _) = egui::containers::menu::MenuButton::from_button(
                    egui::Button::new(text).frame(false),
                )
                .ui(ui, |ui| {
                    if ui.button("Copier l'adresse").clicked() {
                        ui.ctx().copy_text(full.clone());
                        ui.close();
                    }
                    if ui.button("Se déconnecter / changer de serveur").clicked() {
                        *logout = true;
                        ui.close();
                    }
                });
                let used_line = used
                    .map(|u| format!("\nAdresse utilisée (à distance) : {u}"))
                    .unwrap_or_default();
                reply.on_hover_text(format!(
                    "{full}{used_line}\nServeur {server_version}\nClic : copier l'adresse ou changer de serveur"
                ));
            }
            Link::Resyncing => {
                ui.label(
                    RichText::new("● resynchronisation…")
                        .size(size)
                        .color(p.text_secondary),
                );
            }
            Link::Reconnecting { .. } | Link::Connecting => {
                let address = self
                    .server
                    .as_deref()
                    .or(self.saved_address.as_deref())
                    .map(server_address)
                    .map(|a| shorten(&a, MAX_ADDRESS_CHARS))
                    .unwrap_or_default();
                let lost = matches!(self.link, Link::Reconnecting { .. });
                let (color, word) = if lost {
                    (p.warn, "injoignable")
                } else {
                    (p.text_secondary, "connexion…")
                };
                // « ● adresse · injoignable » : le point en premier, comme la version connectée.
                // Appelé dans une rangée de droite à gauche : on ajoute donc le dernier mot d'abord.
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(word).size(size).color(color).strong());
                ui.label(
                    RichText::new(format!("{address} ·"))
                        .size(size)
                        .color(p.text_secondary),
                );
                ui.label(RichText::new("●").size(size).color(color));
            }
            _ => {}
        }
    }

    /// Phrase d'état, 36 px : une icône, le titre en gras, le détail, et « Voir l'appareil » quand
    /// il y a quelque chose à ouvrir.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let p = *theme::palette(ui);
        let phrase = self.status_phrase();
        let fill = match phrase.tone {
            Tone::Fine => p.surface,
            Tone::Watch | Tone::Lost => p.banner_warn,
            Tone::Connecting => p.banner_info,
        };
        let mut go = None;
        egui::Panel::top("status")
            .exact_size(STATUS_H)
            .frame(bar_frame(&p).fill(fill))
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                    match phrase.tone {
                        Tone::Fine => paint_check(ui.painter(), icon, p.ok),
                        Tone::Watch | Tone::Lost => paint_alert(ui.painter(), icon, p.warn),
                        Tone::Connecting => paint_info(ui.painter(), icon, p.accent_text),
                    }
                    let lead_color = match phrase.tone {
                        Tone::Fine | Tone::Connecting => p.text,
                        Tone::Watch | Tone::Lost => p.warn,
                    };
                    ui.label(RichText::new(&phrase.lead).strong().color(lead_color));
                    if !phrase.detail.is_empty() {
                        ui.label(RichText::new(&phrase.detail).color(p.text));
                    }
                    if let Some(target) = phrase.target {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = match target {
                                Target::Device(_) => "Voir l'appareil",
                                Target::Cameras => "Voir la caméra",
                            };
                            let button = egui::Button::new(
                                RichText::new(label)
                                    .size(theme::tokens().font.size.body - 1.0)
                                    .strong()
                                    .color(p.text),
                            )
                            .fill(p.surface)
                            .stroke(Stroke::new(1.0, p.line))
                            .corner_radius(CornerRadius::same(theme::tokens().radius.md))
                            .min_size(Vec2::new(0.0, 24.0));
                            if ui.add(button).clicked() {
                                go = Some(target);
                            }
                        });
                    }
                });
            });
        match go {
            Some(Target::Device(id)) => {
                self.view = View::Devices;
                self.select(id);
            }
            Some(Target::Cameras) => {
                self.close_detail();
                self.view = View::Cameras;
            }
            None => {}
        }
    }

    /// Onglets, 42 px, soulignés : Appareils, Plan, Températures, Caméras, Automatisations. Ils
    /// restent visibles mais grisés quand la liaison est perdue. À droite, les brouillons et choix
    /// en attente, visibles depuis toutes les vues.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let p = *theme::palette(ui);
        let connected = self.connected();
        egui::Panel::top("tabs")
            .exact_size(TABS_H)
            .frame(bar_frame(&p))
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // Mes connexions : « ‹ Maison » ramène à la maison, le seul sous-onglet
                    // de cette page est actif.
                    if self.view == View::Connections {
                        if tab_link(ui, false, connected, "‹ Maison").clicked() {
                            self.view = View::Devices;
                        }
                        tab_link(ui, true, connected, View::Connections.title());
                        return;
                    }
                    for view in View::ALL {
                        if tab_link(ui, self.view == view, connected, view.title()).clicked() {
                            if view != self.view {
                                self.close_detail();
                            }
                            self.view = view;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for alert in self.sync_alerts() {
                            let text = RichText::new(&alert.label).strong().color(p.warn);
                            let clicked = ui
                                .add(egui::Button::new(text).frame(false))
                                .on_hover_text(&alert.detail)
                                .clicked();
                            if clicked && connected {
                                self.view = alert.view;
                            }
                        }
                    });
                });
            });
    }

    /// Bandeau d'erreur : il reste jusqu'au × et porte « Réessayer » quand l'erreur vient d'une
    /// commande.
    fn error_bar(&mut self, ui: &mut egui::Ui) {
        let Some((_, text)) = self.error_banner.clone() else {
            return;
        };
        let p = *theme::palette(ui);
        let mut close = false;
        let mut retry = false;
        let can_retry = self.error_retry.is_some() && self.connected();
        egui::Panel::top("error")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(SIDE, 8)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                theme::error_banner(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let (icon, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        paint_cross(ui.painter(), icon, p.error);
                        // « Ce qui a échoué : » en gras, puis la cause.
                        match text.split_once(" : ") {
                            Some((what, why)) => {
                                ui.label(
                                    RichText::new(format!("{what} :")).strong().color(p.error),
                                );
                                ui.label(why);
                            }
                            None => {
                                ui.label(RichText::new(&text).strong().color(p.error));
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("×").color(p.text_secondary))
                                        .frame(false),
                                )
                                .on_hover_text("Fermer le message")
                                .clicked()
                            {
                                close = true;
                            }
                            if can_retry && theme::primary_button(ui, "Réessayer").clicked() {
                                retry = true;
                            }
                        });
                    });
                });
            });
        if retry {
            self.retry_last_command();
        }
        if close {
            self.error_banner = None;
            self.error_retry = None;
        }
    }

    /// Le journal, replié sur une ligne de 28 px (sa dernière ligne) ; un clic le déplie.
    pub(super) fn log_bar(&mut self, ui: &mut egui::Ui) {
        let p = *theme::palette(ui);
        let height = if self.log_open { LOG_OPEN_H } else { LOG_H };
        egui::Panel::bottom("log")
            .exact_size(height)
            .frame(bar_frame(&p))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), LOG_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let arrow = if self.log_open { "▾" } else { "▸" };
                            let last = self
                                .log
                                .back()
                                .filter(|_| !self.log_open)
                                .map(|l| format!(" · {l}"))
                                .unwrap_or_default();
                            let text = RichText::new(format!("{arrow} Journal{last}"))
                                .size(theme::tokens().font.size.body - 1.0)
                                .color(p.text_secondary);
                            let r = ui
                                .add(egui::Button::new(text).frame(false))
                                .on_hover_text("Montrer ou replier le journal");
                            if r.clicked() {
                                self.log_open = !self.log_open;
                            }
                        },
                    );
                    if self.log_open {
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                for line in &self.log {
                                    ui.label(
                                        RichText::new(line)
                                            .size(theme::tokens().font.size.caption + 1.0)
                                            .color(p.text_secondary),
                                    );
                                }
                            });
                    }
                });
            });
    }

    /// Renvoie la dernière commande qui a échoué.
    pub(super) fn retry_last_command(&mut self) {
        if let Some((what, request)) = self.error_retry.take() {
            self.error_banner = None;
            self.request(Pending::Command { what, node: None }, request);
        }
    }

    /// La carte du milieu quand la liaison est perdue ou en cours d'ouverture : 480 px, centrée.
    pub(super) fn lost_panel(&mut self, ui: &mut egui::Ui) {
        let p = *theme::palette(ui);
        let connecting = !matches!(self.link, Link::Reconnecting { .. });
        let retry_in = match &self.link {
            Link::Reconnecting { retry_in_s, .. } => Some(*retry_in_s),
            _ => None,
        };
        let address = self
            .server
            .as_deref()
            .or(self.saved_address.as_deref())
            .map(server_address)
            .unwrap_or_default();
        let never_reached = !self.ever_connected;
        let (mut change, mut again) = (false, false);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(theme::tokens().space.xl);
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(480.0, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        theme::card_frame(ui)
                            .inner_margin(Margin::symmetric(24, 22))
                            .show(ui, |ui| {
                                ui.set_width(480.0 - 48.0);
                                ui.spacing_mut().item_spacing.y = 10.0;
                                if connecting {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Connexion au serveur…")
                                                .size(theme::tokens().font.size.heading)
                                                .strong(),
                                        );
                                    });
                                    if !address.is_empty() {
                                        ui.label(
                                            RichText::new(format!(
                                                "Ostal essaie de joindre {address}."
                                            ))
                                            .color(p.text_secondary),
                                        );
                                    }
                                    return;
                                }
                                ui.label(
                                    RichText::new("Plus de connexion à la maison")
                                        .size(theme::tokens().font.size.heading)
                                        .strong(),
                                );
                                ui.label(lost_reason_text(never_reached, &address));
                                ui.label(
                                    RichText::new(
                                        "Le Mac serveur est peut-être en veille ou éteint : \
                                         vérifiez qu'il est allumé et qu'Ostal Server est ouvert.",
                                    )
                                    .color(p.text_secondary),
                                );
                                if let Some(s) = retry_in {
                                    ui.label(
                                        RichText::new(format!(
                                            "Nouvel essai automatique dans {s} s au plus."
                                        ))
                                        .color(p.text_secondary),
                                    );
                                }
                                ui.horizontal(|ui| {
                                    change =
                                        theme::secondary_button(ui, "Changer de serveur").clicked();
                                    again =
                                        theme::primary_button(ui, "Réessayer maintenant").clicked();
                                });
                                ui.label(
                                    RichText::new(
                                        "Aucune donnée n'est affichée ni modifiable tant que le \
                                         serveur n'est pas joint.",
                                    )
                                    .size(theme::tokens().font.size.body - 1.0)
                                    .color(p.text_secondary),
                                );
                            });
                    },
                );
            });
        });
        if change {
            self.backend.logout();
        }
        if again {
            self.backend.reconnect();
        }
    }
}

/// La première ligne de la carte « Plus de connexion à la maison » : la liaison perdue, ou une
/// adresse où aucun serveur n'a encore répondu.
pub(super) fn lost_reason_text(never_reached: bool, address: &str) -> String {
    if never_reached {
        if address.is_empty() {
            "Aucun serveur n'a répondu.".into()
        } else {
            format!("Aucun serveur n'a répondu à {address}.")
        }
    } else {
        "Liaison perdue : le serveur ne répond plus.".into()
    }
}

/// Cadre d'une barre : surface, marge latérale de 24 px, sans trait propre (la ligne de
/// séparation de l'`egui::Panel` fait le filet du bas).
fn bar_frame(p: &theme::Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.surface)
        .inner_margin(Margin::symmetric(SIDE, 0))
}

/// Un onglet : texte seul, souligné de 2 px en `accent` quand il est choisi, grisé quand la
/// liaison est perdue.
fn tab_link(ui: &mut egui::Ui, selected: bool, enabled: bool, label: &str) -> egui::Response {
    let p = *theme::palette(ui);
    let size = theme::tokens().font.size.body;
    let color = if !enabled {
        p.text_faint
    } else if selected {
        p.accent_text
    } else {
        p.text_secondary
    };
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), egui::FontId::proportional(size), color);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 28.0, ui.available_height()),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, enabled, selected, label));
    let painter = ui.painter();
    if enabled && response.hovered() && !selected {
        painter.rect_filled(
            rect.shrink2(Vec2::new(2.0, 6.0)),
            6.0,
            theme::tokens().hover(&p),
        );
    }
    painter.galley(
        egui::pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if selected && enabled {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - 2.0),
                rect.right_bottom(),
            ),
            CornerRadius::ZERO,
            p.accent,
        );
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Triangle d'alerte du kit (§16), dans `rect`.
fn paint_alert(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let k = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    let stroke = Stroke::new(1.8 * (k * 1.5).clamp(1.0, 1.4), color);
    painter.add(egui::Shape::closed_line(
        vec![at(12.0, 4.0), at(21.5, 20.0), at(2.5, 20.0)],
        stroke,
    ));
    painter.line_segment([at(12.0, 10.0), at(12.0, 14.0)], stroke);
    painter.circle_filled(at(12.0, 17.0), 1.1, color);
}

/// Coche dans un cercle (kit §9), dans `rect`.
fn paint_check(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let k = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    painter.circle_stroke(rect.center(), 9.0 * k, Stroke::new(1.6, color));
    painter.add(egui::Shape::line(
        vec![at(8.0, 12.5), at(10.8, 15.3), at(16.5, 9.5)],
        Stroke::new(1.8, color),
    ));
}

/// Information : un « i » dans un cercle.
fn paint_info(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let k = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    painter.circle_stroke(rect.center(), 9.0 * k, Stroke::new(1.6, color));
    painter.line_segment([at(12.0, 11.0), at(12.0, 16.0)], Stroke::new(1.8, color));
    painter.circle_filled(at(12.0, 8.0), 1.1, color);
}

/// Croix dans un cercle : une erreur (kit §10).
fn paint_cross(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let k = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * k;
    let stroke = Stroke::new(1.6, color);
    painter.circle_stroke(rect.center(), 9.0 * k, stroke);
    painter.line_segment([at(9.0, 9.0), at(15.0, 15.0)], stroke);
    painter.line_segment([at(15.0, 9.0), at(9.0, 15.0)], stroke);
}
