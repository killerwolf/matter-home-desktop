//! Veille : tant que le serveur tourne, le Mac ne se met pas en veille tout seul. `caffeinate -i`
//! (outil d'Apple, sans droits administrateur, ni `pmset`) en processus enfant, avec `-w <pid>` :
//! il s'arrête de lui-même si l'application disparaît, même en cas de plantage.
//!
//! Cela ne couvre PAS le capot fermé : un portable qu'on referme s'endort quoi qu'on demande
//! sans écran ni alimentation externe (voir `docs/installation-serveur.md`).

use std::process::{Child, Command, Stdio};

pub fn caffeinate_args(pid: u32) -> Vec<String> {
    vec!["-i".into(), "-w".into(), pid.to_string()]
}

pub struct SleepGuard {
    child: Child,
}

impl SleepGuard {
    pub fn start() -> std::io::Result<Self> {
        let child = Command::new("/usr/bin/caffeinate")
            .args(caffeinate_args(std::process::id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child })
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_hold_off_idle_sleep_while_we_live() {
        assert_eq!(caffeinate_args(42), ["-i", "-w", "42"]);
        assert!(!caffeinate_args(42).iter().any(|a| a == "-s" || a == "-d"));
    }
}
