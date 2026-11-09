//! L'interface : icône de la barre de menus (tray-icon) et petite fenêtre egui, toutes deux sur
//! le fil principal, dans la boucle d'événements d'eframe. La fenêtre est cachée tant qu'on ne la
//! demande pas ; `ui()` continue de tourner (réveillée par les événements du menu et par un
//! minuteur) et met l'icône et le menu à jour.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use matter_home_server::backup;
use matter_home_server::setup::{self, IdentityComparison, IdentityState};
use matter_home_server_app::controller::{Cmd, Handle, Notice, State};
use matter_home_server_app::menu::{self, Action, Entry, MenuInput};
use matter_home_server_app::startup::SetupNeed;
use matter_home_server_app::status::{icon_kind, tooltip, IconKind, Phase};
use matter_home_server_app::{agent, icon, system};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Attente maximale de l'arrêt propre du serveur à la fermeture.
const EXIT_WAIT: Duration = Duration::from_secs(20);

const SLEEP_NOTE: &str = "Tant que Matter Home Server est ouvert, ce Mac ne se met pas en veille tout seul (l'écran peut s'éteindre). Si vous fermez le capot d'un portable, il s'endort quand même : laissez le capot ouvert, ou branchez-le sur secteur avec un écran externe.";

pub fn run(handle: Handle, demo: bool) -> eframe::Result {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Matter Home Server")
            .with_inner_size([500.0, 640.0])
            .with_min_inner_size([420.0, 480.0])
            .with_visible(false),
        // Pas d'icône dans le Dock : l'application ne vit que dans la barre de menus (la clé
        // LSUIElement de l'Info.plist fait la même chose pour l'application installée).
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        })),
        ..Default::default()
    };
    // Portable allumé jour et nuit : le GPU intégré suffit à dessiner une petite fenêtre, le GPU
    // discret (choix par défaut d'egui-wgpu, « haute performance ») resterait allumé en permanence.
    low_power_gpu(&mut options.wgpu_options);
    eframe::run_native(
        "Matter Home Server",
        options,
        Box::new(move |cc| {
            // L'icône doit être créée sur le fil principal, une fois la boucle d'événements lancée.
            let (tx, rx) = mpsc::channel::<Action>();
            let ctx = cc.egui_ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some(action) = Action::from_id(&event.id.0) {
                    let _ = tx.send(action);
                    ctx.request_repaint();
                }
            }));
            let tray = TrayIconBuilder::new()
                .with_icon_templated(tray_image(IconKind::Starting))
                .with_tooltip("Matter Home Server")
                .build()
                .map_err(|e| format!("icône de la barre de menus : {e}"))?;
            Ok(Box::new(App::new(handle, demo, tray, rx)))
        }),
    )
}

/// Demande l'adaptateur graphique économe (le GPU intégré).
fn low_power_gpu(config: &mut eframe::egui_wgpu::WgpuConfiguration) {
    use eframe::egui_wgpu::{wgpu, WgpuSetup};
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.power_preference = wgpu::PowerPreference::LowPower;
    }
}

fn tray_image(kind: IconKind) -> Icon {
    Icon::from_rgba(icon::rgba(kind), icon::SIZE, icon::SIZE).expect("icône valide")
}

#[derive(Default)]
enum Page {
    #[default]
    Start,
    Restore(Box<Restore>),
    NewHome,
}

struct Restore {
    file: PathBuf,
    summary: String,
    comparison: IdentityComparison,
    replace_ack: bool,
    stopped_ack: bool,
}

#[derive(Default)]
struct AccountForm {
    username: String,
    password: String,
    confirm: String,
}

/// Lit et compare une sauvegarde (hors du fil principal).
fn inspect_backup(data_dir: &std::path::Path, file: PathBuf) -> Result<Restore, String> {
    let inspected = backup::inspect(&file).map(|s| s.describe());
    let comparison = setup::compare_identities(data_dir, &file);
    match (inspected, comparison) {
        (Ok(summary), Ok(comparison)) => Ok(Restore {
            file,
            summary,
            comparison,
            replace_ack: false,
            stopped_ack: false,
        }),
        (Err(e), _) | (_, Err(e)) => Err(format!(
            "Ce fichier n'est pas une sauvegarde utilisable : {e:#}"
        )),
    }
}

