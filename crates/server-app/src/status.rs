//! Ce que l'application dit de l'état du serveur : phase, santé des appareils, texte du menu.

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
    Problem,
}

pub fn icon_kind(phase: &Phase) -> IconKind {
    match phase {
        Phase::Running => IconKind::Running,
        Phase::Starting | Phase::Stopping => IconKind::Starting,
        Phase::NeedsSetup | Phase::Blocked(_) | Phase::Problem(_) => IconKind::Problem,
    }
}

fn devices_text(health: &Health) -> String {
    match health.total {
        0 => "aucun appareil appairé".to_string(),
        1 => format!("{} appareil sur 1 en ligne", health.online),
        n => format!(
            "{} appareil{} sur {n} en ligne",
            health.online,
            if health.online > 1 { "s" } else { "" }
        ),
    }
}

/// La première ligne du menu, par exemple « En marche · 12 appareils sur 13 en ligne ».
pub fn status_line(phase: &Phase, health: &Health) -> String {
    match phase {
        Phase::Starting => "Démarrage…".into(),
        Phase::Running => format!("En marche · {}", devices_text(health)),
        Phase::NeedsSetup => "Configuration à terminer".into(),
        Phase::Blocked(b) => format!("À l'arrêt · {}", b.short()),
        Phase::Problem(_) => "Problème : le serveur ne tourne pas".into(),
        Phase::Stopping => "Arrêt en cours…".into(),
    }
}

/// Une phrase pour l'infobulle de l'icône.
pub fn tooltip(phase: &Phase, health: &Health) -> String {
    format!("Matter Home Server : {}", status_line(phase, health))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(total: usize, online: usize) -> Health {
        Health {
            total,
            online,
            unreachable: vec![],
        }
    }

    #[test]
    fn running_line_counts_devices() {
        assert_eq!(
            status_line(&Phase::Running, &health(13, 12)),
            "En marche · 12 appareils sur 13 en ligne"
        );
        assert_eq!(
            status_line(&Phase::Running, &health(13, 1)),
            "En marche · 1 appareil sur 13 en ligne"
        );
        assert_eq!(
            status_line(&Phase::Running, &health(1, 1)),
            "En marche · 1 appareil sur 1 en ligne"
        );
        assert_eq!(
            status_line(&Phase::Running, &health(0, 0)),
            "En marche · aucun appareil appairé"
        );
    }

    #[test]
    fn other_phases() {
        let h = Health::default();
        assert_eq!(status_line(&Phase::Starting, &h), "Démarrage…");
        assert_eq!(
            status_line(&Phase::NeedsSetup, &h),
            "Configuration à terminer"
        );
        assert!(status_line(&Phase::Problem("x".into()), &h).starts_with("Problème"));
        assert!(status_line(&Phase::Blocked(Blocker::DesktopV02), &h).starts_with("À l'arrêt"));
    }

    #[test]
    fn icon_follows_phase() {
        assert_eq!(icon_kind(&Phase::Running), IconKind::Running);
        assert_eq!(icon_kind(&Phase::Starting), IconKind::Starting);
        assert_eq!(icon_kind(&Phase::NeedsSetup), IconKind::Problem);
        assert_eq!(
            icon_kind(&Phase::Blocked(Blocker::OtherServer)),
            IconKind::Problem
        );
        assert_eq!(icon_kind(&Phase::Problem("x".into())), IconKind::Problem);
    }
}
