//! SIGTERM et SIGINT (fin de session, `kill`, Ctrl-C au Terminal) : sans eux, macOS tue
//! l'application sans arrêt propre du serveur. Le gestionnaire ne fait que poser un drapeau
//! (seule opération sûre dans un gestionnaire de signal) ; un fil le lit et envoie `Cmd::Quit`,
//! comme `watch_signals` du serveur en ligne de commande.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::controller::Cmd;

extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

pub const SIGINT: i32 = 2;
pub const SIGTERM: i32 = 15;

static RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signum: i32) {
    RECEIVED.store(true, Ordering::SeqCst);
}

/// Intercepte `signals` et envoie `Cmd::Quit` à la première réception. À appeler avant le
/// démarrage de l'interface.
pub fn watch(signals: &[i32], tx: Sender<Cmd>) {
    for &s in signals {
        // SAFETY : `on_signal` ne fait qu'une écriture atomique ; l'adresse est celle d'une
        // fonction `extern "C"` du bon type.
        unsafe { signal(s, on_signal as extern "C" fn(i32) as usize) };
    }
    std::thread::Builder::new()
        .name("signaux".into())
        .spawn(move || loop {
            if RECEIVED.swap(false, Ordering::SeqCst) {
                log::info!("signal d'arrêt reçu : arrêt propre");
                let _ = tx.send(Cmd::Quit);
            }
            std::thread::sleep(Duration::from_millis(100));
        })
        .expect("fil des signaux");
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn raise(signum: i32) -> i32;
    }

    #[test]
    fn a_signal_becomes_a_quit_command() {
        let (tx, rx) = std::sync::mpsc::channel();
        // SIGUSR1 (30 sur macOS) : sans effet sur le processus de test une fois intercepté.
        watch(&[30], tx);
        unsafe { raise(30) };
        let cmd = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Quit envoyé");
        assert!(matches!(cmd, Cmd::Quit));
    }
}
