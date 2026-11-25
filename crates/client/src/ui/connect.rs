//! Écran de connexion : adresse du serveur, identifiant, mot de passe ; confirmation de
//! l'empreinte du certificat à la première connexion ; refus clair si le certificat a changé.
//!
//! Le mot de passe n'est gardé en mémoire que le temps de la saisie, puis effacé dès qu'il est
//! envoyé ; il n'est jamais écrit sur disque ni dans le journal.

use eframe::egui::{self, RichText};
use protocol::wire::ErrorCode;

use super::theme;
use ostal_client::net::discovery::{Finder, Found};
use ostal_client::net::tls::normalize_fingerprint;
use ostal_client::net::{Backend, Link, NetError, ServerAddr};

enum Stage {
    Form,
    /// Lecture de l'empreinte du certificat en cours.
    Probing,
    /// Empreinte lue, à approuver par l'utilisateur avant d'envoyer le mot de passe.
    Confirm {
        url: String,
        fingerprint: String,
    },
    LoggingIn,
}

pub struct ConnectScreen {
    url: String,
    /// Autres adresses du même serveur (à distance : nom Tailscale, IP 100.x), facultatif.
    remote: String,
    /// Le champ `remote` a été rempli depuis le fichier de connexion (une fois par écran).
    remote_loaded: bool,
    /// Adresse principale et texte du champ « à distance » tels que chargés depuis le fichier :
    /// si l'adresse change sans que le champ soit touché, les anciennes alternatives sont celles
    /// d'un autre serveur (voir [`alts_for_submit`]).
    loaded_primary: Option<String>,
    loaded_remote: String,
    username: String,
    password: String,
    /// Adresse déjà approuvée (fichier de connexion) : pas de nouvelle empreinte à confirmer.
    pinned_url: Option<String>,
    stage: Stage,
    /// Empreinte attendue, collée par l'utilisateur pour la comparer (facultatif).
    expected: String,
    message: Option<(bool, String)>,
    /// Sous quel champ s'affiche le message (`General` : en tête, pour tout message, même `ok`).
    message_field: Field,
    /// Case à cocher avant de réinitialiser l'empreinte épinglée.
    confirm_reset: bool,
    /// La note de sécurité est dépliée (« Détails »).
    show_security: bool,
    /// La recherche d'Ostal Server sur le réseau de la maison (Bonjour), lancée à la première
    /// ouverture, quand aucun serveur n'est encore connu.
    finder: Option<Finder>,
    /// L'adresse est saisie à la main : plus de serveur trouvé proposé.
    manual: bool,
    /// Lequel des serveurs trouvés est choisi (s'il y en a plusieurs).
    found_choice: usize,
    /// Le prochain retour à l'écran de connexion vient de « Changer de serveur » : la session
    /// est restée ouverte, on peut y revenir.
    resume_armed: bool,
    /// « Revenir à ce serveur » est proposé (annule un « Changer de serveur » fait par erreur).
    resume: bool,
}

/// Sous quel champ s'affiche un message d'erreur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Sous l'adresse du serveur.
    Server,
    /// Sous le mot de passe (identifiants refusés).
    Password,
    /// Sous l'adresse à distance.
    Remote,
    /// Pas de champ en cause : en tête du formulaire.
    General,
}

/// Forme canonique d'une adresse saisie (`None` si elle est illisible).
fn canonical(text: &str) -> Option<String> {
    ServerAddr::parse(text).ok().map(|a| a.url())
}

/// Texte du champ « à distance » à envoyer. Quand l'adresse principale a changé pour une adresse
/// pas encore épinglée (première connexion, changement de serveur) et que le champ est resté
/// celui qui avait été chargé, il décrivait un autre serveur : il est vidé. Un champ que
/// l'utilisateur a modifié est gardé tel quel.
fn alts_for_submit(
    primary: Option<&str>,
    loaded_primary: Option<&str>,
    loaded_remote: &str,
    remote: &str,
    pinned: bool,
) -> String {
    if !pinned && primary != loaded_primary && remote == loaded_remote {
        String::new()
    } else {
        remote.to_string()
    }
}

