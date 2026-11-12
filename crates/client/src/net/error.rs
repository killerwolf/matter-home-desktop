//! Erreurs de la couche réseau, et leur traduction en français.
//!
//! Le serveur signale tout échec par un code stable (`ErrorCode`) : le client choisit lui-même
//! ses phrases. Le `detail` du serveur, texte humain facultatif, est ajouté tel quel.

use protocol::wire::ErrorCode;

#[derive(Debug, Clone, PartialEq)]
pub enum NetError {
    /// Adresse saisie inutilisable.
    BadAddress(String),
    /// Le certificat présenté n'est pas celui qui a été épinglé. Refus, sans contournement.
    CertificateChanged { expected: String, found: String },
    /// Adresse `*.ts.net` (port 443) : le certificat public présenté par Tailscale n'est pas
    /// valable (autorité inconnue, nom, dates). Refus, sans contournement.
    PublicCertificateRejected(String),
    /// Serveur introuvable, connexion refusée ou coupée, handshake TLS raté…
    Unreachable(String),
    /// Aucune réponse à temps.
    Timeout,
    /// Le serveur a répondu par une erreur.
    Api {
        status: u16,
        code: Option<ErrorCode>,
        detail: Option<String>,
        retry_after_s: Option<u64>,
    },
    /// Réponse que ce client ne comprend pas.
    Protocol(String),
    /// Problème sur ce Mac (fichier de connexion, fichier choisi).
    Local(String),
}

impl NetError {
    pub fn api_code(&self) -> Option<ErrorCode> {
        match self {
            NetError::Api { code, .. } => *code,
            _ => None,
        }
    }

    /// Le jeton n'est plus accepté (absent, expiré, révoqué).
    pub fn is_unauthorized(&self) -> bool {
        self.api_code() == Some(ErrorCode::Unauthorized)
    }

