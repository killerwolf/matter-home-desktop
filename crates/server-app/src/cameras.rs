//! La fenêtre « Caméras » : le formulaire et les textes, sans dessin (le dessin est dans
//! `cameras_ui.rs`, côté binaire). Tout ce qui se décide ici se teste.
//!
//! Le mot de passe d'une caméra ne quitte jamais le serveur : le formulaire ne le reçoit pas
//! (il ne sait que « il y en a un »), et saisir un nouveau mot de passe est le seul moyen d'en
//! changer. Une modification qui laisse le champ vide le **garde** ; la case « Supprimer le mot
//! de passe » l'efface.

use matter_home_server::cameras::{
    describe_stream, split_credentials, CameraAdmin, CameraDraft, StreamKind, StreamTest,
};
use protocol::wire::CameraStatus;

/// Ce que la fenêtre dit des sauvegardes (demandé par le ticket #4 : « à dire dans l'interface »).
pub const BACKUP_NOTE: &str = "Les adresses et les mots de passe des caméras restent sur ce Mac : les clients ne reçoivent que le nom et l'état. Mais ils sont inclus, en clair, dans les sauvegardes « .matterhome » (comme les clés de la maison) : gardez ces fichiers comme des secrets.";

/// Aide pour une caméra Aqara G100 (non vérifiée sur l'appareil).
pub const AQARA_HINT: &str = "Aqara G100 (à vérifier sur votre caméra) : dans l'application Aqara Home, ouvrez la caméra, Réglages de l'appareil, RTSP, puis activez-le. L'application donne l'adresse, le port (554), l'identifiant et le mot de passe. Flux principal : rtsp://adresse:554/live/ch00_0 (peut être instable) ; flux réduit : …/live/ch00_1 (720p, stable, à préférer pour la mosaïque).";

/// Couleur d'une ligne d'état.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Neutral,
}

/// L'état d'une caméra, en une ligne.
pub fn status_line(camera: &CameraAdmin) -> (Tone, String) {
    match &camera.status {
        CameraStatus::Unknown => (Tone::Neutral, "Pas encore vérifiée".into()),
        CameraStatus::Online => {
            let stream = camera.stream.as_ref().map(|info| {
                let which = if camera.sub_url.is_some() {
                    "flux réduit"
                } else {
                    "flux principal"
                };
                format!(" · {} ({which})", describe_stream(info))
            });
            (
                Tone::Good,
                format!("Joignable{}", stream.unwrap_or_default()),
            )
        }
        CameraStatus::Unreachable { detail, .. } => (
            Tone::Bad,
            match detail {
                Some(d) => format!("Injoignable : {d}"),
                None => "Injoignable".into(),
            },
        ),
    }
}

/// Le résultat du bouton « Tester », une ligne par flux.
pub fn test_lines(results: &[StreamTest]) -> Vec<(Tone, String)> {
    results
        .iter()
        .map(|r| {
            (
                if r.outcome.is_ok() {
                    Tone::Good
                } else {
                    Tone::Bad
                },
                r.describe(),
            )
        })
        .collect()
}

/// Le formulaire d'ajout ou de modification.
#[derive(Default)]
pub struct Form {
    /// `Some` : modification de cette caméra ; `None` : ajout.
    pub editing: Option<u64>,
    pub name: String,
    pub url: String,
    pub sub_url: String,
    pub username: String,
    /// Mot de passe saisi (vide : inchangé en modification).
    pub password: String,
    /// La caméra modifiée a un mot de passe enregistré (qu'on ne montre jamais).
    pub had_password: bool,
    /// Effacer le mot de passe enregistré.
    pub clear_password: bool,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn edit(camera: &CameraAdmin) -> Self {
        Self {
            editing: Some(camera.id),
            name: camera.name.clone(),
            url: camera.url.clone(),
            sub_url: camera.sub_url.clone().unwrap_or_default(),
            username: camera.username.clone().unwrap_or_default(),
            password: String::new(),
            had_password: camera.has_password,
            clear_password: false,
        }
    }

