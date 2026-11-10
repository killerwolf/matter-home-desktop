//! Caméras RTSP vues par les clients.
//!
//! Le serveur est le **seul** à connaître l'adresse RTSP d'une caméra et son mot de passe
//! (voir `crates/server/src/cameras.rs`) : un client ne reçoit jamais que ce module décrit,
//! c'est-à-dire un identifiant, un nom, un état et ce que la caméra annonce de son flux.
//! Aucun type de ce module n'a de champ pour une adresse ou un secret : ce n'est pas une
//! question de discipline, ils ne peuvent pas fuir par ici.
//!
//! # Où va chaque information (pour les tickets suivants)
//!
//! | Donnée | Où | Ticket |
//! |---|---|---|
//! | identifiant, nom, état, codec, taille | ici ([`CameraView`]) | #4 |
//! | adresse RTSP, identifiants | serveur seulement (`cameras.json`) | #4 |
//! | pièce, position, orientation, ouverture, portée | **le plan** (`home::Item`) : un objet `Camera { camera_id }` | #6 |
//! | transport vidéo (relais du serveur ou accès direct) | à décider | #5 |
//!
//! Le placement n'est donc **pas** dans la caméra : une caméra existe sans être posée sur le
//! plan, et le plan garde ses règles (un objet par pièce, coordonnées bornées, sauvegarde,
//! `PlanChanged`). #6 ajoute `ItemKind::Camera` et `Item::rotation` ; [`is_camera_id`] sert à
//! valider qu'un identifiant désigne bien une caméra.

use serde::{Deserialize, Serialize};

use super::text_ids::text_id;

/// Plage des identifiants de caméra : 2^35 valeurs tout en haut de l'espace des identifiants
/// 64 bits, que Matter réserve (jamais attribuée à un nœud appairé, voir `matter::pair`). Les
/// appareils derrière un pont tirent les leurs juste en dessous (`BRIDGED_ID_MIN..=BRIDGED_ID_MAX`,
/// `persistence.rs`) : aucun recoupement. Tous dépassent 2^53 : en JSON ils sont du texte.
pub const CAMERA_ID_MIN: u64 = 0xFFFF_FFF8_0000_0000;
pub const CAMERA_ID_MAX: u64 = u64::MAX;

/// Nombre maximal de caméras : au-delà, le serveur refuse l'ajout. Un vieux Mac ne tiendrait
/// de toute façon pas plus de quelques flux (voir #5).
pub const MAX_CAMERAS: usize = 32;

/// `true` si `id` est dans la plage réservée aux caméras (cela ne dit pas qu'elle existe).
pub fn is_camera_id(id: u64) -> bool {
    (CAMERA_ID_MIN..=CAMERA_ID_MAX).contains(&id)
}

/// Pourquoi une caméra est injoignable. Codes stables (`snake_case`) : le client choisit sa
/// propre phrase ; `detail` n'est qu'un complément humain, sans adresse ni mot de passe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraProblem {
    /// Pas de réponse : éteinte, hors réseau, RTSP désactivé, mauvais port.
    Unreachable,
    /// La caméra répond mais refuse l'identifiant ou le mot de passe.
    Unauthorized,
    /// La caméra répond mais ce chemin n'existe pas (mauvaise adresse de flux).
    NotFound,
    /// La caméra répond, mais pas comme une caméra RTSP.
    Protocol,
    /// Autre échec.
    Failed,
}

/// État d'une caméra. Le serveur ne garde **aucune connexion ouverte** vers une caméra : il la
/// vérifie par un `DESCRIBE` bref, à l'ajout et ensuite toutes les minutes environ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CameraStatus {
    /// Pas encore vérifiée (ajout ou démarrage récent).
    Unknown,
    /// La dernière vérification a réussi.
    Online,
    /// La dernière vérification a échoué.
    Unreachable {
        problem: CameraProblem,
        #[serde(default)]
        detail: Option<String>,
    },
}

/// Ce qu'un flux annonce de lui-même (décrit par la caméra, avant toute lecture).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Codec vidéo en minuscules : `h264`, `h265`, `jpeg`… Un client qui ne décode pas ce
    /// codec doit le dire (#5 : « H.265 non pris en charge »).
    pub codec: String,
    /// Dimensions en pixels, si la caméra les annonce dès la description (sinon `None` : on ne
    /// les connaît qu'au premier flux).
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Une caméra, sans aucun secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraView {
    /// Dans [`CAMERA_ID_MIN`]`..=`[`CAMERA_ID_MAX`], en texte dans le JSON.
    #[serde(with = "text_id")]
    pub id: u64,
    pub name: String,
    pub status: CameraStatus,
    /// Un flux réduit est configuré : la mosaïque (#7) et la lecture par défaut (#5) le
    /// préfèrent ; sinon il n'y a que le flux principal.
    pub has_sub_stream: bool,
    /// Le flux surveillé : le réduit s'il existe, sinon le principal. Absent tant qu'aucune
    /// vérification n'a réussi.
    #[serde(default)]
    pub stream: Option<StreamInfo>,
}

/// Réponse de `GET /api/cameras` : les caméras triées par nom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CamerasResponse {
    pub cameras: Vec<CameraView>,
}
