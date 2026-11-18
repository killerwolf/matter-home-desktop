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
use crate::mode::HomeMode;

/// Plage dans laquelle le serveur tire les identifiants de **nouvelles** caméras : 2^35 valeurs
/// tout en haut de l'espace 64 bits, que Matter réserve (jamais attribuée à un nœud appairé).
/// Tous dépassent 2^53 : en JSON ils sont du texte.
///
/// **Cette plage n'est pas exclusive.** Avant les caméras, les appareils derrière un pont tiraient
/// leurs identifiants sur toute la plage `0xFFFF_FFF0_0000_0000..` : environ la moitié des appareils
/// pontés déjà attribués tombent donc ici. Un identifiant dans cette plage ne prouve **pas** que
/// c'est une caméra. Ce qui le prouve, c'est sa présence dans la liste des caméras du serveur ; et
/// le serveur ne tire jamais pour une caméra un identifiant déjà pris par un appareil connu (ni
/// l'import d'une sauvegarde n'en accepte un).
pub const CAMERA_ID_MIN: u64 = 0xFFFF_FFF8_0000_0000;
pub const CAMERA_ID_MAX: u64 = u64::MAX;

/// Nombre maximal de caméras : au-delà, le serveur refuse l'ajout. Un vieux Mac ne tiendrait
/// de toute façon pas plus de quelques flux (voir #5).
pub const MAX_CAMERAS: usize = 32;

/// `true` si `id` est dans la plage de tirage des caméras. **Un simple contrôle de plage** : il ne
/// dit ni que la caméra existe, ni que l'identifiant n'est pas celui d'un appareil ponté (voir
/// [`CAMERA_ID_MIN`]). Pour valider une référence, chercher la caméra dans la liste du serveur.
pub fn is_camera_id(id: u64) -> bool {
    (CAMERA_ID_MIN..=CAMERA_ID_MAX).contains(&id)
}

/// Le flux d'une caméra : le principal (plein format) ou le réduit (mosaïque, lecture par défaut).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Main,
    Sub,
}

impl StreamKind {
    pub fn label(self) -> &'static str {
        match self {
            StreamKind::Main => "Flux principal",
            StreamKind::Sub => "Flux réduit",
        }
    }

    /// Valeur du paramètre `kind` de la route du flux (`main` ou `sub`).
    pub fn key(self) -> &'static str {
        match self {
            StreamKind::Main => "main",
            StreamKind::Sub => "sub",
        }
    }

    pub fn from_key(key: &str) -> Option<StreamKind> {
        match key {
            "main" => Some(StreamKind::Main),
            "sub" => Some(StreamKind::Sub),
            _ => None,
        }
    }
}

/// Modèle de la route du flux vidéo d'une caméra (ticket n° 5), sous le même jeton que le reste :
/// `GET /api/cameras/{id}/stream?kind=main|sub` (`sub` par défaut). **Réservée** : le serveur
/// répond 404 tant que #5 ne l'a pas implémentée.
pub const CAMERA_STREAM_PATH: &str = "/api/cameras/{id}/stream";

/// Chemin concret de la route du flux : `/api/cameras/<id>/stream?kind=sub`.
pub fn camera_stream_path(id: u64, kind: StreamKind) -> String {
    format!("/api/cameras/{id}/stream?kind={}", kind.key())
}

/// Modèle de la route de la vignette d'une caméra (ticket n° 12) : `GET /api/cameras/{id}/snapshot`
/// rend une image JPEG récente (15 s au plus, partagée entre les clients), sous le même jeton que le
/// reste. Un serveur sans cette route répond 404 **sans** `detail`.
pub const CAMERA_SNAPSHOT_PATH: &str = "/api/cameras/{id}/snapshot";

/// Chemin concret de la route de la vignette : `/api/cameras/<id>/snapshot`.
pub fn camera_snapshot_path(id: u64) -> String {
    format!("/api/cameras/{id}/snapshot")
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
    /// Autre échec. Aussi lu quand un serveur plus récent envoie un code que ce client ne connaît pas.
    #[serde(other)]
    Failed,
}

/// État d'une caméra. Le serveur ne garde **aucune connexion ouverte** vers une caméra : il la
/// vérifie par un `DESCRIBE` bref, à l'ajout et ensuite toutes les minutes environ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CameraStatus {
    /// La dernière vérification a réussi.
    Online,
    /// La dernière vérification a échoué.
    Unreachable {
        problem: CameraProblem,
        #[serde(default)]
        detail: Option<String>,
        /// Depuis quand la caméra est injoignable (secondes Unix), fixé quand elle le devient
        /// (après 2 échecs d'affilée si elle était joignable) et inchangé tant qu'elle le reste,
        /// même si le motif change. Absent d'un serveur plus ancien : « depuis quand » inconnu.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
    },
    /// Éteinte dans le mode actuel de la maison (ticket n° 8) : aucune connexion n'est ouverte vers
    /// elle, on ne sait donc rien de sa joignabilité. Un client plus ancien le lit `unknown`.
    Off,
    /// Pas encore vérifiée (ajout ou démarrage récent). Aussi lu quand un serveur plus récent
    /// envoie un état que ce client ne connaît pas : il l'affiche comme « inconnu ».
    #[serde(other)]
    Unknown,
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