    /// La saisie telle que le serveur la comprend.
    pub fn draft(&self) -> CameraDraft {
        let password = if !self.password.is_empty() {
            Some(self.password.clone())
        } else if self.clear_password {
            Some(String::new())
        } else {
            None
        };
        CameraDraft {
            name: self.name.clone(),
            url: self.url.clone(),
            sub_url: self.sub_url.clone(),
            username: self.username.clone(),
            password,
        }
    }

    pub fn title(&self) -> &'static str {
        if self.editing.is_some() {
            "Modifier la caméra"
        } else {
            "Ajouter une caméra"
        }
    }

    /// Tester ou enregistrer demande au moins un nom et une adresse (le serveur contrôle le reste).
    pub fn is_fillable(&self) -> bool {
        !self.name.trim().is_empty() && !self.url.trim().is_empty()
    }

    /// Si l'adresse du flux principal ou du flux réduit vient d'être collée avec
    /// `identifiant:mot-de-passe@`, retire ces identifiants de l'adresse et les range dans leurs
    /// champs (ce qu'on voit à l'écran ne garde pas le mot de passe en clair dans l'adresse). Un
    /// champ déjà rempli n'est pas écrasé. `true` si quelque chose a changé.
    pub fn absorb_credentials(&mut self) -> bool {
        let mut changed = false;
        for field in [0, 1] {
            let text = if field == 0 { &self.url } else { &self.sub_url };
            let Some((clean, (user, password))) = split_credentials(text) else {
                continue;
            };
            if field == 0 {
                self.url = clean;
            } else {
                self.sub_url = clean;
            }
            if self.username.trim().is_empty() {
                self.username = user;
            }
            if self.password.is_empty() {
                if let Some(p) = password {
                    self.password = p;
                }
            }
            changed = true;
        }
        changed
    }

    /// Texte sous le champ du mot de passe.
    pub fn password_hint(&self) -> &'static str {
        if self.editing.is_some() && self.had_password {
            "inchangé (saisissez-en un pour le remplacer)"
        } else {
            "facultatif"
        }
    }
}

