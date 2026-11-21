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

/// Les appareils vus par le serveur.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    pub total: usize,
    pub online: usize,
    /// Noms des appareils injoignables (triés).
    pub unreachable: Vec<String>,
}

/// L'état résumé par l'icône de la barre de menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Starting,
    Running,
    /// Le serveur tourne, mais un point est à regarder (appareil injoignable, caméra hors ligne…).
    Attention,
    Problem,
}

/// L'icône : la phase du serveur d'abord ; en marche, elle signale aussi un point à regarder.
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
    fn a_running_server_with_a_point_to_look_at_has_its_own_icon() {
        let o = overview_of(&Phase::Running, &["Salon"]);
        assert_eq!(icon_kind(&Phase::Running, &o), IconKind::Attention);
    }

    #[test]
    fn tooltip_says_what_is_wrong_and_nothing_else() {
        assert_eq!(
            tooltip(&overview_of(&Phase::Running, &[])),
            "Ostal Server : Tout va bien"
        );
        assert_eq!(
            tooltip(&overview_of(&Phase::Running, &["Salon"])),
            "Ostal Server : 1 point à regarder\nAppareils : 4 sur 5 joignables · injoignable : Salon"
        );
        assert_eq!(
            tooltip(&overview_of(&Phase::Blocked(Blocker::DesktopV02), &[])),
            "Ostal Server : Le serveur est à l'arrêt\nServeur : l'ancienne application de bureau tourne"
        );
    }
}
