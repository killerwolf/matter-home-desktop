//! Les réglages des descriptions (ticket n° 19) : ce qui est envoyé, à quel modèle, pour quelles
//! caméras et quelles classes, et le plafond quotidien. Voir `docs/descriptions.md`.
//!
//! Le fichier `serveur/descriptions.json` (droits 0600, dossier 0700) garde les réglages **et la
//! clé d'API**, comme `cameras.json` garde les mots de passe des caméras. Il est dans `serveur/` :
//! **jamais exporté** dans une sauvegarde `.matterhome` (une clé d'API dépense de l'argent : on ne
//! la laisse pas dans un fichier que l'on transfère par AirDrop ou clé USB), et recopié tel quel
//! d'un dossier à l'autre à l'import d'une sauvegarde. Sur un nouveau Mac, on la saisit de nouveau.

use serde::{Deserialize, Serialize};

use protocol::wire::{CameraPlacement, DetectedClass};

/// Nom du fichier dans `serveur/`.
pub const FILE: &str = "descriptions.json";
/// Plafond quotidien par défaut (nombre de descriptions).
pub const DEFAULT_DAILY_CAP: u32 = 200;
/// Le plus grand plafond accepté (un chiffre fou est une faute de frappe).
pub const MAX_DAILY_CAP: u32 = 100_000;

/// Le modèle qui décrit. Les identifiants sont ceux de l'API Anthropic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Model {
    #[serde(rename = "claude-haiku-5-5")]
    Haiku,
    #[serde(rename = "claude-sonnet-5-5")]
    Sonnet,
    #[default]
    #[serde(rename = "claude-opus-5-5")]
    Opus,
}

impl Model {
    pub const ALL: [Model; 3] = [Model::Haiku, Model::Sonnet, Model::Opus];

    /// L'identifiant envoyé à l'API.
    pub fn id(self) -> &'static str {
        match self {
            Model::Haiku => "claude-haiku-5-5",
            Model::Sonnet => "claude-sonnet-5-5",
            Model::Opus => "claude-opus-5-5",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Haiku => "Haiku 5.5",
            Model::Sonnet => "Sonnet 5.5",
            Model::Opus => "Opus 5.5",
        }
    }

    /// Coût approximatif d'un événement (~1 500 jetons en entrée, ~150 en sortie).
    pub fn cost(self) -> &'static str {
        match self {
            Model::Haiku => "≈ 0,02 centime",
            Model::Sonnet => "≈ 0,45 centime",
            Model::Opus => "≈ 0,9 centime",
        }
    }

    /// Reprise côté Anthropic en cas de refus du modèle (en-tête bêta et `"fallbacks"`), pour
    /// Opus 5.5 et Sonnet 5.5.
    pub fn server_fallback(self) -> bool {
        !matches!(self, Model::Haiku)
    }
}

/// Les caméras dont les événements sont décrits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", content = "ids", rename_all = "snake_case")]
pub enum CameraFilter {
    /// Toutes les caméras.
    #[default]
    All,
    /// Les caméras dont l'emplacement est « Extérieure ».
    Exterior,
    /// Celles-ci seulement.
    List(Vec<u64>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Décrire les événements (désactivé par défaut : rien ne part sans choix de l'utilisateur).
    pub enabled: bool,
    /// Décrire aussi dès l'ouverture du passage (première capture). Désactivé par défaut : une
    /// seule description, à la fin, avec la meilleure capture. Activé, chaque passage coûte deux
    /// descriptions.
    pub on_open: bool,
    pub model: Model,
    pub cameras: CameraFilter,
    pub classes: Vec<DetectedClass>,
    /// Nombre maximal de descriptions par jour (au-delà : sans description, une ligne au journal).
    pub daily_cap: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            on_open: false,
            model: Model::default(),
            cameras: CameraFilter::default(),
            classes: DetectedClass::ALL.to_vec(),
            daily_cap: DEFAULT_DAILY_CAP,
        }
    }
}

impl Settings {
    /// L'événement d'une caméra est-il à décrire (hors clé et plafond) ?
    pub fn wants(
        &self,
        camera_id: u64,
        placement: Option<CameraPlacement>,
        classes: &[DetectedClass],
    ) -> bool {
        if !self.enabled || !classes.iter().any(|c| self.classes.contains(c)) {
            return false;
        }
        match &self.cameras {
            CameraFilter::All => true,
            CameraFilter::Exterior => placement == Some(CameraPlacement::Exterieure),
            CameraFilter::List(ids) => ids.contains(&camera_id),
        }
    }

    /// Réglages ramenés à des valeurs acceptables.
    pub fn sanitized(mut self) -> Self {
        self.daily_cap = self.daily_cap.min(MAX_DAILY_CAP);
        self.classes.sort();
        self.classes.dedup();
        self.classes.retain(|c| *c != DetectedClass::Unknown);
        if let CameraFilter::List(ids) = &mut self.cameras {
            ids.sort_unstable();
            ids.dedup();
        }
        self
    }
}