impl AccountForm {
    fn check(&self) -> Result<(), String> {
        setup::check_credentials(&self.username, &self.password)?;
        if self.password != self.confirm {
            return Err("Les deux mots de passe sont différents.".into());
        }
        Ok(())
    }
}

struct App {
    handle: Handle,
    demo: bool,
    tray: TrayIcon,
    actions: Receiver<Action>,
    shown_menu: Vec<Entry>,
    shown_icon: Option<IconKind>,
    shown_tooltip: String,
    visible: bool,
    last_phase: Option<Phase>,
    page: Page,
    form: AccountForm,
    change_account: bool,
    /// La configuration a été faite dans cette session : on montre l'écran de fin.
    setup_session: bool,
    show_done: bool,
    login_enabled: bool,
    login_checked_at: std::time::Instant,
    local_notice: Option<Notice>,
    /// Analyse d'une sauvegarde en cours (fil d'arrière-plan).
    picking: Option<Receiver<Result<Restore, String>>>,
    // Interrupteur : l'écran de fin a déjà activé l'ouverture à la connexion.
    login_defaulted: bool,
}

impl App {
    fn new(handle: Handle, demo: bool, tray: TrayIcon, actions: Receiver<Action>) -> Self {
        let app = Self {
            handle,
            demo,
            tray,
            actions,
            shown_menu: Vec::new(),
            shown_icon: None,
            shown_tooltip: String::new(),
            visible: false,
            last_phase: None,
            page: Page::Start,
            form: AccountForm::default(),
            change_account: false,
            setup_session: false,
            show_done: false,
            login_enabled: false,
            login_checked_at: std::time::Instant::now() - Duration::from_secs(60),
            local_notice: None,
            login_defaulted: false,
            picking: None,
        };
        // Application déplacée : l'agent pointait ailleurs, on le réécrit.
        if !app.demo {
            if let (Some(dir), Ok(exe)) = (agent::default_dir(), std::env::current_exe()) {
                agent::rewrite_if_stale(&dir, &exe);
            }
        }
        app
    }

    fn login_available(&self) -> bool {
        !self.demo
            && agent::default_dir().is_some()
            && std::env::current_exe().is_ok_and(|e| agent::is_installable(&e))
    }

    /// Première fois que le serveur tourne sans choix enregistré (fin de la configuration, ou
    /// passage du service à l'application avec des données déjà là) : ouverture à la connexion
    /// activée par défaut. Un choix déjà enregistré, même « non », est respecté.
    fn apply_login_default(&mut self) {
        if self.login_defaulted || self.demo {
            return;
        }
        let (Some(dir), Some(marker), Ok(exe)) = (
            agent::default_dir(),
            agent::default_marker_dir(),
            std::env::current_exe(),
        ) else {
            return;
        };
        self.login_defaulted = true;
        match agent::apply_default(&dir, &marker, &exe, self.demo) {
            agent::DefaultOutcome::Failed(e) => {
                log::warn!("ouverture à la connexion non activée : {e}");
                self.local_notice = Some(Notice::Err(e));
            }
            agent::DefaultOutcome::Enabled => {
                log::info!("ouverture à la connexion activée par défaut");
                self.login_checked_at = std::time::Instant::now() - Duration::from_secs(60);
            }
            _ => {}
        }
    }

    fn refresh_login(&mut self) {
        if self.login_checked_at.elapsed() > Duration::from_secs(2) {
            self.login_checked_at = std::time::Instant::now();
            self.login_enabled = agent::default_dir().is_some_and(|d| agent::is_enabled(&d));
        }
    }

