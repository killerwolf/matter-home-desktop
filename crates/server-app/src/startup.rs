//! La décision de démarrage : le serveur démarre-t-il, faut-il configurer, ou un autre programme
//! tient-il déjà les données ? Pure : toutes les observations (verrou, processus, fichiers) sont
//! passées en argument, donc testables sans toucher au système.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use matter_home_server::setup::IdentityState;

/// Nom du binaire de cette application et du serveur en ligne de commande.
pub const APP_BINARY: &str = "matter-home-server-app";
pub const CLI_BINARY: &str = "matter-home-server";
/// Le binaire de l'application de bureau (v0.2.1 comme client actuel, qui porte le même nom).
pub const DESKTOP_BINARY: &str = "matter-home-desktop";
/// Le service installé par l'archive en ligne de commande.
pub const SERVICE_PLIST: &str = "/Library/LaunchDaemons/dev.matter-home-desktop.server.plist";

/// Un processus de la liste de `ps` : numéro, temps écoulé depuis son lancement (s'il est lisible)
/// et chemin de l'exécutable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub elapsed: Option<Duration>,
    pub path: String,
}

impl Proc {
    fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Le temps écoulé de `ps -o etime` : `[[jj-]hh:]mm:ss`.
pub fn parse_etime(text: &str) -> Option<Duration> {
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, text),
    };
    let mut seconds = 0u64;
    let parts: Vec<&str> = rest.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    for part in parts {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(days * 86_400 + seconds))
}

