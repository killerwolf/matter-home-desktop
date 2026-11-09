//! La décision de démarrage : le serveur démarre-t-il, faut-il configurer, ou un autre programme
//! tient-il déjà les données ? Pure : toutes les observations (verrou, processus, fichiers) sont
//! passées en argument, donc testables sans toucher au système.

use std::path::{Path, PathBuf};

use matter_home_server::setup::IdentityState;

/// Nom du binaire de cette application et du serveur en ligne de commande.
pub const APP_BINARY: &str = "matter-home-server-app";
pub const CLI_BINARY: &str = "matter-home-server";
/// Le binaire de l'application de bureau (v0.2.1 comme client actuel).
pub const DESKTOP_BINARY: &str = "matter-home-desktop";
/// Le service installé par l'archive en ligne de commande.
pub const SERVICE_PLIST: &str = "/Library/LaunchDaemons/dev.matter-home-desktop.server.plist";

/// Un processus de la liste de `ps` : numéro et chemin de l'exécutable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub path: String,
}

impl Proc {
    fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Lit la sortie de `ps -axo pid=,comm=`.
pub fn parse_ps(output: &str) -> Vec<Proc> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, path) = line.split_once(char::is_whitespace)?;
            Some(Proc {
                pid: pid.parse().ok()?,
                path: path.trim().to_string(),
            })
        })
        .collect()
}

/// Qui tient le verrou d'instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// Une autre fenêtre de cette application.
    OtherApp,
    /// Le service installé au Terminal (plist dans `/Library/LaunchDaemons`).
    Service,
    /// Un `matter-home-server` lancé à la main.
    OtherServer,
    Unknown,
}

pub fn classify_holder(holder_pid: Option<u32>, procs: &[Proc], service_installed: bool) -> Holder {
    if let Some(pid) = holder_pid {
        if procs
            .iter()
            .any(|p| p.pid == pid && p.file_name() == APP_BINARY)
        {
            return Holder::OtherApp;
        }
    }
    if service_installed {
        return Holder::Service;
    }
    if procs.iter().any(|p| p.file_name() == CLI_BINARY) {
        return Holder::OtherServer;
    }
    Holder::Unknown
}

/// Ce qui empêche le serveur de démarrer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// Cette application est déjà ouverte (quitter sans bruit).
    OtherApp,
    /// Le service launchd installé au Terminal tient les données.
    Service,
    /// Un autre serveur (ligne de commande) tient les données.
    OtherServer,
    /// Quelqu'un tient le verrou, on ne sait pas qui.
    Unknown,
    /// L'ancienne application de bureau (v0.2.x) tourne : elle ne prend pas le verrou.
    DesktopV02,
}

impl Blocker {
    pub fn from_holder(h: Holder) -> Blocker {
        match h {
            Holder::OtherApp => Blocker::OtherApp,
            Holder::Service => Blocker::Service,
            Holder::OtherServer => Blocker::OtherServer,
            Holder::Unknown => Blocker::Unknown,
        }
    }

    /// Quelques mots pour le menu.
    pub fn short(&self) -> &'static str {
        match self {
            Blocker::OtherApp => "déjà ouvert",
            Blocker::Service => "le service installé tient les données",
            Blocker::OtherServer => "un autre serveur tient les données",
            Blocker::Unknown => "les données sont utilisées ailleurs",
            Blocker::DesktopV02 => "l'ancienne application tourne",
        }
    }

    /// L'explication complète, avec la marche à suivre.
    pub fn explain(&self) -> String {
        match self {
            Blocker::OtherApp => "Matter Home Server est déjà ouvert : cherchez son icône dans la barre de menus.".into(),
            Blocker::Service => "Le serveur est déjà installé sur ce Mac comme service (ancienne méthode, au Terminal) et il utilise les données. Pour passer à cette application : ouvrez le Terminal une seule fois, allez dans le dossier de l'archive du serveur et lancez « ./uninstall.sh » (vos données restent en place), puis rouvrez Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::OtherServer => "Un autre serveur Matter Home (lancé à la main) utilise déjà les données. Arrêtez-le, puis rouvrez Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::Unknown => "Les données sont déjà utilisées par un autre programme Matter Home. Fermez-le, puis rouvrez Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::DesktopV02 => "L'ancienne application de bureau (version 0.2) est ouverte. Elle ne signale pas qu'elle utilise les données : si le serveur démarrait en même temps, les deux se disputeraient vos appareils. Fermez-la (sur tous vos Mac), le serveur démarrera alors tout seul.".into(),
        }
    }
}

