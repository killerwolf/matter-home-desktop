//! Barre de menus native de macOS (ticket n° 20) : menu de l'application, Fichier, Édition et
//! Fenêtre, en haut de l'écran, à la place de la rangée « Fichier » de la fenêtre.
//!
//! La partie commune (commandes, état grisé ou non, traduction des commandes d'édition en
//! événements egui) ne dépend d'aucun système et se teste partout. La partie AppKit (module
//! `appkit`) n'existe que sur macOS ; ailleurs `NativeMenu::install` rend `None` et l'interface
//! garde son menu dans la fenêtre.
//!
//! Un clic sur un élément de menu est reçu par le thread principal (AppKit), qui dépose une
//! commande dans un canal et réveille l'interface ; `MatterApp::logic` la lit, comme les
//! événements du réseau, et appelle les mêmes fonctions que l'ancien menu egui.

use std::sync::mpsc;

use eframe::egui;

/// Commandes du menu Édition, rejouées dans le champ de texte qui a le focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCommand {
    Undo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Ce qu'un clic sur la barre de menus demande à l'application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    Export,
    Import,
    Logout,
    /// Quitter (⌘Q) : passe par la fermeture de la fenêtre, donc par la question « modifications
    /// non envoyées » si besoin.
    Quit,
    Edit(EditCommand),
}

/// Éléments du menu Fichier utilisables (les autres sont grisés).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMenuState {
    pub export: bool,
    pub import: bool,
    pub logout: bool,
}

impl FileMenuState {
    /// Comme l'ancien menu de la fenêtre : exporter et importer demandent une connexion ;
    /// « Se déconnecter » existe dès qu'une session est ouverte (pas sur l'écran de connexion,
    /// où l'ancien menu n'était pas affiché).
    pub fn new(connected: bool, session_open: bool) -> Self {
        Self {
            export: connected,
            import: connected,
            logout: session_open,
        }
    }

    /// Une commande est-elle permise dans cet état ? (Un clic peut arriver juste après que
    /// l'élément a été grisé.)
    pub fn allows(&self, command: MenuCommand) -> bool {
        match command {
            MenuCommand::Export => self.export,
            MenuCommand::Import => self.import,
            MenuCommand::Logout => self.logout,
            MenuCommand::Quit | MenuCommand::Edit(_) => true,
        }
    }
}

/// Événements egui qui reproduisent une commande d'Édition. `clipboard` est le texte du
/// presse-papiers, lu au moment du clic (utile pour Coller seulement).
pub fn edit_events(command: EditCommand, clipboard: Option<String>) -> Vec<egui::Event> {
    // Touche enfoncée puis relâchée, comme au clavier.
    let key = |key| {
        let modifiers = egui::Modifiers {
            command: true,
            mac_cmd: true,
            ..Default::default()
        };
        [true, false].map(|pressed| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        })
    };
    match command {
        EditCommand::Undo => key(egui::Key::Z).to_vec(),
        EditCommand::SelectAll => key(egui::Key::A).to_vec(),
        EditCommand::Cut => vec![egui::Event::Cut],
        EditCommand::Copy => vec![egui::Event::Copy],
        EditCommand::Paste => clipboard
            .filter(|t| !t.is_empty())
            .map(|t| vec![egui::Event::Paste(t)])
            .unwrap_or_default(),
    }
}

/// La barre de menus installée et le canal de ses commandes.
pub struct NativeMenu {
    rx: mpsc::Receiver<MenuCommand>,
    #[cfg(target_os = "macos")]
    appkit: appkit::Handles,
    last_state: Option<FileMenuState>,
}

impl NativeMenu {
    /// Installe la barre de menus du Mac. `None` hors macOS (ou hors fil principal) : l'interface
    /// garde alors son menu Fichier dans la fenêtre.
    pub fn install(ctx: &egui::Context) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let (tx, rx) = mpsc::channel();
            let appkit = appkit::install(tx, ctx.clone())?;
            Some(Self {
                rx,
                appkit,
                last_state: None,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            None
        }
    }

    /// Prochaine commande cliquée, s'il y en a une.
    pub fn try_recv(&self) -> Option<MenuCommand> {
        self.rx.try_recv().ok()
    }

    /// Grise ou dégrise les éléments du menu Fichier (rien si l'état n'a pas changé).
    pub fn set_state(&mut self, state: FileMenuState) {
        if self.last_state == Some(state) {
            return;
        }
        self.last_state = Some(state);
        #[cfg(target_os = "macos")]
        self.appkit.apply(state);
    }

