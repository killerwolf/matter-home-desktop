//! Corps des requêtes et des réponses de l'API HTTPS.

use serde::{Deserialize, Serialize};

use super::message::Metric;
use super::text_ids::text_id;
use crate::automation::{CoverCommand, OnOffCommand};
use crate::home::Symbol;
use crate::mode::{HomeMode, ModeSource};

/// `GET /api/version` (sans jeton) : de quoi vérifier la compatibilité avant de se connecter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub protocol_version: u32,
    pub server_version: String,
}

/// `POST /api/login`
#[derive(Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// Version du protocole parlée par le client.
    pub protocol_version: u32,
    /// Le téléphone ou l'ordinateur qui se connecte (protocole 4). Absent chez un client plus
    /// ancien (2.0.0, 2.1.0-beta.1) : sa connexion est alors « inconnue ».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<Installation>,
}

/// Sorte de client d'une connexion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionKind {
    /// L'app web, dans un navigateur.
    Web,
    /// L'app Ostal sur Mac.
    Mac,
    /// Une valeur qu'une version plus récente aurait ajoutée.
    #[serde(other)]
    Other,
}

/// Sorte d'appareil d'une connexion : « iPhone », « Android »…
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceClass {
    Iphone,
    Ipad,
    Android,
    Computer,
    #[default]
    #[serde(other)]
    Other,
}

/// Une connexion telle que le client la déclare à l'ouverture de session. Le type et le nom
/// servent à s'y retrouver (traçabilité), jamais à décider d'un droit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    /// Identifiant tiré au hasard à la première ouverture de l'app, gardé ensuite : se reconnecter
    /// depuis la même app retrouve la même connexion.
    pub id: String,
    pub kind: ConnectionKind,
    #[serde(default)]
    pub device: DeviceClass,
    /// « iPhone de Hamdi », « Mac du salon ».
    pub name: String,
}

/// Où se trouvait le client à son dernier accès.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// Sur le réseau de la maison.
    Local,
    /// Par Tailscale.
    Tailscale,
}

/// Une connexion ouverte, vue de l'API : `GET /api/me/installations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionView {
    /// Pour la couper (`DELETE /api/me/installations/{id}`).
    pub id: String,
    pub name: String,
    pub kind: ConnectionKind,
    #[serde(default)]
    pub device: DeviceClass,
    /// Dernier accès (secondes Unix).
    pub last_seen: f64,
    pub via: Via,
    /// C'est la connexion qui pose la question : on ne la coupe pas d'ici.
    pub current: bool,
    /// Le client n'a pas dit qui il est (app pas encore à jour) : « Connexion inconnue ».
    #[serde(default)]
    pub unknown: bool,
}

/// Réponse de `GET /api/me/installations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsResponse {
    pub connections: Vec<ConnectionView>,
}

// Pas de `Debug` dérivé : le mot de passe ne doit jamais finir dans un journal.
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"…")
            .field("protocol_version", &self.protocol_version)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    /// Jeton de session opaque, à envoyer dans `Authorization: Bearer <jeton>`.
    pub token: String,
    /// Expiration (secondes Unix) si le jeton n'est pas utilisé d'ici là ; chaque
    /// utilisation la repousse, dans la limite de `max_expires_at`.
    pub expires_at: f64,
    pub max_expires_at: f64,
    pub protocol_version: u32,
    pub server_version: String,
    /// Adresses Tailscale **directes** du serveur (`https://100.x.y.z:port`), pour joindre la
    /// maison de loin avec le même certificat. Absent chez un serveur plus ancien.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tailnet_urls: Vec<String>,
}

impl std::fmt::Debug for LoginResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginResponse")
            .field("token", &"…")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// `POST /api/pairing`
#[derive(Clone, Serialize, Deserialize)]
pub struct PairRequest {
    /// Code à 11 chiffres ou `MT:…`.
    pub code: String,
}

impl std::fmt::Debug for PairRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairRequest { code: … }")
    }
}

/// `PUT /api/devices/{id}/customization` : `null` ou absent = revenir à l'automatique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomizeRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<Symbol>,
}

/// `POST /api/devices/{id}/on-off`
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OnOffRequest {
    pub command: OnOffCommand,
}

/// `POST /api/devices/{id}/cover`
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CoverRequest {
    pub command: CoverCommand,
}

/// `PUT /api/mode` : le mode voulu. Accepté avec un jeton de session **ou** avec le jeton de mode
/// (Raccourcis de l'iPhone), qui ne peut rien faire d'autre. `source` (facultatif, ajouté après
/// la 2.0) : qui le demande ; le serveur ne retient que ce que le jeton permet
/// (`ModeSource::claimed`). Un ancien client ne l'envoie pas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeRequest {
    pub mode: HomeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ModeSource>,
}