/// Lit la sortie de `ps -axo pid=,etime=,comm=` (le chemin, qui peut contenir des espaces, en
/// dernier).
pub fn parse_ps(output: &str) -> Vec<Proc> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let (etime, path) = rest.trim_start().split_once(char::is_whitespace)?;
            Some(Proc {
                pid: pid.parse().ok()?,
                elapsed: parse_etime(etime),
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
    /// L'ancienne application de bureau (v0.2.x) tourne, ou (au démarrage seulement) on ne peut
    /// pas exclure que ce soit elle : elle ne prend pas le verrou. Le client V1, de même nom de
    /// processus, n'en prend aucun et ne touche jamais aux données : il ne bloque pas.
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
            Blocker::DesktopV02 => "l'ancienne application de bureau tourne",
        }
    }

    /// L'explication complète, avec la marche à suivre.
    pub fn explain(&self) -> String {
        match self {
            Blocker::OtherApp => "Matter Home Server est déjà ouvert : cherchez son icône dans la barre de menus.".into(),
            Blocker::Service => "Le serveur est déjà installé sur ce Mac comme service (ancienne méthode, au Terminal) et il utilise les données. Pour passer à cette application : ouvrez le Terminal une seule fois, allez dans le dossier de l'archive du serveur (celui où se trouve « uninstall.sh ») et lancez « ./uninstall.sh » : il demande le mot de passe administrateur du Mac, et vos données restent en place. Si ce dossier n'existe plus, retéléchargez l'archive sur la page des versions, ou tapez ces deux commandes : « sudo launchctl bootout system/dev.matter-home-desktop.server » puis « sudo rm -f /Library/LaunchDaemons/dev.matter-home-desktop.server.plist /usr/local/bin/matter-home-server ». Rouvrez ensuite Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::OtherServer => "Un autre serveur Matter Home (lancé à la main) utilise déjà les données. Arrêtez-le, puis rouvrez Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::Unknown => "Les données sont déjà utilisées par un autre programme Matter Home. Fermez-le, puis rouvrez Matter Home Server. Rien n'est démarré en attendant.".into(),
            Blocker::DesktopV02 => "L'ancienne application de bureau (version 0.2) est ouverte. Elle ne signale pas qu'elle utilise les données : si le serveur démarrait en même temps, les deux se disputeraient vos appareils. Fermez-la (sur tous vos Mac), le serveur démarrera alors tout seul. Au démarrage, même message si l'application de bureau a été mise à jour pendant qu'elle était ouverte, ou si on ne peut pas savoir laquelle tourne : fermez-la puis rouvrez-la. Le client Matter Home V1 n'est pas concerné.".into(),
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

/// Ce qu'on sait de l'application dont l'exécutable tourne : sa version et la date de dernière
/// modification de son `Info.plist` (la plus récente de mtime et ctime : un copier-glisser conserve
/// la date de modification d'origine, pas la date de changement).
#[derive(Debug, Clone, Default)]
pub struct BundleInfo {
    pub version: Option<String>,
    pub plist_changed: Option<SystemTime>,
}

/// Marge sur l'heure de lancement (le temps écoulé de `ps` est arrondi à la seconde).
const START_MARGIN: Duration = Duration::from_secs(5);

/// Ce que l'on sait d'un processus nommé `matter-home-desktop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Ce n'est pas l'ancienne v0.2 : exécutable hors d'une `.app` (`cargo run`, `target/debug`,
    /// client V1 de développement) ou application d'une autre version, intacte depuis le lancement.
    Healthy,
    /// Preuve positive : une version 0.2.x a été lue pour ce processus.
    V02,
    /// On ne peut pas savoir (chemin introuvable, `Info.plist` illisible, ou modifié après le
    /// lancement : application remplacée pendant qu'elle tourne).
    Uncertain,
}

/// `<App>.app/Contents/MacOS/<binaire>` : l'exécutable d'une application distribuée.
pub fn is_app_bundle_executable(exe: &Path) -> bool {
    fn name(p: Option<&Path>) -> Option<&str> {
        p.and_then(|p| p.file_name()).and_then(|n| n.to_str())
    }
    let macos = exe.parent();
    let contents = macos.and_then(Path::parent);
    let app = contents.and_then(Path::parent);
    name(macos) == Some("MacOS")
        && name(contents) == Some("Contents")
        && name(app).is_some_and(|n| n.ends_with(".app"))
}

/// Juge un processus d'après le chemin réel de son exécutable (`proc_pidpath`, jamais `argv[0]`).
pub fn judge(
    p: &Proc,
    exe: Option<&Path>,
    inspect: impl Fn(&Path) -> BundleInfo,
    now: SystemTime,
) -> Verdict {
    let Some(exe) = exe else {
        return Verdict::Uncertain;
    };
    if !is_app_bundle_executable(exe) {
        return Verdict::Healthy;
    }
    let info = inspect(exe);
    let Some(version) = &info.version else {
        return Verdict::Uncertain;
    };
    if version == "0.2" || version.starts_with("0.2.") {
        return Verdict::V02;
    }
    match (p.elapsed, info.plist_changed) {
        (Some(elapsed), Some(changed)) => {
            let started = now.checked_sub(elapsed).unwrap_or(SystemTime::UNIX_EPOCH);
            if changed > started + START_MARGIN {
                Verdict::Uncertain
            } else {
                Verdict::Healthy
            }
        }
        _ => Verdict::Healthy,
    }
}

/// Verdicts retenus par (numéro, heure de lancement) : un processus jugé sain n'est pas
/// réévalué, donc un `xattr`, un déplacement ou une mise à jour de son `Info.plist` ensuite ne
/// change rien. Un numéro réutilisé (heure de lancement différente) est jugé de nouveau.
/// Le doute n'est pas retenu : il est réexaminé à chaque contrôle.
#[derive(Debug, Default)]
pub struct VerdictCache {
    known: Vec<(u32, SystemTime, Verdict)>,
}

impl VerdictCache {
    fn get(&self, pid: u32, started: SystemTime) -> Option<Verdict> {
        self.known
            .iter()
            .find(|(p, s, _)| *p == pid && abs_diff(*s, started) <= START_MARGIN)
            .map(|(_, _, v)| *v)
    }
}

fn abs_diff(a: SystemTime, b: SystemTime) -> Duration {
    a.duration_since(b)
        .or_else(|_| b.duration_since(a))
        .unwrap_or_default()
}

/// Résultat d'un contrôle de l'ancienne application de bureau.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopScan {
    /// Une v0.2.x lue pour un processus en cours.
    pub v02: bool,
    /// Numéros des processus sur lesquels on ne peut pas conclure.
    pub uncertain: Vec<u32>,
}

/// Cherche l'ancienne application de bureau parmi les processus. `exe_of` donne le chemin réel
/// de l'exécutable d'un processus, `inspect` lit l'application correspondante.
pub fn scan_desktop(
    procs: &[Proc],
    now: SystemTime,
    cache: &mut VerdictCache,
    exe_of: impl Fn(u32) -> Option<PathBuf>,
    inspect: impl Fn(&Path) -> BundleInfo,
) -> DesktopScan {
    let mut scan = DesktopScan::default();
    let mut seen = Vec::new();
    for p in procs.iter().filter(|p| p.file_name() == DESKTOP_BINARY) {
        let started = now
            .checked_sub(p.elapsed.unwrap_or_default())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let verdict = match (p.elapsed.is_some(), cache.get(p.pid, started)) {
            (true, Some(v)) => v,
            _ => {
                let v = judge(p, exe_of(p.pid).as_deref(), &inspect, now);
                if v != Verdict::Uncertain && p.elapsed.is_some() {
                    cache.known.retain(|(pid, _, _)| *pid != p.pid);
                    cache.known.push((p.pid, started, v));
                }
                v
            }
        };
        seen.push(p.pid);
        match verdict {
            Verdict::V02 => scan.v02 = true,
            Verdict::Uncertain => scan.uncertain.push(p.pid),
            Verdict::Healthy => {}
        }
    }
    cache.known.retain(|(pid, _, _)| seen.contains(pid));
    scan
}

