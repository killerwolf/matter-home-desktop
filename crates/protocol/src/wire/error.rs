//! Codes d'erreur stables et corps d'erreur.

use serde::{Deserialize, Serialize};

/// Code d'erreur du protocole. Les noms (`snake_case` dans le JSON) ne changent plus une
/// fois publiés ; le client choisit sa propre phrase selon le code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Jeton absent, invalide, expiré ou révoqué.
    Unauthorized,
    /// Identifiant ou mot de passe incorrect.
    InvalidCredentials,
    /// Trop de tentatives de connexion : réessayer après `retry_after_s`.
    TooManyAttempts,
    /// Le client et le serveur ne parlent pas la même version du protocole.
    VersionIncompatible,
    /// Requête mal formée ou valeur refusée.
    BadRequest,
    /// Le plan envoyé n'a aucun étage (`floors: []`) : un plan en a toujours au moins un.
    PlanWithoutFloor,
    /// Appareil, règle ou ressource inconnu.
    NotFound,
    /// Corps de requête trop gros.
    PayloadTooLarge,
    /// Cette route n'existe pas avec cette méthode HTTP (l'en-tête `Allow` donne les bonnes).
    MethodNotAllowed,
    /// Le corps de la requête n'est pas arrivé à temps (client trop lent).
    RequestTimeout,
    /// Un import est déjà validé et en attente : le serveur va redémarrer pour l'appliquer.
    ImportPending,
    /// Un appairage est déjà en cours.
    PairingBusy,
    /// L'appairage a échoué (le détail donne la cause).
    PairingFailed,
    /// Fichier d'import illisible ou qui n'est pas une sauvegarde Matter Home.
    InvalidImport,
    /// La sauvegarde porte une autre identité Matter (fabric) que celle du serveur : l'importer
    /// ferait oublier les appareils appairés. Il faut `replace_identity=1` explicite.
    IdentityMismatch,
    /// Rien à exporter pour l'instant (aucun appareil appairé).
    NoData,
    /// Appareil injoignable.
    DeviceUnreachable,
    /// Le serveur ne peut pas traiter la demande pour l'instant (arrêt en cours).
    Unavailable,
    /// Erreur interne du serveur.
    Internal,
}

impl ErrorCode {
    /// Statut HTTP associé.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::Unauthorized | ErrorCode::InvalidCredentials => 401,
            ErrorCode::TooManyAttempts => 429,
            ErrorCode::VersionIncompatible => 426,
            ErrorCode::BadRequest | ErrorCode::PlanWithoutFloor | ErrorCode::InvalidImport => 400,
            ErrorCode::NotFound | ErrorCode::NoData => 404,
            ErrorCode::PayloadTooLarge => 413,
            ErrorCode::MethodNotAllowed => 405,
            ErrorCode::RequestTimeout => 408,
            ErrorCode::PairingBusy | ErrorCode::ImportPending | ErrorCode::IdentityMismatch => 409,
            ErrorCode::PairingFailed | ErrorCode::DeviceUnreachable => 502,
            ErrorCode::Unavailable => 503,
            ErrorCode::Internal => 500,
        }
    }
}

/// Erreur renvoyée par l'API : un code, et en plus un texte humain facultatif (français,
/// jamais de chemin local) et, pour `too_many_attempts`, le délai avant le prochain essai.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            detail: None,
            retry_after_s: None,
        }
    }

    pub fn with_detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            detail: Some(detail.into()),
            ..Self::new(code)
        }
    }
}

/// Corps JSON d'une réponse d'erreur : `{"error": {"code": "...", ...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ApiError,
}