/// Réponse de `PUT /api/mode` (200). `changed` est faux si c'était déjà le mode actuel : rien n'a
/// été enregistré, diffusé ni déclenché.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeResponse {
    pub mode: HomeMode,
    pub changed: bool,
}

/// Réponse des commandes asynchrones (202) : le serveur les a transmises ; le résultat
/// arrive en événements sur le WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub accepted: bool,
}

/// Paramètres de `GET /api/devices/{id}/history` et `…/history.csv` (dans l'URL).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryQuery {
    /// Début de la fenêtre (secondes Unix). Par défaut : `until` moins une heure.
    #[serde(default)]
    pub since: Option<f64>,
    /// Fin de la fenêtre (secondes Unix). Par défaut : maintenant.
    #[serde(default)]
    pub until: Option<f64>,
    /// Grandeurs voulues, séparées par des virgules (`temperature,humidity`). Par défaut : toutes.
    #[serde(default)]
    pub metrics: Option<String>,
    /// Nombre maximal de mesures (`HISTORY_MAX_LIMIT` au plus). Si la fenêtre en contient
    /// davantage, ce sont les plus récentes qui sont renvoyées (`truncated`).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Réduction par tranches de temps : la fenêtre est découpée en `buckets` tranches (au plus
    /// `HISTORY_MAX_BUCKETS`) et chaque grandeur garde le minimum et le maximum de chaque tranche
    /// (au plus `2 × buckets + 2` points par grandeur). Toute la fenêtre est ainsi couverte, même
    /// pour un capteur très bavard, avant l'application de `limit`. Absent : toutes les mesures.
    /// Un serveur plus ancien ignore ce paramètre et renvoie les mesures brutes.
    #[serde(default)]
    pub buckets: Option<usize>,
}

/// Une mesure de l'historique.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub at: f64,
    pub metric: Metric,
    pub value: f64,
}

/// Réponse de `GET /api/devices/{id}/history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryResponse {
    #[serde(with = "text_id")]
    pub node_id: u64,
    /// Fenêtre réellement appliquée (bornée à la rétention).
    pub since: f64,
    pub until: f64,
    /// `true` si la fenêtre contenait plus de `limit` mesures : seules les plus récentes
    /// sont renvoyées ; relire avec `until` = `at` de la première pour remonter.
    pub truncated: bool,
    /// Ordre chronologique.
    pub points: Vec<HistoryPoint>,
}

/// Contenu d'une sauvegarde, pour l'afficher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSummary {
    /// Date de création, ISO 8601.
    pub created: String,
    pub devices: usize,
    /// Pièces du plan de la maison (tous étages).
    #[serde(default)]
    pub rooms: usize,
    /// Règles d'automatisation.
    #[serde(default)]
    pub rules: usize,
    /// Caméras RTSP. Leurs **mots de passe** sont dans la sauvegarde. Absent d'un serveur de
    /// version 1 (lu comme 0).
    #[serde(default)]
    pub cameras: usize,
    pub history_files: usize,
    pub has_plan: bool,
    pub has_rules: bool,
    /// Noms des règles que l'import désactive parce que l'adresse de leur webhook est
    /// incomplète (brouillons de la v0.2.1) : leur contenu est conservé, elles ne s'exécutent
    /// pas tant qu'elles ne sont pas corrigées puis réactivées.
    #[serde(default)]
    pub disabled_rules: Vec<String>,
}

/// Réponse de `POST /api/import` (202) : la sauvegarde est validée et mise de côté ; le
/// serveur va s'arrêter pour que son superviseur le relance et l'applique. Avec
/// `?dry_run=1`, réponse 200 avec `restarting: false` : la sauvegarde est validée et résumée,
/// rien n'est mis de côté et le serveur ne s'arrête pas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportResponse {
    pub summary: ImportSummary,
    pub restarting: bool,
    /// Les identités Matter en jeu : celle de la sauvegarde et celle du serveur. Toujours
    /// présent côté serveur ; absent d'un serveur plus ancien.
    #[serde(default)]
    pub identity: Option<ImportIdentity>,
}

/// Identité Matter (fabric) d'une sauvegarde ou du serveur. Le numéro de fabric est du texte
/// hexadécimal (`0x…`), comme les autres identifiants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityInfo {
    pub fabric_id: String,
    /// Appareils appairés avec cette identité (nœuds et appareils pontés).
    pub devices: usize,
}

/// Comparaison des identités d'un import. `differs` : la sauvegarde porte une autre fabric que le
/// serveur (ou celle du serveur est illisible) ; l'import réel exige alors `replace_identity=1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportIdentity {
    pub incoming: IdentityInfo,
    /// `None` : le serveur n'a pas (encore) d'identité, rien n'est remplacé.
    pub current: Option<IdentityInfo>,
    pub differs: bool,
}

/// Type de contenu de l'export `.ostal` (JSON). Il contient des clés privées en clair.
pub const EXPORT_CONTENT_TYPE: &str = "application/json";
