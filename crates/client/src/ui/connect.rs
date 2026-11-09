//! Écran de connexion : adresse du serveur, identifiant, mot de passe ; confirmation de
//! l'empreinte du certificat à la première connexion ; refus clair si le certificat a changé.
//!
//! Le mot de passe n'est gardé en mémoire que le temps de la saisie, puis effacé dès qu'il est
//! envoyé ; il n'est jamais écrit sur disque ni dans le journal.

use eframe::egui::{self, Color32, RichText};

use matter_home_client::net::tls::normalize_fingerprint;
use matter_home_client::net::{Backend, Link, ServerAddr};

const RED: Color32 = Color32::from_rgb(210, 60, 50);
const GREEN: Color32 = Color32::from_rgb(40, 160, 70);

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
    username: String,
    password: String,
    /// Adresse déjà approuvée (fichier de connexion) : pas de nouvelle empreinte à confirmer.
    pinned_url: Option<String>,
    stage: Stage,
    /// Empreinte attendue, collée par l'utilisateur pour la comparer (facultatif).
    expected: String,
    message: Option<(bool, String)>,
    /// Case à cocher avant de réinitialiser l'empreinte épinglée.
    confirm_reset: bool,
}

impl ConnectScreen {
    pub fn new() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            password: String::new(),
            pinned_url: None,
            stage: Stage::Form,
            expected: String::new(),
            message: None,
            confirm_reset: false,
        }
    }

    /// Retour à l'écran de connexion (démarrage, déconnexion, jeton expiré).
    pub fn logged_out(&mut self, url: Option<String>, pinned: bool, reason: Option<String>) {
        self.password.clear();
        self.stage = Stage::Form;
        self.pinned_url = url.clone().filter(|_| pinned);
        if let Some(url) = url {
            self.url = url;
        }
        self.message = reason.map(|r| (false, r));
        self.confirm_reset = false;
    }

    /// Connexion réussie : plus rien à garder.
    pub fn connected(&mut self) {
        self.password.clear();
        self.expected.clear();
        self.stage = Stage::Form;
        self.message = None;
    }

    pub fn probed(&mut self, url: String, fingerprint: String) {
        self.stage = Stage::Confirm { url, fingerprint };
    }

    pub fn failed(&mut self, message: String) {
        self.password.clear();
        self.stage = Stage::Form;
        self.message = Some((false, message));
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
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("Matter Home Desktop");
                ui.label("Connexion à votre serveur Matter Home");
                ui.add_space(16.0);
            });
            let width = 460.0_f32.min(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.allocate_ui(egui::vec2(width, 0.0), |ui| match link {
                    Link::CertificateChanged {
                        url,
                        expected,
                        found,
                    } => self.certificate_changed(ui, backend, url, expected, found),
                    Link::Incompatible { message } => Self::incompatible(ui, backend, message),
                    _ => self.form(ui, backend),
                });
            });
        });
    }

    fn form(&mut self, ui: &mut egui::Ui, backend: &Backend) {
        if let Some((ok, text)) = &self.message {
            ui.colored_label(if *ok { GREEN } else { RED }, text);
            ui.add_space(8.0);
        }
        if let Stage::Confirm { url, fingerprint } = &self.stage {
            let (url, fingerprint) = (url.clone(), fingerprint.clone());
            self.confirm(ui, backend, &url, &fingerprint);
            return;
        }
        let busy = matches!(self.stage, Stage::Probing | Stage::LoggingIn);
        egui::Grid::new("connect_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Serveur :");
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.url)
                        .hint_text("192.168.1.20:8443 ou mac-mini.local")
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label("Identifiant :");
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.username).desired_width(300.0),
                );
                ui.end_row();
                ui.label("Mot de passe :");
                let password = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(300.0),
                );
                ui.end_row();
                let enter = password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label("");
                ui.horizontal(|ui| {
                    let ready = !self.url.trim().is_empty()
                        && !self.username.trim().is_empty()
                        && !self.password.is_empty();
                    let go = ui
                        .add_enabled(!busy && ready, egui::Button::new("Se connecter"))
                        .clicked()
                        || (enter && !busy && ready);
                    if busy {
                        ui.spinner();
                    }
                    if go {
                        self.submit(backend);
                    }
                });
                ui.end_row();
            });
        ui.add_space(12.0);
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
        ui.add_space(12.0);
        ui.small(
            "La connexion est toujours chiffrée. Le mot de passe n'est jamais conservé : seuls \
             l'adresse, l'empreinte du certificat et un jeton de session sont gardés sur ce Mac.",
        );
    }

    fn submit(&mut self, backend: &Backend) {
        self.message = None;
        let url = self.url.trim().to_string();
        if let Err(e) = ServerAddr::parse(&url) {
            self.message = Some((false, e.message()));
            return;
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
        ui.heading("Approuver ce serveur ?");
        ui.label(format!("Première connexion à {url}."));
        ui.add_space(6.0);
        ui.label(
            "Le serveur présente un certificat qu'il a fabriqué lui-même. Comparez son empreinte \
             avec celle affichée au lancement du serveur (ou par la commande \
             « matter-home-server fingerprint »). Si elles sont identiques, approuvez : seul ce \
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
        let mismatch = !self.expected.trim().is_empty()
            && normalize_fingerprint(&self.expected).as_deref() != Some(fingerprint);
        if mismatch {
            ui.colored_label(
                RED,
                "Cette empreinte est différente de celle que vous avez saisie : n'approuvez pas.",
            );
        } else if !self.expected.trim().is_empty() {
            ui.colored_label(GREEN, "Identique à l'empreinte saisie.");
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!mismatch, egui::Button::new("Approuver et se connecter"))
                .clicked()
            {
                backend.login(url, fingerprint, self.username.trim(), &self.password);
                self.password.clear();
                self.expected.clear();
                self.stage = Stage::LoggingIn;
            }
            if ui.button("Annuler").clicked() {
                self.password.clear();
                self.expected.clear();
                self.stage = Stage::Form;
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
            RED,
            RichText::new("Le certificat du serveur a changé").heading(),
        );
        ui.add_space(8.0);
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
            if ui.button("Réessayer").clicked() {
                backend.reconnect();
            }
        });
    }

    fn incompatible(ui: &mut egui::Ui, backend: &Backend, message: &str) {
        ui.colored_label(RED, RichText::new("Versions incompatibles").heading());
        ui.add_space(8.0);
        ui.label(message);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Réessayer").clicked() {
                backend.reconnect();
            }
            if ui.button("Changer de serveur").clicked() {
                backend.logout();
            }
        });
    }
}
