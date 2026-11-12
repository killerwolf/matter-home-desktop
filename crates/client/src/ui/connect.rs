//! Écran de connexion : adresse du serveur, identifiant, mot de passe ; confirmation de
//! l'empreinte du certificat à la première connexion ; refus clair si le certificat a changé.
//!
//! Le mot de passe n'est gardé en mémoire que le temps de la saisie, puis effacé dès qu'il est
//! envoyé ; il n'est jamais écrit sur disque ni dans le journal.

use eframe::egui::{self, RichText};
use protocol::wire::ErrorCode;

use super::theme;
use matter_home_client::net::tls::normalize_fingerprint;
use matter_home_client::net::{Backend, Link, NetError, ServerAddr};

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
        }
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
        self.password.clear();
        self.expected.clear();
        self.stage = Stage::Form;
        self.message = None;
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
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(theme::tokens().space.xl * 2.0);
                ui.heading("Maison");
                ui.label(
                    RichText::new("Connexion à votre serveur Matter Home")
                        .color(theme::palette(ui).text_secondary),
                );
                ui.add_space(theme::tokens().space.lg);
            });
            let width = 400.0_f32.min(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| match link {
                        Link::CertificateChanged {
                            url,
                            expected,
                            found,
                        } => {
                            self.show_message(ui);
                            self.certificate_changed(ui, backend, url, expected, found);
                        }
                        Link::Incompatible { message } => {
                            self.show_message(ui);
                            Self::incompatible(ui, backend, message);
                        }
                        _ => self.form(ui, backend),
                    },
                );
            });
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
        let busy = matches!(self.stage, Stage::Probing | Stage::LoggingIn);
        let p = theme::palette(ui);
        let error = |ui: &mut egui::Ui, which: Field| {
            if let Some((false, text)) = message.as_ref().filter(|_| at == Some(which)) {
                ui.colored_label(p.error, RichText::new(text).small());
            }
        };
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.label(RichText::new(text).color(p.text_secondary));
        };
        let full = ui.available_width();

        label(ui, "Adresse du serveur");
        ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.url)
                .hint_text("192.168.1.20:8443 ou mac-mini.local")
                .desired_width(full),
        );
        error(ui, Field::Server);
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Elle s'affiche sur l'autre Mac : menu du serveur, « Connecter un client ».",
            )
            .small()
            .color(p.text_secondary),
        );
        ui.add_space(theme::tokens().space.sm);

        label(ui, "Adresse à distance (facultatif)");
        ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.remote)
                .hint_text("mac-mini.tail1234.ts.net ou 100.x.y.z")
                .desired_width(full),
        );
        error(ui, Field::Remote);
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Le même serveur, joint par Tailscale quand vous n'êtes pas chez vous. \
                 L'app essaie l'adresse ci-dessus, puis celle-ci. Plusieurs : séparez par un espace.",
            )
            .small()
            .color(p.text_secondary),
        );
        if ServerAddr::parse_list(&self.remote)
            .is_ok_and(|l| l.iter().any(ServerAddr::is_tailscale_serve))
        {
            ui.label(
                RichText::new(
                    "Vérifiée par le certificat public de Tailscale : vérifiez bien le nom de \
                     votre Mac.",
                )
                .small()
                .color(p.text_secondary),
            );
        }
        ui.add_space(theme::tokens().space.md);

        label(ui, "Identifiant");
        ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.username).desired_width(full),
        );
        ui.add_space(theme::tokens().space.sm);

        label(ui, "Mot de passe");
        let password = ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.password)
                .password(true)
                .desired_width(full),
        );
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
        ui.add_space(theme::tokens().space.md);
        // Note de sécurité : une ligne, dépliable.
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("La connexion est chiffrée ; le mot de passe n'est jamais conservé.")
                    .small()
                    .color(p.text_secondary),
            );
            let text = if self.show_security {
                "Masquer"
            } else {
                "Détails"
            };
            if ui.small_button(text).clicked() {
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
                theme::palette(ui).error,
                "Cette empreinte est différente de celle que vous avez saisie : n'approuvez pas.",
            );
        } else if !self.expected.trim().is_empty() {
            ui.colored_label(theme::palette(ui).ok, "Identique à l'empreinte saisie.");
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
            theme::palette(ui).error,
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
        ui.colored_label(
            theme::palette(ui).error,
            RichText::new("Versions incompatibles").heading(),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

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