    fn set_login(&mut self, on: bool) {
        let (Some(dir), Ok(exe)) = (agent::default_dir(), std::env::current_exe()) else {
            return;
        };
        let result = if on {
            agent::enable(&dir, &exe)
        } else {
            agent::disable(&dir)
        };
        match result {
            Ok(()) => {
                // Le choix de l'utilisateur est enregistré : il sera respecté aux lancements suivants.
                self.local_notice = match agent::default_marker_dir()
                    .map(|d| agent::record_choice(&d, on))
                    .transpose()
                {
                    Ok(_) => None,
                    Err(e) => Some(Notice::Err(format!("{e:#}"))),
                };
            }
            Err(e) => self.local_notice = Some(Notice::Err(format!("{e:#}"))),
        }
        self.login_checked_at = std::time::Instant::now() - Duration::from_secs(60);
        self.refresh_login();
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.visible = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn confirm_quit(&self) -> bool {
        use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
        let quit = "Quitter".to_string();
        let result = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Quitter Matter Home Server ?")
            .set_description(
                "Si vous quittez, les automatismes (règles) ne s'exécutent plus, les capteurs ne sont plus relevés et le client ne peut plus se connecter, jusqu'à la prochaine ouverture de Matter Home Server.",
            )
            .set_buttons(MessageButtons::OkCancelCustom(quit.clone(), "Annuler".into()))
            .show();
        result == MessageDialogResult::Ok || result == MessageDialogResult::Custom(quit)
    }

    fn do_action(&mut self, ctx: &egui::Context, action: Action, st: &State) {
        match action {
            Action::CopyUrl => {
                if let Some(u) = &st.url {
                    let _ = system::copy_to_clipboard(u);
                }
            }
            Action::CopyFingerprint => {
                if let Some(f) = &st.fingerprint {
                    let _ = system::copy_to_clipboard(f);
                }
            }
            Action::OpenLog => system::open_file(&st.log_path),
            Action::ToggleLogin => {
                if self.login_available() {
                    let on = !self.login_enabled;
                    self.set_login(on);
                }
            }
            Action::Settings => self.show_window(ctx),
            Action::Quit => {
                if self.confirm_quit() {
                    let _ = self.handle.tx.send(Cmd::Quit);
                }
            }
        }
    }

    /// Met à jour l'icône, l'infobulle et le menu quand ils ont changé.
    fn sync_tray(&mut self, st: &State) {
        let kind = icon_kind(&st.phase);
        if self.shown_icon != Some(kind) {
            self.shown_icon = Some(kind);
            let _ = self.tray.set_icon_templated(Some(tray_image(kind)));
        }
        let tip = tooltip(&st.phase, &st.health);
        if tip != self.shown_tooltip {
            let _ = self.tray.set_tooltip(Some(&tip));
            self.shown_tooltip = tip;
        }
        let model = menu::build(&MenuInput {
            phase: &st.phase,
            health: &st.health,
            url: st.url.as_deref(),
            fingerprint: st.fingerprint.as_deref(),
            login_enabled: self.login_enabled,
            login_available: self.login_available(),
        });
        if model != self.shown_menu {
            let native = Menu::new();
            for entry in &model {
                match entry {
                    Entry::Info(text) => {
                        let _ = native.append(&MenuItem::new(text, false, None));
                    }
                    Entry::Separator => {
                        let _ = native.append(&PredefinedMenuItem::separator());
                    }
                    Entry::Item {
                        action,
                        label,
                        enabled,
                        checked: None,
                    } => {
                        let _ =
                            native.append(&MenuItem::with_id(action.id(), label, *enabled, None));
                    }
                    Entry::Item {
                        action,
                        label,
                        enabled,
                        checked: Some(c),
                    } => {
                        let _ = native.append(&CheckMenuItem::with_id(
                            action.id(),
                            label,
                            *enabled,
                            *c,
                            None,
                        ));
                    }
                }
            }
            self.tray.set_menu(Some(Box::new(native)));
            self.shown_menu = model;
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        // Cmd+Q ou fin de session : arrêt propre du serveur avant de sortir.
        let _ = self.handle.tx.send(Cmd::Quit);
        // L'arrêt du serveur peut durer environ 17 s : on attend le signal du chef d'orchestre,
        // borné à 20 s.
        if !self.handle.exited.wait(EXIT_WAIT) {
            log::warn!(
                "arrêt propre non confirmé en {} s : sortie",
                EXIT_WAIT.as_secs()
            );
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let st = self.handle.snapshot();
        self.refresh_login();
        self.poll_picking();
        while let Ok(action) = self.actions.try_recv() {
            self.do_action(&ctx, action, &st);
        }

        // Changements de phase : la configuration s'ouvre toute seule ; la fin de la
        // configuration active l'ouverture à la connexion (par défaut).
        if self.last_phase.as_ref() != Some(&st.phase) {
            if st.phase == Phase::NeedsSetup {
                self.setup_session = true;
                self.show_window(&ctx);
            }
            if st.phase == Phase::Running {
                self.apply_login_default();
            }
            if st.phase == Phase::Running && self.setup_session {
                self.show_done = true;
                self.show_window(&ctx);
            }
            self.last_phase = Some(st.phase.clone());
        }
        self.sync_tray(&st);

        if ui.input(|i| i.viewport().close_requested()) {
            // La croix cache la fenêtre : le serveur continue, seul « Quitter » l'arrête.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(&ctx);
        }
        if self.visible {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.content(ui, &ctx, &st));
            });
        }
        // Fenêtre cachée : deux secondes suffisent pour tenir l'icône à jour (les clics du menu
        // réveillent l'interface tout de suite).
        ctx.request_repaint_after(Duration::from_millis(if self.visible { 500 } else { 2000 }));
    }
}

