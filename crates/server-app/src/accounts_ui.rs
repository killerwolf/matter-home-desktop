//! Le dessin de Réglages › Comptes (maquettes Serveur-Comptes, Serveur-Compte-Ajouter,
//! Serveur-Compte-Fiche) : la liste des comptes du foyer, « Ajouter un compte » et la fiche d'un
//! compte (nouveau mot de passe, désactivation). La logique est dans `ostal_server_app::accounts`.
//!
//! Le calcul d'un mot de passe (argon2) se fait dans un fil à part : la fenêtre ne se fige pas.
//!
//! Les connexions (maquettes Serveur-Connexions et Serveur-Compte-Fiche) : la colonne
//! « Connexions » de la liste, la page « Toutes les connexions » et la carte « Connexions de … »
//! de la fiche, chacune avec « Couper ». Elles se lisent dans les sessions du serveur qui tourne.
//!
//! Une place est gardée pour la suite, sans rien dessiner d'ici là : [`AccountsUi::phone_section`],
//! « Téléphone relié » de la fiche (présence, étape 6).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use ostal_server::hub::Hub;
use ostal_server::net::Auth;
use ostal_server::security::account::User;
use ostal_server_app::accounts::{self, ConnectionRow, NewAccount};
use ostal_server_app::confirm::{self, Confirmation};
use ostal_server_app::controller::Notice;
use ostal_theme as theme;

use crate::ui::widgets;

/// Les comptes sont relus à cette cadence tant que la page est ouverte : une commande lancée
/// dans le Terminal (`ostal-server user …`) apparaît toute seule.
const RELOAD_EVERY: Duration = Duration::from_secs(2);

#[derive(Default, Clone, PartialEq, Eq)]
enum View {
    #[default]
    Closed,
    List,
    Add,
    /// La fiche d'un compte (son identifiant interne).
    Detail(String),
    /// Toutes les connexions de la maison.
    Connections,
}

/// Ce que fait le fil d'arrière-plan en cours.
enum Job {
    Add,
    Password,
    Active(bool),
}

struct Done {
    job: Job,
    result: Result<User, String>,
}

#[derive(Default)]
struct PasswordForm {
    open: bool,
    password: String,
    confirm: String,
}

#[derive(Default)]
pub struct AccountsUi {
    view: View,
    users: Vec<User>,
    loaded_at: Option<Instant>,
    load_error: Option<String>,
    add: NewAccount,
    password: PasswordForm,
    job: Option<Receiver<Done>>,
    notice: Option<Notice>,
    /// Les connexions ouvertes, relues à chaque image (les sessions sont en mémoire).
    rows: Vec<ConnectionRow>,
    /// « Couper » demandé pendant le dessin : (nom, identifiant), traité à la fin de l'image.
    cut: Option<(String, String)>,
}

impl AccountsUi {
    /// La page est ouverte (liste, ajout ou fiche).
    pub fn is_open(&self) -> bool {
        self.view != View::Closed
    }

    /// Une saisie est en cours : on ne la jette pas sans prévenir.
    pub fn editing(&self) -> bool {
        self.view == View::Add || self.password.open
    }

    /// Ouvre la liste des comptes.
    pub fn open(&mut self) {
        self.reset();
        self.view = View::List;
    }

    /// Ferme la page et efface toute saisie (aucun mot de passe ne reste en mémoire).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn reload(&mut self, data_dir: &Path) {
        match accounts::list(data_dir) {
            Ok(users) => {
                self.users = users;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(e),
        }
        self.loaded_at = Some(Instant::now());
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        data_dir: &Path,
        job: Job,
        run: impl FnOnce(PathBuf) -> Result<User, String> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let (ctx, data_dir) = (ctx.clone(), data_dir.to_path_buf());
        std::thread::spawn(move || {
            let _ = tx.send(Done {
                job,
                result: run(data_dir),
            });
            ctx.request_repaint();
        });
        self.notice = None;
        self.job = Some(rx);
    }

