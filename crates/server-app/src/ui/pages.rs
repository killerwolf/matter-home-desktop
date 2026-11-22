//! Les pages de la fenêtre (voir `docs/ux-serveur.md`).
//!
//! - Première installation : trois étapes (la maison, le compte, « c'est prêt »).
//! - Ensuite, cinq onglets : **Vue d'ensemble** (la maison va bien ?), **Appareils**,
//!   **Caméras**, **Connexion** (Mac ou PC de la maison, iPhone à distance, Raccourcis) et
//!   **Réglages** (compte, démarrage, sauvegarde, dépannage).
//!
//! Les mots sont ceux de l'utilisateur ; le terme technique, quand il aide à se repérer dans la
//! documentation, suit entre parenthèses. Aucun secret n'est affiché, sauf le jeton de mode à sa
//! création (une seule fois).

use super::widgets::{self, StepState};
use super::*;
use ostal_server_app::confirm::{self, Confirmation};
use ostal_server_app::controller::Notice;
use ostal_server_app::overview::Level;
use ostal_server_app::startup::Blocker;
use ostal_server_app::system::HelpPage;
use ostal_server_app::{descriptions, version};
use protocol::mode::HomeMode;

/// Les étapes de la première installation.
const SETUP_STEPS: usize = 3;

fn notice_label(ui: &mut egui::Ui, notice: &Notice) {
    let p = theme::palette(ui);
    match notice {
        Notice::Ok(t) => ui.colored_label(p.ok, t),
        Notice::Err(t) => ui.colored_label(p.error, t),
    };
}

/// L'en-tête d'une étape de la première installation.
fn setup_header(ui: &mut egui::Ui, step: usize, title: &str) {
    let p = theme::palette(ui);
    ui.label(
        RichText::new(format!(
            "Première installation · étape {step} sur {SETUP_STEPS}"
        ))
        .small()
        .color(p.text_secondary),
    );
    ui.heading(title);
    ui.add_space(6.0);
}

/// Une ligne de la liste des appareils : le nom, puis « joignable » ou « injoignable ».
fn device_line(ui: &mut egui::Ui, name: &str, reachable: bool) {
    ui.horizontal(|ui| {
        ui.set_width(280.0);
        ui.allocate_ui_with_layout(
            egui::vec2(168.0, 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(egui::Label::new(name).truncate());
            },
        );
        if reachable {
            theme::state_word(ui, theme::StateWord::Ok, "joignable");
        } else {
            theme::state_word(ui, theme::StateWord::Watch, "injoignable");
        }
    });
}

/// Ce que fait chaque mode, en une phrase.
fn mode_hint(mode: HomeMode) -> &'static str {
    match mode {
        HomeMode::Present => "Présent : quelqu'un est à la maison.",
        HomeMode::Absent => "Absent : la maison est vide pour la journée.",
        HomeMode::Nuit => "Nuit : tout le monde dort.",
        HomeMode::Vacances => "Vacances : la maison est vide pour plusieurs jours.",
    }
}

/// Une préparation de raccourcis (ou du widget) lancée depuis l'iPhone tient le jeton quelques
/// secondes : la fenêtre ne l'attend pas, elle le dit.
const BUSY_NOTICE: &str =
    "Une préparation est en cours depuis l'iPhone (raccourcis ou widget) : réessayez dans un instant.";

