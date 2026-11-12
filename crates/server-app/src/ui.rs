//! L'interface : icône de la barre de menus (tray-icon) et petite fenêtre egui, toutes deux sur
//! le fil principal, dans la boucle d'événements d'eframe. La fenêtre est cachée tant qu'on ne la
//! demande pas.
//!
//! Tout ce qui ne dessine pas (clics du menu, icône, infobulle, ligne d'état, phases du serveur,
//! ouverture à la connexion, croix de la fenêtre) se fait dans `App::logic`, pas dans `App::ui` :
//! tant que la fenêtre est cachée après son premier affichage, eframe 0.37 n'appelle que `logic()`
//! (passe « logique seule », sans egui), réveillée par les clics du menu (`request_repaint`) et
//! par un minuteur lent. Une fenêtre jamais montrée n'en profite pas : elle reçoit une passe
//! complète (`ui()` compris, qui ne dessine rien) à chaque réveil.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use crate::cameras_ui::CamerasUi;
use eframe::egui::{self, RichText};
use matter_home_server::backup;
use matter_home_server::setup::{self, IdentityComparison, IdentityState};
use matter_home_server_app::controller::{Cmd, Handle, Notice, State};
use matter_home_server_app::menu::{self, Action, Entry, MenuInput};
use matter_home_server_app::startup::SetupNeed;
use matter_home_server_app::status::{icon_kind, tooltip, IconKind, Phase};
use matter_home_server_app::tailscale::{self, WebAccess};
use matter_home_server_app::{agent, icon, system, version};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Attente maximale de l'arrêt propre du serveur à la fermeture.
const EXIT_WAIT: Duration = Duration::from_secs(20);

/// Délai entre deux passes de `logic()` : fenêtre cachée, deux secondes suffisent pour tenir
/// l'icône à jour (les clics du menu réveillent tout de suite) ; visible, on reste réactif.
const HIDDEN_TICK: Duration = Duration::from_millis(2000);
const VISIBLE_TICK: Duration = Duration::from_millis(500);

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
            // Le design system partagé avec le client (crate `matter-home-theme`).
            matter_home_theme::apply(&ctx);
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

fn confirm_quit() -> bool {
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

/// Ce qu'un changement de phase du serveur déclenche.
#[derive(Debug, Default, PartialEq, Eq)]
struct PhaseEffects {
    /// Une configuration commence dans cette session.
    start_setup_session: bool,
    /// Le serveur tourne : ouverture à la connexion par défaut.
    apply_login_default: bool,
    /// La configuration vient de se terminer : écran de fin.
    show_done: bool,
    /// La fenêtre doit s'ouvrir toute seule.
    open_window: bool,
}

/// La configuration s'ouvre toute seule ; sa fin active l'ouverture à la connexion (par défaut)
/// et montre l'écran de fin.
fn phase_effects(now: &Phase, setup_session: bool) -> PhaseEffects {
    let mut e = PhaseEffects::default();
    if *now == Phase::NeedsSetup {
        e.start_setup_session = true;
        e.open_window = true;
    }
    if *now == Phase::Running {
        e.apply_login_default = true;
        if setup_session {
            e.show_done = true;
            e.open_window = true;
        }
    }
    e
}

/// Comment passer du menu affiché au menu voulu.
#[derive(Debug, PartialEq, Eq)]
enum MenuPlan {
    /// Identique : rien à faire.
    Keep,
    /// Même structure (mêmes lignes, mêmes actions, mêmes cases) : on change textes, états et
    /// coches des éléments existants, sans toucher au menu natif (donc sans le refermer).
    InPlace,
    /// Structure différente (ligne ajoutée ou retirée) : il faut un nouveau menu natif.
    Rebuild,
}

/// Deux entrées ont la même forme si un élément natif existant peut passer de l'une à l'autre
/// par `set_text` / `set_enabled` / `set_checked`.
fn same_shape(a: &Entry, b: &Entry) -> bool {
    match (a, b) {
        (Entry::Info(_), Entry::Info(_)) | (Entry::Separator, Entry::Separator) => true,
        (Entry::Submenu { entries: x, .. }, Entry::Submenu { entries: y, .. }) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same_shape(a, b))
        }
        (
            Entry::Item {
                action: x,
                checked: cx,
                ..
            },
            Entry::Item {
                action: y,
                checked: cy,
                ..
            },
        ) => x == y && cx.is_some() == cy.is_some(),
        _ => false,
    }
}