    fn poll(&mut self, data_dir: &Path) {
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
        self.reload(data_dir);
        match (done.job, done.result) {
            (Job::Add, Ok(user)) => {
                self.add = NewAccount::default();
                self.view = View::List;
                self.notice = Some(Notice::Ok(format!(
                    "Compte de {} créé. Donnez-lui son identifiant et son mot de passe.",
                    user.display_name
                )));
            }
            (Job::Password, Ok(user)) => {
                self.password = PasswordForm::default();
                self.notice = Some(Notice::Ok(format!(
                    "Mot de passe de {} changé. Ses connexions ouvertes sont fermées.",
                    user.display_name
                )));
            }
            (Job::Active(false), Ok(user)) => {
                self.notice = Some(Notice::Ok(format!(
                    "Compte de {} désactivé. Ses connexions sont fermées.",
                    user.display_name
                )));
            }
            (Job::Active(true), Ok(user)) => {
                self.notice = Some(Notice::Ok(format!(
                    "Compte de {} réactivé.",
                    user.display_name
                )));
            }
            (_, Err(e)) => self.notice = Some(Notice::Err(e)),
        }
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
        self.poll(data_dir);
        let (now, offset) = accounts::clock();
        self.rows = auth
            .map(|a| accounts::connection_rows(a, &self.users, now, offset))
            .unwrap_or_default();
        if self.loaded_at.is_none_or(|at| at.elapsed() > RELOAD_EVERY) && self.job.is_none() {
            self.reload(data_dir);
        }
        ctx.request_repaint_after(RELOAD_EVERY);
        let busy = self.job.is_some();
        let mut leave = false;
        match self.view.clone() {
            View::Closed => leave = true,
            View::List => leave = self.list(ui, busy),
            View::Add => self.add_form(ui, ctx, data_dir, busy),
            View::Detail(id) => self.detail(ui, ctx, data_dir, &id, busy),
            View::Connections => leave = self.connections_page(ui),
        }
        if let Some((name, id)) = self.cut.take() {
            self.cut_connection(auth, hub, &name, &id);
        }
        if busy {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Un instant…");
            });
        }
        leave
    }

    fn show_notice(&self, ui: &mut egui::Ui) {
        if let Some(notice) = &self.notice {
            ui.add_space(6.0);
            let p = theme::palette(ui);
            match notice {
                Notice::Ok(t) => ui.colored_label(p.ok, t),
                Notice::Err(t) => ui.colored_label(p.error, t),
            };
        }
    }

    /// Coupe une connexion après confirmation, et le dit.
    fn cut_connection(
        &mut self,
        auth: Option<&Arc<Auth>>,
        hub: Option<&Arc<Hub>>,
        name: &str,
        id: &str,
    ) {
        let Some(auth) = auth else { return };
        if !confirm::ask(&Confirmation::CutConnection { name: name.into() }) {
            return;
        }
        self.notice = Some(
            match accounts::cut_connection(auth, hub.map(|h| &**h), id) {
                Ok(true) => Notice::Ok(format!("Connexion « {name} » coupée.")),
                Ok(false) => Notice::Err("Cette connexion était déjà fermée.".into()),
                Err(e) => Notice::Err(format!(
                "Connexion « {name} » fermée, mais ses raccourcis n'ont pas pu être coupés : {e}"
            )),
            },
        );
    }

    /// « Réglages › Comptes » : « Réglages » ramène en arrière.
    fn breadcrumb(ui: &mut egui::Ui, back_to: &str, here: &str) -> bool {
        let p = theme::palette(ui);
        let mut back = false;
        ui.horizontal(|ui| {
            back = ui.link(format!("‹ {back_to}")).clicked();
            ui.label(RichText::new("›").color(p.text_secondary));
            ui.label(RichText::new(here).color(p.text_secondary));
        });
        back
    }

    // ---- la liste --------------------------------------------------------------------------

    fn list(&mut self, ui: &mut egui::Ui, busy: bool) -> bool {
        let p = theme::palette(ui);
        let leave = Self::breadcrumb(ui, "Réglages", "Comptes");
        ui.add_space(4.0);
        let mut add = false;
        ui.horizontal(|ui| {
            ui.heading("Comptes du foyer");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                add = ui
                    .add_enabled(!busy, egui::Button::new("+ Ajouter un compte"))
                    .clicked();
            });
        });
        ui.label("Chacun a son identifiant, son téléphone et ses connexions. Tout le monde a les mêmes droits pour cette version.");
        ui.add_space(8.0);
        if let Some(e) = &self.load_error {
            ui.colored_label(p.error, e);
        }
        let mut open: Option<String> = None;
        widgets::card(ui, |ui| {
            if self.users.is_empty() && self.load_error.is_none() {
                ui.label("Aucun compte pour l'instant.");
            }
            for (i, user) in self.users.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    avatar(ui, user, 34.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&user.display_name).strong());
                        ui.label(
                            RichText::new(format!("identifiant : {}", user.username))
                                .small()
                                .color(p.text_secondary),
                        );
                    });
                    let count = self.rows.iter().filter(|r| r.user_id == user.id).count();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::secondary_button(ui, "Modifier").clicked() {
                            open = Some(user.id.clone());
                        }
                        ui.label(
                            RichText::new(accounts::count_label(count)).color(p.text_secondary),
                        );
                        if user.active {
                            theme::state_word(ui, theme::StateWord::Ok, "actif");
                        } else {
                            theme::state_word(ui, theme::StateWord::Neutral, "désactivé");
                        }
                    });
                });
            }
        });
        let mut all = false;
        if !self.users.is_empty() {
            all = ui
                .link(format!(
                    "Voir toutes les connexions ({}) ›",
                    self.rows.len()
                ))
                .clicked();
        }
        self.show_notice(ui);
        if all {
            self.notice = None;
            self.view = View::Connections;
        }
        if add {
            self.add = NewAccount::default();
            self.notice = None;
            self.view = View::Add;
        }
        if let Some(id) = open {
            self.password = PasswordForm::default();
            self.notice = None;
            self.view = View::Detail(id);
        }
        leave
    }

    // ---- ajouter un compte -----------------------------------------------------------------

    fn add_form(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, data_dir: &Path, busy: bool) {
        let leave = Self::breadcrumb(ui, "Comptes", "Ajouter un compte");
        ui.add_space(4.0);
        ui.heading("Ajouter un compte");
        ui.label("Vous créez le compte ici, puis vous donnez l'identifiant et le mot de passe à la personne. Il n'y a pas d'inscription depuis l'app.");
        ui.add_space(8.0);
        ui.label("Nom affiché");
        ui.text_edit_singleline(&mut self.add.display_name);
        widgets::hint(ui, "Le prénom que voient les autres dans la maison.");
        ui.add_space(4.0);
        ui.label("Identifiant");
        ui.text_edit_singleline(&mut self.add.username);
        widgets::hint(
            ui,
            "Sert à se connecter sur son téléphone ou son ordinateur.",
        );
        ui.add_space(4.0);
        ui.label("Mot de passe");
        ui.add(egui::TextEdit::singleline(&mut self.add.password).password(true));
        widgets::hint(ui, "10 caractères au minimum.");
        ui.add_space(4.0);
        ui.label("Confirmez le mot de passe");
        ui.add(egui::TextEdit::singleline(&mut self.add.confirm).password(true));
        let check = self.add.check();
        // La raison du bouton grisé se lit à côté : en gris tant que rien n'est écrit, en rouge
        // ensuite.
        match &check {
            Ok(()) => widgets::hint(ui, "Les deux mots de passe sont identiques."),
            Err(e) => {
                let started = !(self.add.display_name.is_empty()
                    && self.add.username.is_empty()
                    && self.add.password.is_empty());
                if started {
                    ui.colored_label(theme::palette(ui).error, e);
                } else {
                    widgets::hint(ui, e);
                }
            }
        }
        self.show_notice(ui);
        ui.add_space(8.0);
        let (mut cancel, mut create) = (false, false);
        ui.horizontal(|ui| {
            cancel = ui
                .add_enabled(!busy, egui::Button::new("Annuler"))
                .clicked();
            create = ui
                .add_enabled(check.is_ok() && !busy, egui::Button::new("Créer le compte"))
                .clicked();
        });
        if create {
            let form = std::mem::take(&mut self.add);
            self.spawn(ctx, data_dir, Job::Add, move |dir| {
                accounts::add(&dir, &form)
            });
        } else if cancel || leave {
            self.add = NewAccount::default();
            self.notice = None;
            self.view = View::List;
        }
    }

    // ---- la fiche d'un compte --------------------------------------------------------------

    fn detail(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        data_dir: &Path,
        id: &str,
        busy: bool,
    ) {
        let Some(user) = self.users.iter().find(|u| u.id == id).cloned() else {
            // Le compte a disparu (fichier remplacé pendant ce temps) : retour à la liste.
            self.view = View::List;
            return;
        };
        let p = theme::palette(ui);
        if Self::breadcrumb(ui, "Comptes", &user.display_name) {
            self.password = PasswordForm::default();
            self.notice = None;
            self.view = View::List;
            return;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            avatar(ui, &user, 40.0);
            ui.vertical(|ui| {
                ui.heading(&user.display_name);
                ui.label(
                    RichText::new(format!(
                        "Identifiant : {} · Tous les droits, comme les autres comptes",
                        user.username
                    ))
                    .small()
                    .color(p.text_secondary),
                );
            });
        });
        ui.add_space(8.0);
        if !user.active {
            ui.colored_label(
                p.warn,
                "Ce compte est désactivé : ses connexions et ses raccourcis ne marchent plus.",
            );
            ui.add_space(6.0);
        }
        self.phone_section(ui, &user);
        self.connections_section(ui, &user);

        let last_active = accounts::is_last_active(&self.users, &user.id);
        let (mut new_password, mut save, mut cancel) = (false, false, false);
        let (mut disable, mut enable) = (false, false);
        widgets::section(ui, "Gérer le compte", |ui| {
            ui.label(RichText::new("Mot de passe").strong());
            if self.password.open {
                ui.label(format!("Nouveau mot de passe pour {}.", user.display_name));
                ui.add(egui::TextEdit::singleline(&mut self.password.password).password(true));
                widgets::hint(ui, "10 caractères au minimum.");
                ui.label("Confirmez le mot de passe");
                ui.add(egui::TextEdit::singleline(&mut self.password.confirm).password(true));
                let check =
                    accounts::check_passwords(&self.password.password, &self.password.confirm);
                if let Err(e) = &check {
                    if self.password.password.is_empty() {
                        widgets::hint(ui, e);
                    } else {
                        ui.colored_label(p.error, e);
                    }
                }
                ui.horizontal(|ui| {
                    cancel = ui
                        .add_enabled(!busy, egui::Button::new("Annuler"))
                        .clicked();
                    save = ui
                        .add_enabled(
                            check.is_ok() && !busy,
                            egui::Button::new("Enregistrer le mot de passe"),
                        )
                        .clicked();
                });
                widgets::hint(
                    ui,
                    "Ses connexions ouvertes se ferment : elle devra se reconnecter.",
                );
            } else {
                ui.label(format!(
                    "Définir un nouveau mot de passe pour {}. Les autres comptes ne sont pas touchés.",
                    user.display_name
                ));
                new_password = ui
                    .add_enabled(!busy, egui::Button::new("Nouveau mot de passe…"))
                    .clicked();
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new("Accès à la maison").strong());
            if user.active {
                ui.label("Ses connexions, ses raccourcis et son widget cessent de marcher tant que le compte est désactivé. Le compte reste là : vous pouvez le réactiver, et ses raccourcis et son widget remarcheront.");
                ui.horizontal(|ui| {
                    // Un bouton grisé dit toujours pourquoi, juste à côté.
                    let enabled = !busy && !last_active;
                    disable = ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(RichText::new("Désactiver le compte…").color(
                                if enabled { p.error } else { p.text_secondary },
                            )),
                        )
                        .clicked();
                    if last_active {
                        widgets::hint(
                            ui,
                            "C'est le seul compte actif : le désactiver empêcherait toute connexion.",
                        );
                    }
                });
            } else {
                ui.label("Ce compte est désactivé. Le réactiver lui permet de se reconnecter avec son mot de passe ; ses anciennes connexions ne reviennent pas.");
                enable = ui
                    .add_enabled(!busy, egui::Button::new("Réactiver le compte"))
                    .clicked();
            }
        });
        self.show_notice(ui);

        let id = user.id.clone();
        if new_password {
            self.notice = None;
            self.password.open = true;
        } else if cancel {
            self.password = PasswordForm::default();
        } else if save {
            let password = std::mem::take(&mut self.password.password);
            self.password.confirm.clear();
            self.spawn(ctx, data_dir, Job::Password, move |dir| {
                accounts::change_password(&dir, &id, &password)
            });
        } else if disable
            && confirm::ask(&Confirmation::DisableAccount {
                name: user.display_name.clone(),
            })
        {
            self.spawn(ctx, data_dir, Job::Active(false), move |dir| {
                accounts::set_active(&dir, &id, false)
            });
        } else if enable {
            self.spawn(ctx, data_dir, Job::Active(true), move |dir| {
                accounts::set_active(&dir, &id, true)
            });
        }
    }

    /// « Téléphone relié » : la présence (Wi-Fi de la box) arrive à l'étape 6. Rien n'est dessiné
    /// d'ici là.
    fn phone_section(&self, _ui: &mut egui::Ui, _user: &User) {}

    /// « Connexions de … » : les téléphones et ordinateurs où le compte est ouvert, avec « Couper ».
    fn connections_section(&mut self, ui: &mut egui::Ui, user: &User) {
        let p = theme::palette(ui);
        let mine: Vec<ConnectionRow> = self
            .rows
            .iter()
            .filter(|r| r.user_id == user.id)
            .cloned()
            .collect();
        let mut cut = None;
        widgets::section(ui, &format!("Connexions de {}", user.display_name), |ui| {
            ui.label("Les téléphones et ordinateurs où le compte est ouvert. Couper ferme Ostal sur ce téléphone ou cet ordinateur.");
            ui.add_space(4.0);
            if mine.is_empty() {
                ui.label(
                    RichText::new("Aucune connexion ouverte pour l'instant.")
                        .color(p.text_secondary),
                );
            }
            for (i, row) in mine.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&row.name).strong());
                        ui.label(
                            RichText::new(format!(
                                "{} · dernier accès {} · {}",
                                row.detail,
                                row.seen.to_lowercase(),
                                row.via
                            ))
                            .small()
                            .color(p.text_secondary),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::secondary_button(ui, "Couper").clicked() {
                            cut = Some((row.name.clone(), row.id.clone()));
                        }
                    });
                });
            }
        });
        if cut.is_some() {
            self.cut = cut;
        }
    }

    // ---- toutes les connexions -------------------------------------------------------------

    /// « Réglages › Comptes › Toutes les connexions ». Rend `true` pour revenir aux Réglages.
    fn connections_page(&mut self, ui: &mut egui::Ui) -> bool {
        let p = theme::palette(ui);
        if Self::breadcrumb(ui, "Comptes", "Toutes les connexions") {
            self.notice = None;
            self.view = View::List;
            return false;
        }
        ui.add_space(4.0);
        ui.heading("Toutes les connexions");
        ui.label(accounts::summary(&self.rows));
        ui.add_space(8.0);
        let rows = self.rows.clone();
        let mut cut = None;
        widgets::card(ui, |ui| {
            ui.label(RichText::new("Connexions de la maison").strong());
            ui.label(
                RichText::new("Chaque téléphone ou ordinateur où un compte Ostal est ouvert.")
                    .small()
                    .color(p.text_secondary),
            );
            ui.add_space(6.0);
            if rows.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Aucune connexion ouverte pour l'instant.")
                            .color(p.text_secondary),
                    );
                });
                return;
            }
            egui::Grid::new("all-connections")
                .num_columns(5)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    for head in ["Personne", "Connexion", "Dernier accès", "Où", ""] {
                        ui.label(RichText::new(head).small().color(p.text_secondary));
                    }
                    ui.end_row();
                    for row in &rows {
                        ui.label(&row.person);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&row.name).strong());
                            ui.label(RichText::new(&row.detail).small().color(p.text_secondary));
                        });
                        ui.label(&row.seen);
                        ui.label(row.via);
                        if theme::secondary_button(ui, "Couper").clicked() {
                            cut = Some((row.name.clone(), row.id.clone()));
                        }
                        ui.end_row();
                    }
                });
        });
        ui.add_space(6.0);
        widgets::hint(
            ui,
            "Couper ferme la connexion tout de suite. Ce téléphone ou cet ordinateur devra se reconnecter avec son identifiant et son mot de passe.",
        );
        widgets::hint(
            ui,
            "Une « connexion inconnue » reste ainsi tant que son app n'est pas mise à jour.",
        );
        self.show_notice(ui);
        if cut.is_some() {
            self.cut = cut;
        }
        false
    }
}

/// La pastille ronde avec l'initiale du nom (grisée si le compte est désactivé).
fn avatar(ui: &mut egui::Ui, user: &User, size: f32) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    let (fill, text) = if user.active {
        (p.accent_soft, p.accent_text)
    } else {
        (p.surface_alt, p.text_secondary)
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        accounts::initial(&user.display_name),
        egui::FontId::proportional(size * 0.45),
        text,
    );
}