/// Phrase sous un champ d'adresses quand l'une d'elles est jugée par un certificat public (nom de
/// domaine sur le 443) : dit clairement à quel nom l'app se connectera, sans empreinte à
/// comparer. `None` s'il n'y en a pas.
fn public_chain_note(text: &str) -> Option<String> {
    let names: Vec<String> = ServerAddr::parse_list(text)
        .ok()?
        .iter()
        .filter(|a| a.is_public_chain())
        .map(|a| a.host.clone())
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "Vérifiée par un certificat public, sans empreinte : l'app se connectera à {} et refusera \
         tout autre nom. Vérifiez qu'il s'agit bien de votre serveur.",
        names.join(", ")
    ))
}

/// Texte de l'écran d'approbation quand le certificat est public et valable pour le nom.
fn public_confirm_text() -> &'static str {
    "Le certificat de ce nom a été vérifié : il est délivré par une autorité publique pour \
     exactement ce nom, et encore valable. Il n'y a aucune empreinte à comparer. Approuvez \
     seulement si ce nom est bien celui de votre serveur : l'app refusera ensuite tout autre \
     nom ou tout certificat qui ne serait pas valable pour lui."
}

/// Le champ concerné par une erreur, d'après sa nature (jamais d'après son texte).
pub fn field_of(error: &NetError) -> Field {
    match error {
        NetError::BadAddress(_)
        | NetError::Unreachable(_)
        | NetError::Timeout
        | NetError::CertificateChanged { .. }
        | NetError::PublicCertificateRejected(_)
        | NetError::Protocol(_) => Field::Server,
        NetError::Api {
            code: Some(ErrorCode::InvalidCredentials | ErrorCode::TooManyAttempts),
            ..
        } => Field::Password,
        NetError::Api { .. } | NetError::Local(_) => Field::General,
    }
}

impl ConnectScreen {
    pub fn new() -> Self {
        Self {
            url: String::new(),
            remote: String::new(),
            remote_loaded: false,
            loaded_primary: None,
            loaded_remote: String::new(),
            username: String::new(),
            password: String::new(),
            pinned_url: None,
            stage: Stage::Form,
            expected: String::new(),
            message: None,
            message_field: Field::General,
            confirm_reset: false,
            show_security: false,
            finder: None,
            manual: false,
            found_choice: 0,
            resume_armed: false,
            resume: false,
        }
    }

    /// « Changer de serveur » vient d'être demandé : l'écran de connexion qui suit propose de
    /// revenir au serveur quitté.
    pub fn arm_resume(&mut self) {
        self.resume_armed = true;
    }

    #[cfg(test)]
    pub fn can_resume(&self) -> bool {
        self.resume
    }

    /// Retour à l'écran de connexion (démarrage, déconnexion, jeton expiré).
    pub fn logged_out(&mut self, url: Option<String>, pinned: bool, reason: Option<String>) {
        self.password.clear();
        self.stage = Stage::Form;
        self.remote_loaded = false;
        self.pinned_url = url.clone().filter(|_| pinned);
        if let Some(url) = url {
            self.url = url;
        }
        self.message = reason.map(|r| (false, r));
        self.message_field = Field::General;
        self.confirm_reset = false;
        self.resume = std::mem::take(&mut self.resume_armed) && self.pinned_url.is_some();
    }

    /// Un message à montrer sur les écrans qui ne passent pas par `logged_out` (certificat
    /// changé, versions incompatibles) : ce que le retour à l'écran de connexion a fait perdre.
    /// `None` efface le message précédent.
    pub fn note(&mut self, text: Option<String>) {
        self.message = text.map(|t| (false, t));
        self.message_field = Field::General;
    }

    #[cfg(test)]
    pub fn message_text(&self) -> Option<&str> {
        self.message.as_ref().map(|(_, t)| t.as_str())
    }

    fn show_message(&self, ui: &mut egui::Ui) {
        if let Some((ok, text)) = &self.message {
            ui.colored_label(
                if *ok {
                    theme::palette(ui).ok
                } else {
                    theme::palette(ui).error
                },
                text,
            );
            ui.add_space(8.0);
        }
    }

    /// Connexion réussie : plus rien à garder.
    pub fn connected(&mut self) {
        self.finder = None;
        self.manual = false;
        self.password.clear();
        self.expected.clear();
        self.stage = Stage::Form;
        self.message = None;
        self.resume = false;
        self.resume_armed = false;
    }

    pub fn probed(&mut self, url: String, fingerprint: String) {
        self.stage = Stage::Confirm { url, fingerprint };
    }

    pub fn failed(&mut self, error: &NetError) {
        self.password.clear();
        self.stage = Stage::Form;
        self.message = Some((false, error.message()));
        self.message_field = field_of(error);
    }

