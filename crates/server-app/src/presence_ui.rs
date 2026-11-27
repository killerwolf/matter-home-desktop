//! Le dessin de Réglages › Présence (maquette Serveur-Box) et du choix du téléphone d'un compte
//! (carte « Téléphone relié » de Serveur-Compte-Fiche) : la box de la maison, l'interrupteur
//! « Changer le mode de la maison selon la présence », les téléphones reliés et les téléphones
//! libres. La logique et les phrases sont dans `ostal_server_app::presence`.
//!
//! Se relier à la box, changer son mot de passe, la délier et relever le Wi-Fi attendent la box
//! (jusqu'à une dizaine de secondes) : cela se fait dans un fil à part, la fenêtre ne se fige pas.
//! Le mot de passe saisi n'est gardé en mémoire que le temps de la saisie, et jamais réaffiché.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use ostal_server::audit::Who;
use ostal_server::extensions::bbox::DEFAULT_URL;
use ostal_server::hub::Hub;
use ostal_server::net::Auth;
use ostal_server::security::account::User;
use ostal_server_app::accounts;
use ostal_server_app::confirm::{self, Confirmation};
use ostal_server_app::controller::Notice;
use ostal_server_app::presence::{self, Snapshot};
use ostal_theme as theme;

use crate::ui::widgets;

const RELOAD_EVERY: Duration = Duration::from_secs(2);

/// Le choix du téléphone d'un compte, dans un petit encadré : les appareils libres (noms seulement),
/// celui qu'il a déjà, ou « Aucun téléphone ».
#[derive(Default)]
pub struct PhonePicker {
    /// Le compte dont on choisit le téléphone.
    user_id: Option<String>,
    /// `None` : aucun téléphone.
    choice: Option<String>,
    error: Option<String>,
}

impl PhonePicker {
    pub fn is_open_for(&self, user_id: &str) -> bool {
        self.user_id.as_deref() == Some(user_id)
    }

    pub fn open(&mut self, user_id: &str, current: Option<String>) {
        self.user_id = Some(user_id.to_string());
        self.choice = current;
        self.error = None;
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Dessine l'encadré ; rend `true` quand le téléphone vient d'être enregistré.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        hub: &Hub,
        user: &User,
        current: Option<&str>,
    ) -> bool {
        let p = theme::palette(ui);
        let mut names = hub.wifi().free_hosts(false);
        if let Some(c) = current {
            names.insert(0, c.to_string());
        }
        ui.add_space(4.0);
        ui.label("Choisissez son téléphone parmi ceux vus sur le Wi-Fi.");
        if names.is_empty() {
            widgets::hint(ui, "Aucun téléphone libre vu sur le Wi-Fi pour l'instant.");
        }
        for name in &names {
            let selected = self
                .choice
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(name));
            if ui.radio(selected, name).clicked() {
                self.choice = Some(name.clone());
            }
        }
        if ui.radio(self.choice.is_none(), "Aucun téléphone").clicked() {
            self.choice = None;
        }
        if let Some(e) = &self.error {
            ui.colored_label(p.error, e);
        }
        let (mut save, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            cancel = ui.button("Annuler").clicked();
            save = ui.button("Enregistrer").clicked();
        });
        if cancel {
            self.close();
        } else if save {
            match hub.wifi_assign(
                &user.id,
                self.choice.as_deref(),
                false,
                &Who::server_app(),
                &user.display_name,
            ) {
                Ok(_) => {
                    self.close();
                    return true;
                }
                Err(e) => {
                    self.error = Some(
                        match e {
                            ostal_server::wifi::AssignError::NotFree => {
                                "Ce téléphone n'est plus vu sur le Wi-Fi."
                            }
                            ostal_server::wifi::AssignError::Taken => {
                                "Ce téléphone a déjà un propriétaire."
                            }
                            ostal_server::wifi::AssignError::Storage => {
                                "Le choix n'a pas pu être enregistré."
                            }
                        }
                        .into(),
                    );
                }
            }
        }
        false
    }
}