/// « Flux principal » ou « Flux réduit », pour les libellés.
pub fn kind_label(kind: StreamKind) -> &'static str {
    kind.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use matter_home_server::cameras::{CameraBook, ProbeError};
    use protocol::wire::{CameraProblem, StreamInfo};

    fn admin(name: &str, sub: bool) -> CameraAdmin {
        let mut book = CameraBook::default();
        book.add(&CameraDraft {
            name: name.into(),
            url: "rtsp://192.0.2.4:554/live/ch00_0".into(),
            sub_url: if sub {
                "rtsp://192.0.2.4:554/live/ch00_1".into()
            } else {
                String::new()
            },
            username: "admin".into(),
            password: Some("secret".into()),
        })
        .unwrap();
        book.admin_list().remove(0)
    }

    #[test]
    fn the_form_keeps_clears_or_replaces_the_password() {
        let camera = admin("Salon", true);
        assert!(camera.has_password);
        let mut form = Form::edit(&camera);
        assert_eq!(form.editing, Some(camera.id));
        assert_eq!(
            form.password_hint(),
            "inchangé (saisissez-en un pour le remplacer)"
        );
        // Champ vide : on garde.
        assert_eq!(form.draft().password, None);
        // Case cochée : on efface.
        form.clear_password = true;
        assert_eq!(form.draft().password, Some(String::new()));
        // Un nouveau mot de passe l'emporte sur la case.
        form.password = "nouveau".into();
        assert_eq!(form.draft().password.as_deref(), Some("nouveau"));
        // Le formulaire ne contient jamais l'ancien mot de passe.
        let fresh = Form::edit(&camera);
        assert!(fresh.password.is_empty());
        let draft = fresh.draft();
        assert_eq!(draft.url, "rtsp://192.0.2.4:554/live/ch00_0");
        assert_eq!(draft.sub_url, "rtsp://192.0.2.4:554/live/ch00_1");
        assert_eq!(draft.username, "admin");
        // Un ajout n'a pas de caméra existante.
        let new = Form::new();
        assert_eq!(
            (new.editing, new.title(), new.password_hint()),
            (None, "Ajouter une caméra", "facultatif")
        );
        assert!(!new.is_fillable());
    }

    #[test]
    fn status_lines_are_in_french_and_say_which_stream() {
        let mut camera = admin("Salon", true);
        assert_eq!(
            status_line(&camera),
            (Tone::Neutral, "Pas encore vérifiée".into())
        );
        camera.status = CameraStatus::Online;
        camera.stream = Some(StreamInfo {
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
        });
        assert_eq!(
            status_line(&camera),
            (
                Tone::Good,
                "Joignable · H.264 · 1280 × 720 (flux réduit)".into()
            )
        );
        camera.sub_url = None;
        assert!(status_line(&camera).1.ends_with("(flux principal)"));
        camera.status = CameraStatus::Unreachable {
            problem: CameraProblem::Unauthorized,
            detail: Some("La caméra refuse l'identifiant ou le mot de passe.".into()),
        };
        assert_eq!(
            status_line(&camera),
            (
                Tone::Bad,
                "Injoignable : La caméra refuse l'identifiant ou le mot de passe.".into()
            )
        );
        // Jamais d'adresse ni de mot de passe dans ces lignes.
        assert!(!status_line(&camera).1.contains("192.0.2.4"));
    }

    #[test]
    fn test_results_are_one_line_per_stream() {
        let ok = StreamTest {
            kind: StreamKind::Main,
            outcome: Ok(StreamInfo {
                codec: "h265".into(),
                width: None,
                height: None,
            }),
        };
        let bad = StreamTest {
            kind: StreamKind::Sub,
            outcome: Err(ProbeError {
                problem: CameraProblem::NotFound,
                message: "La caméra ne connaît pas cette adresse de flux.".into(),
            }),
        };
        let lines = test_lines(&[ok, bad]);
        assert_eq!(lines[0].0, Tone::Good);
        assert!(
            lines[0].1.starts_with("Flux principal : H.265"),
            "{}",
            lines[0].1
        );
        assert_eq!(
            lines[1],
            (
                Tone::Bad,
                "Flux réduit : La caméra ne connaît pas cette adresse de flux.".into()
            )
        );
    }

    #[test]
    fn pasted_credentials_leave_the_address_at_once() {
        let mut form = Form::new();
        form.url = "rtsp://admin:s3cret@192.0.2.4:554/live/ch00_0".into();
        form.sub_url = "rtsp://admin:s3cret@192.0.2.4:554/live/ch00_1".into();
        assert!(form.absorb_credentials());
        assert_eq!(form.url, "rtsp://192.0.2.4:554/live/ch00_0");
        assert_eq!(form.sub_url, "rtsp://192.0.2.4:554/live/ch00_1");
        assert_eq!(
            (form.username.as_str(), form.password.as_str()),
            ("admin", "s3cret")
        );
        // Rien de plus à faire la fois suivante, et un champ rempli n'est pas écrasé.
        assert!(!form.absorb_credentials());
        let mut filled = Form::new();
        filled.username = "moi".into();
        filled.url = "rtsp://autre:x@192.0.2.4/m".into();
        assert!(filled.absorb_credentials());
        assert_eq!(filled.username, "moi");
        assert_eq!(filled.url, "rtsp://192.0.2.4/m");
    }

    #[test]
    fn the_notes_say_what_matters() {
        assert!(BACKUP_NOTE.contains("sauvegardes") && BACKUP_NOTE.contains("mots de passe"));
        assert!(AQARA_HINT.contains("ch00_1") && AQARA_HINT.contains("RTSP"));
        assert!(AQARA_HINT.contains("à vérifier"));
        // Typographie : espace insécable avant « : », « » et « ? ».
        assert!(!BACKUP_NOTE.contains(" :") && BACKUP_NOTE.contains(" :"));
    }
}