fn notice_label(ui: &mut egui::Ui, notice: &Notice) {
    match notice {
        Notice::Ok(t) => ui.colored_label(Color32::from_rgb(40, 150, 70), t),
        Notice::Err(t) => ui.colored_label(Color32::from_rgb(210, 60, 50), t),
    };
}

fn copy_row(ui: &mut egui::Ui, title: &str, value: &str) {
    ui.label(RichText::new(title).strong());
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
    });
    if ui.button("Copier").clicked() {
        let _ = system::copy_to_clipboard(value);
    }
    ui.add_space(6.0);
}

impl App {
    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &State) {
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
            _ => self.settings_page(ui, st),
        }
        if let Some(n) = &self.local_notice {
            ui.add_space(8.0);
            notice_label(ui, n);
        }
    }

    fn identity_page(&mut self, ui: &mut egui::Ui, st: &State, unreadable: Option<&str>) {
        ui.heading("Bienvenue dans Matter Home Server");
        ui.add_space(6.0);
        ui.label("Ce Mac va devenir le serveur de votre maison : il pilote vos appareils Matter, exécute les automatismes et garde l'historique des capteurs. Vos autres appareils (Mac, ordinateurs) s'y connectent avec l'application cliente.");
        ui.add_space(8.0);
        if let Some(n) = &st.notice {
            notice_label(ui, n);
            ui.add_space(6.0);
        }
        match &mut self.page {
            Page::Start => {
                if let Some(reason) = unreadable {
                    ui.colored_label(
                        Color32::from_rgb(210, 60, 50),
                        format!("Les données de ce Mac contiennent une identité Matter illisible ({reason}). Rien n'a été modifié. Vous pouvez reprendre une sauvegarde pour la remplacer."),
                    );
                    ui.add_space(6.0);
                } else {
                    ui.label("Aucune donnée n'a été trouvée ici. Que voulez-vous faire ?");
                    ui.add_space(8.0);
                }
                ui.label(RichText::new("J'ai déjà des appareils appairés").strong());
                ui.label("Reprenez la sauvegarde (fichier .matterhome) de l'ancienne application ou d'un autre Mac : rien à réappairer.");
                if self.picking.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Lecture de la sauvegarde…");
                    });
                } else if ui.button("Reprendre une sauvegarde…").clicked() {
                    self.pick_backup(&ui.ctx().clone(), st);
                }
                if setup::import_in_progress(&st.data_dir) {
                    ui.add_space(6.0);
                    ui.label("Un import précédent n'est pas terminé.");
                    if ui.button("Renoncer à l'import en attente").clicked() {
                        let _ = self.handle.tx.send(Cmd::AbandonImport);
                    }
                }
                if unreadable.is_none() {
                    ui.add_space(14.0);
                    ui.label(RichText::new("C'est ma première installation").strong());
                    ui.label("Vous partez de zéro : aucun appareil pour l'instant, vous les appairerez ensuite depuis le client.");
                    if ui.button("Créer une nouvelle maison…").clicked() {
                        self.page = Page::NewHome;
                    }
                }
            }
            Page::NewHome => {
                ui.label(RichText::new("Créer une nouvelle maison").strong());
                ui.label("Une nouvelle identité Matter va être créée sur ce Mac. Aucun appareil n'est repris : ceux déjà appairés avec une autre installation ne seront pas reconnus et il faudra les appairer de nouveau. Si vous avez une sauvegarde, ne continuez pas : reprenez-la.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Retour").clicked() {
                        self.page = Page::Start;
                    }
                    if ui.button("Créer une nouvelle maison").clicked() {
                        let _ = self.handle.tx.send(Cmd::NewHome);
                        self.page = Page::Start;
                    }
                });
            }
            Page::Restore(r) => {
                let mut back = false;
                let mut go = false;
                ui.label(RichText::new("Reprendre cette sauvegarde").strong());
                ui.label(format!(
                    "Fichier : {}",
                    r.file.file_name().unwrap_or_default().to_string_lossy()
                ));
                ui.label(format!("Contenu : {}", r.summary));
                ui.label(format!(
                    "Identité de la sauvegarde : {}",
                    r.comparison.incoming.describe()
                ));
                ui.label(match &r.comparison.current {
                    IdentityState::Missing => {
                        "Identité actuelle de ce Mac : aucune (rien n'est remplacé).".to_string()
                    }
                    IdentityState::Present(c) => {
                        format!("Identité actuelle de ce Mac : {}", c.describe())
                    }
                    IdentityState::Unreadable(e) => {
                        format!("Identité actuelle de ce Mac : illisible ({e})")
                    }
                });
                if r.comparison.differs {
                    ui.add_space(6.0);
                    ui.colored_label(
                        Color32::from_rgb(210, 60, 50),
                        setup::replace_refusal_for_app(),
                    );
                    ui.checkbox(&mut r.replace_ack, "Je veux bien remplacer l'identité actuelle (l'ancien dossier est conservé)");
                } else if matches!(r.comparison.current, IdentityState::Present(_)) {
                    ui.label("Même identité : les appareils appairés restent reconnus.");
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
                    if ui.button("Retour").clicked() {
                        back = true;
                    }
                    if ui
                        .add_enabled(ready, egui::Button::new("Reprendre cette sauvegarde"))
                        .clicked()
                    {
                        go = true;
                    }
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
            .set_title("Choisir une sauvegarde Matter Home")
            .add_filter("Sauvegarde Matter Home", &[backup::EXTENSION])
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
    fn poll_picking(&mut self) {
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
            ui.heading("Créez votre compte");
            ui.add_space(6.0);
            ui.label("C'est avec cet identifiant et ce mot de passe que vous vous connecterez depuis le client. Il n'y a qu'un seul compte.");
        } else {
            ui.label("Choisissez un nouvel identifiant et un nouveau mot de passe. Les clients déjà connectés devront se reconnecter, et le serveur redémarre quelques secondes.");
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
                ui.colored_label(Color32::from_rgb(210, 60, 50), e);
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
        ui.heading("Tout est prêt");
        ui.add_space(6.0);
        ui.label("Le serveur tourne. Dans le client Matter Home Desktop, saisissez l'adresse ci-dessous, puis comparez l'empreinte affichée par le client avec celle-ci avant d'accepter : si elles sont identiques, c'est bien votre serveur.");
        ui.add_space(8.0);
        if let Some(u) = &st.url {
            copy_row(ui, "Adresse du serveur", u);
        }
        if let Some(f) = &st.fingerprint {
            copy_row(ui, "Empreinte du certificat", f);
        }
        if self.login_available() {
            let mut on = self.login_enabled;
            if ui
                .checkbox(&mut on, "Ouvrir Matter Home Server à la connexion")
                .changed()
            {
                self.set_login(on);
            }
            ui.add_space(6.0);
        }
        ui.label(SLEEP_NOTE);
        ui.add_space(10.0);
        if ui.button("Terminer").clicked() {
            self.show_done = false;
            self.setup_session = false;
            self.hide_window(ctx);
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, st: &State) {
        ui.heading("Matter Home Server");
        ui.add_space(4.0);
        ui.label(matter_home_server_app::status::status_line(
            &st.phase, &st.health,
        ));
        ui.add_space(8.0);
        match &st.phase {
            Phase::Blocked(b) => {
                ui.label(b.explain());
                ui.add_space(6.0);
                if ui.button("Réessayer maintenant").clicked() {
                    let _ = self.handle.tx.send(Cmd::Retry);
                }
            }
            Phase::Problem(e) => {
                ui.colored_label(Color32::from_rgb(210, 60, 50), e);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Ouvrir le journal").clicked() {
                        system::open_file(&st.log_path);
                    }
                    if ui.button("Réessayer").clicked() {
                        let _ = self.handle.tx.send(Cmd::Retry);
                    }
                });
            }
            Phase::Running => {
                if let Some(u) = &st.url {
                    copy_row(ui, "Adresse du serveur", u);
                }
                if let Some(f) = &st.fingerprint {
                    copy_row(ui, "Empreinte du certificat", f);
                }
                if !st.health.unreachable.is_empty() {
                    ui.label(RichText::new("Appareils injoignables").strong());
                    for n in &st.health.unreachable {
                        ui.label(format!("• {n}"));
                    }
                    ui.add_space(6.0);
                }
                ui.separator();
                ui.label(RichText::new("Compte").strong());
                if self.change_account {
                    self.account_page(ui, st, true);
                    if ui.button("Annuler").clicked() {
                        self.change_account = false;
                        // Aucun mot de passe ne reste en mémoire dans le formulaire.
                        self.form = AccountForm::default();
                    }
                } else {
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
            }
            _ => {}
        }
        ui.add_space(10.0);
        ui.separator();
        if self.login_available() {
            let mut on = self.login_enabled;
            if ui
                .checkbox(&mut on, "Ouvrir Matter Home Server à la connexion")
                .changed()
            {
                self.set_login(on);
            }
        } else if self.demo {
            ui.label("Mode démo : l'ouverture à la connexion est désactivée.");
        } else if let Ok(exe) = std::env::current_exe() {
            if agent::is_temporary_location(&exe) {
                ui.label(agent::refusal(&exe));
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Veille").strong());
        ui.label(SLEEP_NOTE);
        ui.add_space(6.0);
        ui.label(format!("Données : {}", st.data_dir.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui_wgpu::{wgpu, WgpuConfiguration, WgpuSetup};

    #[test]
    fn gpu_preference_is_low_power() {
        let mut config = WgpuConfiguration::default();
        low_power_gpu(&mut config);
        let WgpuSetup::CreateNew(setup) = &config.wgpu_setup else {
            panic!("configuration inattendue");
        };
        assert_eq!(setup.power_preference, wgpu::PowerPreference::LowPower);
    }

    #[test]
    fn an_unreadable_backup_is_reported_without_a_panic() {
        let dir = std::env::temp_dir().join(format!("mh-ui-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("faux.matterhome");
        std::fs::write(&file, "ce n'est pas une sauvegarde").unwrap();
        let err = inspect_backup(&dir, file).err().expect("refusé");
        assert!(err.contains("pas une sauvegarde utilisable"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
