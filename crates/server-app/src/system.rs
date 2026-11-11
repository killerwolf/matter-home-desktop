//! Petits services du système (aucun ne demande de droits administrateur) : processus,
//! nom du Mac, presse-papiers, ouverture d'un fichier.

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::startup::{self, BundleInfo, Proc};

pub fn processes() -> Vec<Proc> {
    Command::new("/bin/ps")
        .args(["-axo", "pid=,etime=,comm="])
        .output()
        .map(|o| startup::parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, size: u32) -> i32;
}

/// Chemin réel de l'exécutable d'un processus (libproc, déjà dans libSystem). Jamais `argv[0]`,
/// que `ps` rapporte dans `comm` : il peut être relatif ou trompeur.
pub fn exe_path_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // PROC_PIDPATHINFO_MAXSIZE = 4 * MAXPATHLEN.
    let mut buf = vec![0u8; 4096];
    // SAFETY : `buf` est valide en écriture sur `size` octets ; `proc_pidpath` n'en écrit pas plus.
    let n = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

extern "C" {
    fn CFRunLoopGetCurrent() -> *const std::ffi::c_void;
    fn CFRunLoopCopyCurrentMode(rl: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CFEqual(a: *const std::ffi::c_void, b: *const std::ffi::c_void) -> u8;
    fn CFRelease(cf: *const std::ffi::c_void);
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
}

/// Vrai quand la boucle d'événements tourne dans son mode normal. Pendant qu'un menu de la barre
/// de menus est ouvert (ou qu'un clic y est suivi), AppKit la fait tourner dans un autre mode
/// (`NSEventTrackingRunLoopMode`) : c'est le seul signal fiable que muda et tray-icon laissent
/// (aucun événement « menu ouvert / fermé »). Sert à ne pas remplacer le menu sous les doigts.
pub fn run_loop_is_default_mode() -> bool {
    // SAFETY : CoreFoundation est déjà lié (via AppKit) ; appelé sur le fil principal, la boucle
    // courante est valide, le mode copié (« Copy » : +1) est relâché avant de sortir.
    unsafe {
        let mode = CFRunLoopCopyCurrentMode(CFRunLoopGetCurrent());
        if mode.is_null() {
            return true;
        }
        let is_default = CFEqual(mode, kCFRunLoopDefaultMode) != 0;
        CFRelease(mode);
        is_default
    }
}

/// Version de l'application dont l'exécutable est `exe` et date de dernier changement de son
/// `Info.plist` (la plus récente de mtime et ctime : un copier-glisser conserve mtime, jamais ctime).
pub fn bundle_info_of(exe: &Path) -> BundleInfo {
    use std::os::unix::fs::MetadataExt;
    let Some(plist) = startup::info_plist_of(exe) else {
        return BundleInfo::default();
    };
    let version = std::fs::read_to_string(&plist)
        .ok()
        .and_then(|t| startup::parse_bundle_version(&t));
    let plist_changed = std::fs::metadata(&plist).ok().map(|m| {
        let secs = m.mtime().max(m.ctime()).max(0) as u64;
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    });
    BundleInfo {
        version,
        plist_changed,
    }
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

/// Ouvre dans le navigateur par défaut (`open`, aucun shell) une adresse https dont l'hôte est
/// `login.tailscale.com` ou un nom `.ts.net` : rien d'autre.
pub fn open_url(url: &str) {
    if crate::tailscale::is_openable(url) {
        let _ = Command::new("/usr/bin/open").arg(url).spawn();
    }
}

pub fn open_file(path: &Path) {
    let _ = Command::new("/usr/bin/open").arg("-t").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_exe_path_of_this_process() {
        let me = exe_path_of(std::process::id()).expect("proc_pidpath");
        assert_eq!(
            me.canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert!(exe_path_of(u32::MAX - 1).is_none());
    }

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
