//! Détection sur les caméras en Surveillance (ticket n° 18) : classes, réglages par caméra,
//! événements et routes.
//!
//! Tout est ajouté **dans le protocole 3**, sans le relever : les champs nouveaux sont
//! `#[serde(default)]`, la classe a une variante de repli ([`DetectedClass::Unknown`]) et
//! `WireEvent::Detection` est ignoré par un client qui ne le connaît pas (`WireEvent::Unknown`).

use serde::{Deserialize, Serialize};

use super::text_ids::text_id;

/// Ce que la détection sait reconnaître.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedClass {
    Personne,
    Chat,
    Chien,
    /// Voiture, camion, bus, moto, vélo.
    Vehicule,
    /// Une classe d'un serveur plus récent : à afficher comme « objet ».
    #[serde(other)]
    Unknown,
}

impl DetectedClass {
    pub const ALL: [DetectedClass; 4] = [
        DetectedClass::Personne,
        DetectedClass::Chat,
        DetectedClass::Chien,
        DetectedClass::Vehicule,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DetectedClass::Personne => "Personne",
            DetectedClass::Chat => "Chat",
            DetectedClass::Chien => "Chien",
            DetectedClass::Vehicule => "Véhicule",
            DetectedClass::Unknown => "Objet",
        }
    }

    /// Le mot dans une phrase (« personne détectée »).
    pub fn lower(self) -> &'static str {
        match self {
            DetectedClass::Personne => "personne",
            DetectedClass::Chat => "chat",
            DetectedClass::Chien => "chien",
            DetectedClass::Vehicule => "véhicule",
            DetectedClass::Unknown => "objet",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            DetectedClass::Personne => "personne",
            DetectedClass::Chat => "chat",
            DetectedClass::Chien => "chien",
            DetectedClass::Vehicule => "vehicule",
            DetectedClass::Unknown => "unknown",
        }
    }

    pub fn from_key(key: &str) -> Option<DetectedClass> {
        DetectedClass::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// Score minimal par défaut d'une détection (0 à 1).
pub const DEFAULT_MIN_SCORE: f32 = 0.5;

/// Un polygone en coordonnées **normalisées** (0 à 1, origine en haut à gauche de l'image).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Polygon {
    pub points: Vec<(f32, f32)>,
}

impl Polygon {
    /// Le point est-il dans le polygone (règle pair-impair) ? Un polygone de moins de 3 points ne
    /// contient rien.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let pts = &self.points;
        if pts.len() < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    /// Valide : au moins 3 points, tous entre 0 et 1.
    pub fn is_valid(&self) -> bool {
        self.points.len() >= 3
            && self
                .points
                .iter()
                .all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
    }