/// `<App>.app/Contents/MacOS/<binaire>` → `<App>.app/Contents/Info.plist`.
pub fn info_plist_of(exe: &Path) -> Option<PathBuf> {
    if !is_app_bundle_executable(exe) {
        return None;
    }
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
        assert!(Blocker::Service
            .explain()
            .contains("mot de passe administrateur"));
        // Le dossier de l'archive a pu disparaître : les commandes équivalentes sont données.
        assert!(Blocker::Service.explain().contains("launchctl bootout"));
        assert!(Blocker::Service.explain().contains(SERVICE_PLIST));
        assert!(Blocker::DesktopV02.explain().contains("version 0.2"));
    }

    const PS: &str = "    1    05-03:04:05 /sbin/launchd\n  501       01:02 /Applications/Matter Home Server.app/Contents/MacOS/matter-home-server-app\n  777    12:00:01 /usr/local/bin/matter-home-server\n  900       45:10 /Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop\nbruit\n";

    #[test]
    fn parses_ps_with_spaces_in_paths() {
        let procs = parse_ps(PS);
        assert_eq!(procs.len(), 4);
        assert_eq!(procs[1].pid, 501);
        assert!(procs[1].path.contains("Matter Home Server.app"));
        assert_eq!(procs[1].elapsed, Some(Duration::from_secs(62)));
        assert_eq!(procs[2].elapsed, Some(Duration::from_secs(12 * 3600 + 1)));
        assert_eq!(
            procs[0].elapsed,
            Some(Duration::from_secs(5 * 86_400 + 3 * 3600 + 4 * 60 + 5))
        );
    }

    #[test]
    fn parses_elapsed_time_formats() {
        assert_eq!(parse_etime("00:07"), Some(Duration::from_secs(7)));
        assert_eq!(parse_etime("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(
            parse_etime("2-00:00:01"),
            Some(Duration::from_secs(172_801))
        );
        assert_eq!(parse_etime("n/a"), None);
        assert_eq!(parse_etime("12"), None);
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

    const APP: &str = "/Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop";

    fn info(version: Option<&str>, changed_ago_s: Option<u64>, now: SystemTime) -> BundleInfo {
        BundleInfo {
            version: version.map(Into::into),
            plist_changed: changed_ago_s.map(|s| now - Duration::from_secs(s)),
        }
    }

    fn proc(pid: u32, elapsed_s: u64) -> Proc {
        Proc {
            pid,
            elapsed: Some(Duration::from_secs(elapsed_s)),
            // `comm` de `ps` : argv[0], pas le chemin réel.
            path: "./matter-home-desktop".into(),
        }
    }

    fn scan(
        procs: &[Proc],
        now: SystemTime,
        cache: &mut VerdictCache,
        exe: Option<&str>,
        inspected: BundleInfo,
    ) -> DesktopScan {
        scan_desktop(
            procs,
            now,
            cache,
            |_| exe.map(PathBuf::from),
            |_| inspected.clone(),
        )
    }

    #[test]
    fn a_v1_client_outside_an_app_never_stops_the_server() {
        let now = SystemTime::now();
        let mut c = VerdictCache::default();
        for exe in [
            "/Users/h/git/matter-home-desktop/target/debug/matter-home-desktop",
            "/Users/h/git/matter-home-desktop/target/release/matter-home-desktop",
        ] {
            // Même si on lisait une version 0.2 ailleurs : hors `.app`, ce n'est pas une v0.2.1 distribuée.
            let r = scan(
                &[proc(10, 600)],
                now,
                &mut c,
                Some(exe),
                info(Some("0.2.1"), None, now),
            );
            assert_eq!(r, DesktopScan::default());
        }
    }

    #[test]
    fn relative_launch_uses_the_real_path_not_argv0() {
        let now = SystemTime::now();
        let mut c = VerdictCache::default();
        // `comm` = « ./matter-home-desktop », chemin réel dans une `.app` V1 saine.
        let r = scan(
            &[proc(10, 600)],
            now,
            &mut c,
            Some(APP),
            info(Some("1.0.0"), Some(86_400), now),
        );
        assert_eq!(r, DesktopScan::default());
    }

    #[test]
    fn moved_or_touched_v1_app_does_not_stop_a_running_server() {
        let now = SystemTime::now();
        let mut c = VerdictCache::default();
        // Jugée saine au premier contrôle (plist ancien) : mémorisée.
        let healthy = scan(
            &[proc(10, 600)],
            now,
            &mut c,
            Some(APP),
            info(Some("1.0.0"), Some(86_400), now),
        );
        assert_eq!(healthy, DesktopScan::default());
        // Ensuite : plist « modifié » (xattr), illisible (app déplacée), chemin introuvable.
        let later = now + Duration::from_secs(30);
        for (exe, inspected) in [
            (Some(APP), info(Some("1.0.0"), Some(5), later)),
            (Some(APP), BundleInfo::default()),
            (None, BundleInfo::default()),
        ] {
            let r = scan(&[proc(10, 630)], later, &mut c, exe, inspected);
            assert_eq!(r, DesktopScan::default());
        }
    }

    #[test]
    fn new_uncertain_process_is_reported_but_does_not_stop() {
        let now = SystemTime::now();
        let mut c = VerdictCache::default();
        // Plist modifié après le lancement, jamais jugé avant : doute, pas de v0.2.
        let r = scan(
            &[proc(10, 600)],
            now,
            &mut c,
            Some(APP),
            info(Some("1.0.0"), Some(10), now),
        );
        assert!(!r.v02);
        assert_eq!(r.uncertain, vec![10]);
        // Plist illisible, chemin introuvable : doute aussi.
        let r = scan(
            &[proc(11, 600)],
            now,
            &mut c,
            Some(APP),
            BundleInfo::default(),
        );
        assert_eq!((r.v02, r.uncertain), (false, vec![11]));
        let r = scan(&[proc(12, 600)], now, &mut c, None, BundleInfo::default());
        assert_eq!((r.v02, r.uncertain), (false, vec![12]));
        // Modifié avant le lancement ou dans la marge : sain.
        for ago in [2711, 2708, 86_400] {
            let r = scan(
                &[proc(13, 2713)],
                now,
                &mut VerdictCache::default(),
                Some(APP),
                info(Some("1.0.0"), Some(ago), now),
            );
            assert_eq!(r, DesktopScan::default());
        }
    }

    #[test]
    fn a_read_0_2_version_stops_the_server() {
        let now = SystemTime::now();
        for v in ["0.2", "0.2.1"] {
            let r = scan(
                &[proc(10, 600)],
                now,
                &mut VerdictCache::default(),
                Some(APP),
                info(Some(v), Some(86_400), now),
            );
            assert!(r.v02, "{v}");
        }
        // Pas de processus du bon nom : rien.
        let other = Proc {
            pid: 5,
            elapsed: None,
            path: "/sbin/launchd".into(),
        };
        assert_eq!(
            scan(
                &[other],
                now,
                &mut VerdictCache::default(),
                Some(APP),
                info(Some("0.2.1"), None, now)
            ),
            DesktopScan::default()
        );
    }

    #[test]
    fn memoized_verdict_holds_and_reused_pid_is_judged_again() {
        let now = SystemTime::now();
        let mut c = VerdictCache::default();
        let healthy = info(Some("1.0.0"), Some(86_400), now);
        assert!(!scan(&[proc(10, 600)], now, &mut c, Some(APP), healthy).v02);
        // Même (pid, lancement) un peu plus tard, même si le disque dit maintenant 0.2.1 : verdict tenu.
        let later = now + Duration::from_secs(31);
        assert!(
            !scan(
                &[proc(10, 631)],
                later,
                &mut c,
                Some(APP),
                info(Some("0.2.1"), Some(86_400), later)
            )
            .v02
        );
        // Le numéro 10 réapparaît avec une autre heure de lancement : nouveau processus, réévalué.
        assert!(
            scan(
                &[proc(10, 40)],
                later,
                &mut c,
                Some(APP),
                info(Some("0.2.1"), Some(86_400), later)
            )
            .v02
        );
        // Un verdict v0.2 est retenu aussi.
        assert!(
            scan(
                &[proc(10, 70)],
                later + Duration::from_secs(30),
                &mut c,
                Some(APP),
                healthy_info(later)
            )
            .v02
        );
    }

    fn healthy_info(now: SystemTime) -> BundleInfo {
        info(Some("1.0.0"), Some(86_400), now)
    }

    #[test]
    fn recognises_app_bundle_executables() {
        assert!(is_app_bundle_executable(Path::new(APP)));
        assert!(!is_app_bundle_executable(Path::new(
            "/x/target/debug/matter-home-desktop"
        )));
        assert!(!is_app_bundle_executable(Path::new("matter-home-desktop")));
        assert!(!is_app_bundle_executable(Path::new(
            "/x/Contents/MacOS/matter-home-desktop"
        )));
        assert!(info_plist_of(Path::new("/x/target/debug/m")).is_none());
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