/// Où est la caméra : le choix pré-remplit son comportement par mode (voir [`CameraModes`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPlacement {
    /// Dans la maison : éteinte quand on est là.
    Interieure,
    /// Dehors : on la regarde et on la surveille. **Valeur des caméras déjà enregistrées**
    /// (ticket n° 8) : elles se comportent comme avant (jamais éteintes).
    #[default]
    Exterieure,
}

impl CameraPlacement {
    pub const ALL: [CameraPlacement; 2] =
        [CameraPlacement::Interieure, CameraPlacement::Exterieure];

    pub fn label(self) -> &'static str {
        match self {
            CameraPlacement::Interieure => "Intérieure",
            CameraPlacement::Exterieure => "Extérieure",
        }
    }
}

/// Ce que fait une caméra dans un mode de la maison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraBehavior {
    /// Éteinte : le serveur refuse le direct et les images fixes, et ferme les lectures en cours.
    Eteinte,
    /// On peut la regarder (direct et images fixes).
    Diffusion,
    /// Regardable comme en `Diffusion`, et destinée à la détection (ticket ultérieur : pour
    /// l'instant les deux se comportent pareil).
    Surveillance,
}

impl CameraBehavior {
    pub const ALL: [CameraBehavior; 3] = [
        CameraBehavior::Eteinte,
        CameraBehavior::Diffusion,
        CameraBehavior::Surveillance,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CameraBehavior::Eteinte => "Éteinte",
            CameraBehavior::Diffusion => "Diffusion",
            CameraBehavior::Surveillance => "Surveillance",
        }
    }

    /// Le serveur sert-il le direct et les images fixes ? (Tout sauf `Eteinte`.)
    pub fn is_viewable(self) -> bool {
        !matches!(self, CameraBehavior::Eteinte)
    }
}

/// Le comportement d'une caméra dans chacun des quatre modes. Tout champ absent d'un ancien
/// fichier vaut la valeur d'une caméra **extérieure** (`CameraModes::default()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraModes {
    pub present: CameraBehavior,
    pub absent: CameraBehavior,
    pub nuit: CameraBehavior,
    pub vacances: CameraBehavior,
}

impl Default for CameraModes {
    fn default() -> Self {
        CameraModes::for_placement(CameraPlacement::Exterieure)
    }
}

impl CameraModes {
    /// Le tableau proposé pour un emplacement. Extérieure : diffusion quand on est là,
    /// surveillance sinon. Intérieure : éteinte présent et nuit, surveillance absent et vacances.
    pub fn for_placement(placement: CameraPlacement) -> CameraModes {
        use CameraBehavior::*;
        match placement {
            CameraPlacement::Exterieure => CameraModes {
                present: Diffusion,
                absent: Surveillance,
                nuit: Surveillance,
                vacances: Surveillance,
            },
            CameraPlacement::Interieure => CameraModes {
                present: Eteinte,
                absent: Surveillance,
                nuit: Eteinte,
                vacances: Surveillance,
            },
        }
    }

    pub fn get(&self, mode: HomeMode) -> CameraBehavior {
        match mode {
            HomeMode::Present => self.present,
            HomeMode::Absent => self.absent,
            HomeMode::Nuit => self.nuit,
            HomeMode::Vacances => self.vacances,
        }
    }

    pub fn set(&mut self, mode: HomeMode, behavior: CameraBehavior) {
        match mode {
            HomeMode::Present => self.present = behavior,
            HomeMode::Absent => self.absent = behavior,
            HomeMode::Nuit => self.nuit = behavior,
            HomeMode::Vacances => self.vacances = behavior,
        }
    }
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
    /// Où est la caméra (absent d'un serveur plus ancien : extérieure).
    #[serde(default)]
    pub emplacement: CameraPlacement,
    /// Ce que fait la caméra dans chaque mode de la maison (absent d'un serveur plus ancien :
    /// tableau d'une caméra extérieure, jamais éteinte).
    #[serde(default)]
    pub modes: CameraModes,
}

/// Réponse de `GET /api/cameras` : les caméras triées par nom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CamerasResponse {
    pub cameras: Vec<CameraView>,
}
