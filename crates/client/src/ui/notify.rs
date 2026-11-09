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

/// Affiche la notification, dans un fil à part (le fil de l'interface n'attend rien).
pub fn system_notifier() -> Notifier {
    Box::new(|title, message| {
        if !cfg!(target_os = "macos") {
            return;
        }
        let args = osascript_args(title, message);
        let spawned = std::thread::Builder::new()
            .name("matter-notify".into())
            .spawn(
                move || match std::process::Command::new("osascript").args(&args).output() {
                    Ok(out) if !out.status.success() => log::warn!(
                        "osascript : {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    Ok(_) => {}
                    Err(e) => log::warn!("osascript introuvable : {e}"),
                },
            );
        if let Err(e) = spawned {
            log::warn!("notification impossible : {e}");
        }
    })
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

    #[test]
    fn long_texts_are_clipped_on_char_boundaries() {
        let args = osascript_args(&"é".repeat(1000), &"ü".repeat(5000));
        assert_eq!(args[args.len() - 2].chars().count(), MAX_TITLE);
        assert_eq!(args[args.len() - 1].chars().count(), MAX_MESSAGE);
    }
}
