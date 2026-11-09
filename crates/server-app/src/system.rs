//! Petits services du système (aucun ne demande de droits administrateur) : processus,
//! nom du Mac, presse-papiers, ouverture d'un fichier.

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::startup::{self, Proc};

pub fn processes() -> Vec<Proc> {
    Command::new("/bin/ps")
        .args(["-axo", "pid=,comm="])
        .output()
        .map(|o| startup::parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Version de l'application dont l'exécutable est `exe`, lue dans son `Info.plist`.
pub fn bundle_version_of(exe: &Path) -> Option<String> {
    let plist = startup::info_plist_of(exe)?;
    startup::parse_bundle_version(&std::fs::read_to_string(plist).ok()?)
}

pub fn service_installed() -> bool {
    Path::new(startup::SERVICE_PLIST).exists()
}

/// Nom Bonjour du Mac (« Mac-de-Hamdi »), celui de « nom-du-mac.local ».
pub fn local_host_name() -> Option<String> {
    for (program, args) in [
        ("/usr/sbin/scutil", &["--get", "LocalHostName"][..]),
        ("/bin/hostname", &["-s"][..]),
    ] {
        if let Ok(out) = Command::new(program).args(args).output() {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// L'adresse à saisir dans le client. Une écoute sur la boucle locale (essais) reste telle
/// quelle ; sinon « nom-du-mac.local » avec le port réellement ouvert.
pub fn server_url(listen: SocketAddr, host: Option<&str>) -> String {
    match host {
        Some(host) if !listen.ip().is_loopback() => {
            format!("https://{host}.local:{}", listen.port())
        }
        _ => format!("https://{listen}"),
    }
}

pub fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("stdin demandé")
        .write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

pub fn open_file(path: &Path) {
    let _ = Command::new("/usr/bin/open").arg("-t").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_uses_the_bonjour_name_on_the_network() {
        let any: SocketAddr = "0.0.0.0:8443".parse().unwrap();
        assert_eq!(
            server_url(any, Some("Mac-de-Hamdi")),
            "https://Mac-de-Hamdi.local:8443"
        );
        assert_eq!(server_url(any, None), "https://0.0.0.0:8443");
    }

    #[test]
    fn url_keeps_loopback_for_trials() {
        let lo: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        assert_eq!(server_url(lo, Some("Mac")), "https://127.0.0.1:50123");
    }
}
