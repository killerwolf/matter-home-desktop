//! Un passage = un événement. Le premier objet sûr (classe suivie, score suffisant, dans une zone,
//! hors masque) **ouvre** le passage ; chaque nouvelle détection le prolonge ; sans détection
//! pendant [`TrackerConfig::close_after`], il se **ferme**. On garde la meilleure image (le plus
//! haut score) pour la capture. Pur, avec l'heure en paramètre : testé sans caméra ni horloge.

use std::collections::BTreeMap;

use protocol::wire::{DetectedClass, DetectionSettings};

use super::detector::Detection;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// Secondes sans détection avant de fermer le passage (le clip garde ce temps « après »).
    pub close_after: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self { close_after: 8.0 }
    }
}

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

/// Un passage, ouvert ou terminé. `T` : la meilleure image (un JPEG en pratique).
#[derive(Debug, Clone, PartialEq)]
pub struct Passage<T> {
    pub start: f64,
    pub last: f64,
    /// Meilleur score par classe.
    pub classes: BTreeMap<DetectedClass, f32>,
    pub best: Option<(f32, T)>,
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
    /// Un passage vient de commencer (le clip démarre).
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
}

impl<T> Tracker<T> {
    pub fn new(config: TrackerConfig) -> Self {
        Self { config, open: None }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Une image analysée à `now` (s Unix), avec ses détections **déjà acceptées**. `frame` ne
    /// sert (et n'est appelée) que si l'image est la meilleure jusque-là.
    pub fn observe(&mut self, now: f64, seen: &[Detection], frame: impl FnOnce() -> T) -> Step<T> {
        if let Some(closed) = self.expire(now) {
            // Le passage précédent était fini avant cette image : il se ferme, celle-ci
            // ouvrira le suivant à l'image d'après.
            return Step::Closed(closed);
        }
        let Some(top) = seen.iter().map(|d| d.score).reduce(f32::max) else {
            return Step::Nothing;
        };
        let opened = self.open.is_none();
        let passage = self.open.get_or_insert_with(|| Passage {
            start: now,
            last: now,
            classes: BTreeMap::new(),
            best: None,
        });
        passage.last = now;
        for d in seen {
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

    /// Le temps passe sans image (ou sans détection) : ferme un passage resté sans nouvelle.
    pub fn tick(&mut self, now: f64) -> Option<Passage<T>> {
        self.expire(now)
    }

    /// Ferme tout de suite (arrêt de la surveillance).
    pub fn flush(&mut self) -> Option<Passage<T>> {
        self.open.take()
    }

    fn expire(&mut self, now: f64) -> Option<Passage<T>> {
        if self
            .open
            .as_ref()
            .is_some_and(|p| now - p.last >= self.config.close_after)
        {
            self.open.take()
        } else {
            None
        }
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
        let p = det(DetectedClass::Personne, 0.7, 0.5, 0.9);
        assert_eq!(t.observe(0.0, &[], || 0), Step::Nothing);
        assert_eq!(t.observe(1.0, &[p], || 1), Step::Opened);
        // Images sans rien pendant moins de 8 s : toujours le même passage.
        for i in 0..10 {
            let now = 1.5 + i as f64 * 0.5;
            assert!(matches!(t.observe(now, &[], || 9), Step::Nothing));
        }
        let better = det(DetectedClass::Personne, 0.9, 0.5, 0.9);
        let dog = det(DetectedClass::Chien, 0.6, 0.2, 0.9);
        assert_eq!(t.observe(7.0, &[better, dog], || 2), Step::Extended);
        assert_eq!(t.tick(14.0), None);
        let closed = t.tick(15.0).expect("fermé après 8 s sans détection");
        assert_eq!((closed.start, closed.last), (1.0, 7.0));
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
        let p = det(DetectedClass::Chat, 0.8, 0.5, 0.5);
        t.observe(0.0, &[p], || ());
        assert!(matches!(t.observe(20.0, &[p], || ()), Step::Closed(_)));
        assert_eq!(t.observe(20.5, &[p], || ()), Step::Opened);
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
}