/// Ce qu'il faut configurer avant de démarrer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupNeed {
    /// Pas d'identité Matter : reprendre une sauvegarde ou créer une nouvelle maison.
    /// `Some(raison)` si le fichier existe mais est illisible (on ne propose alors pas de nouvelle maison).
    Identity {
        unreadable: Option<String>,
    },
    Account,
}

/// Le résultat de la prise du verrou d'instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    Held(Holder),
    Failed(String),
}

/// Tout ce que la décision observe.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub lock: LockOutcome,
    pub desktop_v02_running: bool,
    /// Le service launchd est installé : même s'il est entre deux relances (verrou libre un
    /// instant), l'application ne lui prend pas les données.
    pub service_installed: bool,
    pub identity: IdentityState,
    pub account_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    Setup(SetupNeed),
    Blocked(Blocker),
    Problem(String),
}

pub fn decide(i: &Inputs) -> Decision {
    match &i.lock {
        LockOutcome::Held(h) => return Decision::Blocked(Blocker::from_holder(*h)),
        LockOutcome::Failed(e) => return Decision::Problem(e.clone()),
        LockOutcome::Acquired => {}
    }
    if i.service_installed {
        return Decision::Blocked(Blocker::Service);
    }
    if i.desktop_v02_running {
        return Decision::Blocked(Blocker::DesktopV02);
    }
    match &i.identity {
        IdentityState::Unreadable(e) => {
            return Decision::Setup(SetupNeed::Identity {
                unreadable: Some(e.clone()),
            })
        }
        IdentityState::Missing => return Decision::Setup(SetupNeed::Identity { unreadable: None }),
        IdentityState::Present(_) => {}
    }
    if !i.account_exists {
        return Decision::Setup(SetupNeed::Account);
    }
    Decision::Run
}

