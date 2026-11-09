//! « Ouvrir à la connexion » sans droits administrateur : un LaunchAgent d'utilisateur, un simple
//! fichier dans `~/Library/LaunchAgents` que launchd lit à l'ouverture de session. Pas de
//! `launchctl`, pas de mot de passe. (L'API `SMAppService` de macOS 13 serait plus propre, mais
//! l'application vise macOS 10.15 sur Intel.)
//!
//! `KeepAlive` avec `SuccessfulExit = false` : relancée après un plantage ou une sortie en erreur
//! (code 1, 75), jamais après « Quitter » (code 0). L'application ne sort jamais en code 78 :
//! sans identité, elle ouvre la fenêtre de configuration et reste ouverte, donc aucune boucle
//! de relance. `ThrottleInterval` espace de toute façon les relances.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const LABEL: &str = "dev.matter-home-desktop.server-app";
/// Variable d'environnement posée par le LaunchAgent : elle seule prouve que launchd supervise
/// ce processus (le fichier peut exister sans être encore chargé, avant la prochaine connexion).
pub const SUPERVISED_ENV: &str = "MATTER_HOME_SERVER_LAUNCH_AGENT";
const THROTTLE_SECONDS: u32 = 30;

pub fn agent_path(launch_agents_dir: &Path) -> PathBuf {
    launch_agents_dir.join(format!("{LABEL}.plist"))
}

/// `~/Library/LaunchAgents`.
pub fn default_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/LaunchAgents"))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Le contenu du LaunchAgent pour l'exécutable `exe` (celui de l'application installée).
pub fn plist_text(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>{SUPERVISED_ENV}</key><string>1</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key><false/>
  </dict>
  <key>ThrottleInterval</key><integer>{THROTTLE_SECONDS}</integer>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy())
    )
}

/// L'exécutable est-il dans une application (`.app`) ?
pub fn is_in_bundle(exe: &Path) -> bool {
    exe.ancestors()
        .any(|a| a.extension().is_some_and(|e| e == "app"))
}

/// L'application est lancée depuis une image disque (`/Volumes/...`) ou depuis la copie
/// temporaire que macOS fabrique pour une application téléchargée jamais déplacée
/// (« App Translocation ») : ces chemins disparaissent, un agent qui y pointerait ne
/// démarrerait plus rien.
pub fn is_temporary_location(exe: &Path) -> bool {
    exe.starts_with("/Volumes") || exe.to_string_lossy().contains("/AppTranslocation/")
}

/// L'agent peut pointer ici : dans une application, à un emplacement durable.
pub fn is_installable(exe: &Path) -> bool {
    is_in_bundle(exe) && !is_temporary_location(exe)
}

/// Pourquoi l'agent ne peut pas pointer ici, en une phrase pour l'utilisateur.
pub fn refusal(exe: &Path) -> &'static str {
    if is_in_bundle(exe) && is_temporary_location(exe) {
        "L'application tourne depuis l'image disque (ou une copie temporaire) : glissez « Matter Home Server » dans le dossier Applications, ouvrez-la depuis là, puis réglez l'ouverture à la connexion."
    } else {
        "L'ouverture à la connexion n'est possible que depuis l'application installée (« Matter Home Server.app »)."
    }
}

pub fn is_enabled(dir: &Path) -> bool {
    agent_path(dir).is_file()
}

/// L'agent existe-t-il pour un autre emplacement de l'application (elle a été déplacée) ?
pub fn is_stale(dir: &Path, exe: &Path) -> bool {
    std::fs::read_to_string(agent_path(dir)).is_ok_and(|t| t != plist_text(exe))
}