fn plan_menu(shown: &[Entry], wanted: &[Entry]) -> MenuPlan {
    if shown == wanted {
        MenuPlan::Keep
    } else if shown.len() == wanted.len() && shown.iter().zip(wanted).all(|(a, b)| same_shape(a, b))
    {
        MenuPlan::InPlace
    } else {
        MenuPlan::Rebuild
    }
}

/// Un élément du menu natif, gardé pour être mis à jour sur place.
enum Native {
    Info(MenuItem),
    Separator,
    Item(MenuItem),
    Check(CheckMenuItem),
    Sub(Submenu, Vec<Native>),
}

/// Ajoute une entrée à un menu ou à un sous-menu natif et rend l'élément gardé pour les mises à
/// jour sur place.
fn append_entry(parent: &dyn Appendable, entry: &Entry) -> Native {
    match entry {
        Entry::Info(text) => {
            let item = MenuItem::new(text, false, None);
            parent.append_item(&item);
            Native::Info(item)
        }
        Entry::Separator => {
            parent.append_item(&PredefinedMenuItem::separator());
            Native::Separator
        }
        Entry::Item {
            action,
            label,
            enabled,
            checked: None,
        } => {
            let item = MenuItem::with_id(action.id(), label, *enabled, None);
            parent.append_item(&item);
            Native::Item(item)
        }
        Entry::Item {
            action,
            label,
            enabled,
            checked: Some(c),
        } => {
            let item = CheckMenuItem::with_id(action.id(), label, *enabled, *c, None);
            parent.append_item(&item);
            Native::Check(item)
        }
        Entry::Submenu { label, entries } => {
            let sub = Submenu::new(label, true);
            let children = entries.iter().map(|e| append_entry(&sub, e)).collect();
            parent.append_item(&sub);
            Native::Sub(sub, children)
        }
    }
}

/// Un menu ou un sous-menu natif auquel on ajoute des éléments.
trait Appendable {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem);
}

impl Appendable for Menu {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem) {
        let _ = self.append(item);
    }
}

impl Appendable for Submenu {
    fn append_item(&self, item: &dyn tray_icon::menu::IsMenuItem) {
        let _ = self.append(item);
    }
}