/// « Téléphone relié » de la fiche d'un compte (maquette Serveur-Compte-Fiche).
pub fn phone_card(
    ui: &mut egui::Ui,
    picker: &mut PhonePicker,
    hub: Option<&Arc<Hub>>,
    user: &User,
) {
    let Some(hub) = hub else { return };
    let p = theme::palette(ui);
    let current = hub.wifi().phone_of(&user.id);
    let status = hub.wifi().status();
    widgets::section(ui, "Téléphone relié", |ui| {
        if !status.linked && current.is_none() {
            ui.label("Aucune box n'est reliée. Reliez-la dans Réglages › Présence pour savoir qui est à la maison.");
            return;
        }
        ui.horizontal(|ui| {
            match &current {
                Some(name) => {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).strong());
                        let declared = hub
                            .declared_presence()
                            .into_iter()
                            .find(|d| d.user_id == user.id);
                        let mut line = presence::presence_text(declared.as_ref());
                        if hub.wifi().is_seen(name) {
                            if let Some(at) = status.last_ok {
                                line.push_str(&format!(
                                    " · vu sur le Wi-Fi à {}",
                                    presence::clock_text(at)
                                ));
                            }
                        }
                        ui.label(RichText::new(line).small().color(p.text_secondary));
                    });
                }
                None => {
                    ui.label(RichText::new("Pas de téléphone relié").color(p.text_secondary));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !picker.is_open_for(&user.id)
                    && ui
                        .add_enabled(status.linked, egui::Button::new("Changer"))
                        .clicked()
                {
                    picker.open(&user.id, current.clone());
                }
            });
        });
        if picker.is_open_for(&user.id) {
            picker.show(ui, hub, user, current.as_deref());
        }
        widgets::hint(
            ui,
            "Ostal sait qui est à la maison grâce au Wi-Fi de la box. Seul le nom du téléphone est gardé, jamais son adresse.",
        );
    });
}

enum Job {
    Link,
    Unlink,
    Poll,
}

struct Done {
    job: Job,
    result: Result<(), String>,
}

#[derive(Default)]
pub struct PresenceUi {
    open: bool,
    users: Vec<User>,
    loaded_at: Option<Instant>,
    snapshot: Option<Snapshot>,
    url: String,
    password: String,
    /// « Changer le mot de passe… » ou la première liaison : le formulaire est ouvert.
    editing: bool,
    job: Option<Receiver<Done>>,
    notice: Option<Notice>,
    picker: PhonePicker,
    /// Un téléphone libre qu'on veut attribuer : on choisit d'abord la personne.
    assigning: Option<String>,
}

impl PresenceUi {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Une saisie est en cours (le mot de passe) : on ne la jette pas sans prévenir.
    pub fn editing(&self) -> bool {
        self.open && self.editing
    }

    pub fn open(&mut self) {
        self.reset();
        self.open = true;
    }