    /// Texte d'édition : `x,y x,y x,y` (la fenêtre du serveur).
    pub fn to_text(&self) -> String {
        self.points
            .iter()
            .map(|(x, y)| format!("{},{}", round3(*x), round3(*y)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Lit `x,y x,y x,y` (virgule ou point-virgule entre les nombres admis). `None` si illisible ou
    /// invalide.
    pub fn parse(text: &str) -> Option<Polygon> {
        let mut points = Vec::new();
        for pair in text.split_whitespace() {
            let (x, y) = pair.split_once([',', ';'])?;
            points.push((x.trim().parse().ok()?, y.trim().parse().ok()?));
        }
        let polygon = Polygon { points };
        polygon.is_valid().then_some(polygon)
    }
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

/// Réglages de la détection d'une caméra (fenêtre du serveur, `cameras.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectionSettings {
    /// Classes suivies ; vide : aucune (la caméra est regardée mais rien ne fait d'événement).
    pub classes: Vec<DetectedClass>,
    /// Score minimal par classe ; une classe absente prend [`DEFAULT_MIN_SCORE`].
    pub min_scores: Vec<(DetectedClass, f32)>,
    /// Zones : si la liste n'est pas vide, seule une détection dont le **bas** de la boîte (les
    /// pieds) est dans l'une d'elles compte.
    pub zones: Vec<Polygon>,
    /// Masques : le mouvement et les détections dont le bas de la boîte est dedans sont ignorés.
    pub masks: Vec<Polygon>,
}

impl Default for DetectionSettings {
    fn default() -> Self {
        Self {
            classes: DetectedClass::ALL.to_vec(),
            min_scores: Vec::new(),
            zones: Vec::new(),
            masks: Vec::new(),
        }
    }
}

impl DetectionSettings {
    pub fn min_score(&self, class: DetectedClass) -> f32 {
        self.min_scores
            .iter()
            .find(|(c, _)| *c == class)
            .map(|(_, s)| *s)
            .unwrap_or(DEFAULT_MIN_SCORE)
    }

    pub fn set_min_score(&mut self, class: DetectedClass, score: f32) {
        self.min_scores.retain(|(c, _)| *c != class);
        if (score - DEFAULT_MIN_SCORE).abs() > f32::EPSILON {
            self.min_scores.push((class, score.clamp(0.05, 0.99)));
        }
    }

    /// Un point (normalisé) est-il masqué ?
    pub fn masked(&self, x: f32, y: f32) -> bool {
        self.masks.iter().any(|m| m.contains(x, y))
    }

    /// Un point (normalisé) compte-t-il : hors masque, et dans une zone s'il y en a ?
    pub fn counts(&self, x: f32, y: f32) -> bool {
        !self.masked(x, y) && (self.zones.is_empty() || self.zones.iter().any(|z| z.contains(x, y)))
    }
}

/// Un événement de détection : un passage, du premier au dernier instant où quelque chose a été vu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionEventView {
    /// Identifiant de l'événement (texte opaque : `<horodatage>-<caméra>`).
    pub id: String,
    #[serde(with = "text_id")]
    pub camera_id: u64,
    pub camera_name: String,
    /// Les classes vues pendant le passage, la plus sûre d'abord.
    pub classes: Vec<DetectedClass>,
    /// Meilleur score (0 à 1).
    pub score: f32,
    /// Début et fin du passage (s Unix).
    pub start: f64,
    pub end: f64,
    /// Un clip MP4 est disponible (`GET /api/events/{id}/clip`).
    #[serde(default)]
    pub has_clip: bool,
    /// Durée du clip en secondes (avant + passage + après).
    #[serde(default)]
    pub clip_s: f64,
    /// Le passage est **en cours** : annoncé dès la première détection (capture, pas encore de
    /// clip), puis envoyé de nouveau, complet, à sa fin (même `id`). Un client remplace l'ancien
    /// par le nouveau.
    #[serde(default)]
    pub in_progress: bool,
    /// Description en une phrase (ticket n° 19, API Anthropic), quand le serveur en produit : elle
    /// arrive **après** l'événement (même `id`, nouvel envoi). Absente sinon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Niveau de menace donné avec la description : 0 normal, 1 suspect, 2 critique. Absent quand
    /// la description est « indisponible » (refus du modèle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub niveau: Option<u8>,
}

/// Les mots des niveaux de menace (`{niveau}` dans les textes des règles, affichage).
pub fn niveau_label(niveau: u8) -> &'static str {
    match niveau {
        0 => "normal",
        1 => "suspect",
        _ => "critique",
    }
}

impl DetectionEventView {
    /// La classe principale (la plus sûre).
    pub fn class(&self) -> DetectedClass {
        self.classes
            .first()
            .copied()
            .unwrap_or(DetectedClass::Unknown)
    }
}

/// `GET /api/events?since=&camera=&limit=` : les plus récents d'abord.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<DetectionEventView>,
    /// État de la détection (modèle présent, moteur, caméras surveillées).
    #[serde(default)]
    pub detection: DetectionStatusView,
}

/// Où en est la détection sur le serveur.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DetectionStatusView {
    /// Un détecteur est chargé et prêt.
    pub available: bool,
    /// Moteur en clair (« CoreML (GPU) », « CPU, 2 fils »), ou la raison de l'absence.
    pub engine: String,
    /// Caméras analysées en ce moment.
    #[serde(default, with = "vec_text_id")]
    pub watched: Vec<u64>,
    /// La détection est suspendue sur au moins une caméra (machine surchargée ou trop chaude).
    #[serde(default)]
    pub paused: bool,
    /// Les caméras où elle est suspendue.
    #[serde(default, with = "vec_text_id")]
    pub paused_cameras: Vec<u64>,
    /// Pourquoi, en clair (« analyse trop lente », « Mac trop chaud »).
    #[serde(default)]
    pub pause_reason: Option<String>,
}

/// Une liste d'identifiants en texte décimal (comme [`text_id`]).
mod vec_text_id {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ids: &[u64], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(ids.iter().map(|id| id.to_string()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u64>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|t| super::super::text_ids::parse_id(t).map_err(serde::de::Error::custom))
            .collect()
    }
}

pub const EVENTS_PATH: &str = "/api/events";
pub const EVENT_PATH: &str = "/api/events/{id}";
pub const EVENT_SNAPSHOT_PATH: &str = "/api/events/{id}/snapshot";
pub const EVENT_CLIP_PATH: &str = "/api/events/{id}/clip";
/// Nombre d'événements rendus par défaut, et maximum.
pub const EVENTS_DEFAULT_LIMIT: usize = 100;
pub const EVENTS_MAX_LIMIT: usize = 1000;

