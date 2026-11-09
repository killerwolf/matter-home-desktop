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

/// L'exécutable n'est utilisable par un LaunchAgent que s'il est dans une application (`.app`).
pub fn is_in_bundle(exe: &Path) -> bool {
    exe.ancestors()
        .any(|a| a.extension().is_some_and(|e| e == "app"))
}

pub fn is_enabled(dir: &Path) -> bool {
    agent_path(dir).is_file()
}

/// L'agent existe-t-il pour un autre emplacement de l'application (elle a été déplacée) ?
pub fn is_stale(dir: &Path, exe: &Path) -> bool {
    std::fs::read_to_string(agent_path(dir)).is_ok_and(|t| t != plist_text(exe))
}

pub fn enable(dir: &Path, exe: &Path) -> Result<()> {
    if !is_in_bundle(exe) {
        bail!(
            "l'ouverture à la connexion n'est possible que depuis l'application installée (« Matter Home Server.app »)"
        );
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

    #[test]
    fn refuses_a_bare_binary() {
        let dir = temp("bare");
        assert!(enable(&dir, Path::new("/tmp/target/debug/matter-home-server-app")).is_err());
        assert!(!dir.exists());
    }
}