    /// Texte du presse-papiers du système (Coller).
    pub fn clipboard_text(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            appkit::clipboard_text()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod appkit {
    use std::sync::mpsc::Sender;

    use eframe::egui;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSPasteboard,
        NSPasteboardTypeString,
    };
    use objc2_foundation::{NSObject, NSString};

    use super::{EditCommand, FileMenuState, MenuCommand};

    /// Ce que la cible garde : où déposer la commande, et qui réveiller.
    struct Ivars {
        tx: Sender<MenuCommand>,
        ctx: egui::Context,
    }

    define_class!(
        /// Cible des éléments de menu : traduit chaque clic en `MenuCommand`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "MatterHomeMenuTarget"]
        #[ivars = Ivars]
        struct Target;

        impl Target {
            #[unsafe(method(exportData:))]
            fn export_data(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Export);
            }

            #[unsafe(method(importData:))]
            fn import_data(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Import);
            }

            #[unsafe(method(logOut:))]
            fn log_out(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Logout);
            }

            #[unsafe(method(quitApp:))]
            fn quit_app(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Quit);
            }

            #[unsafe(method(editUndo:))]
            fn edit_undo(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Edit(EditCommand::Undo));
            }

            #[unsafe(method(editCut:))]
            fn edit_cut(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Edit(EditCommand::Cut));
            }

            #[unsafe(method(editCopy:))]
            fn edit_copy(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Edit(EditCommand::Copy));
            }

            #[unsafe(method(editPaste:))]
            fn edit_paste(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Edit(EditCommand::Paste));
            }

            #[unsafe(method(editSelectAll:))]
            fn edit_select_all(&self, _sender: Option<&AnyObject>) {
                self.send(MenuCommand::Edit(EditCommand::SelectAll));
            }
        }

        unsafe impl NSObjectProtocol for Target {}
    );

    impl Target {
        fn new(mtm: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            unsafe { msg_send![super(this), init] }
        }

        fn send(&self, command: MenuCommand) {
            // Édition : seule la fenêtre egui, au premier plan, reçoit la commande. Une fenêtre
            // modale (sélecteur de fichier) ou une autre fenêtre (À propos) garde l'action
            // standard (`copy:`, `paste:`…), envoyée à la chaîne des répondeurs.
            if let MenuCommand::Edit(edit) = command {
                let app = NSApplication::sharedApplication(self.mtm());
                let egui_is_key = app.modalWindow().is_none()
                    && matches!((app.keyWindow(), app.mainWindow()), (Some(k), Some(m)) if k == m);
                if !egui_is_key {
                    let action = match edit {
                        EditCommand::Undo => sel!(undo:),
                        EditCommand::Cut => sel!(cut:),
                        EditCommand::Copy => sel!(copy:),
                        EditCommand::Paste => sel!(paste:),
                        EditCommand::SelectAll => sel!(selectAll:),
                    };
                    unsafe { app.sendAction_to_from(action, None, None) };
                    return;
                }
            }
            let ivars = self.ivars();
            // Le récepteur n'existe plus quand l'application se ferme : rien à faire alors.
            if ivars.tx.send(command).is_ok() {
                ivars.ctx.request_repaint();
            }
        }
    }

    /// Ce qu'il faut garder en vie (AppKit ne retient pas la cible des éléments) et modifier
    /// ensuite (les éléments grisables).
    pub struct Handles {
        _target: Retained<Target>,
        export: Retained<NSMenuItem>,
        import: Retained<NSMenuItem>,
        logout: Retained<NSMenuItem>,
    }

    impl Handles {
        pub fn apply(&self, state: FileMenuState) {
            self.export.setEnabled(state.export);
            self.import.setEnabled(state.import);
            self.logout.setEnabled(state.logout);
        }
    }

    fn item(
        mtm: MainThreadMarker,
        title: &str,
        action: Option<Sel>,
        key: &str,
    ) -> Retained<NSMenuItem> {
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                action,
                &NSString::from_str(key),
            )
        }
    }

    /// Élément qui agit sur la cible (`target`) ; sans cible, l'action suit la chaîne des
    /// répondeurs (application, fenêtre).
    fn item_to(
        mtm: MainThreadMarker,
        target: Option<&Target>,
        title: &str,
        action: Sel,
        key: &str,
    ) -> Retained<NSMenuItem> {
        let it = item(mtm, title, Some(action), key);
        if let Some(target) = target {
            unsafe { it.setTarget(Some(target)) };
        }
        it
    }

    fn submenu(mtm: MainThreadMarker, title: &str, items: &[&NSMenuItem]) -> Retained<NSMenuItem> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for it in items {
            menu.addItem(it);
        }
        let top = item(mtm, title, None, "");
        top.setSubmenu(Some(&menu));
        top
    }

    fn menu_of(top: &NSMenuItem) -> Retained<NSMenu> {
        top.submenu().expect("sous-menu posé à la création")
    }

    pub fn install(tx: Sender<MenuCommand>, ctx: egui::Context) -> Option<Handles> {
        let mtm = MainThreadMarker::new()?;
        let target = Target::new(mtm, Ivars { tx, ctx });
        let t = Some(&*target);
        let app = NSApplication::sharedApplication(mtm);
        let name = "Matter Home Desktop";

        // Menu de l'application.
        let hide_others = item_to(
            mtm,
            None,
            "Masquer les autres",
            sel!(hideOtherApplications:),
            "h",
        );
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        let app_menu = submenu(
            mtm,
            name,
            &[
                &item_to(
                    mtm,
                    None,
                    &format!("À propos de {name}"),
                    sel!(orderFrontStandardAboutPanel:),
                    "",
                ),
                &NSMenuItem::separatorItem(mtm),
                &item_to(mtm, None, &format!("Masquer {name}"), sel!(hide:), "h"),
                &hide_others,
                &item_to(mtm, None, "Tout afficher", sel!(unhideAllApplications:), ""),
                &NSMenuItem::separatorItem(mtm),
                &item_to(mtm, t, &format!("Quitter {name}"), sel!(quitApp:), "q"),
            ],
        );

        // Fichier : mêmes actions que l'ancien menu de la fenêtre.
        let export = item_to(mtm, t, "Exporter mes données…", sel!(exportData:), "");
        let import = item_to(mtm, t, "Importer des données…", sel!(importData:), "");
        let logout = item_to(mtm, t, "Se déconnecter", sel!(logOut:), "");
        let file = submenu(
            mtm,
            "Fichier",
            &[&export, &import, &NSMenuItem::separatorItem(mtm), &logout],
        );
        // Griser à la main (selon la connexion), pas selon ce que la cible sait faire.
        menu_of(&file).setAutoenablesItems(false);

        // Édition : rejoue les commandes dans le champ de texte d'egui.
        let edit = submenu(
            mtm,
            "Édition",
            &[
                &item_to(mtm, t, "Annuler", sel!(editUndo:), "z"),
                &NSMenuItem::separatorItem(mtm),
                &item_to(mtm, t, "Couper", sel!(editCut:), "x"),
                &item_to(mtm, t, "Copier", sel!(editCopy:), "c"),
                &item_to(mtm, t, "Coller", sel!(editPaste:), "v"),
                &item_to(mtm, t, "Tout sélectionner", sel!(editSelectAll:), "a"),
            ],
        );

        // Fenêtre.
        let window = submenu(
            mtm,
            "Fenêtre",
            &[
                &item_to(mtm, None, "Réduire", sel!(performMiniaturize:), "m"),
                &item_to(mtm, None, "Zoom", sel!(performZoom:), ""),
            ],
        );

        let bar = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
        for top in [&app_menu, &file, &edit, &window] {
            bar.addItem(top);
        }
        // Remplace le menu par défaut installé par winit.
        app.setMainMenu(Some(&bar));
        app.setWindowsMenu(Some(&menu_of(&window)));

        // État de départ : rien n'est connecté.
        let handles = Handles {
            _target: target,
            export,
            import,
            logout,
        };
        handles.apply(FileMenuState::new(false, false));
        Some(handles)
    }

    pub fn clipboard_text() -> Option<String> {
        let board = NSPasteboard::generalPasteboard();
        let text = unsafe { board.stringForType(NSPasteboardTypeString) }?;
        // Le presse-papiers peut contenir des fins de ligne CRLF : egui attend « \n ».
        Some(text.to_string().replace("\r\n", "\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_items_follow_the_connection() {
        let off = FileMenuState::new(false, true);
        assert!(!off.allows(MenuCommand::Export));
        assert!(!off.allows(MenuCommand::Import));
        assert!(off.allows(MenuCommand::Logout));
        let on = FileMenuState::new(true, true);
        assert!(on.allows(MenuCommand::Export) && on.allows(MenuCommand::Import));
    }

    #[test]
    fn everything_but_quit_and_edit_is_greyed_on_the_login_screen() {
        let s = FileMenuState::new(false, false);
        assert!(!s.allows(MenuCommand::Export));
        assert!(!s.allows(MenuCommand::Import));
        assert!(!s.allows(MenuCommand::Logout));
        assert!(s.allows(MenuCommand::Quit));
        assert!(s.allows(MenuCommand::Edit(EditCommand::Paste)));
    }

    #[test]
    fn edit_commands_become_egui_events() {
        assert_eq!(edit_events(EditCommand::Copy, None), [egui::Event::Copy]);
        assert_eq!(edit_events(EditCommand::Cut, None), [egui::Event::Cut]);
        assert_eq!(
            edit_events(EditCommand::Paste, Some("abc".into())),
            [egui::Event::Paste("abc".into())]
        );
        // Presse-papiers vide : rien à coller.
        assert!(edit_events(EditCommand::Paste, None).is_empty());
        assert!(edit_events(EditCommand::Paste, Some(String::new())).is_empty());
        for (cmd, expected) in [
            (EditCommand::Undo, egui::Key::Z),
            (EditCommand::SelectAll, egui::Key::A),
        ] {
            match edit_events(cmd, None).as_slice() {
                [egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                }, egui::Event::Key {
                    key: released,
                    pressed: false,
                    ..
                }] => {
                    assert_eq!(*key, expected);
                    assert_eq!(*released, expected);
                    assert!(modifiers.command);
                }
                other => panic!("événement inattendu : {other:?}"),
            }
        }
    }
}
