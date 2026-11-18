//! Un passage = un événement, avec un suivi des objets (comme les objets « stationary » de
//! Frigate). Pur, avec l'heure en paramètre : testé sans caméra ni horloge.
//!
//! - Chaque détection est rattachée à un **objet suivi** de même classe (chevauchement IoU). Un
//!   objet dont la boîte ne bouge plus depuis [`TrackerConfig::stationary_after`] est **immobile** :
//!   il ne compte plus (voiture garée, chat endormi). Il redevient actif s'il bouge.
//! - Le premier objet actif **ouvre** le passage, les suivants le prolongent ; sans objet actif
//!   pendant [`TrackerConfig::close_after`], il se **ferme**. Un passage ne dure jamais plus de
//!   [`TrackerConfig::max_duration`] (la durée maximale d'un clip) : il est alors fermé et
//!   enregistré, et le suivant demande un objet actif (donc du mouvement).
//! - L'appelant ne passe ici que des détections **en mouvement** (le détecteur ne tourne que
//!   s'il y a du mouvement, et une boîte sans mouvement dedans est écartée : voir `watcher.rs`).

use std::collections::BTreeMap;

use protocol::wire::{DetectedClass, DetectionSettings};

use super::detector::Detection;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// Secondes sans objet actif avant de fermer le passage (le clip garde ce temps « après »).
    pub close_after: f64,
    /// Durée maximale d'un passage (celle d'un clip).
    pub max_duration: f64,
    /// Un objet dont la boîte ne bouge plus depuis ce temps est immobile.
    pub stationary_after: f64,
    /// Un objet qu'on n'a pas revu depuis ce temps est oublié.
    pub forget_after: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            close_after: 8.0,
            max_duration: 120.0,
            stationary_after: 10.0,
            forget_after: 30.0 * 60.0,
        }
    }
}

/// Chevauchement minimal pour reconnaître le même objet d'une image à l'autre.
const SAME_OBJECT_IOU: f32 = 0.3;
/// Au-dessus de ce chevauchement avec sa dernière position, l'objet n'a pas bougé.
const STILL_IOU: f32 = 0.85;

/// Garde les détections qui comptent pour cette caméra.
pub fn accept(detections: &[Detection], settings: &DetectionSettings) -> Vec<Detection> {
    detections
        .iter()
        .filter(|d| settings.classes.contains(&d.class))
        .filter(|d| d.score >= settings.min_score(d.class))
        .filter(|d| {
            let (x, y) = d.anchor();
            settings.counts(x, y)
        })
        .copied()
        .collect()
}

pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = w * h;
    let area = |r: &[f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Un objet suivi.
#[derive(Debug, Clone, PartialEq)]
struct Track {
    class: DetectedClass,
    /// Position quand il a bougé pour la dernière fois.
    still_box: [f32; 4],
    still_since: f64,
    last_seen: f64,
}

/// Un passage, ouvert ou terminé. `T` : la meilleure image (un JPEG en pratique).
#[derive(Debug, Clone, PartialEq)]
pub struct Passage<T> {
    pub start: f64,
    pub last: f64,
    /// Meilleur score par classe.
    pub classes: BTreeMap<DetectedClass, f32>,
    pub best: Option<(f32, T)>,
    /// Fermé parce qu'il durait trop (et non faute d'objet actif).
    pub cut: bool,
}

impl<T> Passage<T> {
    pub fn score(&self) -> f32 {
        self.classes.values().copied().fold(0.0, f32::max)
    }

    /// Les classes, la plus sûre d'abord.
    pub fn ranked(&self) -> Vec<DetectedClass> {
        let mut v: Vec<(DetectedClass, f32)> = self.classes.iter().map(|(c, s)| (*c, *s)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().map(|(c, _)| c).collect()
    }
}

#[derive(Debug, PartialEq)]
pub enum Step<T> {
    Nothing,
    /// Un passage vient de commencer (le clip démarre ; voir [`Tracker::current`]).
    Opened,
    /// Prolongé.
    Extended,
    /// Terminé : l'événement à enregistrer.
    Closed(Passage<T>),
}

#[derive(Debug)]
pub struct Tracker<T> {
    config: TrackerConfig,
    open: Option<Passage<T>>,
    tracks: Vec<Track>,
}

impl<T> Tracker<T> {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            open: None,
            tracks: Vec::new(),
        }
    }

    pub fn set_config(&mut self, config: TrackerConfig) {
        self.config = config;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Le passage en cours.
    pub fn current(&self) -> Option<&Passage<T>> {
        self.open.as_ref()
    }

    /// Les objets suivis en ce moment (diagnostic, tests).
    pub fn tracked(&self) -> usize {
        self.tracks.len()
    }

    /// Rattache les détections aux objets suivis ; rend celles des objets **actifs**.
    fn follow(&mut self, now: f64, seen: &[Detection]) -> Vec<Detection> {
        let forget = self.config.forget_after;
        self.tracks.retain(|t| now - t.last_seen < forget);
        let mut order: Vec<&Detection> = seen.iter().collect();
        order.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut taken = vec![false; self.tracks.len()];
        let mut active = Vec::new();
        for d in order {
            let best = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, t)| !taken[*i] && t.class == d.class)
                .map(|(i, t)| (i, iou(&t.still_box, &d.bbox)))
                .filter(|(_, o)| *o >= SAME_OBJECT_IOU)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let still_since = match best {
                Some((i, overlap)) => {
                    taken[i] = true;
                    let t = &mut self.tracks[i];
                    t.last_seen = now;
                    if overlap < STILL_IOU {
                        // Il a bougé : nouvelle position de référence.
                        t.still_box = d.bbox;
                        t.still_since = now;
                    }
                    t.still_since
                }
                None => {
                    self.tracks.push(Track {
                        class: d.class,
                        still_box: d.bbox,
                        still_since: now,
                        last_seen: now,
                    });
                    taken.push(true);
                    now
                }
            };
            if now - still_since < self.config.stationary_after {
                active.push(*d);
            }
        }
        active
    }

    /// Une image analysée à `now` (s Unix), avec ses détections **déjà acceptées et en
    /// mouvement**. `frame` ne sert (et n'est appelée) que si l'image est la meilleure jusque-là.
    pub fn observe(&mut self, now: f64, seen: &[Detection], frame: impl FnOnce() -> T) -> Step<T> {
        if let Some(closed) = self.expire(now) {
            // Le passage précédent était fini avant cette image : il se ferme, celle-ci
            // ouvrira le suivant à l'image d'après.
            return Step::Closed(closed);
        }
        let active = self.follow(now, seen);
        let Some(top) = active.iter().map(|d| d.score).reduce(f32::max) else {
            return Step::Nothing;
        };
        let opened = self.open.is_none();
        let passage = self.open.get_or_insert_with(|| Passage {
            start: now,
            last: now,
            classes: BTreeMap::new(),
            best: None,
            cut: false,
        });
        passage.last = now;
        for d in &active {
            let s = passage.classes.entry(d.class).or_insert(0.0);
            *s = s.max(d.score);
        }
        if passage.best.as_ref().is_none_or(|(s, _)| top > *s) {
            passage.best = Some((top, frame()));
        }
        if opened {
            Step::Opened
        } else {
            Step::Extended
        }
    }

    /// Le temps passe sans image (ou sans détection) : ferme un passage resté sans nouvelle, ou
    /// trop long.
    pub fn tick(&mut self, now: f64) -> Option<Passage<T>> {
        self.expire(now)
    }

    /// Ferme tout de suite (arrêt de la surveillance).
    pub fn flush(&mut self) -> Option<Passage<T>> {
        self.open.take()
    }

    fn expire(&mut self, now: f64) -> Option<Passage<T>> {
        let p = self.open.as_ref()?;
        if now - p.start >= self.config.max_duration {
            let mut p = self.open.take()?;
            p.cut = true;
            // Ce qui était là reste suivi : il faudra qu'il bouge encore pour un nouveau passage.
            return Some(p);
        }
        if now - p.last >= self.config.close_after {
            return self.open.take();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::Polygon;

    fn det(class: DetectedClass, score: f32, x: f32, y: f32) -> Detection {
        Detection {
            class,
            score,
            bbox: [x - 0.05, y - 0.2, x + 0.05, y],
        }
    }

    #[test]
    fn one_passage_makes_one_event() {
        let mut t: Tracker<u32> = Tracker::new(TrackerConfig::default());
        assert_eq!(t.observe(0.0, &[], || 0), Step::Nothing);
        // Une personne qui marche (sa boîte avance).
        assert_eq!(
            t.observe(1.0, &[det(DetectedClass::Personne, 0.7, 0.2, 0.9)], || 1),
            Step::Opened
        );
        assert_eq!(t.current().unwrap().start, 1.0);
        for i in 0..10 {
            let now = 1.5 + f64::from(i) * 0.5;
            assert!(matches!(t.observe(now, &[], || 9), Step::Nothing));
        }
        let better = det(DetectedClass::Personne, 0.9, 0.3, 0.9);
        let dog = det(DetectedClass::Chien, 0.6, 0.7, 0.9);
        assert_eq!(t.observe(7.0, &[better, dog], || 2), Step::Extended);
        assert_eq!(t.tick(14.0), None);
        let closed = t.tick(15.0).expect("fermé après 8 s sans détection");
        assert_eq!((closed.start, closed.last, closed.cut), (1.0, 7.0, false));
        assert_eq!(
            closed.ranked(),
            [DetectedClass::Personne, DetectedClass::Chien]
        );
        assert_eq!(closed.best, Some((0.9, 2)));
        assert!(!t.is_open());
    }

    #[test]
    fn a_late_image_closes_before_opening_again() {
        let mut t: Tracker<()> = Tracker::new(TrackerConfig::default());
        t.observe(0.0, &[det(DetectedClass::Chat, 0.8, 0.2, 0.5)], || ());
        let moved = det(DetectedClass::Chat, 0.8, 0.5, 0.5);
        assert!(matches!(t.observe(20.0, &[moved], || ()), Step::Closed(_)));
        assert_eq!(t.observe(20.5, &[moved], || ()), Step::Opened);
    }

    /// Une voiture garée (ou un chat endormi) : après 10 s sans bouger, elle ne compte plus ; le
    /// passage se ferme, et rien ne se rouvre tant qu'elle ne bouge pas.
    #[test]
    fn a_stationary_object_stops_counting_and_does_not_reopen() {
        let mut t: Tracker<()> = Tracker::new(TrackerConfig::default());
        let car = det(DetectedClass::Vehicule, 0.9, 0.5, 0.8);
        assert_eq!(t.observe(0.0, &[car], || ()), Step::Opened);
        let mut closed = None;
        for i in 1..200 {
            let now = f64::from(i) * 0.5;
            if let Step::Closed(p) = t.observe(now, &[car], || ()) {
                closed = Some((now, p));
                break;
            }
        }
        let (at, p) = closed.expect("le passage se ferme");
        assert!(!p.cut, "fermé faute d'objet actif, pas coupé");
        assert!(at < 20.0, "fermé vers 10 s + 8 s ({at})");
        for i in 0..100 {
            let now = at + f64::from(i);
            assert_eq!(
                t.observe(now, &[car], || ()),
                Step::Nothing,
                "rien ne se rouvre"
            );
        }
        // Elle repart : nouveau passage.
        let away = det(DetectedClass::Vehicule, 0.9, 0.6, 0.8);
        assert_eq!(t.observe(at + 200.0, &[away], || ()), Step::Opened);
    }

    /// Un passage qui n'en finit pas est coupé à la durée maximale et enregistré.
    #[test]
    fn a_passage_never_lasts_more_than_the_maximum() {
        let config = TrackerConfig {
            max_duration: 30.0,
            ..TrackerConfig::default()
        };
        let mut t: Tracker<()> = Tracker::new(config);
        let mut cut = None;
        for i in 0..100 {
            // Elle avance un peu à chaque image : toujours active.
            let x = 0.1 + i as f32 * 0.012;
            let now = f64::from(i);
            if let Step::Closed(p) =
                t.observe(now, &[det(DetectedClass::Personne, 0.8, x, 0.9)], || ())
            {
                cut = Some((now, p));
                break;
            }
        }
        let (at, p) = cut.expect("coupé");
        assert!(p.cut);
        assert_eq!(at, 30.0);
        assert!(t.tick(31.0).is_none());
    }

    #[test]
    fn classes_scores_zones_and_masks_filter() {
        let mut settings = DetectionSettings {
            classes: vec![DetectedClass::Personne, DetectedClass::Vehicule],
            ..DetectionSettings::default()
        };
        settings.set_min_score(DetectedClass::Vehicule, 0.8);
        let all = [
            det(DetectedClass::Personne, 0.6, 0.5, 0.5),
            det(DetectedClass::Personne, 0.4, 0.5, 0.5), // trop faible
            det(DetectedClass::Chat, 0.9, 0.5, 0.5),     // non suivi
            det(DetectedClass::Vehicule, 0.7, 0.5, 0.5), // sous son seuil
            det(DetectedClass::Vehicule, 0.85, 0.5, 0.5),
        ];
        assert_eq!(accept(&all, &settings).len(), 2);
        // Un masque sur la moitié droite : seuls les pieds comptent.
        settings.masks.push(Polygon {
            points: vec![(0.5, 0.0), (1.0, 0.0), (1.0, 1.0), (0.5, 1.0)],
        });
        let right = det(DetectedClass::Personne, 0.9, 0.8, 0.9);
        let left = det(DetectedClass::Personne, 0.9, 0.2, 0.9);
        assert_eq!(accept(&[right, left], &settings), [left]);
        // Une zone en bas à gauche.
        settings.masks.clear();
        settings.zones.push(Polygon {
            points: vec![(0.0, 0.5), (0.5, 0.5), (0.5, 1.0), (0.0, 1.0)],
        });
        assert_eq!(accept(&[right, left], &settings), [left]);
    }

    /// Une personne dont les pieds touchent le bas de l'image est dans une zone qui va jusqu'au
    /// bord (le point d'appui est ramené juste à l'intérieur).
    #[test]
    fn an_anchor_on_the_image_edge_is_inside_a_full_zone() {
        let settings = DetectionSettings {
            zones: vec![Polygon {
                points: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            }],
            ..DetectionSettings::default()
        };
        let edge = Detection {
            class: DetectedClass::Personne,
            score: 0.9,
            bbox: [0.9, 0.5, 1.0, 1.0],
        };
        assert_eq!(accept(&[edge], &settings).len(), 1);
    }
}