/// Le contenu du fichier : réglages et clé. `Debug` ne montre jamais la clé.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    #[serde(flatten)]
    pub settings: Settings,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("settings", &self.settings)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Une clé d'API acceptable : des caractères imprimables ASCII, sans guillemet ni barre oblique
/// inverse (elle est transmise à `curl` dans un fichier de configuration, jamais en argument).
pub fn valid_key(key: &str) -> bool {
    (8..=512).contains(&key.len())
        && key
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'"' && b != b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_describe_nothing_and_use_opus_with_a_cap() {
        let s = Settings::default();
        assert!(!s.enabled && !s.on_open);
        assert_eq!(s.model, Model::Opus);
        assert_eq!(s.daily_cap, 200);
        assert!(!s.wants(1, None, &[DetectedClass::Personne]), "désactivé");
    }

    #[test]
    fn models_have_the_api_ids_costs_and_fallbacks() {
        assert_eq!(Model::Haiku.id(), "claude-haiku-5-5");
        assert_eq!(Model::Sonnet.id(), "claude-sonnet-5-5");
        assert_eq!(Model::Opus.id(), "claude-opus-5-5");
        assert_eq!(Model::Haiku.cost(), "≈ 0,02 centime");
        assert!(Model::Opus.server_fallback() && Model::Sonnet.server_fallback());
        assert!(!Model::Haiku.server_fallback());
        let json = serde_json::to_string(&Model::Sonnet).unwrap();
        assert_eq!(json, "\"claude-sonnet-5-5\"");
        assert_eq!(serde_json::from_str::<Model>(&json).unwrap(), Model::Sonnet);
    }

    #[test]
    fn filters_pick_cameras_and_classes() {
        let mut s = Settings {
            enabled: true,
            ..Settings::default()
        };
        let person = [DetectedClass::Personne];
        let cat = [DetectedClass::Chat];
        assert!(s.wants(1, None, &person));
        s.cameras = CameraFilter::Exterior;
        assert!(s.wants(1, Some(CameraPlacement::Exterieure), &person));
        assert!(!s.wants(1, Some(CameraPlacement::Interieure), &person));
        assert!(!s.wants(1, None, &person), "emplacement inconnu : non");
        s.cameras = CameraFilter::List(vec![2, 3]);
        assert!(s.wants(3, Some(CameraPlacement::Interieure), &person));
        assert!(!s.wants(1, Some(CameraPlacement::Exterieure), &person));
        s.cameras = CameraFilter::All;
        s.classes = vec![DetectedClass::Personne];
        assert!(s.wants(1, None, &[DetectedClass::Chat, DetectedClass::Personne]));
        assert!(!s.wants(1, None, &cat));
        s.classes.clear();
        assert!(!s.wants(1, None, &person), "aucune classe : rien");
    }

    #[test]
    fn the_file_round_trips_and_never_prints_the_key() {
        let stored = Stored {
            settings: Settings {
                enabled: true,
                cameras: CameraFilter::List(vec![7, 9]),
                ..Settings::default()
            },
            api_key: Some("sk-ant-secret-value".into()),
        };
        let text = serde_json::to_string(&stored).unwrap();
        let back: Stored = serde_json::from_str(&text).unwrap();
        assert_eq!(back.settings, stored.settings);
        assert_eq!(back.api_key, stored.api_key);
        let shown = format!("{stored:?}");
        assert!(!shown.contains("secret"), "{shown}");
        // Un fichier partiel ou ancien : le reste prend ses valeurs par défaut.
        let partial: Stored = serde_json::from_str(r#"{"enabled": true}"#).unwrap();
        assert!(partial.settings.enabled && partial.api_key.is_none());
        assert_eq!(partial.settings.daily_cap, 200);
    }

    #[test]
    fn keys_are_checked_and_settings_sanitized() {
        assert!(valid_key("sk-ant-api03-abcDEF_123-xyz"));
        assert!(!valid_key("court"));
        assert!(!valid_key("a b c d e f g h"));
        assert!(!valid_key("sk-ant-\"injection\"-xxxxx"));
        assert!(!valid_key("sk-ant-back\\slash-xxxx"));
        assert!(!valid_key("sk-ant-é-xxxxxxxxxxxx"));
        let s = Settings {
            daily_cap: u32::MAX,
            classes: vec![
                DetectedClass::Chat,
                DetectedClass::Chat,
                DetectedClass::Unknown,
            ],
            cameras: CameraFilter::List(vec![5, 3, 5]),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(s.daily_cap, MAX_DAILY_CAP);
        assert_eq!(s.classes, vec![DetectedClass::Chat]);
        assert_eq!(s.cameras, CameraFilter::List(vec![3, 5]));
    }
}