pub fn enable(dir: &Path, exe: &Path) -> Result<()> {
    if !is_installable(exe) {
        bail!("{}", refusal(exe));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    let path = agent_path(dir);
    let tmp = path.with_extension("plist.tmp");
    std::fs::write(&tmp, plist_text(exe))
        .with_context(|| format!("écriture de {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("écriture de {}", path.display()))?;
    Ok(())
}

pub fn disable(dir: &Path) -> Result<()> {
    match std::fs::remove_file(agent_path(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("retrait de l'ouverture à la connexion"),
    }
}

/// Le dossier où l'application range ses petits réglages personnels : volontairement hors du
/// dossier de données (qui s'exporte et s'importe d'un Mac à l'autre : le choix d'un Mac ne
/// doit pas suivre l'autre).
pub fn default_marker_dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/Application Support/Matter Home Server"),
    )
}

fn choice_path(marker_dir: &Path) -> PathBuf {
    marker_dir.join("ouverture-a-la-connexion")
}

/// Le choix enregistré de l'utilisateur : `Some(true)` activé, `Some(false)` retiré.
pub fn recorded_choice(marker_dir: &Path) -> Option<bool> {
    match std::fs::read_to_string(choice_path(marker_dir))
        .ok()?
        .trim()
    {
        "oui" => Some(true),
        "non" => Some(false),
        _ => None,
    }
}

pub fn record_choice(marker_dir: &Path, on: bool) -> Result<()> {
    std::fs::create_dir_all(marker_dir)
        .with_context(|| format!("création de {}", marker_dir.display()))?;
    std::fs::write(choice_path(marker_dir), if on { "oui\n" } else { "non\n" })
        .context("enregistrement du choix d'ouverture à la connexion")
}

/// Ce qu'a donné `apply_default`.
#[derive(Debug, PartialEq, Eq)]
pub enum DefaultOutcome {
    /// Agent activé et choix enregistré (première exécution).
    Enabled,
    /// Un choix est déjà enregistré : respecté, rien n'est touché.
    AlreadyChosen,
    /// Démo, ou emplacement non durable : rien n'est écrit, rien n'est enregistré (le choix
    /// par défaut reste à faire quand l'application sera à sa place).
    Unavailable,
    Failed(String),
}

/// À la première exécution sans choix enregistré, active l'ouverture à la connexion (comme
/// à la fin de la configuration) et l'enregistre ; ensuite, respecte le choix (une case
/// décochée le reste). Jamais en démo, jamais depuis un emplacement non durable.
pub fn apply_default(
    agent_dir: &Path,
    marker_dir: &Path,
    exe: &Path,
    demo: bool,
) -> DefaultOutcome {
    if demo || !is_installable(exe) {
        return DefaultOutcome::Unavailable;
    }
    if recorded_choice(marker_dir).is_some() {
        return DefaultOutcome::AlreadyChosen;
    }
    if let Err(e) = enable(agent_dir, exe) {
        return DefaultOutcome::Failed(format!("{e:#}"));
    }
    match record_choice(marker_dir, true) {
        Ok(()) => DefaultOutcome::Enabled,
        Err(e) => DefaultOutcome::Failed(format!("{e:#}")),
    }
}

/// Application déplacée : l'agent pointait ailleurs, on le réécrit, sauf vers un emplacement
/// non durable.
pub fn rewrite_if_stale(agent_dir: &Path, exe: &Path) {
    if is_installable(exe) && is_stale(agent_dir, exe) {
        let _ = enable(agent_dir, exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/Applications/Matter Home Server.app/Contents/MacOS/matter-home-server-app";

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mh-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn plist_restarts_only_after_failures_without_loop() {
        let t = plist_text(Path::new(EXE));
        assert!(t.contains("<key>SuccessfulExit</key><false/>"));
        assert!(t.contains("<key>RunAtLoad</key><true/>"));
        assert!(t.contains("<key>ThrottleInterval</key><integer>30</integer>"));
        assert!(t.contains("<key>LimitLoadToSessionType</key><string>Aqua</string>"));
        assert!(t.contains(&format!("<key>{SUPERVISED_ENV}</key><string>1</string>")));
        assert!(t.contains(EXE));
        assert!(t.contains(LABEL));
    }

    #[test]
    fn plist_escapes_the_path() {
        let t = plist_text(Path::new("/A&B/x.app/Contents/MacOS/y"));
        assert!(t.contains("/A&amp;B/"));
    }

    #[test]
    fn enable_and_disable_in_a_temporary_folder() {
        let dir = temp("on-off");
        assert!(!is_enabled(&dir));
        enable(&dir, Path::new(EXE)).unwrap();
        assert!(is_enabled(&dir));
        assert!(!is_stale(&dir, Path::new(EXE)));
        assert!(is_stale(
            &dir,
            Path::new("/Users/x/Matter Home Server.app/Contents/MacOS/matter-home-server-app")
        ));
        disable(&dir).unwrap();
        assert!(!is_enabled(&dir));
        disable(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    const DMG_EXE: &str =
        "/Volumes/Matter Home Server/Matter Home Server.app/Contents/MacOS/matter-home-server-app";
    const TRANSLOCATED: &str = "/private/var/folders/ab/xyz/T/AppTranslocation/0000-1111/d/Matter Home Server.app/Contents/MacOS/matter-home-server-app";

    #[test]
    fn refuses_disk_images_and_translocation() {
        assert!(is_installable(Path::new(EXE)));
        assert!(!is_installable(Path::new(DMG_EXE)));
        assert!(!is_installable(Path::new(TRANSLOCATED)));
        assert!(!is_installable(Path::new("/tmp/matter-home-server-app")));
        let dir = temp("dmg");
        let err = enable(&dir, Path::new(DMG_EXE)).unwrap_err().to_string();
        assert!(err.contains("Applications"), "{err}");
        assert!(!dir.exists());
        // Une application déplacée vers un emplacement non durable n'est pas réécrite.
        enable(&dir, Path::new(EXE)).unwrap();
        rewrite_if_stale(&dir, Path::new(DMG_EXE));
        assert!(!std::fs::read_to_string(agent_path(&dir))
            .unwrap()
            .contains("/Volumes"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_run_enables_once_then_respects_the_choice() {
        let (agents, marker) = (temp("d-agents"), temp("d-marker"));
        let exe = Path::new(EXE);
        assert_eq!(recorded_choice(&marker), None);
        assert_eq!(
            apply_default(&agents, &marker, exe, false),
            DefaultOutcome::Enabled
        );
        assert!(is_enabled(&agents));
        assert_eq!(recorded_choice(&marker), Some(true));
        // L'utilisateur retire la coche : le choix est enregistré, il est respecté ensuite.
        disable(&agents).unwrap();
        record_choice(&marker, false).unwrap();
        assert_eq!(
            apply_default(&agents, &marker, exe, false),
            DefaultOutcome::AlreadyChosen
        );
        assert!(!is_enabled(&agents));
        let _ = std::fs::remove_dir_all(&agents);
        let _ = std::fs::remove_dir_all(&marker);
    }

    #[test]
    fn default_writes_nothing_in_demo_or_from_a_disk_image() {
        let (agents, marker) = (temp("n-agents"), temp("n-marker"));
        assert_eq!(
            apply_default(&agents, &marker, Path::new(EXE), true),
            DefaultOutcome::Unavailable
        );
        assert_eq!(
            apply_default(&agents, &marker, Path::new(DMG_EXE), false),
            DefaultOutcome::Unavailable
        );
        assert!(!agents.exists() && !marker.exists());
        // Le choix par défaut n'est pas « consommé » : une fois dans Applications, il s'applique.
        assert_eq!(
            apply_default(&agents, &marker, Path::new(EXE), false),
            DefaultOutcome::Enabled
        );
        let _ = std::fs::remove_dir_all(&agents);
        let _ = std::fs::remove_dir_all(&marker);
    }

    #[test]
    fn failed_enable_records_nothing() {
        let marker = temp("f-marker");
        // Un fichier à la place du dossier des agents : l'écriture échoue.
        let blocker = temp("f-block");
        std::fs::write(&blocker, "x").unwrap();
        let out = apply_default(&blocker.join("sub"), &marker, Path::new(EXE), false);
        assert!(matches!(out, DefaultOutcome::Failed(_)), "{out:?}");
        assert_eq!(recorded_choice(&marker), None);
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn refuses_a_bare_binary() {
        let dir = temp("bare");
        assert!(enable(&dir, Path::new("/tmp/target/debug/matter-home-server-app")).is_err());
        assert!(!dir.exists());
    }
}
