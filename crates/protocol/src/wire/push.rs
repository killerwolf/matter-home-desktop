//! Notifications web push de l'app web (ticket n° 17) : les abonnements des appareils (iPhone,
//! iPad…) et le réglage général. Tout passe sous le jeton de session, comme le reste de l'API.
//!
//! | route | corps | réponse |
//! |---|---|---|
//! | `GET /api/push` | | [`PushStatus`] (clé publique VAPID, réglage, abonnements) |
//! | `PUT /api/push` | [`PushSettingsRequest`] | [`PushStatus`] |
//! | `POST /api/push/subscriptions` | [`PushSubscribeRequest`] | [`PushSubscriptionView`] (même `id` pour la même adresse) |
//! | `PUT /api/push/subscriptions/{id}` | [`PushUpdateRequest`] | [`PushSubscriptionView`] |
//! | `DELETE /api/push/subscriptions/{id}` | | `202` ; `404` s'il n'existe pas |
//! | `POST /api/push/subscriptions/{id}/test` | | `202` : une notification d'essai part vers cet appareil |
//!
//! L'adresse d'abonnement (`endpoint`) n'est jamais renvoyée : c'est elle qui permet d'écrire à
//! l'appareil. Seul le nom du service (« Apple »…) l'est.
//!
//! Routes et types **ajoutés** : un ancien client les ignore, la version du protocole ne change pas.

use serde::{Deserialize, Serialize};

pub const PUSH_PATH: &str = "/api/push";
pub const PUSH_SUBSCRIPTIONS_PATH: &str = "/api/push/subscriptions";
pub const PUSH_SUBSCRIPTION_PATH: &str = "/api/push/subscriptions/{id}";
pub const PUSH_TEST_PATH: &str = "/api/push/subscriptions/{id}/test";

/// Abonnements gardés au plus.
pub const PUSH_MAX_SUBSCRIPTIONS: usize = 20;
/// Longueur maximale du nom d'un appareil abonné (caractères).
pub const PUSH_LABEL_MAX: usize = 60;

/// Ce qu'un appareil veut recevoir. Tout est coché par défaut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PushCategories {
    /// Les actions « Notifier » des règles d'automatisation.
    pub rules: bool,
    /// Les passages détectés sur les caméras en Surveillance (un par passage).
    pub detections: bool,
    /// Pile faible (moins de 15 %).
    pub battery: bool,
    /// Appareil injoignable depuis plus de 10 minutes.
    pub unreachable: bool,
    /// Une demande d'un agent d'IA à valider (serveur MCP intégré). Part vers les appareils de la
    /// personne à qui appartient l'agent, jamais vers ceux des autres.
    pub agent_requests: bool,
}

impl Default for PushCategories {
    fn default() -> Self {
        Self {
            rules: true,
            detections: true,
            battery: true,
            unreachable: true,
            agent_requests: true,
        }
    }
}

/// Un appareil abonné, tel que les clients le voient (sans l'adresse d'abonnement ni les clés).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscriptionView {
    /// Identifiant opaque (16 caractères hexadécimaux).
    pub id: String,
    /// Nom donné par l'app web (« iPhone »…).
    pub label: String,
    /// L'en-tête `User-Agent` de l'abonnement, raccourci.
    #[serde(default)]
    pub user_agent: String,
    /// Le service de push de l'appareil : « Apple », « Google », « Mozilla » ou « Microsoft ».
    pub service: String,
    pub created_at: f64,
    /// Dernier envoi accepté par le service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sent_at: Option<f64>,
    /// Dernier échec d'envoi (texte court, sans adresse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub categories: PushCategories,
}

/// `GET /api/push`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushStatus {
    /// Réglage général : coupé, rien ne part (les abonnements sont gardés).
    pub enabled: bool,
    /// Clé publique VAPID du serveur (point P-256 non compressé, base64url sans `=`), à donner à
    /// `pushManager.subscribe({applicationServerKey})`.
    pub public_key: String,
    pub subscriptions: Vec<PushSubscriptionView>,
}

/// `PUT /api/push`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSettingsRequest {
    pub enabled: bool,
}

/// Les clés de l'abonnement, telles que `PushSubscription.toJSON()` les donne (base64url).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

/// `POST /api/push/subscriptions` : le résultat de `PushSubscription.toJSON()`, plus un nom.
/// La même adresse déjà connue met l'abonnement à jour (mêmes `id` et choix, sauf `categories`
/// donné).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscribeRequest {
    pub endpoint: String,
    pub keys: PushKeys,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<PushCategories>,
}

/// `PUT /api/push/subscriptions/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<PushCategories>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Un identifiant d'abonnement bien formé (16 caractères hexadécimaux minuscules).
pub fn is_push_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Ce que reçoit le service worker de l'app web (chiffré de bout en bout, RFC 8291).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    /// La page de l'app web à ouvrir au toucher (`/app/…`, même origine).
    pub url: String,
    /// Une notification de même `tag` remplace la précédente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_default_to_everything_and_missing_fields_stay_on() {
        let c: PushCategories = serde_json::from_str(r#"{"battery": false}"#).unwrap();
        assert!(c.rules && c.detections && c.unreachable && !c.battery);
        assert_eq!(
            PushCategories::default(),
            serde_json::from_str("{}").unwrap()
        );
    }

    #[test]
    fn subscription_ids_are_checked() {
        assert!(is_push_id("0123456789abcdef"));
        assert!(!is_push_id("0123456789ABCDEF"));
        assert!(!is_push_id("../../etc/passwd"));
        assert!(!is_push_id(""));
    }

    #[test]
    fn the_subscribe_request_reads_what_the_browser_gives() {
        let r: PushSubscribeRequest = serde_json::from_str(
            r#"{"endpoint":"https://web.push.apple.com/x","expirationTime":null,
                "keys":{"p256dh":"BAA","auth":"AAA"},"label":"iPhone"}"#,
        )
        .unwrap();
        assert_eq!(r.label, "iPhone");
        assert!(r.categories.is_none());
    }
}
