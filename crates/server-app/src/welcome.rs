//! Première installation : ce qu'il faut avoir avant de commencer, ce qui sort (ou non) de la
//! maison, et l'essai avec des appareils simulés.
//!
//! L'essai relance **ce même programme** avec `--demo` : un autre processus, avec son propre
//! dossier temporaire (voir `main.rs`), sans réseau Matter, sans ouverture à la connexion. Il ne
//! lit ni n'écrit jamais les données réelles de la maison.

use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::{agent, signals};

/// Ce qu'Ostal suppose, dit avant tout choix (README, première installation).
pub const PREREQUISITES: [&str; 4] = [
    "Ce Mac reste allumé en permanence, sur le même réseau que vos appareils.",
    "Vos appareils Matter sont déjà installés dans l'app Maison d'Apple : Ostal s'y ajoute comme un contrôleur de plus.",
    "Un HomePod ou une Apple TV sert de hub et de routeur Thread : un Mac ne peut pas jouer ce rôle.",
    "Tailscale (gratuit en usage personnel) est nécessaire seulement pour consulter la maison depuis l'extérieur.",
];

/// La promesse de confidentialité, sans « 100 % local » : elle dit ce qui est vrai.
pub const PRIVACY_PROMISE: &str =
    "Aucun cloud de l'éditeur : rien de la maison ne part sans une option que vous activez. L'historique des mesures est gardé 7 jours sur ce Mac.";

/// Les seules exceptions, chacune derrière une option ou un service que vous choisissez.
pub const PRIVACY_EXCEPTIONS: [&str; 3] = [
    "Descriptions des événements par IA, si vous les activez : une image par événement choisi part vers l'API d'Anthropic, avec votre clé.",
    "Notifications push, si un appareil s'abonne : le message, chiffré, passe par le service d'Apple, de Google, de Mozilla ou de Microsoft.",
    "Tailscale, si vous l'utilisez pour l'accès à distance : le trafic passe par votre réseau privé Tailscale.",
];

/// Les arguments de l'essai : la démonstration, ouverte sur la vue d'ensemble. Jamais de
/// `--data-dir` ni de `--listen` : le dossier est un dossier temporaire neuf et l'écoute reste
/// sur la boucle locale (valeurs de la démonstration dans `main.rs`).
pub const DEMO_ARGS: [&str; 3] = ["--demo", "--open", "vue"];

/// La commande qui lance la démonstration avec le programme `exe`. Pas de shell, aucune sortie
/// gardée, et pas la variable qui dit « lancé par launchd » (la démo n'est jamais supervisée).
pub fn demo_command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.args(DEMO_ARGS)
        .env_remove(agent::SUPERVISED_ENV)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// La démonstration lancée depuis cette fenêtre (une seule à la fois).
#[derive(Default)]
pub struct DemoSession {
    child: Option<Child>,
}

impl DemoSession {
    /// Vrai tant que le processus de démonstration tourne.
    pub fn is_open(&mut self) -> bool {
        match self.child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.child = None;
                false
            }
            None => false,
        }
    }

    /// Lance la démonstration, sauf si elle est déjà ouverte.
    pub fn start(&mut self, exe: &Path) -> std::io::Result<()> {
        if self.is_open() {
            return Ok(());
        }
        self.child = Some(demo_command(exe).spawn()?);
        Ok(())
    }

    /// Demande à la démonstration de s'arrêter proprement (SIGTERM : elle efface son dossier
    /// temporaire), sans l'attendre.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if matches!(child.try_wait(), Ok(None)) {
                // SAFETY : `kill` ne prend que deux entiers ; le pid est celui d'un enfant vivant
                // (pas encore attendu), il ne peut pas avoir été réutilisé.
                unsafe { kill(child.id() as i32, signals::SIGTERM) };
                // Le processus se termine seul ; un fil le récolte pour ne pas laisser de zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}

impl Drop for DemoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_demo_always_runs_in_demo_mode_without_real_data_or_network_options() {
        let c = demo_command(Path::new("/x/ostal-server-app"));
        let args: Vec<&OsStr> = c.get_args().collect();
        assert_eq!(args, ["--demo", "--open", "vue"]);
        for forbidden in ["--data-dir", "--listen", "--demo-setup"] {
            assert!(!args.iter().any(|a| *a == forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_demo_is_never_started_as_supervised() {
        let c = demo_command(Path::new("/x/ostal-server-app"));
        let removed = c
            .get_envs()
            .any(|(k, v)| k == OsStr::new(agent::SUPERVISED_ENV) && v.is_none());
        assert!(removed, "la variable launchd doit être retirée");
        assert_eq!(c.get_current_dir(), Some(Path::new("/")));
    }

    #[test]
    fn the_prerequisites_name_each_condition() {
        let all = PREREQUISITES.join(" ");
        for needle in ["allumé", "Maison", "HomePod", "Thread", "Tailscale"] {
            assert!(all.contains(needle), "{needle}");
        }
    }

    #[test]
    fn the_privacy_promise_is_honest() {
        assert!(!PRIVACY_PROMISE.contains("100"));
        assert!(PRIVACY_PROMISE.contains("Aucun cloud de l'éditeur"));
        assert!(PRIVACY_PROMISE.contains("7 jours"));
        let all = PRIVACY_EXCEPTIONS.join(" ");
        for needle in ["IA", "Apple", "Google", "Mozilla", "Microsoft", "Tailscale"] {
            assert!(all.contains(needle), "{needle}");
        }
    }

    #[test]
    fn a_session_that_never_started_is_closed_and_stop_is_harmless() {
        let mut s = DemoSession::default();
        assert!(!s.is_open());
        s.stop();
    }

    #[test]
    fn a_failing_launch_reports_the_error_and_stays_closed() {
        let mut s = DemoSession::default();
        assert!(s.start(Path::new("/nonexistent/ostal-server-app")).is_err());
        assert!(!s.is_open());
    }

    #[test]
    fn a_running_demo_is_not_started_twice_and_stop_ends_it() {
        // `sleep` tient lieu de programme : on ne lance jamais le vrai serveur dans un test.
        let mut s = DemoSession {
            child: Some(Command::new("/bin/sleep").arg("30").spawn().unwrap()),
        };
        assert!(s.is_open());
        s.start(Path::new("/nonexistent/ostal-server-app")).unwrap();
        assert!(s.is_open(), "déjà ouverte : aucun second lancement");
        s.stop();
        assert!(!s.is_open());
    }
}