    /// Ferme la page et efface le mot de passe saisi.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        job: Job,
        run: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done { job, result: run() });
            ctx.request_repaint();
        });
        self.notice = None;
        self.job = Some(rx);
    }

    fn poll(&mut self) {
        let Some(rx) = &self.job else { return };
        let done = match rx.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                return;
            }
        };
        self.job = None;
        match (done.job, done.result) {
            (Job::Link, Ok(())) => {
                self.editing = false;
                self.notice = Some(Notice::Ok("La box est reliée.".into()));
            }
            (Job::Unlink, Ok(())) => {
                self.editing = false;
                self.notice = Some(Notice::Ok(
                    "La box est déliée : le mode automatique est désactivé.".into(),
                ));
            }
            (Job::Poll, Ok(())) => self.notice = Some(Notice::Ok("Wi-Fi relevé.".into())),
            (_, Err(e)) => self.notice = Some(Notice::Err(e)),
        }
        self.loaded_at = None;
    }

    /// Dessine la page. Rend `true` si l'utilisateur veut revenir aux Réglages.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        data_dir: &Path,
        auth: Option<&Arc<Auth>>,
        hub: Option<&Arc<Hub>>,
    ) -> bool {
        self.poll();
        let p = theme::palette(ui);
        let mut leave = false;
        ui.horizontal(|ui| {
            leave = ui.link("‹ Réglages").clicked();
            ui.label(RichText::new("›").color(p.text_secondary));
            ui.label(RichText::new("Présence").color(p.text_secondary));
        });
        let Some(hub) = hub else {
            ui.add_space(8.0);
            ui.label("La présence se règle quand le serveur tourne.");
            return leave;
        };
        if self.loaded_at.is_none_or(|at| at.elapsed() > RELOAD_EVERY) && self.job.is_none() {
            if let Ok(users) = accounts::list(data_dir) {
                self.users = users;
            }
            self.loaded_at = Some(Instant::now());
        }
        let snap = presence::snapshot(hub, &self.users);
        self.snapshot = Some(snap.clone());
        ctx.request_repaint_after(RELOAD_EVERY);
        if self.url.is_empty() {
            self.url = hub
                .extensions()
                .config(ostal_server::extensions::bbox::ID)
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or(DEFAULT_URL)
                .to_string();
        }
        let busy = self.job.is_some();
        ui.add_space(4.0);
        ui.heading("Présence, box et téléphones");
        ui.add_space(6.0);
        self.box_card(ui, ctx, hub, auth, &snap, busy);
        self.auto_card(ui, hub, &snap, busy);
        self.phones_card(ui, hub, &snap);
        self.free_card(ui, hub, &snap);
        if busy {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Un instant…");
            });
        }
        if let Some(notice) = &self.notice {
            ui.add_space(6.0);
            match notice {
                Notice::Ok(t) => ui.colored_label(p.ok, t),
                Notice::Err(t) => ui.colored_label(p.error, t),
            };
        }
        leave
    }

    fn box_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        hub: &Arc<Hub>,
        auth: Option<&Arc<Auth>>,
        snap: &Snapshot,
        busy: bool,
    ) {
        let p = theme::palette(ui);
        let (good, line) = presence::link_line(&snap.status);
        let (mut change, mut unlink, mut poll) = (false, false, false);
        let (mut submit, mut cancel) = (false, false);
        widgets::section(ui, "Box de la maison", |ui| {
            ui.colored_label(if good { p.ok } else { p.warn }, line);
            widgets::hint(
                ui,
                "La box est l'extension « Bbox » : ses réglages (adresse, mot de passe) sont aussi dans l'app web, Réglages › Extensions › Bbox.",
            );
            if snap.status.linked && !self.editing {
                widgets::hint(
                    ui,
                    "Le mot de passe de la box est gardé par Ostal Server, il n'est jamais réaffiché.",
                );
                ui.horizontal(|ui| {
                    change = ui
                        .add_enabled(!busy, egui::Button::new("Changer le mot de passe…"))
                        .clicked();
                    unlink = ui
                        .add_enabled(!busy, egui::Button::new("Délier la box…"))
                        .clicked();
                    poll = ui
                        .add_enabled(
                            !busy && auth.is_some(),
                            egui::Button::new("Relever maintenant"),
                        )
                        .clicked();
                });
            } else {
                if !snap.status.linked {
                    ui.label("Ostal Server lit la liste des appareils connectés à la box pour savoir quels téléphones sont à la maison. Il ne modifie rien sur la box.");
                }
                ui.label("Adresse de la box");
                ui.add_enabled(
                    !snap.status.linked && !busy,
                    egui::TextEdit::singleline(&mut self.url),
                );
                ui.label("Mot de passe d'administration de la box");
                widgets::hint(
                    ui,
                    "Pour la Bbox : celui de sa page d'administration (mabbox.bytel.fr), pas celui du Wi-Fi.",
                );
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.horizontal(|ui| {
                    if snap.status.linked {
                        cancel = ui
                            .add_enabled(!busy, egui::Button::new("Annuler"))
                            .clicked();
                    }
                    submit = ui
                        .add_enabled(
                            !busy && !self.password.is_empty(),
                            egui::Button::new(if snap.status.linked {
                                "Enregistrer le mot de passe"
                            } else {
                                "Relier la box"
                            }),
                        )
                        .clicked();
                });
            }
        });
        if change {
            self.editing = true;
            self.notice = None;
        } else if cancel {
            self.editing = false;
            self.password.clear();
        } else if submit {
            let password = std::mem::take(&mut self.password);
            let (url, hub) = (self.url.clone(), hub.clone());
            self.spawn(ctx, Job::Link, move || {
                hub.wifi_link(&url, &password, &Who::server_app())
                    .map_err(|e| e.message().to_string())
            });
        } else if unlink && confirm::ask(&Confirmation::UnlinkBox) {
            let hub = hub.clone();
            self.url.clear();
            self.spawn(ctx, Job::Unlink, move || {
                hub.wifi_unlink(&Who::server_app())
                    .map_err(|e| e.message().to_string())
            });
        } else if poll {
            if let Some(auth) = auth {
                let (hub, auth) = (hub.clone(), auth.clone());
                self.spawn(ctx, Job::Poll, move || {
                    hub.wifi_poll(&auth.accounts, true);
                    Ok(())
                });
            }
        }
        ui.add_space(8.0);
    }

    fn auto_card(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>, snap: &Snapshot, busy: bool) {
        let mut on = snap.status.auto_mode;
        widgets::card(ui, |ui| {
            let changed = ui
                .add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut on, "Changer le mode de la maison selon la présence"),
                )
                .changed();
            widgets::hint(
                ui,
                "Absent quand plus personne n'est là, Présent dès qu'une personne arrive. Nuit et Vacances restent à la main. Éteint, « Je pars » et « Je rentre » changent le mode directement. Marche aussi sans box, avec les seuls raccourcis.",
            );
            ui.label(&snap.who_is_home);
            if changed {
                if let Err(e) = hub.wifi_set_auto_mode(on, &Who::server_app()) {
                    self.notice = Some(Notice::Err(e.to_string()));
                }
            }
        });
        ui.add_space(8.0);
    }

    fn phones_card(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>, snap: &Snapshot) {
        let p = theme::palette(ui);
        let mut change: Option<(String, Option<String>)> = None;
        widgets::section(ui, "Téléphones reliés", |ui| {
            if snap.rows.is_empty() {
                ui.label(RichText::new("Aucun compte.").color(p.text_secondary));
            }
            egui::Grid::new("phones")
                .num_columns(4)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    for h in ["Personne", "Téléphone", "Présence", "Action"] {
                        ui.label(RichText::new(h).small().color(p.text_secondary));
                    }
                    ui.end_row();
                    for row in &snap.rows {
                        ui.label(RichText::new(&row.name).strong());
                        ui.label(row.phone.clone().unwrap_or_else(|| "—".into()));
                        ui.label(&row.state);
                        if ui
                            .add_enabled(
                                snap.status.linked,
                                egui::Button::new(if row.phone.is_some() {
                                    "Changer"
                                } else {
                                    "Attribuer…"
                                }),
                            )
                            .clicked()
                        {
                            change = Some((row.user_id.clone(), row.phone.clone()));
                        }
                        ui.end_row();
                    }
                });
            if let Some(user) = self
                .picker
                .user_id
                .clone()
                .and_then(|id| self.users.iter().find(|u| u.id == id).cloned())
            {
                ui.separator();
                ui.label(RichText::new(format!("Téléphone de {}", user.display_name)).strong());
                let current = hub.wifi().phone_of(&user.id);
                self.picker.show(ui, hub, &user, current.as_deref());
            }
        });
        if let Some((id, current)) = change {
            self.picker.open(&id, current);
        }
        ui.add_space(8.0);
    }

    fn free_card(&mut self, ui: &mut egui::Ui, hub: &Arc<Hub>, snap: &Snapshot) {
        let p = theme::palette(ui);
        let mut pick: Option<String> = None;
        widgets::section(ui, "Téléphones libres sur le Wi-Fi", |ui| {
            if snap.free.is_empty() {
                ui.label(
                    RichText::new("Aucun appareil libre vu sur le Wi-Fi.").color(p.text_secondary),
                );
            }
            for name in &snap.free {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Sans propriétaire")
                            .small()
                            .color(p.text_secondary),
                    );
                    ui.label(RichText::new(name).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Attribuer…").clicked() {
                            pick = Some(name.clone());
                        }
                    });
                });
            }
            if let Some(name) = &self.assigning {
                ui.separator();
                ui.label(format!("À qui est « {name} » ?"));
                let mut done = false;
                for user in self.users.iter().filter(|u| u.active) {
                    if ui.button(&user.display_name).clicked() {
                        match hub.wifi_assign(
                            &user.id,
                            Some(name),
                            false,
                            &Who::server_app(),
                            &user.display_name,
                        ) {
                            Ok(_) => done = true,
                            Err(_) => {
                                self.notice = Some(Notice::Err(
                                    "Ce téléphone n'est plus libre ou plus vu sur le Wi-Fi.".into(),
                                ));
                                done = true;
                            }
                        }
                    }
                }
                if ui.button("Annuler").clicked() || done {
                    self.assigning = None;
                }
            }
            widgets::hint(ui, presence::PRIVACY_NOTE);
        });
        if pick.is_some() {
            self.assigning = pick;
        }
    }
}
