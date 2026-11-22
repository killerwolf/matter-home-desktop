//! Les comptes du foyer (Réglages › Comptes) : tout ce qui se décide sans interface, pour que ce
//! soit testable. Le dessin est dans `accounts_ui.rs`.
//!
//! Les comptes vivent dans `serveur/comptes.json` (voir `ostal_server::security::account`). Le
//! serveur qui tourne relit ce fichier dès qu'il change : créer, désactiver ou changer le mot de
//! passe d'un compte prend effet tout de suite, sans redémarrer, et coupe les sessions de ce
//! compte seulement.

use std::path::{Path, PathBuf};

use ostal_server::persistence::SERVER_DIR;
use ostal_server::security::account::{self, User};

/// Le message quand le fichier des comptes ne peut pas être lu (le détail va au journal).
pub const UNREADABLE: &str = "Les comptes n'ont pas pu être lus.";

/// Le dossier des comptes dans le dossier de données.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SERVER_DIR)
}

/// Les comptes, dans l'ordre de création.
pub fn list(data_dir: &Path) -> Result<Vec<User>, String> {
    account::list(&dir(data_dir)).map_err(|e| {
        log::warn!("comptes illisibles : {e:#}");
        UNREADABLE.to_string()
    })
}

/// La première lettre du nom, en majuscule : l'initiale de la pastille.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Les deux mots de passe d'un formulaire (création ou changement) : règle du serveur d'abord,
/// puis l'égalité.
pub fn check_passwords(password: &str, confirm: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Choisissez un mot de passe.".into());
    }
    account::check_password(password).map_err(|e| {
        // La règle du serveur parle de « mot de passe trop court (10 caractères au minimum) ».
        let text = format!("{e:#}");
        let mut chars = text.chars();
        let first = chars.next().map(|c| c.to_uppercase().to_string());
        format!("{}{}.", first.unwrap_or_default(), chars.as_str())
    })?;
    if password != confirm {
        return Err("Les deux mots de passe sont différents.".into());
    }
    Ok(())
}

/// Le formulaire « Ajouter un compte ».
#[derive(Default)]
pub struct NewAccount {
    pub display_name: String,
    pub username: String,
    pub password: String,
    pub confirm: String,
}

impl NewAccount {
    /// `Err` : la phrase qui dit ce qui manque (jamais le détail technique).
    pub fn check(&self) -> Result<(), String> {
        if self.display_name.trim().is_empty() {
            return Err("Écrivez le prénom qui s'affichera dans la maison.".into());
        }
        if self.username.trim().is_empty() {
            return Err("Choisissez un identifiant pour continuer.".into());
        }
        account::check_display_name(self.display_name.trim())
            .map_err(|_| "Le nom affiché fait 64 caractères au plus, sans retour à la ligne.")?;
        account::check_username(self.username.trim())
            .map_err(|_| "L'identifiant fait 64 caractères au plus, sans retour à la ligne.")?;
        check_passwords(&self.password, &self.confirm)
    }
}

/// Crée le compte (calcul du mot de passe : à lancer hors du fil principal).
pub fn add(data_dir: &Path, form: &NewAccount) -> Result<User, String> {
    form.check()?;
    account::add(
        &dir(data_dir),
        form.username.trim(),
        Some(form.display_name.trim()),
        &form.password,
    )
    .map_err(|e| format!("{e:#}"))
}

/// Définit un nouveau mot de passe : les sessions de ce compte, et d'elles seules, sont coupées.
pub fn change_password(data_dir: &Path, user_id: &str, password: &str) -> Result<User, String> {
    account::set_password(&dir(data_dir), user_id, password).map_err(|e| format!("{e:#}"))
}

/// Désactive (sessions coupées) ou réactive un compte. Le dernier compte actif ne se désactive pas.
pub fn set_active(data_dir: &Path, user_id: &str, active: bool) -> Result<User, String> {
    account::set_active(&dir(data_dir), user_id, active).map_err(|e| format!("{e:#}"))
}

/// Ce compte est-il le seul à pouvoir encore se connecter ?
pub fn is_last_active(users: &[User], id: &str) -> bool {
    users.iter().filter(|u| u.active).count() <= 1 && users.iter().any(|u| u.id == id && u.active)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "un-mot-de-passe-solide";

    fn temp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ostal-app-comptes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(SERVER_DIR)).unwrap();
        d
    }

    fn form(name: &str, user: &str, pw: &str, confirm: &str) -> NewAccount {
        NewAccount {
            display_name: name.into(),
            username: user.into(),
            password: pw.into(),
            confirm: confirm.into(),
        }
    }

    #[test]
    fn the_form_says_what_is_missing_in_plain_words() {
        assert!(form("", "sam", PW, PW)
            .check()
            .unwrap_err()
            .contains("prénom"));
        assert!(form("Sam", "", PW, PW)
            .check()
            .unwrap_err()
            .contains("identifiant"));
        assert_eq!(
            form("Sam", "sam", "", "").check().unwrap_err(),
            "Choisissez un mot de passe."
        );
        let short = form("Sam", "sam", "court", "court").check().unwrap_err();
        assert!(
            short.starts_with("Mot de passe trop court") && short.contains("10"),
            "{short}"
        );
        assert_eq!(
            form("Sam", "sam", PW, "autre-chose-12")
                .check()
                .unwrap_err(),
            "Les deux mots de passe sont différents."
        );
        assert!(form("Sam", "sam", PW, PW).check().is_ok());
    }

    #[test]
    fn add_change_password_and_disable_go_through_the_server_files() {
        let data = temp("flow");
        account::create(&dir(&data), "hamdi", PW, false).unwrap();
        let sam = add(&data, &form("Sam", " sam ", PW, PW)).unwrap();
        assert_eq!(
            (sam.username.as_str(), sam.display_name.as_str()),
            ("sam", "Sam")
        );
        assert!(add(&data, &form("Sam bis", "SAM", PW, PW))
            .unwrap_err()
            .contains("déjà pris"));

        let users = list(&data).unwrap();
        assert_eq!(users.len(), 2);
        assert!(!is_last_active(&users, &sam.id));

        let before = sam.session_epoch;
        let changed = change_password(&data, &sam.id, "un-nouveau-mot-de-passe").unwrap();
        assert!(changed.session_epoch > before, "ses sessions sont coupées");
        let hamdi = users.iter().find(|u| u.username == "hamdi").unwrap();
        assert_eq!(
            list(&data)
                .unwrap()
                .iter()
                .find(|u| u.id == hamdi.id)
                .unwrap()
                .session_epoch,
            hamdi.session_epoch,
            "celles des autres comptes ne bougent pas"
        );

        assert!(!set_active(&data, &sam.id, false).unwrap().active);
        let users = list(&data).unwrap();
        assert!(is_last_active(&users, &hamdi.id));
        let refused = set_active(&data, &hamdi.id, false).unwrap_err();
        assert!(refused.contains("dernier compte actif"), "{refused}");
        assert!(set_active(&data, &sam.id, true).unwrap().active);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn initials_skip_symbols_and_never_panic() {
        assert_eq!(initial("sam"), "S");
        assert_eq!(initial("élodie"), "É");
        assert_eq!(initial("  -léa"), "L");
        assert_eq!(initial(""), "?");
    }

    #[test]
    fn an_unreadable_file_gives_one_plain_sentence() {
        let data = temp("bad");
        std::fs::write(dir(&data).join("comptes.json"), "pas du json").unwrap();
        assert_eq!(list(&data).unwrap_err(), UNREADABLE);
        let _ = std::fs::remove_dir_all(data);
    }
}