impl App {
    /// Les onglets ne s'affichent qu'une fois la première installation finie.
    pub(super) fn tabs_visible(&self, st: &State) -> bool {
        st.busy.is_none() && st.phase != Phase::NeedsSetup && !self.show_done
    }

    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = theme::tokens().space.xs;
            ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
            for tab in Tab::ALL {
                if theme::tab(ui, self.tab == tab, tab.label()).clicked() {
                    self.open_tab(tab);
                }
            }
        });
    }

    /// Un bouton « Copier » qui dit « Copié » deux secondes. `key` distingue les boutons ; la
    /// valeur copiée n'est gardée nulle part.
    fn copy_button(&mut self, ui: &mut egui::Ui, key: &'static str, value: &str) {
        let done = self
            .copied
            .is_some_and(|(k, at)| k == key && at.elapsed() < Duration::from_secs(2));
        if ui.button(if done { "Copié" } else { "Copier" }).clicked()
            && system::copy_to_clipboard(value).is_ok()
        {
            self.copied = Some((key, std::time::Instant::now()));
        }
    }

    /// Une valeur à recopier ailleurs, avec son bouton « Copier ».
    fn copy_field(&mut self, ui: &mut egui::Ui, key: &'static str, value: &str) {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.copy_button(ui, key, value);
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    widgets::value_box(ui, value);
                });
            });
        });
    }

    pub(super) fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &State) {
        if let Some(busy) = &st.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(busy);
            });
            return;
        }
        if let Some(migrated) = &st.migrated {
            notice_label(ui, &Notice::Ok(migrated.clone()));
            ui.add_space(8.0);
        }
        match (&st.phase, &st.setup) {
            (Phase::NeedsSetup, Some(SetupNeed::Identity { unreadable })) => {
                self.identity_page(ui, st, unreadable.as_deref())
            }
            (Phase::NeedsSetup, Some(SetupNeed::Account)) => self.account_page(ui, st, false),
            (Phase::Running, _) if self.show_done => self.done_page(ui, ctx, st),
            _ => match self.tab {
                Tab::Overview => self.overview_page(ui, st),
                Tab::Devices => self.devices_page(ui, st),
                Tab::Cameras => self.cameras_page(ui, ctx, st),
                Tab::Connect => self.connect_page(ui, st),
                Tab::Settings => self.settings_page(ui, st),
            },
        }
        self.show_local_notice(ui, st);
    }

    /// Une erreur : sa cause, « Réessayer » quand on peut reprendre, « ✕ » pour la fermer. Une
    /// info (« Code supprimé ») reste une ligne. Le détail technique va au journal.
    fn show_local_notice(&mut self, ui: &mut egui::Ui, st: &State) {
        let Some(notice) = self.local_notice.clone() else {
            return;
        };
        ui.add_space(8.0);
        match notice {
            Notice::Ok(_) => notice_label(ui, &notice),
            Notice::Err(text) => {
                let (again, close) = widgets::error_notice(ui, &text, self.local_retry.is_some());
                if close {
                    self.local_notice = None;
                    self.local_retry = None;
                } else if again {
                    let retry = self.local_retry.take();
                    self.local_notice = None;
                    if let (Some(retry), Some(hub)) = (retry, st.hub.as_ref().map(|h| h.0.clone()))
                    {
                        self.run_retry(retry, &hub);
                    }
                }
            }
        }
    }

    /// Signale un échec : la cause en une phrase (le détail est écrit au journal), et ce que
    /// « Réessayer » refera.
    fn fail(&mut self, what: &str, detail: impl std::fmt::Debug, retry: Option<Retry>) {
        log::warn!("{what} : {detail:?}");
        self.local_notice = Some(Notice::Err(format!(
            "{what} : le serveur n'a pas pu enregistrer ce changement. Vérifiez l'espace libre sur le Mac, puis réessayez."
        )));
        self.local_retry = retry;
    }

    fn run_retry(&mut self, retry: Retry, hub: &Arc<ostal_server::hub::Hub>) {
        match retry {
            Retry::Device(id) => self.retry_device(id, hub),
            Retry::PushEnabled(on) => self.set_push(on, hub),
            Retry::RevokeSubscription(id) => self.revoke_subscription(&id, hub),
            Retry::RevokeReadToken => self.revoke_read(hub),
            Retry::DeleteModeToken => self.delete_mode_token(hub),
            Retry::GenerateModeToken => self.generate_mode_token(hub),
        }
    }

    fn retry_device(&mut self, id: u64, hub: &Arc<ostal_server::hub::Hub>) {
        if let Err(e) = hub.retry(id) {
            self.local_notice = Some(Notice::Err(
                "L'appareil n'a pas répondu à cet essai. Vérifiez son alimentation, puis réessayez."
                    .into(),
            ));
            log::warn!("nouvel essai d'un appareil : {:?}", e.code);
            self.local_retry = Some(Retry::Device(id));
        }
    }

    fn set_push(&mut self, on: bool, hub: &Arc<ostal_server::hub::Hub>) {
        if let Err(e) = hub.push().set_enabled(on) {
            self.fail(
                "Le réglage n'a pas été enregistré",
                e,
                Some(Retry::PushEnabled(on)),
            );
        }
    }

    fn revoke_subscription(&mut self, id: &str, hub: &Arc<ostal_server::hub::Hub>) {
        if let Err(e) = hub.push().remove(id) {
            self.fail(
                "L'appareil n'a pas été révoqué",
                e,
                Some(Retry::RevokeSubscription(id.to_string())),
            );
        }
    }

    fn revoke_read(&mut self, hub: &Arc<ostal_server::hub::Hub>) {
        match hub.revoke_read_token() {
            Ok(()) => {
                self.local_notice = Some(Notice::Ok(
                    "Jeton de lecture révoqué : le widget ne lit plus rien.".into(),
                ))
            }
            Err(e) if e.code == protocol::wire::ErrorCode::Busy => {
                self.local_notice = Some(Notice::Err(BUSY_NOTICE.into()));
                self.local_retry = Some(Retry::RevokeReadToken);
            }
            Err(e) => self.fail(
                "Le jeton n'a pas été révoqué",
                e.code,
                Some(Retry::RevokeReadToken),
            ),
        }
    }

    fn delete_mode_token(&mut self, hub: &Arc<ostal_server::hub::Hub>) {
        match hub.revoke_mode_token() {
            Ok(()) => {
                self.local_notice = Some(Notice::Ok(
                    "Code secret supprimé : Raccourcis ne peut plus changer le mode.".into(),
                ))
            }
            Err(e) if e.code == protocol::wire::ErrorCode::Busy => {
                self.local_notice = Some(Notice::Err(BUSY_NOTICE.into()));
                self.local_retry = Some(Retry::DeleteModeToken);
            }
            Err(e) => self.fail(
                "Le code n'a pas été supprimé",
                e.code,
                Some(Retry::DeleteModeToken),
            ),
        }
    }

    // ---- première installation -----------------------------------------------------------------

    fn identity_page(&mut self, ui: &mut egui::Ui, st: &State, unreadable: Option<&str>) {
        setup_header(ui, 1, "Bienvenue : votre maison");
        ui.label("Ce Mac va devenir le serveur de votre maison : il pilote vos appareils Matter, exécute les automatismes et garde l'historique des capteurs, jour et nuit. Vous le consulterez depuis vos autres appareils (ordinateur, iPhone).");
        ui.add_space(8.0);
        if let Some(n) = &st.notice {
            notice_label(ui, n);
            ui.add_space(6.0);
        }
        match &mut self.page {
            Page::Start => {
                if let Some(reason) = unreadable {
                    ui.colored_label(
                        theme::palette(ui).error,
                        format!("Les données de la maison présentes sur ce Mac sont illisibles ({reason}). Rien n'a été modifié. Vous pouvez reprendre une sauvegarde pour les remplacer."),
                    );
                    ui.add_space(6.0);
                } else {
                    ui.label(
                        "Aucune donnée n'a été trouvée sur ce Mac. Choisissez votre situation :",
                    );
                    ui.add_space(8.0);
                }
                let mut pick = false;
                let mut abandon = false;
                let picking = self.picking.is_some();
                widgets::section(ui, "J'utilisais déjà Ostal (ou Matter Home)", |ui| {
                    ui.label("Reprenez votre sauvegarde (un fichier « .ostal », ou « .matterhome » s'il date d'avant le renommage, créé par l'ancienne application ou sur un autre Mac) : vos appareils, pièces et automatismes reviennent, rien n'est à réinstaller.");
                    ui.add_space(4.0);
                    if picking {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Lecture de la sauvegarde…");
                        });
                    } else if theme::secondary_button(ui, "Reprendre une sauvegarde…").clicked() {
                        pick = true;
                    }
                    if setup::import_in_progress(&st.data_dir) {
                        ui.add_space(6.0);
                        ui.label("Un import précédent n'est pas terminé.");
                        if theme::destructive_button(ui, "Renoncer à l'import en attente…")
                            .clicked()
                            && confirm::ask(&Confirmation::AbandonImport)
                        {
                            abandon = true;
                        }
                    }
                });
                if pick {
                    self.pick_backup(&ui.ctx().clone(), st);
                }
                if abandon {
                    let _ = self.handle.tx.send(Cmd::AbandonImport);
                }
                if unreadable.is_none() {
                    let mut new_home = false;
                    widgets::section(ui, "C'est ma première installation", |ui| {
                        ui.label("Vous partez de zéro : aucun appareil pour l'instant, vous les ajouterez ensuite depuis l'application Ostal.");
                        ui.add_space(4.0);
                        new_home = ui.button("Créer une nouvelle maison…").clicked();
                    });
                    if new_home {
                        self.page = Page::NewHome;
                    }
                }
            }
            Page::NewHome => {
                let (mut back, mut go) = (false, false);
                widgets::section(ui, "Créer une nouvelle maison", |ui| {
                    ui.label("Une nouvelle maison va être créée sur ce Mac (une nouvelle identité Matter). Aucun appareil n'est repris : ceux que vous aviez ajoutés à une autre installation ne seront pas reconnus et il faudra les ajouter de nouveau. Si vous avez une sauvegarde, ne continuez pas : reprenez-la.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        go = theme::primary_button(ui, "Créer une nouvelle maison").clicked();
                        back = ui.button("Retour").clicked();
                    });
                });
                if go {
                    let _ = self.handle.tx.send(Cmd::NewHome);
                }
                if go || back {
                    self.page = Page::Start;
                }
            }
            Page::Restore(r) => {
                let mut back = false;
                let mut go = false;
                widgets::section(ui, "Reprendre cette sauvegarde", |ui| {
                    egui::Grid::new("restore_summary")
                        .num_columns(2)
                        .show(ui, |ui| {
                            theme::detail_row(
                                ui,
                                "Fichier",
                                r.file
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        });
                    ui.label(format!("Contenu : {}", r.summary));
                    ui.label(format!(
                        "Maison de la sauvegarde : {}",
                        r.comparison.incoming.describe()
                    ));
                    ui.label(match &r.comparison.current {
                        IdentityState::Missing => {
                            "Maison actuelle de ce Mac : aucune (rien n'est remplacé).".to_string()
                        }
                        IdentityState::Present(c) => {
                            format!("Maison actuelle de ce Mac : {}", c.describe())
                        }
                        IdentityState::Unreadable(e) => {
                            format!("Maison actuelle de ce Mac : illisible ({e})")
                        }
                    });
                    if r.comparison.differs {
                        ui.add_space(6.0);
                        ui.colored_label(
                            theme::palette(ui).error,
                            setup::replace_refusal_for_app(),
                        );
                        ui.checkbox(&mut r.replace_ack, "Je veux bien remplacer la maison actuelle (l'ancien dossier est conservé)");
                    } else if matches!(r.comparison.current, IdentityState::Present(_)) {
                        ui.label("C'est la même maison : vos appareils restent reconnus.");
                    }
                    ui.add_space(6.0);
                    ui.label(setup::OTHER_INSTANCES_REMINDER);
                    ui.checkbox(
                        &mut r.stopped_ack,
                        "L'ancienne application est arrêtée sur tous mes Mac",
                    );
                    ui.add_space(8.0);
                    let ready = r.stopped_ack && (!r.comparison.differs || r.replace_ack);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(ready, egui::Button::new("Reprendre cette sauvegarde"))
                            .clicked()
                        {
                            go = true;
                        }
                        if ui.button("Retour").clicked() {
                            back = true;
                        }
                    });
                });
                if go {
                    let _ = self.handle.tx.send(Cmd::Import {
                        file: r.file.clone(),
                        replace_identity: r.comparison.differs && r.replace_ack,
                    });
                    back = true;
                }
                if back {
                    self.page = Page::Start;
                }
            }
        }
    }

    /// Le sélecteur de fichier reste sur le fil principal ; la lecture et l'analyse du fichier
    /// (qui peut être gros) se font dans un fil, le résultat revient par `picking`.
    fn pick_backup(&mut self, ctx: &egui::Context, st: &State) {
        let Some(file) = rfd::FileDialog::new()
            .set_title("Choisir une sauvegarde Ostal")
            .add_filter("Sauvegarde Ostal", &backup::OPEN_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (ctx, data_dir) = (ctx.clone(), st.data_dir.clone());
        std::thread::spawn(move || {
            let _ = tx.send(inspect_backup(&data_dir, file));
            ctx.request_repaint();
        });
        self.local_notice = None;
        self.picking = Some(rx);
    }

    /// Reçoit le résultat de l'analyse, s'il est arrivé.
    pub(super) fn poll_picking(&mut self) {
        let Some(rx) = &self.picking else { return };
        match rx.try_recv() {
            Ok(Ok(restore)) => {
                self.page = Page::Restore(Box::new(restore));
                self.picking = None;
            }
            Ok(Err(message)) => {
                self.local_notice = Some(Notice::Err(message));
                self.picking = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
        }
    }

    fn account_page(&mut self, ui: &mut egui::Ui, st: &State, replace: bool) {
        if !replace {
            setup_header(ui, 2, "Créez votre compte");
            ui.label("C'est avec cet identifiant et ce mot de passe que vous ouvrirez Ostal sur vos autres appareils (ordinateur, iPhone). Il n'y a qu'un seul compte : notez-le.");
        } else {
            ui.label("Choisissez un nouvel identifiant et un nouveau mot de passe. Les appareils déjà connectés devront se reconnecter, et le serveur redémarre quelques secondes.");
        }
        if let Some(n) = &st.notice {
            ui.add_space(6.0);
            notice_label(ui, n);
        }
        ui.add_space(8.0);
        ui.label("Identifiant");
        ui.text_edit_singleline(&mut self.form.username);
        ui.label("Mot de passe (10 caractères au minimum)");
        ui.add(egui::TextEdit::singleline(&mut self.form.password).password(true));
        ui.label("Confirmez le mot de passe");
        ui.add(egui::TextEdit::singleline(&mut self.form.confirm).password(true));
        let check = self.form.check();
        // La raison du bouton grisé se lit dès l'ouverture, champs vides compris : en rouge une
        // fois qu'on a commencé à écrire, en gris avant.
        if let Err(e) = &check {
            if self.form.password.is_empty() && self.form.username.is_empty() {
                widgets::hint(ui, e);
            } else {
                ui.colored_label(theme::palette(ui).error, e);
            }
        }
        ui.add_space(8.0);
        let label = if replace {
            "Remplacer le compte"
        } else {
            "Créer le compte"
        };
        if ui
            .add_enabled(check.is_ok(), egui::Button::new(label))
            .clicked()
        {
            let (username, password) = (
                self.form.username.clone(),
                std::mem::take(&mut self.form.password),
            );
            self.form.confirm.clear();
            let _ = self.handle.tx.send(if replace {
                Cmd::ReplaceAccount { username, password }
            } else {
                Cmd::CreateAccount { username, password }
            });
            if replace {
                self.change_account = false;
            }
        }
    }

    fn done_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &State) {
        setup_header(ui, 3, "C'est prêt : le serveur tourne");
        ui.label("Ce Mac travaille maintenant en arrière-plan : son icône (une arche avec un point) reste dans la barre de menus, en haut de l'écran. Il vous reste à ouvrir Ostal sur l'appareil que vous utilisez tous les jours.");
        ui.add_space(8.0);
        widgets::section(ui, "Connecter votre ordinateur", |ui| {
            self.desktop_steps(ui, st);
        });
        widgets::section(ui, "Sur ce Mac", |ui| {
            if self.login_available() {
                let mut on = self.login_enabled;
                if ui
                    .checkbox(
                        &mut on,
                        "Lancer Ostal Server à l'ouverture de session de ce Mac",
                    )
                    .changed()
                {
                    self.set_login(on);
                }
                ui.add_space(4.0);
            }
            widgets::hint(ui, SLEEP_NOTE);
        });
        widgets::hint(ui, "Tout ceci se retrouve plus tard : icône de la barre de menus → « Ouvrir Ostal Server… » (onglets Connexion et Réglages).");
        ui.add_space(8.0);
        if theme::primary_button(ui, "Terminer").clicked() {
            self.hide_window(ctx);
        }
    }

    // ---- vue d'ensemble ------------------------------------------------------------------------

    fn overview_page(&mut self, ui: &mut egui::Ui, st: &State) {
        let o = self.overview.clone();
        let detail: Option<String> = match (&st.phase, o.level) {
            (Phase::Running, Level::Ok) => Some(
                "Le serveur tourne. Vous pouvez fermer cette fenêtre : il continue en arrière-plan."
                    .into(),
            ),
            (Phase::Running, _) => {
                Some("Le serveur tourne, mais quelque chose demande un coup d'œil.".into())
            }
            (Phase::Blocked(b), _) => Some(format!("{} Rien n'est démarré en attendant.", b.short())),
            (Phase::Problem(_), _) => Some("Il s'est arrêté après une erreur.".into()),
            _ => None,
        };
        widgets::headline(ui, o.level, &o.headline, detail.as_deref());
        match &st.phase {
            Phase::Starting | Phase::Stopping | Phase::NeedsSetup => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Un instant…");
                });
            }
            Phase::Blocked(b) => self.blocked_card(ui, st, b),
            Phase::Problem(_) => {
                // Le détail technique ne va qu'au journal : la fenêtre dit la cause probable et
                // propose de réessayer.
                let mut retry = false;
                widgets::section(ui, "Pourquoi, et quoi faire", |ui| {
                    ui.label("Le serveur s'est arrêté après une erreur. Vérifiez l'espace disque du Mac, puis réessayez.");
                    ui.add_space(6.0);
                    retry = theme::primary_button(ui, "Réessayer").clicked();
                });
                if retry {
                    let _ = self.handle.tx.send(Cmd::Retry);
                }
                self.journal_link(ui, st);
            }
            Phase::Running => self.overview_running(ui, st, &o),
        }
    }

    /// Le lien discret vers le journal, au pied de la page.
    fn journal_link(&mut self, ui: &mut egui::Ui, st: &State) {
        ui.add_space(4.0);
        if ui
            .add(egui::Link::new(
                RichText::new("Détail technique dans le journal")
                    .strong()
                    .small(),
            ))
            .clicked()
        {
            system::open_file(&st.log_path);
        }
    }

    /// « Pourquoi, et quoi faire » : la marche à suivre, les commandes à recopier, « Réessayer
    /// maintenant ».
    fn blocked_card(&mut self, ui: &mut egui::Ui, st: &State, b: &Blocker) {
        let mut retry = false;
        let commands = b.commands();
        widgets::section(ui, "Pourquoi, et quoi faire", |ui| {
            ui.add(egui::Label::new(b.advice()).wrap());
            for (i, command) in commands.iter().enumerate() {
                if i == 1 {
                    ui.add_space(4.0);
                    widgets::hint(
                        ui,
                        "Si le dossier de l'archive n'existe plus, ces commandes font la même chose :",
                    );
                }
                let key: &'static str = ["cmd-0", "cmd-1", "cmd-2", "cmd-3", "cmd-4"]
                    .get(i)
                    .copied()
                    .unwrap_or("cmd-n");
                self.copy_field(ui, key, command);
            }
            ui.add_space(6.0);
            retry = theme::primary_button(ui, "Réessayer maintenant").clicked();
        });
        if retry {
            let _ = self.handle.tx.send(Cmd::Retry);
        }
        self.journal_link(ui, st);
    }

    fn overview_running(&mut self, ui: &mut egui::Ui, st: &State, o: &Overview) {
        let mut go = None;
        let health = &st.health;
        widgets::section(ui, "État", |ui| {
            for check in &o.checks {
                // Sans problème, le bouton mène simplement à l'onglet du sujet.
                let (label, tab) = match (&check.fix, check.title) {
                    (Some(fix), _) => (Some(fix.label.clone()), Some(fix.tab)),
                    (None, "Appareils") => (Some("Voir".to_string()), Some(Tab::Devices)),
                    (None, "Caméras") => (Some("Voir".to_string()), Some(Tab::Cameras)),
                    (None, "Accès à distance") => (
                        Some(
                            if check.level == Level::Ok {
                                "Voir"
                            } else {
                                "Configurer"
                            }
                            .to_string(),
                        ),
                        Some(Tab::Connect),
                    ),
                    _ => (None, None),
                };
                let hint = (check.title == "Appareils" && !health.unreachable.is_empty()).then_some(
                    if health.unreachable.len() > 1 {
                        "Vérifiez leur alimentation : ils reviennent tout seuls dès qu'ils répondent."
                    } else {
                        "Vérifiez son alimentation : elle revient toute seule dès qu'elle répond."
                    },
                );
                if widgets::status_line(
                    ui,
                    check.level,
                    check.title,
                    &check.text,
                    label.as_deref(),
                    hint,
                ) {
                    go = tab;
                }
            }
        });

        if let Some(hub) = &st.hub {
            let current = hub.0.mode();
            let mut chosen = None;
            widgets::section(ui, "Mode de la maison", |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for mode in HomeMode::ALL {
                        if widgets::mode_button(
                            ui,
                            mode.label(),
                            mode == current,
                            mode == HomeMode::Present,
                        )
                        .clicked()
                            && mode != current
                        {
                            chosen = Some(mode);
                        }
                    }
                });
                widgets::hint(ui, &format!("{} {}", mode_hint(current), current.effect()));
            });
            if let Some(mode) = chosen {
                self.set_mode(st, mode);
            }
        }

        // Repliée quand une chose est à regarder : l'état passe avant les tâches.
        let alert = o.level.needs_attention();
        let id = ui.make_persistent_id(("tasks_open", alert));
        let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(id, !alert));
        let toggled = widgets::fold_card(ui, "Que voulez-vous faire ?", None, open, |ui| {
            let tasks = [
                ("Connecter un ordinateur de la maison", Tab::Connect),
                ("Utiliser l'iPhone même hors de la maison", Tab::Connect),
                ("Ajouter un appareil", Tab::Devices),
                ("Ajouter ou régler une caméra", Tab::Cameras),
                ("Changer mon mot de passe", Tab::Settings),
                ("Comprendre une panne", Tab::Settings),
            ];
            // Des lignes de 24 px, 2 px entre elles : la vue qui « va bien » tient sans défilement.
            ui.spacing_mut().item_spacing.y = 2.0;
            for (label, tab) in tasks {
                if widgets::link_row(ui, label) {
                    go = Some(tab);
                }
            }
        });
        if toggled {
            open = !open;
            ui.data_mut(|d| d.insert_persisted(id, open));
        }
        if let Some(tab) = go {
            self.open_tab(tab);
        }
    }

    /// Onglet qui a besoin du serveur, alors qu'il ne tourne pas : on renvoie à la vue d'ensemble.
    fn needs_server(&mut self, ui: &mut egui::Ui) {
        let mut go = false;
        widgets::card(ui, |ui| {
            ui.label("Le serveur ne tourne pas pour l'instant : cette page revient dès qu'il a redémarré.");
            ui.add_space(4.0);
            go = ui.button("Voir la vue d'ensemble").clicked();
        });
        if go {
            self.open_tab(Tab::Overview);
        }
    }

    // ---- appareils -----------------------------------------------------------------------------

    fn devices_page(&mut self, ui: &mut egui::Ui, st: &State) {
        let (Phase::Running, Some(hub)) = (&st.phase, st.hub.as_ref().map(|h| h.0.clone())) else {
            return self.needs_server(ui);
        };
        let health = &st.health;
        let t = theme::tokens();
        if health.total == 0 {
            widgets::headline(ui, Level::Info, "Aucun appareil pour l'instant", None);
        } else {
            ui.label(
                RichText::new(overview::devices_sentence(health))
                    .size(t.font.size.heading)
                    .strong(),
            );
            ui.add_space(t.space.md);
        }
        if !health.unreachable_details.is_empty() {
            let mut retry = None;
            widgets::section(ui, "Injoignables en ce moment", |ui| {
                for d in &health.unreachable_details {
                    ui.horizontal(|ui| {
                        widgets::state_icon(ui, Level::Warn, 14.0);
                        let mut line = d.name.clone();
                        if let Some(room) = &d.room {
                            line.push_str(&format!(" · {room}"));
                        }
                        line.push_str(" · injoignable");
                        if let Some(at) = d.last_seen.and_then(ostal_server_app::status::clock) {
                            line.push_str(&format!(" depuis {at}"));
                        }
                        ui.add(egui::Label::new(RichText::new(line).strong()).wrap());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::secondary_button(ui, "Réessayer").clicked() {
                                retry = Some(d.id);
                            }
                        });
                    });
                }
                widgets::hint(ui, "Vérifiez son alimentation : elle revient toute seule dès qu'elle répond. Un capteur sur pile se réveille par un appui sur son bouton.");
            });
            if let Some(id) = retry {
                self.retry_device(id, &hub);
            }
        }
        if !health.listed.is_empty() {
            widgets::section(ui, "Vos appareils", |ui| {
                let any_room = health.listed.iter().any(|d| d.room.is_some());
                if !any_room {
                    // Pas de plan : une simple liste.
                    for d in &health.listed {
                        device_line(ui, &d.name, d.reachable);
                    }
                    return;
                }
                // Par pièce (la liste est déjà triée), deux colonnes de 280 px.
                let mut start = 0;
                while start < health.listed.len() {
                    let room = health.listed[start].room.as_deref();
                    let end = health.listed[start..]
                        .iter()
                        .position(|d| d.room.as_deref() != room)
                        .map_or(health.listed.len(), |n| start + n);
                    ui.add_space(t.space.xs);
                    ui.label(RichText::new(room.unwrap_or("Sans pièce")).strong());
                    egui::Grid::new(("room", start))
                        .num_columns(2)
                        .spacing([0.0, 4.0])
                        .min_col_width(280.0)
                        .show(ui, |ui| {
                            for (i, d) in health.listed[start..end].iter().enumerate() {
                                device_line(ui, &d.name, d.reachable);
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                    start = end;
                }
            });
        }
        // Repliée : l'ajout ne se fait pas ici, mais dans Ostal.
        let id = ui.make_persistent_id("add_device_open");
        let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(id, false));
        let toggled = widgets::fold_card(ui, "Ajouter un appareil", Some("3 étapes"), open, |ui| {
            widgets::step(ui, 1, StepState::Todo, |ui| {
                ui.label("Dans l'app Maison (iPhone, iPad ou Mac) : appui long sur l'appareil, puis Réglages (la roue dentée).");
            });
            widgets::step(ui, 2, StepState::Todo, |ui| {
                ui.label("Tout en bas : « Activer le mode jumelage ». Maison affiche un code à 11 chiffres, valable quelques minutes.");
            });
            widgets::step(ui, 3, StepState::Todo, |ui| {
                ui.label("Dans Ostal : « + Ajouter », collez le code, puis « Appairer ».");
            });
            widgets::hint(ui, "L'appareil reste aussi dans Maison. Un capteur sur pile doit être réveillé (appui sur son bouton) juste avant l'étape 2.");
        });
        if toggled {
            open = !open;
            ui.data_mut(|d| d.insert_persisted(id, open));
        }
        widgets::hint(
            ui,
            "Pour voir et piloter vos appareils, utilisez Ostal sur l'ordinateur ou l'iPhone.",
        );
    }

    // ---- caméras -------------------------------------------------------------------------------

    fn cameras_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &State) {
        let (Phase::Running, Some(hub)) = (&st.phase, st.hub.as_ref().map(|h| h.0.clone())) else {
            return self.needs_server(ui);
        };
        if self.descriptions_open {
            if self.descriptions.show(ui, &hub) {
                self.descriptions_open = false;
                self.descriptions.reset();
            }
            return;
        }
        if self.cameras.editing() {
            return self.cameras.show(ui, ctx, &hub);
        }
        let p = theme::palette(ui);
        widgets::section(ui, "Vos caméras", |ui| {
            self.cameras.show(ui, ctx, &hub);
        });
        widgets::section(ui, "Détection (personnes, animaux, véhicules)", |ui| {
            let status = hub.detection().status();
            let (tone, line) = ostal_server_app::cameras::detection_line(&status);
            let color = match tone {
                ostal_server_app::cameras::Tone::Good => p.ok,
                ostal_server_app::cameras::Tone::Bad => p.error,
                ostal_server_app::cameras::Tone::Neutral => p.text_secondary,
            };
            ui.horizontal(|ui| {
                widgets::dot(ui, color, theme::tokens().size.dot + 2.0);
                ui.add(egui::Label::new(line).wrap());
            });
            widgets::hint(ui, "Une caméra n'est analysée que dans les modes où elle est en « Surveillance » (bouton « Modifier… » de la caméra). Chaque passage devient un événement, avec une capture et un clip, visible dans Ostal.");
        });
        let mut open = false;
        widgets::section(ui, "Décrire les événements par IA (facultatif)", |ui| {
            let d = hub.detection().descriptions();
            let (good, line) = descriptions::status_line(&d.settings(), d.has_key());
            ui.horizontal(|ui| {
                widgets::dot(
                    ui,
                    if good { p.ok } else { p.text_secondary },
                    theme::tokens().size.dot + 2.0,
                );
                ui.label(line);
            });
            widgets::hint(ui, "Une phrase par événement (« Un livreur dépose un colis »), écrite par Claude avec votre clé Anthropic. Payant à l'usage, désactivé par défaut.");
            ui.add_space(4.0);
            open = ui.button("Configurer…").clicked();
        });
        if open {
            self.descriptions_open = true;
        }
    }

    // ---- connexion -----------------------------------------------------------------------------

    /// Les étapes pour connecter le client de bureau : l'adresse, le code de vérification, le
    /// compte. (Aussi sur l'écran de fin de la première installation.)
    fn desktop_steps(&mut self, ui: &mut egui::Ui, st: &State) {
        widgets::step(ui, 1, StepState::Todo, |ui| {
            ui.label("Installez et ouvrez l'application Ostal sur votre ordinateur (Mac, Windows ou Linux), branché sur le même réseau que ce Mac.");
        });
        widgets::step(ui, 2, StepState::Todo, |ui| {
            ui.label("Dans son écran de connexion, saisissez cette adresse :");
            if let Some(url) = &st.url {
                self.copy_field(ui, "url", url);
            }
        });
        widgets::step(ui, 3, StepState::Todo, |ui| {
            ui.label("Elle affiche un code de vérification (« empreinte du certificat »). Acceptez seulement s'il est identique à celui-ci : c'est la preuve que vous parlez bien à ce Mac.");
            if let Some(fingerprint) = &st.fingerprint {
                self.copy_field(ui, "fingerprint", fingerprint);
            }
        });
        widgets::step(ui, 4, StepState::Todo, |ui| {
            ui.label("Entrez l'identifiant et le mot de passe du compte créé sur ce Mac.");
        });
    }

    fn connect_page(&mut self, ui: &mut egui::Ui, st: &State) {
        let (Phase::Running, Some(hub)) = (&st.phase, st.hub.as_ref().map(|h| h.0.clone())) else {
            return self.needs_server(ui);
        };
        let summaries = [
            None,
            Some(match &self.web {
                WebAccess::On { .. } => "Activé".to_string(),
                WebAccess::Checking => "vérification…".to_string(),
                _ => "pas activé".to_string(),
            }),
            Some({
                let n = hub.push().status().subscriptions.len();
                match n {
                    0 => "aucun appareil abonné".to_string(),
                    1 => "1 appareil abonné".to_string(),
                    n => format!("{n} appareils abonnés"),
                }
            }),
            Some(
                if hub.has_mode_token() {
                    "Code secret : en service"
                } else {
                    "pas encore de code secret"
                }
                .to_string(),
            ),
        ];
        let titles = [
            "Depuis un ordinateur de la maison",
            "Depuis l'iPhone, même hors de la maison",
            "Notifications sur l'iPhone et l'iPad",
            "Changer de mode automatiquement",
        ];
        // Une seule carte ouverte à la fois : cliquer l'ouverte la replie.
        let mut clicked = None;
        for (i, title) in titles.iter().enumerate() {
            let open = self.connect_open == i;
            let toggled =
                widgets::fold_card(ui, title, summaries[i].as_deref(), open, |ui| match i {
                    0 => {
                        self.desktop_steps(ui, st);
                        widgets::hint(
                            ui,
                            "Mot de passe oublié ? Il se remplace dans l'onglet Réglages.",
                        );
                    }
                    1 => self.remote_steps(ui, st),
                    2 => self.push_section(ui, &hub),
                    _ => self.shortcuts_steps(ui, &hub),
                });
            if toggled {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            // Le code secret n'est montré qu'une fois : il disparaît avec sa carte.
            if self.connect_open == 3 {
                self.mode_token_shown = None;
            }
            self.connect_open = if self.connect_open == i {
                usize::MAX
            } else {
                i
            };
        }
    }

    /// L'accès à distance par Tailscale, pas à pas : chaque étape dit où on en est.
    fn remote_steps(&mut self, ui: &mut egui::Ui, st: &State) {
        let p = theme::palette(ui);
        ui.label("Ostal s'ouvre dans Safari sur l'iPhone, à la maison comme ailleurs. Rien n'est ouvert sur Internet : l'iPhone et ce Mac se retrouvent par Tailscale, un réseau privé gratuit pour un usage personnel.");
        ui.add_space(6.0);
        let web = self.web.clone();
        let busy = self.web_job.as_ref().is_some_and(|(toggled, _)| *toggled);
        let installed = !matches!(
            web,
            WebAccess::NotInstalled | WebAccess::Disconnected | WebAccess::Checking
        );
        let on = matches!(web, WebAccess::On { .. });
        let mut recheck = false;
        let mut toggle = false;

        // 1. Tailscale sur ce Mac.
        let first = match web {
            WebAccess::Checking => StepState::Todo,
            _ if installed => StepState::Done,
            _ => StepState::Current,
        };
        widgets::step(ui, 1, first, |ui| {
            ui.label(RichText::new("Tailscale sur ce Mac").strong());
            match &web {
                WebAccess::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Vérification…");
                    });
                }
                WebAccess::NotInstalled => {
                    ui.label(format!(
                        "Tailscale n'est pas installé. Installez-le depuis {}, ouvrez-le et connectez-vous (un compte Google ou Apple suffit).",
                        HelpPage::TailscaleDownload.shown()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Ouvrir le site de Tailscale").clicked() {
                            system::open_help(HelpPage::TailscaleDownload);
                        }
                        recheck = ui.button("C'est fait, vérifier").clicked();
                    });
                }
                WebAccess::Disconnected => {
                    ui.label("Tailscale est installé, mais déconnecté. Ouvrez Tailscale (son icône est dans la barre de menus) et connectez-vous.");
                    recheck = ui.button("C'est fait, vérifier").clicked();
                }
                WebAccess::Unavailable => {
                    ui.label("Tailscale ne répond pas pour l'instant.");
                    recheck = ui.button("Vérifier de nouveau").clicked();
                }
                _ => {
                    ui.label(RichText::new("Installé et connecté.").color(p.text_secondary));
                }
            }
        });

        // 2. L'accès à distance lui-même.
        let second = match (&web, on) {
            (_, true) => StepState::Done,
            (WebAccess::Off { .. } | WebAccess::NeedsAuth { .. }, _) => StepState::Current,
            _ => StepState::Todo,
        };
        widgets::step(ui, 2, second, |ui| {
            ui.label(RichText::new("Activer l'accès à distance").strong());
            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Un instant…");
                });
                return;
            }
            match &web {
                WebAccess::On { .. } => {
                    ui.label(RichText::new("Activé : ce Mac est joignable par vos appareils connectés à votre compte Tailscale, et par eux seuls.").color(p.text_secondary));
                    toggle = ui.button("Désactiver l'accès à distance").clicked();
                }
                WebAccess::Off { .. } => {
                    ui.label("Un clic suffit. Ce réglage reste en place, même après un redémarrage du Mac.");
                    toggle = theme::primary_button(ui, "Activer l'accès à distance").clicked();
                }
                WebAccess::NeedsAuth { url } => {
                    ui.label("Tailscale demande une autorisation, une seule fois : ouvrez sa page, acceptez (« Enable HTTPS »), puis revenez activer.");
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, "Ouvrir la page d'autorisation").clicked() {
                            system::open_url(url);
                        }
                        toggle = ui.button("C'est fait, activer").clicked();
                    });
                }
                WebAccess::PortBusy => {
                    ui.colored_label(p.warn, "Tailscale publie déjà autre chose depuis ce Mac (un autre « serve » occupe le port 443). Ostal n'y touche pas : libérez-le dans Tailscale, puis vérifiez.");
                    recheck = ui.button("Vérifier de nouveau").clicked();
                }
                WebAccess::Funnel => {
                    ui.colored_label(p.error, "Attention : Tailscale expose ce Mac à tout Internet (fonction « Funnel »). Ostal ne l'active ni ne le coupe jamais : coupez-le dans Tailscale, puis vérifiez.");
                    recheck = ui.button("Vérifier de nouveau").clicked();
                }
                _ => {
                    ui.label(RichText::new("Après l'étape 1.").color(p.text_secondary));
                }
            }
        });

        // 3 et 4. Côté iPhone.
        let phone = if on {
            StepState::Current
        } else {
            StepState::Todo
        };
        widgets::step(ui, 3, phone, |ui| {
            ui.label(RichText::new("Tailscale sur l'iPhone").strong());
            ui.label("Installez l'app Tailscale depuis l'App Store et connectez-vous avec le même compte que sur ce Mac.");
        });
        widgets::step(ui, 4, phone, |ui| {
            ui.label(RichText::new("Ouvrir Ostal dans Safari").strong());
            match self.web.web_url() {
                Some(url) => {
                    ui.label("Sur l'iPhone, ouvrez cette adresse dans Safari, puis entrez l'identifiant et le mot de passe de votre compte :");
                    self.copy_field(ui, "web-url", &url);
                    ui.horizontal(|ui| {
                        if ui.button("Essayer sur ce Mac").clicked() {
                            system::open_url(&url);
                        }
                    });
                    widgets::hint(ui, "Pour l'avoir comme une app : dans Safari, bouton Partager → « Sur l'écran d'accueil ». Astuce : copiez l'adresse ici, elle arrive sur l'iPhone par le presse-papiers universel, ou envoyez-la-vous par message.");
                }
                None => {
                    ui.label(
                        RichText::new(
                            "L'adresse à ouvrir s'affichera ici une fois l'accès activé.",
                        )
                        .color(p.text_secondary),
                    );
                }
            }
        });
        if recheck {
            self.web_stale = true;
        }
        if toggle {
            self.toggle_web(st);
        }
    }

    /// Les notifications web push (ticket n° 17) : le réglage général et les appareils abonnés,
    /// révocables. L'abonnement lui-même se fait depuis l'app web de l'appareil.
    fn push_section(&mut self, ui: &mut egui::Ui, hub: &Arc<ostal_server::hub::Hub>) {
        let p = theme::palette(ui);
        let push = hub.push();
        let status = push.status();
        ui.label("Ostal peut prévenir l'iPhone ou l'iPad, même app fermée : passage devant une caméra, pile faible, appareil injoignable, règle « Notifier ».");
        ui.add_space(4.0);
        let mut enabled = status.enabled;
        if ui
            .checkbox(
                &mut enabled,
                "Envoyer les notifications aux appareils abonnés",
            )
            .changed()
        {
            self.set_push(enabled, hub);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Appareils abonnés").strong());
        if status.subscriptions.is_empty() {
            ui.label(RichText::new("Aucun pour l'instant.").color(p.text_secondary));
        }
        for sub in &status.subscriptions {
            ui.horizontal(|ui| {
                ui.label(format!("{} · {}", sub.label, sub.service));
                if let Some(error) = &sub.last_error {
                    ui.colored_label(p.error, format!("dernier envoi : {error}"));
                }
                if theme::destructive_button(ui, "Révoquer…").clicked()
                    && confirm::ask(&Confirmation::RevokeSubscription {
                        label: sub.label.clone(),
                    })
                {
                    self.revoke_subscription(&sub.id, hub);
                }
            });
        }
        ui.add_space(4.0);
        widgets::hint(ui, "Pour abonner un appareil : ouvrez Ostal sur l'iPhone depuis son icône de l'écran d'accueil (étape 4 ci-dessus, iOS 16.4 ou plus), puis Appareils › Notifications. Les messages passent chiffrés par le service de notifications d'Apple.");
    }

    /// Le jeton de mode, pas à pas. Le jeton n'est montré qu'à sa création ; ensuite le serveur
    /// n'en connaît que l'empreinte.
    fn shortcuts_steps(&mut self, ui: &mut egui::Ui, hub: &Arc<ostal_server::hub::Hub>) {
        let p = theme::palette(ui);
        ui.label("L'iPhone peut passer la maison en « Absent » quand vous partez et en « Présent » quand vous rentrez, tout seul, avec l'app Raccourcis. Il lui faut un code secret (le « jeton de mode »), qui ne permet rien d'autre que changer le mode.");
        widgets::hint(ui, "Plus simple : sur l'iPhone, app web › Widgets et raccourcis › « Installer les raccourcis » crée un nouveau code et les deux raccourcis tout prêts, sans rien taper.");
        let read = hub.has_read_token();
        ui.horizontal(|ui| {
            ui.label(if read {
                "Widget de l'iPhone : jeton de lecture actif (il ne permet que de lire l'état)."
            } else {
                "Widget de l'iPhone : aucun jeton de lecture."
            });
            if read
                && theme::destructive_button(ui, "Révoquer…").clicked()
                && confirm::ask(&Confirmation::RevokeReadToken)
            {
                self.revoke_read(hub);
            }
        });
        ui.add_space(6.0);
        let remote = matches!(self.web, WebAccess::On { .. });
        let has = hub.has_mode_token();
        widgets::step(
            ui,
            1,
            if remote {
                StepState::Done
            } else {
                StepState::Current
            },
            |ui| {
                ui.label(RichText::new("Accès à distance activé").strong());
                if !remote {
                    ui.label("Activez d'abord l'accès à distance, juste au-dessus : Raccourcis en a besoin.");
                }
            },
        );
        let second = match (remote, has, self.mode_token_shown.is_some()) {
            (_, _, true) => StepState::Current,
            (_, true, _) => StepState::Done,
            (true, false, _) => StepState::Current,
            (false, false, _) => StepState::Todo,
        };
        widgets::step(ui, 2, second, |ui| {
            ui.label(RichText::new("Créer le code secret").strong());
            if let Some(token) = self.mode_token_shown.clone() {
                ui.colored_label(
                    p.ok,
                    "Voici votre code secret. Copiez-le maintenant : il ne sera plus jamais affiché.",
                );
                self.copy_field(ui, "mode-token", &token);
                if ui.button("C'est copié, masquer le code").clicked() {
                    self.mode_token_shown = None;
                }
                return;
            }
            ui.label(if has {
                "Un code secret existe déjà. Il ne peut pas être réaffiché : si vous l'avez perdu, créez-en un nouveau."
            } else {
                "Aucun code secret pour l'instant : Raccourcis ne peut pas changer le mode."
            });
            if self.mode_token_confirm {
                ui.colored_label(
                    p.error,
                    "L'ancien code cessera de fonctionner : les automatisations déjà créées sur l'iPhone devront être mises à jour.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Oui, créer un nouveau code").clicked() {
                        self.generate_mode_token(hub);
                    }
                    if ui.button("Annuler").clicked() {
                        self.mode_token_confirm = false;
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                if has {
                    if ui.button("Créer un nouveau code…").clicked() {
                        self.mode_token_confirm = true;
                    }
                    if theme::destructive_button(ui, "Supprimer le code…").clicked()
                        && confirm::ask(&Confirmation::DeleteModeToken)
                    {
                        self.delete_mode_token(hub);
                    }
                } else {
                    // Sans accès à distance, le code ne servirait à rien : la raison est écrite.
                    let ready = matches!(self.web, WebAccess::On { .. });
                    let button = egui::Button::new(
                        RichText::new("Créer le code secret")
                            .color(if ready { p.on_accent } else { p.text_secondary })
                            .strong(),
                    )
                    .fill(if ready { p.accent } else { p.surface_alt });
                    if ui.add_enabled(ready, button).clicked() {
                        self.generate_mode_token(hub);
                    }
                    if !ready {
                        widgets::hint(ui, "Activez d'abord l'accès à distance.");
                    }
                }
            });
        });
        widgets::step(ui, 3, StepState::Todo, |ui| {
            ui.label(RichText::new("Créer l'automatisation dans Raccourcis").strong());
            ui.label("Sur l'iPhone : Raccourcis → Automatisation → « + » → par exemple « Quitter » la maison → action « Obtenir le contenu de l'URL », avec :");
            let url = match &self.web {
                WebAccess::On { dns } => format!("https://{dns}/api/mode"),
                _ => "https://(l'adresse de l'étape 4 ci-dessus)/api/mode".to_string(),
            };
            egui::Grid::new("shortcut_fields")
                .num_columns(2)
                .show(ui, |ui| {
                    theme::detail_row(ui, "URL", url.clone());
                    theme::detail_row(ui, "Méthode", "PUT");
                    theme::detail_row(
                        ui,
                        "En-tête",
                        "Authorization = Bearer, une espace, puis le code secret",
                    );
                    theme::detail_row(
                        ui,
                        "Corps (JSON)",
                        "mode = absent (ou present, nuit, vacances)",
                    );
                });
            ui.horizontal(|ui| {
                if remote {
                    ui.label("Copier l'URL :");
                    self.copy_button(ui, "mode-url", &url);
                }
                if ui.button("Ouvrir le guide illustré").clicked() {
                    system::open_help(HelpPage::ShortcutsGuide);
                }
            });
            widgets::hint(ui, "Choisissez « Exécuter immédiatement », sinon l'iPhone attend votre confirmation à chaque fois.");
        });
    }

    fn generate_mode_token(&mut self, hub: &Arc<ostal_server::hub::Hub>) {
        self.mode_token_confirm = false;
        match hub.generate_mode_token() {
            Ok(token) => self.mode_token_shown = Some(token),
            Err(e) if e.code == protocol::wire::ErrorCode::Busy => {
                self.local_notice = Some(Notice::Err(BUSY_NOTICE.into()));
                self.local_retry = Some(Retry::GenerateModeToken);
            }
            Err(e) => self.fail(
                "Le code n'a pas été créé",
                e.code,
                Some(Retry::GenerateModeToken),
            ),
        }
    }

    // ---- réglages ------------------------------------------------------------------------------

    fn settings_page(&mut self, ui: &mut egui::Ui, st: &State) {
        let p = theme::palette(ui);
        if st.phase == Phase::Running {
            widgets::section(ui, "Compte", |ui| {
                if self.change_account {
                    self.account_page(ui, st, true);
                    if ui.button("Annuler").clicked() {
                        self.change_account = false;
                        // Aucun mot de passe ne reste en mémoire dans le formulaire.
                        self.form = AccountForm::default();
                    }
                } else {
                    ui.label("L'identifiant et le mot de passe demandés par Ostal sur vos autres appareils. Il n'y a qu'un seul compte.");
                    if let Some(n) = &st.notice {
                        notice_label(ui, n);
                    }
                    if ui
                        .button("Changer l'identifiant ou le mot de passe…")
                        .clicked()
                    {
                        self.change_account = true;
                    }
                }
            });
        }
        widgets::section(ui, "Démarrage et veille", |ui| {
            if self.login_available() {
                let mut on = self.login_enabled;
                if ui
                    .checkbox(
                        &mut on,
                        "Lancer Ostal Server à l'ouverture de session de ce Mac",
                    )
                    .changed()
                {
                    self.set_login(on);
                }
                widgets::hint(ui, "Conseillé : après une coupure de courant ou un redémarrage, le serveur repart tout seul.");
            } else if self.demo {
                ui.label("Mode démo : le lancement à l'ouverture de session est désactivé.");
            } else if let Ok(exe) = std::env::current_exe() {
                if agent::is_temporary_location(&exe) {
                    ui.label(agent::refusal(&exe));
                }
            }
            ui.add_space(4.0);
            widgets::hint(ui, SLEEP_NOTE);
        });
        widgets::section(ui, "Sauvegarde", |ui| {
            ui.label("La sauvegarde se fait depuis Ostal : menu Fichier → « Exporter mes données… ». Elle produit un fichier « .ostal » à garder comme un secret (il contient les clés de la maison et les mots de passe des caméras).");
            widgets::hint(ui, "Pour la reprendre sur un nouveau Mac : installez Ostal Server, et choisissez « Reprendre une sauvegarde » au premier lancement.");
        });
        widgets::section(ui, "Dépannage", |ui| {
            ui.label("Le journal est le fichier où le serveur note ce qu'il fait. En cas de panne, c'est lui qu'on vous demandera.");
            ui.horizontal(|ui| {
                if ui.button("Ouvrir le journal").clicked() {
                    system::open_file(&st.log_path);
                }
                if ui.button("Montrer le dossier des données").clicked() {
                    system::open_folder(&st.data_dir);
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("about").num_columns(2).show(ui, |ui| {
                theme::detail_row(ui, "Version", version::display_version());
                ui.label(RichText::new("Données").color(p.text_secondary));
                ui.add(egui::Label::new(st.data_dir.display().to_string()).wrap());
                ui.end_row();
            });
        });
    }
}
