//! Les pages de la fenêtre (voir `docs/ux-serveur.md`).
//!
//! - Première installation : trois étapes (la maison, le compte, « c'est prêt »).
//! - Ensuite, cinq onglets : **Vue d'ensemble** (est-ce que tout va bien ?), **Appareils**,
//!   **Caméras**, **Connexion** (Mac ou PC de la maison, iPhone à distance, Raccourcis) et
//!   **Réglages** (compte, démarrage, sauvegarde, dépannage).
//!
//! Les mots sont ceux de l'utilisateur ; le terme technique, quand il aide à se repérer dans la
//! documentation, suit entre parenthèses. Aucun secret n'est affiché, sauf le jeton de mode à sa
//! création (une seule fois).

use super::widgets::{self, StepState};
use super::*;
use ostal_server_app::controller::Notice;
use ostal_server_app::overview::Level;
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
        let t = theme::tokens();
        ui.add_space(t.space.sm);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = t.space.xs;
            for tab in Tab::ALL {
                if theme::tab(ui, self.tab == tab, tab.label()).clicked() {
                    self.open_tab(tab);
                }
            }
        });
        ui.add_space(t.space.sm);
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
        ui.add_space(6.0);
        if let Some(busy) = &st.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(busy);
            });
            return;
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
        if let Some(n) = &self.local_notice {
            ui.add_space(8.0);
            notice_label(ui, n);
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
                    } else if ui.button("Reprendre une sauvegarde…").clicked() {
                        pick = true;
                    }
                    if setup::import_in_progress(&st.data_dir) {
                        ui.add_space(6.0);
                        ui.label("Un import précédent n'est pas terminé.");
                        if ui.button("Renoncer à l'import en attente").clicked() {
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
        if let Err(e) = &check {
            if !self.form.password.is_empty() || !self.form.username.is_empty() {
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
        let detail = match (&st.phase, o.level) {
            (Phase::Running, Level::Ok) => Some(
                "Le serveur tourne. Vous pouvez fermer cette fenêtre : il continue en arrière-plan.",
            ),
            (Phase::Running, _) => Some("Le serveur tourne, mais quelque chose demande un coup d'œil."),
            _ => None,
        };
        widgets::headline(ui, o.level, &o.headline, detail);
        match &st.phase {
            Phase::Starting | Phase::Stopping | Phase::NeedsSetup => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Un instant…");
                });
            }
            Phase::Blocked(b) => {
                let mut retry = false;
                widgets::section(ui, "Pourquoi, et quoi faire", |ui| {
                    ui.label(b.explain());
                    ui.add_space(6.0);
                    retry = theme::primary_button(ui, "Réessayer maintenant").clicked();
                });
                if retry {
                    let _ = self.handle.tx.send(Cmd::Retry);
                }
            }
            Phase::Problem(e) => {
                let (mut retry, mut log) = (false, false);
                widgets::section(ui, "Ce qui s'est passé", |ui| {
                    ui.colored_label(theme::palette(ui).error, e);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        retry = theme::primary_button(ui, "Réessayer").clicked();
                        log = ui.button("Ouvrir le journal").clicked();
                    });
                    widgets::hint(ui, "Le journal est le fichier où le serveur note ce qu'il fait : il aide à comprendre une panne.");
                });
                if retry {
                    let _ = self.handle.tx.send(Cmd::Retry);
                }
                if log {
                    system::open_file(&st.log_path);
                }
            }
            Phase::Running => self.overview_running(ui, st, &o),
        }
    }

    fn overview_running(&mut self, ui: &mut egui::Ui, st: &State, o: &Overview) {
        let mut go = None;
        widgets::section(ui, "État", |ui| {
            for check in &o.checks {
                // Sans problème, le bouton mène simplement à l'onglet du sujet.
                let (label, tab) = match (&check.fix, check.title) {
                    (Some(fix), _) => (
                        Some(fix.label.trim_end_matches('…').to_string()),
                        Some(fix.tab),
                    ),
                    (None, "Appareils") => (Some("Voir".to_string()), Some(Tab::Devices)),
                    (None, "Caméras") => (Some("Voir".to_string()), Some(Tab::Cameras)),
                    (None, "Accès à distance") => {
                        (Some("Configurer".to_string()), Some(Tab::Connect))
                    }
                    _ => (None, None),
                };
                let mut text = check.text.clone();
                if let Some(first) = text.get(..1) {
                    text = first.to_uppercase() + &text[1..];
                }
                if widgets::status_row(ui, check.level, check.title, &text, label.as_deref()) {
                    go = tab;
                }
            }
        });

        if let Some(hub) = &st.hub {
            let current = hub.0.mode();
            let mut chosen = None;
            widgets::section(ui, "Mode de la maison", |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = theme::tokens().space.xs;
                    for mode in HomeMode::ALL {
                        if theme::tab(ui, mode == current, mode.label()).clicked()
                            && mode != current
                        {
                            chosen = Some(mode);
                        }
                    }
                });
                widgets::hint(ui, &format!("{} Le mode décide ce que font les caméras (éteintes, visibles, en surveillance) et certains automatismes.", mode_hint(current)));
            });
            if let Some(mode) = chosen {
                self.set_mode(st, mode);
            }
        }

        widgets::section(ui, "Que voulez-vous faire ?", |ui| {
            let tasks = [
                ("Connecter un ordinateur de la maison", Tab::Connect),
                ("Utiliser l'iPhone, même hors de la maison", Tab::Connect),
                ("Ajouter un appareil", Tab::Devices),
                ("Ajouter ou régler une caméra", Tab::Cameras),
                ("Changer mon mot de passe", Tab::Settings),
                ("Comprendre une panne", Tab::Settings),
            ];
            ui.horizontal_wrapped(|ui| {
                for (label, tab) in tasks {
                    if ui.button(label).clicked() {
                        go = Some(tab);
                    }
                }
            });
        });
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
        if st.phase != Phase::Running {
            return self.needs_server(ui);
        }
        let p = theme::palette(ui);
        let health = &st.health;
        widgets::section(ui, "Vos appareils", |ui| {
            if health.total == 0 {
                ui.label("Aucun appareil pour l'instant : ajoutez le premier en suivant les étapes ci-dessous.");
                return;
            }
            ui.label(overview::devices_sentence(health));
            if !health.unreachable.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Injoignables en ce moment").strong());
                for name in &health.unreachable {
                    ui.horizontal(|ui| {
                        widgets::dot(ui, p.error, theme::tokens().size.dot);
                        ui.label(name);
                    });
                }
                ui.add_space(4.0);
                widgets::hint(ui, "Le plus souvent : l'appareil est débranché, sa pile est vide, ou il est trop loin du Wi-Fi ou de son pont. Il revient tout seul dès qu'il répond de nouveau. Sinon, ouvrez-le dans Ostal et cliquez « Reconnecter ».");
            }
        });
        widgets::section(ui, "Ajouter un appareil", |ui| {
            ui.label("L'ajout ne se fait pas ici, mais dans l'application Ostal (sur votre ordinateur), avec l'app Maison d'Apple :");
            ui.add_space(6.0);
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
        let mut connect = false;
        widgets::section(ui, "Voir et piloter vos appareils", |ui| {
            ui.label("Ce Mac ne fait que tourner en arrière-plan. Pour voir les températures, allumer une lampe ou créer un automatisme, ouvrez Ostal sur votre ordinateur ou votre iPhone.");
            ui.add_space(4.0);
            connect = ui.button("Comment se connecter").clicked();
        });
        if connect {
            self.open_tab(Tab::Connect);
        }
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
        widgets::section(ui, "Depuis un ordinateur de la maison", |ui| {
            self.desktop_steps(ui, st);
            widgets::hint(
                ui,
                "Mot de passe oublié ? Il se remplace dans l'onglet Réglages.",
            );
        });
        widgets::section(ui, "Depuis l'iPhone, même hors de la maison", |ui| {
            self.remote_steps(ui, st);
        });
        widgets::section(ui, "Notifications sur l'iPhone et l'iPad", |ui| {
            self.push_section(ui, &hub);
        });
        widgets::section(
            ui,
            "Changer de mode automatiquement (Raccourcis de l'iPhone)",
            |ui| {
                self.shortcuts_steps(ui, &hub);
            },
        );
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
            && push.set_enabled(enabled).is_err()
        {
            self.local_notice = Some(Notice::Err(
                "Le réglage n'a pas pu être enregistré (voir le journal, onglet Réglages).".into(),
            ));
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
                if ui.button("Révoquer").clicked() && push.remove(&sub.id).is_err() {
                    self.local_notice = Some(Notice::Err(
                        "L'abonnement n'a pas pu être révoqué (voir le journal, onglet Réglages)."
                            .into(),
                    ));
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
            if read && ui.button("Révoquer").clicked() {
                self.local_notice = Some(match hub.revoke_read_token() {
                    Ok(()) => {
                        Notice::Ok("Jeton de lecture révoqué : le widget ne lit plus rien.".into())
                    }
                    Err(e) if e.code == protocol::wire::ErrorCode::Busy => {
                        Notice::Err(BUSY_NOTICE.into())
                    }
                    Err(_) => Notice::Err(
                        "Le jeton n'a pas pu être révoqué (voir le journal, onglet Réglages)."
                            .into(),
                    ),
                });
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
                    if ui.button("Supprimer le code").clicked() {
                        self.local_notice = Some(match hub.revoke_mode_token() {
                            Ok(()) => Notice::Ok("Code secret supprimé : Raccourcis ne peut plus changer le mode.".into()),
                            Err(e) if e.code == protocol::wire::ErrorCode::Busy => Notice::Err(BUSY_NOTICE.into()),
                            Err(_) => Notice::Err("Le code n'a pas pu être supprimé (voir le journal, onglet Réglages).".into()),
                        });
                    }
                } else if theme::primary_button(ui, "Créer le code secret").clicked() {
                    self.generate_mode_token(hub);
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
            }
            Err(_) => {
                self.local_notice = Some(Notice::Err(
                    "Le code n'a pas pu être créé (voir le journal, onglet Réglages).".into(),
                ));
            }
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