    /// Oublier le certificat épinglé arrête la session : pas pendant qu'une connexion
    /// (lecture de l'empreinte ou envoi du mot de passe) est en cours.
    pub fn reset_allowed(&self) -> bool {
        !matches!(self.stage, Stage::Probing | Stage::LoggingIn)
    }

    #[cfg(test)]
    pub fn start_logging_in(&mut self) {
        self.stage = Stage::LoggingIn;
    }

    fn url_is_pinned(&self) -> bool {
        self.pinned_url
            .as_deref()
            .is_some_and(|pinned| ServerAddr::parse(&self.url).is_ok_and(|a| a.url() == pinned))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, link: &Link, backend: &Backend) {
        let p = *theme::palette(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(122.0);
                    // La carte : 420 px pour le formulaire, 480 px pour les écrans de
                    // certificat et de versions (maquettes Bureau-*.dc.html).
                    let wide = matches!(
                        link,
                        Link::CertificateChanged { .. } | Link::Incompatible { .. }
                    ) || matches!(self.stage, Stage::Confirm { .. });
                    let width = if wide { 480.0 } else { 420.0 };
                    let pad = if wide { (24, 22) } else { (28, 24) };
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            theme::card_frame(ui)
                                .inner_margin(egui::Margin::symmetric(pad.0, pad.1))
                                .show(ui, |ui| {
                                    ui.set_width(width - 2.0 * pad.0 as f32);
                                    ui.spacing_mut().item_spacing.y = 6.0;
                                    // Les champs sur fond blanc gardent un trait fin visible.
                                    let line = egui::Stroke::new(1.0, p.line);
                                    let w = &mut ui.visuals_mut().widgets;
                                    w.inactive.bg_stroke = line;
                                    w.hovered.bg_stroke = line;
                                    match link {
                                        Link::CertificateChanged {
                                            url,
                                            expected,
                                            found,
                                        } => {
                                            self.show_message(ui);
                                            self.certificate_changed(
                                                ui, backend, url, expected, found,
                                            );
                                        }
                                        Link::Incompatible { message } => {
                                            self.show_message(ui);
                                            Self::incompatible(ui, backend, message);
                                        }
                                        _ => self.form(ui, backend),
                                    }
                                });
                        },
                    );
                });
            });
    }

    /// Le logo, « Maison » et la ligne d'accroche, centrés au-dessus du formulaire.
    fn header(ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            theme::logo(ui, 32.0);
            ui.label(
                RichText::new("Maison")
                    .size(theme::tokens().font.size.heading)
                    .strong(),
            );
            ui.label(
                RichText::new("Connexion à votre serveur Ostal")
                    .color(theme::palette(ui).text_secondary),
            );
        });
    }

    fn form(&mut self, ui: &mut egui::Ui, backend: &Backend) {
        if !self.remote_loaded {
            self.remote_loaded = true;
            self.remote = backend.alt_urls().join(" ");
            self.loaded_primary = canonical(&self.url);
            self.loaded_remote = self.remote.clone();
        }
        // Un message qui ne vise aucun champ reste en tête ; les autres sont sous leur champ.
        let message = self.message.clone();
        let at = message.as_ref().map(|_| self.message_field);
        if let Some((ok, text)) = message.as_ref().filter(|_| at == Some(Field::General)) {
            ui.colored_label(
                if *ok {
                    theme::palette(ui).ok
                } else {
                    theme::palette(ui).error
                },
                text,
            );
            ui.add_space(8.0);
        }
        if let Stage::Confirm { url, fingerprint } = &self.stage {
            let (url, fingerprint) = (url.clone(), fingerprint.clone());
            self.confirm(ui, backend, &url, &fingerprint);
            return;
        }
        Self::header(ui);
        let busy = matches!(self.stage, Stage::Probing | Stage::LoggingIn);
        let p = theme::palette(ui);
        // Aucun serveur connu : on le cherche sur le réseau de la maison (Bonjour). Un serveur
        // déjà approuvé (adresse gardée) n'a rien à chercher.
        let discover = self.pinned_url.is_none() && !self.manual;
        if discover && self.finder.is_none() {
            let ctx = ui.ctx().clone();
            self.finder = Some(Finder::start(std::sync::Arc::new(move || {
                ctx.request_repaint()
            })));
        }
        let found: Vec<Found> = if discover {
            self.finder.as_ref().map(Finder::found).unwrap_or_default()
        } else {
            Vec::new()
        };
        let searching = discover && self.finder.as_ref().is_some_and(Finder::searching);
        if !found.is_empty() {
            self.found_choice = self.found_choice.min(found.len() - 1);
            self.url = found[self.found_choice].url();
        }
        // Une erreur sous son champ : l'icône, le texte en gras (kit §12), role=alert.
        let error = |ui: &mut egui::Ui, which: Field| {
            if let Some((false, text)) = message.as_ref().filter(|_| at == Some(which)) {
                ui.horizontal_wrapped(|ui| {
                    let (icon, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    let c = icon.center();
                    let stroke = egui::Stroke::new(1.5, p.error);
                    ui.painter().circle_stroke(c, 5.5, stroke);
                    ui.painter().line_segment(
                        [c + egui::vec2(-2.5, -2.5), c + egui::vec2(2.5, 2.5)],
                        stroke,
                    );
                    ui.painter().line_segment(
                        [c + egui::vec2(2.5, -2.5), c + egui::vec2(-2.5, 2.5)],
                        stroke,
                    );
                    ui.label(RichText::new(text).size(12.0).strong().color(p.error));
                });
            }
        };
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.label(RichText::new(text).size(12.0).strong().color(p.text));
        };
        // Le champ en erreur garde sa valeur, avec une bordure de 1,5 px en `error`.
        let invalid = |ui: &mut egui::Ui, which: Field| {
            if message.as_ref().is_some_and(|(ok, _)| !ok) && at == Some(which) {
                let w = &mut ui.visuals_mut().widgets;
                for s in [&mut w.inactive, &mut w.hovered, &mut w.active] {
                    s.bg_stroke = egui::Stroke::new(1.5, p.error);
                }
            }
        };
        let full = ui.available_width();

        if found.is_empty() {
            if searching {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Recherche d'Ostal Server sur le réseau de la maison…")
                            .size(12.0)
                            .color(p.text_secondary),
                    );
                });
                ui.add_space(4.0);
            }
            label(ui, "Adresse du serveur");
            ui.scope(|ui| {
                invalid(ui, Field::Server);
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.url)
                        .hint_text("192.168.1.20:8443, mac-mini.local ou https://ostal.exemple.fr")
                        .desired_width(full),
                );
            });
            error(ui, Field::Server);
            ui.label(
                RichText::new(
                    "Elle s'affiche sur le Mac serveur : menu › Connecter un Mac ou un iPhone… Un nom \
                     de domaine (https://nom) est vérifié par son certificat public, sans empreinte.",
                )
                .size(12.0)
                .color(p.text_secondary),
            );
            ui.add_space(theme::tokens().space.sm);

            label(ui, "Adresse à distance (facultatif)");
            ui.scope(|ui| {
                invalid(ui, Field::Remote);
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.remote)
                        .hint_text("https://ostal.exemple.fr, mac-mini.exemple.ts.net ou 100.x.y.z")
                        .desired_width(full),
                );
            });
            error(ui, Field::Remote);
            ui.label(
                RichText::new(
                    "Le même serveur, joint par Tailscale quand vous n'êtes pas chez vous. \
                     L'app essaie l'adresse ci-dessus, puis celle-ci. Plusieurs : séparez par un espace.",
                )
                .size(12.0)
                .color(p.text_secondary),
            );
            if let Some(note) = public_chain_note(&self.remote) {
                ui.label(RichText::new(note).small().color(p.text_secondary));
            }
            ui.add_space(theme::tokens().space.md);
        } else {
            self.found_card(ui, &found, busy);
        }

        label(ui, "Identifiant");
        ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.username).desired_width(full),
        );
        ui.add_space(theme::tokens().space.sm);

        label(ui, "Mot de passe");
        let password = ui
            .scope(|ui| {
                invalid(ui, Field::Password);
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(full),
                )
            })
            .inner;
        error(ui, Field::Password);
        ui.add_space(theme::tokens().space.md);

        let enter = password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ready = !self.url.trim().is_empty()
            && !self.username.trim().is_empty()
            && !self.password.is_empty();
        let go = theme::primary_button_wide(ui, "Se connecter", !busy && ready, full).clicked()
            || (enter && !busy && ready);
        if busy {
            ui.vertical_centered(|ui| ui.spinner());
        }
        if go {
            self.submit(backend);
        }
        if self.resume && !busy && theme::secondary_button(ui, "Revenir à ce serveur").clicked() {
            self.resume = false;
            backend.reconnect();
        }
        ui.add_space(theme::tokens().space.md);
        if self.pinned_url.is_some() && !self.url_is_pinned() {
            ui.small("Autre adresse : son certificat devra être approuvé.");
        } else if self.pinned_url.is_some() {
            ui.small("Le certificat de ce serveur est déjà approuvé.");
            ui.add_space(6.0);
            // Pas pendant la connexion : l'oubli arrêterait la session qui est en train de s'ouvrir.
            if ui
                .add_enabled(
                    self.reset_allowed(),
                    egui::Button::new("Ce serveur a été réinstallé : oublier son certificat")
                        .small(),
                )
                .clicked()
            {
                backend.reset_pinned_certificate(self.pinned_url.clone());
            }
        }
        // Note de sécurité : une phrase, « Détails ▸ » replié.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                RichText::new("La connexion est chiffrée ; le mot de passe n'est jamais conservé.")
                    .size(12.0)
                    .color(p.text_secondary),
            );
            let text = if self.show_security {
                "Détails ▾"
            } else {
                "Détails ▸"
            };
            if ui
                .add(
                    egui::Button::new(RichText::new(text).size(12.0).color(p.accent_text))
                        .frame(false),
                )
                .clicked()
            {
                self.show_security = !self.show_security;
            }
        });
        if self.show_security {
            ui.label(
                RichText::new(
                    "Seuls l'adresse, l'empreinte du certificat et un jeton de session sont \
                     gardés sur ce Mac. Le mot de passe n'est lu que le temps de la connexion, \
                     puis effacé de la mémoire ; il n'est jamais écrit sur le disque ni dans le \
                     journal.",
                )
                .small()
                .color(p.text_secondary),
            );
        }
    }

    /// « Ostal Server trouvé » (maquette Bureau-Serveur-Trouve) : le serveur annoncé sur le réseau,
    /// choisi d'un clic s'il y en a plusieurs, et « Saisir une adresse à la main » en secours. Pas
    /// d'adresse affichée : le nom suffit.
    fn found_card(&mut self, ui: &mut egui::Ui, found: &[Found], busy: bool) {
        let p = *theme::palette(ui);
        ui.label(
            RichText::new("Ostal Server trouvé")
                .size(theme::tokens().font.size.heading)
                .strong(),
        );
        ui.label(
            RichText::new("Ce Mac pilote la maison. Ostal l'a trouvé tout seul.")
                .color(p.text_secondary),
        );
        ui.add_space(6.0);
        for (i, f) in found.iter().enumerate() {
            let selected = i == self.found_choice;
            let frame = egui::Frame::new()
                .fill(if selected { p.accent_soft } else { p.surface })
                .stroke(egui::Stroke::new(
                    if selected { 1.5 } else { 1.0 },
                    if selected { p.accent } else { p.line },
                ))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(10));
            let response = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&f.name).strong());
                            ui.label(
                                RichText::new("Trouvé sur ce réseau")
                                    .small()
                                    .color(p.text_secondary),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            theme::state_word(ui, theme::StateWord::Ok, "joignable");
                        });
                    });
                })
                .response
                .interact(egui::Sense::click());
            if response.clicked() && !busy {
                self.found_choice = i;
            }
        }
        ui.horizontal(|ui| {
            ui.colored_label(p.ok, "✓ Serveur trouvé");
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(
                        RichText::new("Saisir une adresse à la main").color(p.accent_text),
                    )
                    .frame(false),
                )
                .clicked()
            {
                self.manual = true;
                self.finder = None;
            }
        });
        ui.label(
            RichText::new("Puis connectez-vous avec votre compte.")
                .size(12.0)
                .color(p.text_secondary),
        );
        ui.add_space(theme::tokens().space.sm);
    }

    fn submit(&mut self, backend: &Backend) {
        self.message = None;
        let url = self.url.trim().to_string();
        if let Err(e) = ServerAddr::parse(&url) {
            self.message = Some((false, e.message()));
            self.message_field = Field::Server;
            return;
        }
        // Un autre serveur (adresse principale changée, champ « à distance » laissé tel quel) :
        // les anciennes alternatives ne sont pas les siennes.
        let pinned = self.url_is_pinned();
        self.remote = alts_for_submit(
            canonical(&url).as_deref(),
            self.loaded_primary.as_deref(),
            &self.loaded_remote,
            &self.remote,
            pinned,
        );
        self.loaded_primary = canonical(&url);
        self.loaded_remote = self.remote.clone();
        match ServerAddr::parse_list(&self.remote) {
            Ok(list) => backend.set_alt_urls(list.iter().map(ServerAddr::url).collect()),
            Err(e) => {
                self.message = Some((false, e.message()));
                self.message_field = Field::Remote;
                return;
            }
        }
        if self.url_is_pinned() {
            backend.login_saved(self.username.trim(), &self.password);
            self.password.clear();
            self.stage = Stage::LoggingIn;
        } else {
            // Première connexion à cette adresse : on lit d'abord l'empreinte, sans rien envoyer.
            backend.probe(&url);
            self.stage = Stage::Probing;
        }
    }

    fn confirm(&mut self, ui: &mut egui::Ui, backend: &Backend, url: &str, fingerprint: &str) {
        let h = theme::tokens().font.size.heading;
        let public = ServerAddr::parse(url).is_ok_and(|a| a.is_public_chain());
        ui.label(RichText::new("Approuver ce serveur ?").size(h).strong());
        ui.label(
            RichText::new(format!(
                "Première connexion à {}",
                url.trim_start_matches("https://")
            ))
            .color(theme::palette(ui).text_secondary),
        );
        if public {
            // Certificat public valide pour ce nom : rien à comparer, mais le nom est bien visible.
            if let Some(host) = ServerAddr::parse(url).ok().map(|a| a.host) {
                ui.label(RichText::new(host).strong().size(h));
            }
            ui.label(public_confirm_text());
        } else {
            ui.label(
                "Le serveur présente un certificat qu'il a fabriqué lui-même. Comparez son \
                 empreinte avec celle affichée au lancement du serveur (ou par la commande \
                 « ostal-server fingerprint »). Si elles sont identiques, approuvez : seul ce \
                 certificat sera accepté ensuite.",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("Empreinte SHA-256 reçue :").strong());
            ui.add(
                egui::Label::new(RichText::new(fingerprint).monospace())
                    .wrap_mode(egui::TextWrapMode::Wrap),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Empreinte attendue (facultatif) :");
                ui.add(egui::TextEdit::singleline(&mut self.expected).desired_width(240.0));
            });
        }
        let mismatch = !public
            && !self.expected.trim().is_empty()
            && normalize_fingerprint(&self.expected).as_deref() != Some(fingerprint);
        if mismatch {
            ui.colored_label(
                theme::palette(ui).error,
                "Cette empreinte est différente de celle que vous avez saisie : n'approuvez pas.",
            );
        } else if !public && !self.expected.trim().is_empty() {
            ui.colored_label(theme::palette(ui).ok, "Identique à l'empreinte saisie.");
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "Annuler").clicked() {
                self.password.clear();
                self.expected.clear();
                self.stage = Stage::Form;
            }
            if ui
                .add_enabled_ui(!mismatch, |ui| {
                    theme::primary_button(ui, "Approuver et se connecter")
                })
                .inner
                .clicked()
            {
                backend.login(url, fingerprint, self.username.trim(), &self.password);
                self.password.clear();
                self.expected.clear();
                self.stage = Stage::LoggingIn;
            }
        });
    }

    fn certificate_changed(
        &mut self,
        ui: &mut egui::Ui,
        backend: &Backend,
        url: &str,
        expected: &str,
        found: &str,
    ) {
        ui.colored_label(
            theme::palette(ui).error,
            RichText::new("Le certificat du serveur a changé")
                .size(theme::tokens().font.size.heading)
                .strong(),
        );
        ui.label(format!(
            "Le serveur {url} ne présente plus le certificat que vous aviez approuvé. Rien n'a \
             été envoyé et la connexion est refusée."
        ));
        ui.label(
            "Cela arrive si le serveur a été réinstallé ou si son dossier de données a été \
             remplacé. Cela peut aussi signaler un autre appareil qui se fait passer pour lui.",
        );
        ui.add_space(8.0);
        ui.label(RichText::new("Empreinte approuvée :").strong());
        ui.label(RichText::new(expected).monospace());
        ui.label(RichText::new("Empreinte présentée :").strong());
        ui.label(RichText::new(found).monospace());
        ui.add_space(10.0);
        ui.checkbox(
            &mut self.confirm_reset,
            "Je sais pourquoi le certificat a changé et je veux approuver le nouveau",
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.confirm_reset,
                    egui::Button::new("Oublier l'ancien certificat…"),
                )
                .clicked()
            {
                self.confirm_reset = false;
                backend.reset_pinned_certificate(Some(url.to_string()));
            }
            if theme::primary_button(ui, "Réessayer").clicked() {
                backend.reconnect();
            }
        });
    }

    fn incompatible(ui: &mut egui::Ui, backend: &Backend, message: &str) {
        ui.colored_label(
            theme::palette(ui).error,
            RichText::new("Versions incompatibles")
                .size(theme::tokens().font.size.heading)
                .strong(),
        );
        ui.label(RichText::new(message).color(theme::palette(ui).text_secondary));
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "Changer de serveur").clicked() {
                backend.logout();
            }
            if theme::primary_button(ui, "Réessayer").clicked() {
                backend.reconnect();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_is_told_which_name_a_public_certificate_will_check() {
        let note = public_chain_note("https://ostal.klbr.h4md1.fr 100.64.0.5").unwrap();
        assert!(note.contains("ostal.klbr.h4md1.fr") && !note.contains("100.64.0.5"));
        let two = public_chain_note("a.exemple.fr:443, b.exemple.fr:443").unwrap();
        assert!(two.contains("a.exemple.fr, b.exemple.fr"));
        // Rien à dire pour une IP, un nom .local, un autre port, une liste illisible ou vide.
        for none in [
            "",
            "192.168.1.20:443",
            "mac.local:443",
            "ostal.klbr.h4md1.fr:8443",
            "ostal.klbr.h4md1.fr",
            "http://x",
        ] {
            assert_eq!(public_chain_note(none), None, "{none:?}");
        }
        // L'écran d'approbation d'un nom public ne demande aucune empreinte.
        assert!(public_confirm_text().contains("aucune empreinte"));
    }

    #[test]
    fn changing_the_primary_address_does_not_keep_the_old_alternatives() {
        let old = "https://192.168.1.20:8443";
        let new = "https://autre.local:8443";
        let alts = "https://mac.tailnet-xxxx.ts.net:8443";
        // Autre serveur, pas encore épinglé, champ resté tel quel : vidé.
        assert_eq!(alts_for_submit(Some(new), Some(old), alts, alts, false), "");
        // Même serveur : gardé.
        assert_eq!(
            alts_for_submit(Some(old), Some(old), alts, alts, false),
            alts
        );
        // Adresse changée mais champ rempli par l'utilisateur : gardé.
        let typed = "https://autre.tailnet-xxxx.ts.net:8443";
        assert_eq!(
            alts_for_submit(Some(new), Some(old), alts, typed, false),
            typed
        );
        // Adresse déjà épinglée : gardé.
        assert_eq!(
            alts_for_submit(Some(new), Some(old), alts, alts, true),
            alts
        );
    }

    #[test]
    fn an_error_goes_under_the_field_it_is_about_by_its_kind_not_its_text() {
        let api = |code| NetError::Api {
            status: 401,
            code,
            detail: None,
            retry_after_s: None,
        };
        assert_eq!(
            field_of(&api(Some(ErrorCode::InvalidCredentials))),
            Field::Password
        );
        assert_eq!(
            field_of(&api(Some(ErrorCode::TooManyAttempts))),
            Field::Password
        );
        assert_eq!(field_of(&api(Some(ErrorCode::BadRequest))), Field::General);
        assert_eq!(field_of(&api(None)), Field::General);
        assert_eq!(
            field_of(&NetError::Unreachable("refusée".into())),
            Field::Server
        );
        assert_eq!(field_of(&NetError::Timeout), Field::Server);
        assert_eq!(field_of(&NetError::BadAddress("x".into())), Field::Server);
        assert_eq!(field_of(&NetError::Local("fichier".into())), Field::General);
    }

    #[test]
    fn a_failure_keeps_its_field_and_a_note_is_general() {
        let mut screen = ConnectScreen::new();
        screen.failed(&NetError::Timeout);
        assert_eq!(screen.message_field, Field::Server);
        screen.note(Some("Version incompatible.".into()));
        assert_eq!(screen.message_field, Field::General);
        screen.logged_out(None, false, Some("Votre session a expiré.".into()));
        assert_eq!(screen.message_field, Field::General);
        assert_eq!(screen.message_text(), Some("Votre session a expiré."));
    }
}