/// Met à jour un élément natif existant (même forme) depuis son entrée.
fn update_native(old: &Entry, new: &Entry, native: &Native) {
    if old == new {
        return;
    }
    match (new, native) {
        (Entry::Info(text), Native::Info(item)) => item.set_text(text),
        (
            Entry::Item {
                label,
                enabled,
                checked,
                ..
            },
            Native::Item(item),
        ) => {
            item.set_text(label);
            item.set_enabled(*enabled);
            debug_assert!(checked.is_none());
        }
        (
            Entry::Item {
                label,
                enabled,
                checked,
                ..
            },
            Native::Check(item),
        ) => {
            item.set_text(label);
            item.set_enabled(*enabled);
            item.set_checked(checked.unwrap_or(false));
        }
        (
            Entry::Submenu {
                label,
                entries: new_entries,
            },
            Native::Sub(sub, children),
        ) => {
            sub.set_text(label);
            if let Entry::Submenu {
                entries: old_entries,
                ..
            } = old
            {
                for ((o, n), c) in old_entries.iter().zip(new_entries).zip(children) {
                    update_native(o, n, c);
                }
            }
        }
        _ => {}
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
    /// Les éléments du menu natif en place, dans l'ordre de `shown_menu`.
    native_menu: Vec<Native>,
    shown_icon: Option<IconKind>,
    shown_tooltip: String,
    visible: bool,
    /// L'ordre « cacher » a déjà été donné à la fenêtre (il ne faut pas le répéter à chaque passe :
    /// chaque ordre réveille eframe et ferait boucler `ui()` à 10 Hz).
    hide_sent: bool,
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
    /// La page « Caméras » remplace les réglages (menu « Caméras… »).
    cameras_open: bool,
    cameras: CamerasUi,
    /// La confirmation « Quitter » est à l'écran : un second clic n'en ouvre pas une autre.
    quit_dialog_open: Arc<AtomicBool>,
    /// L'accès web par Tailscale, tel que Tailscale le dit (rien n'est gardé sur disque).
    web: WebAccess,
    /// Lecture (`false`) ou activation/coupure (`true`) en cours, dans un fil à part.
    web_job: Option<(bool, Receiver<WebAccess>)>,
    /// Le menu a été ouvert ou refermé depuis la dernière lecture : relire l'état.
    web_stale: bool,
    menu_was_open: bool,
    /// Le lien d'autorisation de Serve, gardé jusqu'à la prochaine tentative d'activation.
    web_auth_url: Option<String>,
}

impl App {
    fn new(handle: Handle, demo: bool, tray: TrayIcon, actions: Receiver<Action>) -> Self {
        let app = Self {
            handle,
            demo,
            tray,
            actions,
            shown_menu: Vec::new(),
            native_menu: Vec::new(),
            shown_icon: None,
            shown_tooltip: String::new(),
            visible: false,
            hide_sent: false,
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
            cameras_open: false,
            cameras: CamerasUi::default(),
            quit_dialog_open: Arc::new(AtomicBool::new(false)),
            web: WebAccess::Checking,
            web_job: None,
            web_stale: true,
            menu_was_open: false,
            web_auth_url: None,
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
        self.hide_sent = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // Fenêtre cachée jusque-là : une passe complète doit dessiner le contenu tout de suite.
        ctx.request_repaint();
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        // Quel que soit le moyen de cacher l'écran de fin (« Terminer » ou la croix), la
        // configuration de cette session est finie : un `Running` ultérieur (après un blocage
        // par exemple) ne doit pas le rouvrir sur une machine sans personne.
        if self.show_done {
            self.show_done = false;
            self.setup_session = false;
        }
        self.visible = false;
        self.hide_sent = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Demande confirmation puis arrête le serveur. rfd fait afficher l'alerte par le fil
    /// principal (`CFUserNotificationDisplayAlert`, synchrone) : tant qu'elle est ouverte, ce fil
    /// est bloqué, donc ni menu, ni icône, ni `logic()` ne tournent ; les clics arrivés entre-temps
    /// sont traités à sa fermeture. Le fil à part ne fait qu'attendre la réponse pendant que le fil
    /// principal affiche l'alerte ; le garde `quit_dialog_open` (pas de seconde alerte) est donc
    /// sans effet réel ici, mais inoffensif : on le laisse.
    fn ask_quit(&self) {
        if self.quit_dialog_open.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, open) = (self.handle.tx.clone(), self.quit_dialog_open.clone());
        std::thread::spawn(move || {
            let confirmed = confirm_quit();
            open.store(false, Ordering::SeqCst);
            if confirmed {
                let _ = tx.send(Cmd::Quit);
            }
        });
    }

    /// Relit l'état de Tailscale à l'ouverture et à la fermeture du menu (jamais sur le fil
    /// principal : un fil à part, 3 s au plus), puis range le résultat.
    fn poll_web(&mut self, st: &State) {
        if let Some((toggled, rx)) = &self.web_job {
            match rx.try_recv() {
                Ok(state) => {
                    let toggled = *toggled;
                    self.web_job = None;
                    if let WebAccess::NeedsAuth { url } = &state {
                        self.web_auth_url = Some(url.clone());
                    }
                    self.web = match (state, &self.web_auth_url) {
                        // Une simple lecture ne fait pas oublier qu'il reste à autoriser Serve.
                        (WebAccess::Off { .. }, Some(url)) if !toggled => {
                            WebAccess::NeedsAuth { url: url.clone() }
                        }
                        (state, _) => state,
                    };
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.web_job = None,
            }
        }
        // `run_loop_is_default_mode` est faux pendant qu'un menu de la barre de menus est suivi.
        let open = !system::run_loop_is_default_mode();
        if open != self.menu_was_open {
            self.menu_was_open = open;
            self.web_stale = true;
        }
        let port = st.url.as_deref().and_then(tailscale::port_of_url);
        if self.web_stale && self.web_job.is_none() {
            if let Some(port) = port {
                self.web_stale = false;
                self.web_job = Some((false, tailscale::spawn_read(self.demo, port)));
            }
        }
    }

    fn toggle_web(&mut self, st: &State) {
        let Some(port) = st.url.as_deref().and_then(tailscale::port_of_url) else {
            return;
        };
        let enable = match self.web {
            WebAccess::Off { .. } | WebAccess::NeedsAuth { .. } => true,
            WebAccess::On { .. } => false,
            _ => return,
        };
        self.web_auth_url = None;
        self.web = WebAccess::Checking;
        self.web_job = Some((true, tailscale::spawn_toggle(self.demo, port, enable)));
    }

    fn do_action(&mut self, ctx: &egui::Context, action: Action, st: &State) {
        match action {
            Action::WebAccess => self.toggle_web(st),
            Action::WebOpen => {
                if let Some(url) = self.web.web_url() {
                    system::open_url(&url);
                }
            }
            Action::WebCopy => {
                if let Some(url) = self.web.web_url() {
                    let _ = system::copy_to_clipboard(&url);
                }
            }
            Action::WebAuth => {
                if let WebAccess::NeedsAuth { url } = &self.web {
                    system::open_url(url);
                }
            }
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
            Action::Cameras => {
                if st.hub.is_some() {
                    self.cameras_open = true;
                    self.show_window(ctx);
                }
            }
            Action::Settings => {
                self.cameras_open = false;
                self.show_window(ctx);
            }
            Action::Quit => self.ask_quit(),
        }
    }

    /// Nouveau menu natif complet (la structure a changé) ; garde les éléments pour les mises à
    /// jour sur place. Les identifiants des actions restent ceux de `Action::id`.
    fn rebuild_menu(&mut self, model: &[Entry]) {
        let menu = Menu::new();
        let native = model.iter().map(|e| append_entry(&menu, e)).collect();
        self.tray.set_menu(Some(Box::new(menu)));
        self.native_menu = native;
    }

    /// Même structure : on ne touche qu'à ce qui a changé, sur les éléments déjà en place (le
    /// menu natif, éventuellement ouvert, n'est pas remplacé ; les identifiants ne bougent pas).
    fn update_menu_in_place(&self, model: &[Entry]) {
        for ((old, new), native) in self.shown_menu.iter().zip(model).zip(&self.native_menu) {
            update_native(old, new, native);
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
        let version = version::display_version();
        let model = menu::build(&MenuInput {
            version: &version,
            web: &self.web,
            phase: &st.phase,
            health: &st.health,
            url: st.url.as_deref(),
            fingerprint: st.fingerprint.as_deref(),
            login_enabled: self.login_enabled,
            desktop_doubt: st.desktop_doubt,
            login_available: self.login_available(),
        });
        match plan_menu(&self.shown_menu, &model) {
            MenuPlan::Keep => {}
            MenuPlan::InPlace => {
                self.update_menu_in_place(&model);
                self.shown_menu = model;
            }
            MenuPlan::Rebuild => {
                // `set_menu` détruit l'ancien menu natif, et muda y met fin au suivi en cours
                // (`cancelTrackingWithoutAnimation`) : le menu ouvert disparaîtrait sous la souris.
                // `logic()` tourne pendant ce suivi (toutes les 2 s) : on attend donc que la boucle
                // soit revenue à son mode normal, c'est-à-dire que le menu soit refermé. La
                // structure ne change que rarement (appareil qui disparaît ou revient).
                if !system::run_loop_is_default_mode() {
                    log::debug!("menu : changement de structure différé (menu ouvert)");
                    return;
                }
                self.rebuild_menu(&model);
                self.shown_menu = model;
            }
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

    /// Le travail qui ne dessine pas. eframe l'appelle avant chaque `ui()` et aussi, fenêtre
    /// cachée après son premier affichage, à chaque `request_repaint` / `request_repaint_after`
    /// (passe logique seule, sans egui). C'est le fil principal : icône, menu et fenêtre se
    /// manipulent ici.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // La croix cache la fenêtre : le serveur continue, seul « Quitter » l'arrête. Cela se
        // traite ici et non dans `ui()` : dans une passe logique seule, l'évènement de fermeture
        // est consommé, et un `CancelClose` manquant serait pris par eframe pour une fermeture
        // définitive (`on_exit`, `Cmd::Quit`, sortie sans confirmation). Le même code sert les
        // deux types de passe (`ctx.input` voit `viewports` dans les deux ; les commandes
        // envoyées sont exécutées dans les deux). Cmd+Q, la déconnexion et l'extinction ne
        // passent pas par là : ce sont des arrêts de l'application, qui finissent dans `on_exit`.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
        let st = self.handle.snapshot();
        self.refresh_login();
        self.poll_picking();
        self.poll_web(&st);
        while let Ok(action) = self.actions.try_recv() {
            self.do_action(ctx, action, &st);
        }

        if self.last_phase.as_ref() != Some(&st.phase) {
            let effects = phase_effects(&st.phase, self.setup_session);
            if effects.start_setup_session {
                self.setup_session = true;
            }
            if effects.apply_login_default {
                self.apply_login_default();
            }
            if effects.show_done {
                self.show_done = true;
            }
            if effects.open_window {
                self.show_window(ctx);
            }
            self.last_phase = Some(st.phase.clone());
        }
        self.sync_tray(&st);

        ctx.request_repaint_after(if self.visible {
            VISIBLE_TICK
        } else {
            HIDDEN_TICK
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.visible {
            // eframe montre la fenêtre après sa première image, même créée cachée : on la recache,
            // une seule fois.
            if !self.hide_sent {
                self.hide_sent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            return;
        }
        let st = self.handle.snapshot();
        // Page Caméras : l'avertissement de sécurité reste en pied de fenêtre.
        if st.busy.is_none()
            && !self.show_done
            && matches!(st.phase, Phase::Running)
            && self.cameras_open
            && st.hub.is_some()
        {
            egui::Panel::bottom("cameras_footer").show(ui, crate::cameras_ui::footer);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.content(ui, &ctx, &st));
        });
    }
}

fn notice_label(ui: &mut egui::Ui, notice: &Notice) {
    let p = matter_home_theme::palette(ui);
    match notice {
        Notice::Ok(t) => ui.colored_label(p.ok, t),
        Notice::Err(t) => ui.colored_label(p.error, t),
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
            (Phase::Running, _) if self.cameras_open && st.hub.is_some() => {
                if let Some(hub) = &st.hub {
                    if self.cameras.show(ui, ctx, &hub.0) {
                        self.cameras_open = false;
                        self.cameras.reset();
                    }
                }
            }
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
                        matter_home_theme::palette(ui).error,
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
                        matter_home_theme::palette(ui).error,
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
                ui.colored_label(matter_home_theme::palette(ui).error, e);
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
                ui.colored_label(matter_home_theme::palette(ui).error, e);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Journal").clicked() {
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
                ui.label(RichText::new("Caméras").strong());
                if st.hub.is_some() && ui.button("Caméras…").clicked() {
                    self.cameras_open = true;
                }
                ui.add_space(6.0);
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

    fn connect_menu(address: &str, with_submenu: bool) -> Vec<Entry> {
        let mut m = vec![Entry::Info("8 appareils en ligne".into()), Entry::Separator];
        if with_submenu {
            m.push(Entry::Submenu {
                label: "Connecter un client".into(),
                entries: vec![
                    Entry::Info(format!("Adresse : {address}")),
                    Entry::Item {
                        action: Action::CopyUrl,
                        label: "Copier l'adresse".into(),
                        enabled: true,
                        checked: None,
                    },
                ],
            });
        }
        m.push(Entry::Item {
            action: Action::Quit,
            label: "Quitter".into(),
            enabled: true,
            checked: None,
        });
        m
    }

    #[test]
    fn a_changed_submenu_text_is_updated_in_place() {
        let shown = connect_menu("https://a.local:8443", true);
        assert_eq!(plan_menu(&shown, &shown), MenuPlan::Keep);
        let wanted = connect_menu("https://b.local:8443", true);
        assert_eq!(plan_menu(&shown, &wanted), MenuPlan::InPlace);
    }

    #[test]
    fn a_submenu_that_appears_or_changes_size_rebuilds_the_menu() {
        let without = connect_menu("https://a.local:8443", false);
        let with = connect_menu("https://a.local:8443", true);
        assert_eq!(plan_menu(&without, &with), MenuPlan::Rebuild);
        assert_eq!(plan_menu(&with, &without), MenuPlan::Rebuild);
        let mut longer = connect_menu("https://a.local:8443", true);
        if let Entry::Submenu { entries, .. } = &mut longer[2] {
            entries.push(Entry::Separator);
        }
        assert_eq!(plan_menu(&with, &longer), MenuPlan::Rebuild);
    }

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
    fn needs_setup_opens_the_window_by_itself() {
        let e = phase_effects(&Phase::NeedsSetup, false);
        assert!(e.start_setup_session && e.open_window);
        assert!(!e.apply_login_default && !e.show_done);
    }

    #[test]
    fn running_after_a_setup_shows_the_done_page() {
        let e = phase_effects(&Phase::Running, true);
        assert!(e.apply_login_default && e.show_done && e.open_window);
    }

    #[test]
    fn running_without_setup_only_applies_the_login_default() {
        let e = phase_effects(&Phase::Running, false);
        assert_eq!(
            e,
            PhaseEffects {
                apply_login_default: true,
                ..PhaseEffects::default()
            }
        );
    }

    #[test]
    fn other_phases_do_not_touch_the_window() {
        for p in [Phase::Starting, Phase::Stopping, Phase::Problem("x".into())] {
            assert_eq!(phase_effects(&p, true), PhaseEffects::default());
        }
    }

    fn running_menu(unreachable: &[&str], login: bool) -> Vec<Entry> {
        let health = matter_home_server_app::status::Health {
            total: 5,
            online: 5 - unreachable.len(),
            unreachable: unreachable.iter().map(|s| s.to_string()).collect(),
        };
        menu::build(&MenuInput {
            version: "1.0.0",
            web: &matter_home_server_app::tailscale::WebAccess::Checking,
            phase: &Phase::Running,
            health: &health,
            url: Some("https://mac.local:8443"),
            fingerprint: Some("AB:CD"),
            login_enabled: login,
            desktop_doubt: false,
            login_available: true,
        })
    }

    #[test]
    fn identical_menus_are_kept() {
        let m = running_menu(&["Salon"], false);
        assert_eq!(plan_menu(&m, &m.clone()), MenuPlan::Keep);
    }

    #[test]
    fn text_and_check_changes_are_updated_in_place() {
        // Même nombre d'appareils injoignables, un autre nom ; case cochée en plus.
        let (a, b) = (
            running_menu(&["Salon"], false),
            running_menu(&["Cuisine"], true),
        );
        assert_ne!(a, b);
        assert_eq!(plan_menu(&a, &b), MenuPlan::InPlace);
    }

    #[test]
    fn a_line_added_or_removed_rebuilds() {
        let (none, one, two) = (
            running_menu(&[], false),
            running_menu(&["Salon"], false),
            running_menu(&["Salon", "Cuisine"], false),
        );
        assert_eq!(plan_menu(&none, &one), MenuPlan::Rebuild);
        assert_eq!(plan_menu(&one, &two), MenuPlan::Rebuild);
        assert_eq!(plan_menu(&[], &none), MenuPlan::Rebuild);
    }

    #[test]
    fn a_swapped_action_or_kind_rebuilds() {
        let a = vec![Entry::Info("a".into()), Entry::Separator];
        let b = vec![Entry::Separator, Entry::Info("a".into())];
        assert_eq!(plan_menu(&a, &b), MenuPlan::Rebuild);
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
