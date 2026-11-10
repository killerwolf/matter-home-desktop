//! Ce que toutes les vues de caméras (plan, page, lecteur) disent et colorent de la même façon :
//! la phrase de chaque motif d'injoignabilité et les couleurs des états.

use eframe::egui::Color32;
use protocol::wire::CameraProblem;

pub const ONLINE: Color32 = Color32::from_rgb(40, 160, 70);
pub const OFFLINE: Color32 = Color32::from_rgb(210, 60, 50);
pub const NEUTRAL: Color32 = Color32::from_rgb(140, 140, 140);

/// Pourquoi une caméra est injoignable, en un morceau de phrase (sans adresse ni détail technique).
pub fn problem_text(problem: CameraProblem) -> &'static str {
    match problem {
        CameraProblem::Unreachable => "ne répond pas (éteinte, hors réseau ou RTSP désactivé)",
        CameraProblem::Unauthorized => "identifiant ou mot de passe refusé",
        CameraProblem::NotFound => "adresse du flux introuvable",
        CameraProblem::Protocol => "réponse inattendue (pas une caméra RTSP ?)",
        CameraProblem::Failed => "échec de la connexion",
    }
}