/// La version d'une application d'après son `Info.plist`.
pub fn parse_bundle_version(plist: &str) -> Option<String> {
    let after = plist
        .split("<key>CFBundleShortVersionString</key>")
        .nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// L'ancienne application de bureau (version 0.2.x) tourne-t-elle ? `version_of` lit la version
/// de l'application dont l'exécutable est donné (le client actuel, lui, prend le verrou).
pub fn desktop_v02_running(procs: &[Proc], version_of: impl Fn(&Path) -> Option<String>) -> bool {
    procs
        .iter()
        .filter(|p| p.file_name() == DESKTOP_BINARY)
        .any(|p| {
            version_of(Path::new(&p.path)).is_some_and(|v| v == "0.2" || v.starts_with("0.2."))
        })
}

/// `<App>.app/Contents/MacOS/<binaire>` → `<App>.app/Contents/Info.plist`.
pub fn info_plist_of(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.parent()?.join("Info.plist"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use matter_home_server::backup::IdentityBrief;

    fn present() -> IdentityState {
        IdentityState::Present(IdentityBrief {
            fabric_id: 1,
            devices: 0,
            nodes: vec![],
        })
    }

    fn inputs() -> Inputs {
        Inputs {
            lock: LockOutcome::Acquired,
            desktop_v02_running: false,
            service_installed: false,
            identity: present(),
            account_exists: true,
        }
    }

    #[test]
    fn existing_identity_and_account_start_directly() {
        assert_eq!(decide(&inputs()), Decision::Run);
    }

    #[test]
    fn missing_identity_asks_for_setup_never_creates() {
        let mut i = inputs();
        i.identity = IdentityState::Missing;
        assert_eq!(
            decide(&i),
            Decision::Setup(SetupNeed::Identity { unreadable: None })
        );
        // Même sans compte : l'identité d'abord.
        i.account_exists = false;
        assert_eq!(
            decide(&i),
            Decision::Setup(SetupNeed::Identity { unreadable: None })
        );
    }

    #[test]
    fn unreadable_identity_is_not_replaced_silently() {
        let mut i = inputs();
        i.identity = IdentityState::Unreadable("x".into());
        assert_eq!(
            decide(&i),
            Decision::Setup(SetupNeed::Identity {
                unreadable: Some("x".into())
            })
        );
    }

    #[test]
    fn missing_account_asks_for_account() {
        let mut i = inputs();
        i.account_exists = false;
        assert_eq!(decide(&i), Decision::Setup(SetupNeed::Account));
    }

    #[test]
    fn held_lock_blocks_before_anything_else() {
        let mut i = inputs();
        i.identity = IdentityState::Missing;
        i.lock = LockOutcome::Held(Holder::Service);
        assert_eq!(decide(&i), Decision::Blocked(Blocker::Service));
        i.lock = LockOutcome::Held(Holder::OtherApp);
        assert_eq!(decide(&i), Decision::Blocked(Blocker::OtherApp));
        i.lock = LockOutcome::Failed("disque".into());
        assert_eq!(decide(&i), Decision::Problem("disque".into()));
    }

    #[test]
    fn installed_service_blocks_even_with_a_free_lock() {
        let mut i = inputs();
        i.service_installed = true;
        assert_eq!(decide(&i), Decision::Blocked(Blocker::Service));
    }

    #[test]
    fn old_desktop_app_blocks() {
        let mut i = inputs();
        i.desktop_v02_running = true;
        assert_eq!(decide(&i), Decision::Blocked(Blocker::DesktopV02));
    }

    #[test]
    fn explanations_give_the_way_out() {
        assert!(Blocker::Service.explain().contains("./uninstall.sh"));
        assert!(Blocker::Service.explain().contains("données restent"));
        assert!(Blocker::DesktopV02.explain().contains("version 0.2"));
    }

    const PS: &str = "    1 /sbin/launchd\n  501 /Applications/Matter Home Server.app/Contents/MacOS/matter-home-server-app\n  777 /usr/local/bin/matter-home-server\n  900 /Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop\nbruit\n";

    #[test]
    fn parses_ps_with_spaces_in_paths() {
        let procs = parse_ps(PS);
        assert_eq!(procs.len(), 4);
        assert_eq!(procs[1].pid, 501);
        assert!(procs[1].path.contains("Matter Home Server.app"));
    }

    #[test]
    fn classifies_lock_holders() {
        let procs = parse_ps(PS);
        assert_eq!(classify_holder(Some(501), &procs, false), Holder::OtherApp);
        assert_eq!(classify_holder(None, &procs, true), Holder::Service);
        assert_eq!(classify_holder(Some(777), &procs, true), Holder::Service);
        assert_eq!(classify_holder(None, &procs, false), Holder::OtherServer);
        assert_eq!(classify_holder(None, &procs[..1], false), Holder::Unknown);
    }

    #[test]
    fn detects_only_the_0_2_desktop_app() {
        let procs = parse_ps(PS);
        assert!(desktop_v02_running(&procs, |_| Some("0.2.1".into())));
        assert!(!desktop_v02_running(&procs, |_| Some("1.0.0".into())));
        assert!(!desktop_v02_running(&procs, |_| None));
        assert!(!desktop_v02_running(&procs[..3], |_| Some("0.2.1".into())));
    }

    #[test]
    fn reads_bundle_version() {
        let plist = "<dict><key>CFBundleName</key><string>X</string>\n<key>CFBundleShortVersionString</key><string>0.2.1</string></dict>";
        assert_eq!(parse_bundle_version(plist).as_deref(), Some("0.2.1"));
        assert_eq!(parse_bundle_version("<dict/>"), None);
        assert_eq!(
            info_plist_of(Path::new("/A.app/Contents/MacOS/x")).unwrap(),
            Path::new("/A.app/Contents/Info.plist")
        );
    }
}
