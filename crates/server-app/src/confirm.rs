//! Les confirmations avant un geste qu'on ne peut pas défaire (voir `docs/ux-serveur.md`).
//!
//! Une alerte native (rfd), sur le modèle de « Quitter Ostal Server ? » : le texte dit ce qui va
//! cesser de marcher, le bouton destructif porte le verbe (« Supprimer », « Révoquer »…) et
//! **Annuler est le bouton par défaut**. Sur macOS le premier bouton est celui qui répond à
//! Entrée et se place le plus à droite : Annuler est donc passé en premier, et seule la réponse
//! « bouton destructif » confirme. Fermer l'alerte, Échap ou toute autre réponse annulent.

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

/// Le bouton qui annule.
pub const CANCEL: &str = "Annuler";

/// Les gestes destructeurs de la fenêtre qui se confirment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// Supprimer le code secret (jeton de mode).
    DeleteModeToken,
    /// Révoquer le jeton de lecture (widget de l'iPhone).
    RevokeReadToken,
    /// Révoquer un appareil abonné aux notifications.
    RevokeSubscription { label: String },
    /// Renoncer à un import de sauvegarde interrompu.
    AbandonImport,
    /// Supprimer une caméra.
    DeleteCamera { name: String },
    /// Retirer la clé d'API des descriptions par IA.
    RemoveApiKey,
}

/// Ce que l'alerte affiche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub body: String,
    /// Le verbe du bouton destructif.
    pub action: &'static str,
}

impl Confirmation {
    /// Le titre, le texte et le verbe du bouton (textes de `Serveur-Confirmer.dc.html`).
    pub fn dialog(&self) -> Dialog {
        let (title, body, action): (String, String, &'static str) = match self {
            Confirmation::DeleteModeToken => (
                "Supprimer le code ?".into(),
                "Les raccourcis et les automatisations qui changent le mode cesseront de marcher. Le code ne se réaffiche pas : il faudra en créer un nouveau.".into(),
                "Supprimer",
            ),
            Confirmation::RevokeReadToken => (
                "Révoquer le jeton de lecture ?".into(),
                "Le widget de l'iPhone ne pourra plus lire l'état de la maison.".into(),
                "Révoquer",
            ),
            Confirmation::RevokeSubscription { .. } => (
                "Révoquer cet appareil ?".into(),
                "Il ne recevra plus de notifications de la maison.".into(),
                "Révoquer",
            ),
            Confirmation::AbandonImport => (
                "Renoncer à l'import en attente ?".into(),
                "La sauvegarde en attente est retirée. Vos données ne changent pas.".into(),
                "Renoncer",
            ),
            Confirmation::DeleteCamera { name } => (
                format!("Supprimer la caméra {name} ?"),
                "Elle disparaît aussi des clients.".into(),
                "Supprimer",
            ),
            Confirmation::RemoveApiKey => (
                "Retirer la clé ?".into(),
                "Les événements ne seront plus décrits.".into(),
                "Retirer",
            ),
        };
        Dialog {
            title,
            body,
            action,
        }
    }
}

/// `true` seulement si la réponse est le bouton destructif.
pub fn is_confirmed(result: &MessageDialogResult, action: &str) -> bool {
    matches!(result, MessageDialogResult::Custom(clicked) if clicked == action)
}

/// Montre l'alerte native et rend la réponse brute.
fn show_native(dialog: &Dialog) -> MessageDialogResult {
    MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title(&dialog.title)
        .set_description(&dialog.body)
        // Annuler d'abord : c'est lui le bouton par défaut.
        .set_buttons(MessageButtons::OkCancelCustom(
            CANCEL.into(),
            dialog.action.into(),
        ))
        .show()
}

/// Pose la question avec `show` (l'alerte réelle, ou un double dans les tests) ; `true` si on
/// confirme.
pub fn ask_with(what: &Confirmation, show: impl FnOnce(&Dialog) -> MessageDialogResult) -> bool {
    let dialog = what.dialog();
    is_confirmed(&show(&dialog), dialog.action)
}

/// Pose la question dans une alerte native. Bloque jusqu'à la réponse (comme le sélecteur de
/// fichier) : `false` si on annule.
pub fn ask(what: &Confirmation) -> bool {
    ask_with(what, show_native)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<Confirmation> {
        vec![
            Confirmation::DeleteModeToken,
            Confirmation::RevokeReadToken,
            Confirmation::RevokeSubscription {
                label: "iPad du salon".into(),
            },
            Confirmation::AbandonImport,
            Confirmation::DeleteCamera {
                name: "Entrée".into(),
            },
            Confirmation::RemoveApiKey,
        ]
    }

    #[test]
    fn the_six_destructive_gestures_each_ask_first() {
        for what in all() {
            let mut shown = 0;
            let done = ask_with(&what, |d| {
                shown += 1;
                assert!(!d.title.is_empty() && !d.body.is_empty(), "{what:?}");
                MessageDialogResult::Custom(CANCEL.into())
            });
            assert_eq!(shown, 1, "{what:?} ouvre une confirmation");
            assert!(!done, "Annuler ne confirme pas : {what:?}");
        }
    }

    #[test]
    fn only_the_destructive_button_confirms() {
        for what in all() {
            let action = what.dialog().action;
            assert!(ask_with(&what, |_| MessageDialogResult::Custom(
                action.into()
            )));
            // Annuler, Échap, fermeture : jamais une confirmation.
            for other in [
                MessageDialogResult::Custom(CANCEL.into()),
                MessageDialogResult::Cancel,
                MessageDialogResult::No,
                MessageDialogResult::Ok,
            ] {
                assert!(!ask_with(&what, |_| other.clone()), "{what:?} {other:?}");
            }
        }
    }

    #[test]
    fn texts_say_what_stops_and_keep_the_mockup_wording() {
        let d = Confirmation::DeleteModeToken.dialog();
        assert_eq!(d.title, "Supprimer le code ?");
        assert!(d.body.starts_with("Les raccourcis et les automatisations"));
        assert!(d.body.contains("Le code ne se réaffiche pas"));
        // Un nom de caméra ou d'appareil est cité, jamais une adresse.
        let d = Confirmation::DeleteCamera {
            name: "Entrée".into(),
        }
        .dialog();
        assert_eq!(d.title, "Supprimer la caméra Entrée ?");
        for what in all() {
            let text = format!("{} {}", what.dialog().title, what.dialog().body);
            assert!(!text.contains("rtsp") && !text.contains("http"), "{text}");
        }
    }
}