    /// Une phrase pour l'utilisateur.
    pub fn message(&self) -> String {
        match self {
            NetError::BadAddress(why) => format!("Adresse du serveur invalide : {why}."),
            NetError::CertificateChanged { .. } => {
                "Le certificat du serveur n'est pas celui que vous aviez approuvé.".into()
            }
            NetError::PublicCertificateRejected(_) => "Le certificat HTTPS de cette adresse \
                Tailscale n'est pas reconnu : vérifiez que « tailscale serve » est actif sur le \
                serveur et que l'HTTPS est activé pour votre tailnet, ou utilisez l'adresse \
                directe du serveur (port 8443)."
                .into(),
            NetError::Unreachable(why) => format!("Serveur injoignable : {why}"),
            NetError::Timeout => "Le serveur n'a pas répondu à temps.".into(),
            NetError::Api {
                status,
                code: Some(code),
                detail,
                retry_after_s,
            } => api_message(*code, detail.as_deref(), *retry_after_s, *status),
            NetError::Api {
                status, code: None, ..
            } => format!("Réponse inattendue du serveur (HTTP {status})."),
            NetError::Protocol(why) => format!("Réponse incompréhensible du serveur : {why}"),
            NetError::Local(why) => why.clone(),
        }
    }
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for NetError {}

/// Phrase française d'un code d'erreur du protocole. La correspondance est exhaustive : un
/// nouveau code oblige à écrire sa phrase.
pub fn api_message(
    code: ErrorCode,
    detail: Option<&str>,
    retry_after_s: Option<u64>,
    _status: u16,
) -> String {
    let with_detail = |base: &str| match detail.filter(|d| !d.trim().is_empty()) {
        Some(d) => format!("{base} ({})", d.trim().trim_end_matches('.')),
        None => base.to_string(),
    };
    match code {
        ErrorCode::Unauthorized => "Votre session n'est plus valable : reconnectez-vous.".into(),
        ErrorCode::InvalidCredentials => "Identifiant ou mot de passe incorrect.".into(),
        ErrorCode::TooManyAttempts => match retry_after_s {
            Some(s) => format!("Trop de tentatives de connexion. Réessayez dans {s} s."),
            None => "Trop de tentatives de connexion. Patientez un instant.".into(),
        },
        ErrorCode::VersionIncompatible => {
            "Ce client et le serveur ne parlent pas la même version du protocole : mettez à jour \
             l'application ou le serveur."
                .into()
        }
        ErrorCode::BadRequest => with_detail("Le serveur a refusé la demande"),
        ErrorCode::PlanWithoutFloor => {
            "Le plan doit garder au moins un étage : il n'a pas été enregistré.".into()
        }
        ErrorCode::NotFound => {
            "Introuvable sur le serveur (appareil ou règle peut-être supprimé).".into()
        }
        ErrorCode::PayloadTooLarge => "Le fichier est trop volumineux pour le serveur.".into(),
        ErrorCode::MethodNotAllowed => {
            "Le serveur ne connaît pas cette opération (versions différentes ?).".into()
        }
        ErrorCode::RequestTimeout => {
            "Le serveur n'a pas reçu la demande à temps. Réessayez.".into()
        }
        ErrorCode::ImportPending => {
            "Un import est déjà en attente : le serveur va redémarrer pour l'appliquer.".into()
        }
        ErrorCode::PairingBusy => "Un appairage est déjà en cours.".into(),
        ErrorCode::PairingFailed => with_detail("L'appairage a échoué"),
        ErrorCode::InvalidImport => {
            with_detail("Ce fichier n'est pas une sauvegarde Matter Home valable")
        }
        ErrorCode::IdentityMismatch => {
            "Cette sauvegarde porte une autre identité Matter que le serveur : l'importer ferait \
             oublier les appareils appairés. Rien n'a été modifié."
                .into()
        }
        ErrorCode::NoData => {
            "Rien à exporter pour l'instant : aucun appareil n'est appairé.".into()
        }
        ErrorCode::DeviceUnreachable => with_detail("L'appareil est injoignable"),
        ErrorCode::Unavailable => with_detail(
            "Le serveur ne peut pas traiter la demande pour l'instant (arrêt en cours ?)",
        ),
        ErrorCode::Internal => "Erreur interne du serveur.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_french_sentence_without_the_code_name() {
        let all = [
            ErrorCode::Unauthorized,
            ErrorCode::InvalidCredentials,
            ErrorCode::TooManyAttempts,
            ErrorCode::VersionIncompatible,
            ErrorCode::BadRequest,
            ErrorCode::PlanWithoutFloor,
            ErrorCode::NotFound,
            ErrorCode::PayloadTooLarge,
            ErrorCode::MethodNotAllowed,
            ErrorCode::RequestTimeout,
            ErrorCode::ImportPending,
            ErrorCode::PairingBusy,
            ErrorCode::PairingFailed,
            ErrorCode::InvalidImport,
            ErrorCode::IdentityMismatch,
            ErrorCode::NoData,
            ErrorCode::DeviceUnreachable,
            ErrorCode::Unavailable,
            ErrorCode::Internal,
        ];
        for code in all {
            let text = api_message(code, None, Some(30), 400);
            assert!(text.len() > 12, "{text}");
            assert!(!text.contains('_'), "code brut dans la phrase : {text}");
        }
        assert_eq!(
            api_message(ErrorCode::TooManyAttempts, None, Some(30), 429),
            "Trop de tentatives de connexion. Réessayez dans 30 s."
        );
        assert_eq!(
            api_message(ErrorCode::PairingFailed, Some("code invalide."), None, 502),
            "L'appairage a échoué (code invalide)"
        );
    }

    #[test]
    fn unauthorized_is_recognised() {
        let e = NetError::Api {
            status: 401,
            code: Some(ErrorCode::Unauthorized),
            detail: None,
            retry_after_s: None,
        };
        assert!(e.is_unauthorized());
        assert!(!NetError::Timeout.is_unauthorized());
    }
}