/// Un identifiant d'événement acceptable (chiffres, lettres minuscules, tirets) : il sert de nom de
/// fichier, rien d'autre n'y passe.
pub fn is_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Polygon {
        Polygon {
            points: vec![(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)],
        }
    }

    #[test]
    fn the_description_is_optional_on_the_wire() {
        // Un événement du ticket n° 18 (sans description) se lit toujours, et ne l'écrit pas.
        let old = r#"{"id":"1-2","camera_id":"2","camera_name":"Allée","classes":["personne"],
            "score":0.8,"start":1.0,"end":2.0}"#;
        let mut event: DetectionEventView = serde_json::from_str(old).unwrap();
        assert!(event.description.is_none() && event.niveau.is_none());
        let text = serde_json::to_string(&event).unwrap();
        assert!(
            !text.contains("description") && !text.contains("niveau"),
            "{text}"
        );
        event.description = Some("Un livreur dépose un colis.".into());
        event.niveau = Some(1);
        let back: DetectionEventView =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(back, event);
        assert_eq!(niveau_label(0), "normal");
        assert_eq!(niveau_label(1), "suspect");
        assert_eq!(niveau_label(2), "critique");
    }

    #[test]
    fn polygon_contains_and_parses() {
        let p = square();
        assert!(p.contains(0.5, 0.5));
        assert!(!p.contains(0.1, 0.5));
        assert!(!Polygon {
            points: vec![(0.0, 0.0), (1.0, 1.0)]
        }
        .contains(0.5, 0.5));
        let text = p.to_text();
        assert_eq!(Polygon::parse(&text), Some(p));
        assert_eq!(Polygon::parse("0,0 1,0"), None);
        assert_eq!(Polygon::parse("0,0 1,0 2,2"), None);
        assert_eq!(Polygon::parse("a,b c,d e,f"), None);
        assert!(Polygon::parse("0;0 1;0 1;1").is_some());
    }

    #[test]
    fn zones_and_masks() {
        let mut s = DetectionSettings::default();
        assert!(s.counts(0.1, 0.1));
        s.masks.push(square());
        assert!(!s.counts(0.5, 0.5) && s.counts(0.1, 0.1));
        s.masks.clear();
        s.zones.push(square());
        assert!(s.counts(0.5, 0.5) && !s.counts(0.1, 0.1));
    }

    #[test]
    fn min_scores_default_and_override() {
        let mut s = DetectionSettings::default();
        assert_eq!(s.min_score(DetectedClass::Chat), DEFAULT_MIN_SCORE);
        s.set_min_score(DetectedClass::Chat, 0.7);
        assert_eq!(s.min_score(DetectedClass::Chat), 0.7);
        s.set_min_score(DetectedClass::Chat, DEFAULT_MIN_SCORE);
        assert!(s.min_scores.is_empty());
    }

    #[test]
    fn unknown_class_and_missing_settings_are_tolerated() {
        let c: DetectedClass = serde_json::from_str("\"girafe\"").unwrap();
        assert_eq!(c, DetectedClass::Unknown);
        let s: DetectionSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, DetectionSettings::default());
        assert_eq!(
            serde_json::to_string(&DetectedClass::Vehicule).unwrap(),
            "\"vehicule\""
        );
    }

    /// Une règle d'un serveur plus récent ne fait pas échouer la lecture des règles.
    #[test]
    fn unknown_rule_parts_are_read_as_unknown() {
        use crate::automation::{Action, Condition, RuleSet, Trigger};
        let text = r#"{"version":1,"rules":[{"id":1,"name":"x","enabled":true,
            "trigger":{"type":"Teleportation","where":"Mars"},
            "conditions":[{"type":"Lune","phase":3}],
            "actions":[{"type":"Fusee"}]}]}"#;
        let set: RuleSet = serde_json::from_str(text).unwrap();
        assert_eq!(set.rules[0].trigger, Trigger::Unknown);
        assert_eq!(set.rules[0].conditions, [Condition::Unknown]);
        assert_eq!(set.rules[0].actions, [Action::Unknown]);
    }

    #[test]
    fn event_ids_are_file_safe() {
        assert!(is_event_id("1763500000123-18446744069414584321"));
        assert!(!is_event_id("../x"));
        assert!(!is_event_id(""));
        assert!(!is_event_id("A"));
    }
}
