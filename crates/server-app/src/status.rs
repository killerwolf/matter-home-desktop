//! Ce que l'application dit de l'état du serveur : phase, santé des appareils, icône et infobulle
//! (le résumé lui-même est dans `overview`).

use crate::overview::Overview;
use crate::startup::Blocker;

/// Où en est le serveur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Running,
    /// Il manque l'identité Matter ou le compte : la fenêtre de première configuration s'ouvre.
    NeedsSetup,
    /// Un autre programme tient les données : le serveur n'est pas démarré.
    Blocked(Blocker),
    Problem(String),
    Stopping,
}

/// Un appareil injoignable : de quoi le nommer, le situer et le relancer.
#[derive(Debug, Clone, PartialEq)]
pub struct Unreachable {
    pub id: u64,
    pub name: String,
    /// La pièce du plan, si l'appareil y est placé.
    pub room: Option<String>,
    /// Dernier signe de vie du hub (secondes depuis 1970), s'il en a un.
    pub last_seen: Option<f64>,
}

/// Un appareil joignable, rangé par pièce pour l'onglet Appareils.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    /// `None` : pas encore placé dans une pièce.
    pub room: Option<String>,
    pub reachable: bool,
}

/// Les appareils vus par le serveur.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub total: usize,
    pub online: usize,
    /// Noms des appareils injoignables (triés).
    pub unreachable: Vec<String>,
    /// Les mêmes, avec leur identifiant, leur pièce et leur dernier signe de vie (même ordre).
    pub unreachable_details: Vec<Unreachable>,
    /// Tous les appareils, triés par pièce puis par nom (onglet Appareils).
    pub listed: Vec<Listed>,
}

/// L'heure locale « 18:02 » d'un instant (secondes depuis 1970). `None` si l'heure locale n'est
/// pas lisible.
pub fn clock(epoch_secs: f64) -> Option<String> {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
    }
    if !epoch_secs.is_finite() || epoch_secs <= 0.0 {
        return None;
    }
    let time = epoch_secs as i64;
    let mut tm = Tm {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        wday: 0,
        yday: 0,
        isdst: 0,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY : `time` et `tm` sont valides pour la durée de l'appel ; `Tm` a la disposition du
    // `struct tm` de macOS.
    let done = unsafe { localtime_r(&time, &mut tm) };
    (!done.is_null()).then(|| format!("{:02}:{:02}", tm.hour, tm.min))
}

/// L'état résumé par l'icône de la barre de menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Starting,
    Running,
    /// Le serveur tourne, mais une chose est à regarder (appareil ou caméra injoignable…).
    Attention,
    Problem,
}

/// L'icône : la phase du serveur d'abord ; en marche, elle signale aussi une chose à regarder.
pub fn icon_kind(phase: &Phase, overview: &Overview) -> IconKind {
    match phase {
        Phase::Running if overview.level.needs_attention() => IconKind::Attention,
        Phase::Running => IconKind::Running,
        Phase::Starting | Phase::Stopping => IconKind::Starting,
        Phase::NeedsSetup | Phase::Blocked(_) | Phase::Problem(_) => IconKind::Problem,
    }
}

/// L'infobulle de l'icône : le titre du résumé, puis ce qui ne va pas (une ligne par point).
pub fn tooltip(overview: &Overview) -> String {
    let mut text = format!("Ostal Server : {}", overview.headline);
    for check in overview.checks.iter().filter(|c| c.level.needs_attention()) {
        text.push_str(&format!("\n{} : {}", check.title, check.text));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview;
    use crate::tailscale::WebAccess;

    fn overview_of(phase: &Phase, unreachable: &[&str]) -> Overview {
        let health = Health {
            total: 5,
            online: 5 - unreachable.len(),
            unreachable: unreachable.iter().map(|s| s.to_string()).collect(),
            ..Health::default()
        };
        overview::build(phase, &health, None, &WebAccess::Checking, false)
    }

    #[test]
    fn icon_follows_phase() {
        let kind = |p: &Phase| icon_kind(p, &overview_of(p, &[]));
        assert_eq!(kind(&Phase::Running), IconKind::Running);
        assert_eq!(kind(&Phase::Starting), IconKind::Starting);
        assert_eq!(kind(&Phase::NeedsSetup), IconKind::Problem);
        assert_eq!(
            kind(&Phase::Blocked(Blocker::OtherServer)),
            IconKind::Problem
        );
        assert_eq!(kind(&Phase::Problem("x".into())), IconKind::Problem);
    }

    #[test]
    fn a_running_server_with_something_to_look_at_has_its_own_icon() {
        let o = overview_of(&Phase::Running, &["Salon"]);
        assert_eq!(icon_kind(&Phase::Running, &o), IconKind::Attention);
    }

    #[test]
    fn tooltip_says_what_is_wrong_and_nothing_else() {
        assert_eq!(
            tooltip(&overview_of(&Phase::Running, &[])),
            "Ostal Server : La maison va bien."
        );
        assert_eq!(
            tooltip(&overview_of(&Phase::Running, &["Salon"])),
            "Ostal Server : Une chose à regarder\nAppareils : 4 sur 5 joignables · « Salon » est injoignable"
        );
        assert_eq!(
            tooltip(&overview_of(&Phase::Blocked(Blocker::DesktopV02), &[])),
            "Ostal Server : Le serveur est à l'arrêt\nServeur : L'ancienne application de bureau tourne"
        );
    }
}
