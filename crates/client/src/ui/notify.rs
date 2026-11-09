//! Notification macOS locale, en plus du toast, quand une règle du serveur notifie et que le
//! client est ouvert (comme la v0.2.1). Un client fermé ne reçoit rien : le serveur n'a pas de
//! file d'attente pour les absents.
//!
//! Le titre et le texte viennent du serveur (donc d'une règle saisie par l'utilisateur, ou
//! d'un nom d'appareil) : ils sont passés en **arguments** d'`osascript` (`on run argv`), jamais
//! collés dans le texte du script.

/// Ce que fait l'application d'une notification reçue (remplaçable dans les tests, qui ne
/// doivent jamais afficher de vraie notification).
pub type Notifier = Box<dyn Fn(&str, &str)>;

const MAX_TITLE: usize = 120;
const MAX_MESSAGE: usize = 500;

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Arguments d'`osascript`. Le script est fixe ; le titre et le texte suivent `--`, qui les
/// empêche d'être lus comme des options si l'un commence par un tiret.
pub fn osascript_args(title: &str, message: &str) -> Vec<String> {
    vec![
        "-e".into(),
        "on run argv".into(),
        "-e".into(),
        "display notification (item 2 of argv) with title (item 1 of argv)".into(),
        "-e".into(),
        "end run".into(),
        "--".into(),
        clip(title, MAX_TITLE),
        clip(message, MAX_MESSAGE),
    ]
}

/// Notifications en attente d'affichage au plus. Au-delà, le surplus n'est pas affiché une par
/// une : un seul résumé le remplace (le journal et les toasts gardent le détail).
const QUEUE_CAPACITY: usize = 8;

/// Ce qui affiche vraiment une notification (titre, texte) ; remplaçable dans les tests.
type Runner = Box<dyn Fn(&str, &str) + Send + 'static>;

/// Texte du résumé qui remplace les notifications en surplus.
fn overflow_summary(count: usize) -> (&'static str, String) {
    (
        "Matter Home",
        format!(
            "{count} autre{} notification{} non affichée{} : voir le journal de l'application.",
            if count > 1 { "s" } else { "" },
            if count > 1 { "s" } else { "" },
            if count > 1 { "s" } else { "" },
        ),
    )
}

/// Un seul fil d'affichage, une file bornée. Une rafale (60 règles qui notifient d'un coup)
/// ne lance pas 60 processus : un seul tourne à la fois, `capacity` attendent au plus, et ce
/// qui dépasse est compté puis remplacé par un résumé unique.
fn queued_notifier(run: Runner, capacity: usize) -> Notifier {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};

    let (tx, rx) = mpsc::sync_channel::<(String, String)>(capacity);
    let dropped = Arc::new(AtomicUsize::new(0));
    let worker_dropped = dropped.clone();
    let spawned = std::thread::Builder::new()
        .name("matter-notify".into())
        .spawn(move || {
            while let Ok((title, message)) = rx.recv() {
                run(&title, &message);
                while let Ok((title, message)) = rx.try_recv() {
                    run(&title, &message);
                }
                let skipped = worker_dropped.swap(0, Ordering::SeqCst);
                if skipped > 0 {
                    let (title, message) = overflow_summary(skipped);
                    run(title, &message);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("notifications impossibles : {e}");
        return Box::new(|_, _| {});
    }
    Box::new(move |title, message| {
        if tx
            .try_send((title.to_string(), message.to_string()))
            .is_err()
        {
            dropped.fetch_add(1, Ordering::SeqCst);
        }
    })
}

/// Affiche la notification du système, dans un fil à part et à la queue leu leu (le fil de
/// l'interface n'attend rien, et jamais plus d'un `osascript` à la fois).
pub fn system_notifier() -> Notifier {
    if !cfg!(target_os = "macos") {
        return Box::new(|_, _| {});
    }
    queued_notifier(
        Box::new(|title, message| {
            let args = osascript_args(title, message);
            match std::process::Command::new("osascript").args(&args).output() {
                Ok(out) if !out.status.success() => log::warn!(
                    "osascript : {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Ok(_) => {}
                Err(e) => log::warn!("osascript introuvable : {e}"),
            }
        }),
        QUEUE_CAPACITY,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_arguments_never_part_of_the_script() {
        let title = "\" & (do shell script \"rm -rf ~\") & \"";
        let message = "-e 'tell application \"Finder\" to quit'";
        let args = osascript_args(title, message);
        let dashes = args.iter().position(|a| a == "--").unwrap();
        // Tout ce qui précède `--` est le script, fixe : le texte reçu n'y apparaît pas.
        let script = args[..dashes].join(" ");
        assert!(!script.contains("rm -rf") && !script.contains("Finder"));
        assert!(script.contains("item 1 of argv") && script.contains("item 2 of argv"));
        // Et les deux textes suivent, seuls, intacts.
        assert_eq!(
            &args[dashes + 1..],
            [title.to_string(), message.to_string()]
        );
    }

    /// 60 notifications d'un coup : un seul fil, quelques-unes affichées, un résumé pour le reste.
    #[test]
    fn a_burst_runs_one_process_at_a_time_and_ends_with_one_summary() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let shown: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let shown_by_worker = shown.clone();
        let threads: Arc<Mutex<std::collections::HashSet<std::thread::ThreadId>>> = Arc::default();
        let threads_by_worker = threads.clone();
        let first = std::sync::atomic::AtomicBool::new(true);
        let notify = queued_notifier(
            Box::new(move |title, message| {
                threads_by_worker
                    .lock()
                    .unwrap()
                    .insert(std::thread::current().id());
                shown_by_worker
                    .lock()
                    .unwrap()
                    .push((title.to_string(), message.to_string()));
                // La première reste « en cours d'affichage » tant que la rafale n'est pas finie.
                if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    started_tx.send(()).unwrap();
                    gate_rx.lock().unwrap().recv().unwrap();
                }
            }),
            8,
        );
        notify("Règle 0", "texte");
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for i in 1..60 {
            notify(&format!("Règle {i}"), "texte");
        }
        gate_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let summary_seen = |shown: &Vec<(String, String)>| {
            shown
                .last()
                .is_some_and(|(title, _)| title == "Matter Home")
        };
        while !summary_seen(&shown.lock().unwrap()) {
            assert!(std::time::Instant::now() < deadline, "pas de résumé");
            std::thread::sleep(Duration::from_millis(10));
        }
        let shown = shown.lock().unwrap();
        // 1 en cours + 8 en file + 1 résumé : 60 notifications, 10 affichages au plus.
        assert_eq!(shown.len(), 1 + 8 + 1, "{shown:?}");
        assert!(shown[0].0 == "Règle 0" && shown[8].0 == "Règle 8");
        assert!(
            shown[9]
                .1
                .starts_with("51 autres notifications non affichées"),
            "{:?}",
            shown[9]
        );
        assert_eq!(threads.lock().unwrap().len(), 1, "un seul fil d'affichage");
    }

    #[test]
    fn the_summary_agrees_in_number() {
        assert!(overflow_summary(1)
            .1
            .starts_with("1 autre notification non affichée"));
        assert!(overflow_summary(2)
            .1
            .starts_with("2 autres notifications non affichées"));
    }

    #[test]
    fn long_texts_are_clipped_on_char_boundaries() {
        let args = osascript_args(&"é".repeat(1000), &"ü".repeat(5000));
        assert_eq!(args[args.len() - 2].chars().count(), MAX_TITLE);
        assert_eq!(args[args.len() - 1].chars().count(), MAX_MESSAGE);
    }
}
