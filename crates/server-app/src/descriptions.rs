//! La fenêtre « Descriptions » (ticket n° 19) : les textes, sans dessin (le dessin est dans
//! `descriptions_ui.rs`, côté binaire). Tout ce qui se décide ici se teste.
//!
//! La clé d'API est saisie une fois, puis jamais réaffichée : la fenêtre ne sait que « il y en a
//! une » (`Descriptions::has_key`).

use matter_home_server::descriptions::settings::{CameraFilter, Model, Settings, MAX_DAILY_CAP};

/// Ce qui part, et où : à lire avant d'activer.
pub const PRIVACY_NOTE: &str = "Quand c'est activé, la capture de chaque événement choisi (une seule image, jamais la vidéo) part vers l'API d'Anthropic, avec le nom de la caméra et la classe détectée. Rien ne part sans clé ni sans cette case cochée. La clé reste sur ce Mac (fichier en lecture seule pour votre compte) et n'est pas dans les sauvegardes « .matterhome » : saisissez-la de nouveau sur un autre Mac.";

/// Le conseil de dépense, avec la limite côté Anthropic.
pub const SPEND_NOTE: &str = "Fixez aussi une limite de dépense dans la console Anthropic (console.anthropic.com, section Limits) : le plafond quotidien ci-dessous protège contre une rafale, pas contre un oubli.";

pub const ON_OPEN_NOTE: &str = "Décrire dès l'ouverture : une description de la première capture, puis une seconde à la fin du passage (deux fois le coût). Décoché : une seule, à la fin, avec la meilleure capture.";

pub const RULES_NOTE: &str = "Une règle dont le texte contient {description} ou {niveau} attend la description pour partir (donc la fin du passage, sauf avec « dès l'ouverture »).";

/// Une ligne du choix du modèle : « Opus 5.5 · ≈ 0,9 centime par événement ».
pub fn model_line(model: Model) -> String {
    format!("{} · {} par événement", model.label(), model.cost())
}

pub fn filter_label(filter: &CameraFilter) -> &'static str {
    match filter {
        CameraFilter::All => "Toutes les caméras",
        CameraFilter::Exterior => "Les caméras extérieures",
        CameraFilter::List(_) => "Ces caméras seulement",
    }
}

/// Le plafond quotidien d'un brouillon de saisie (texte) ; `None` si ce n'est pas un nombre.
pub fn parse_cap(text: &str) -> Option<u32> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=MAX_DAILY_CAP).contains(n))
}

/// L'état en une ligne : activé ou non, clé présente ou non.
pub fn status_line(settings: &Settings, has_key: bool) -> (bool, String) {
    match (settings.enabled, has_key) {
        (true, true) => (true, format!("Activées · {}", settings.model.label())),
        (true, false) => (
            false,
            "Activées, mais sans clé d'API : rien n'est envoyé.".to_string(),
        ),
        (false, _) => (false, "Désactivées : rien n'est envoyé.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_show_their_cost() {
        assert_eq!(
            model_line(Model::Haiku),
            "Haiku 5.5 · ≈ 0,02 centime par événement"
        );
        assert!(model_line(Model::Sonnet).contains("0,45"));
        assert!(model_line(Model::Opus).contains("0,9 centime"));
    }

    #[test]
    fn the_notes_say_what_matters() {
        assert!(PRIVACY_NOTE.contains("une seule image") && PRIVACY_NOTE.contains("sauvegardes"));
        assert!(SPEND_NOTE.contains("console Anthropic"));
        assert!(RULES_NOTE.contains("{description}") && RULES_NOTE.contains("{niveau}"));
    }

    #[test]
    fn the_cap_is_a_reasonable_number() {
        assert_eq!(parse_cap(" 200 "), Some(200));
        assert_eq!(parse_cap("0"), None);
        assert_eq!(parse_cap("-3"), None);
        assert_eq!(parse_cap("beaucoup"), None);
        assert_eq!(parse_cap("1000000"), None);
    }

    #[test]
    fn the_status_line_never_claims_what_is_not_true() {
        let mut s = Settings::default();
        assert!(!status_line(&s, true).0);
        s.enabled = true;
        assert!(!status_line(&s, false).0, "sans clé, rien ne part");
        assert_eq!(status_line(&s, true), (true, "Activées · Opus 5.5".into()));
        assert_eq!(
            filter_label(&CameraFilter::Exterior),
            "Les caméras extérieures"
        );
    }
}
