//! Séries de mesures affichées dans les graphiques.
//!
//! Le client ne garde pas d'historique : une série est lue à la demande auprès du serveur
//! (`GET /api/devices/{id}/history`) quand une fiche s'ouvre ou que la fenêtre change, complétée
//! en direct par les mesures reçues, et **vidée à la déconnexion**.

use std::collections::BTreeMap;

use protocol::wire::{HistoryPoint, Metric, HISTORY_MAX_WINDOW_S};

/// Nombre maximal de points gardés par grandeur (garde-fou pour une fiche ouverte longtemps).
pub const MAX_POINTS: usize = 40_000;

/// Une mesure horodatée (secondes Unix, valeur dans l'unité de la grandeur).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t: f64,
    pub value: f64,
}

/// Points d'une grandeur, du plus ancien au plus récent.
#[derive(Debug, Clone, Default)]
pub struct Series {
    points: Vec<Sample>,
}

impl Series {
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn last(&self) -> Option<Sample> {
        self.points.last().copied()
    }

    pub fn first(&self) -> Option<Sample> {
        self.points.first().copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sample> {
        self.points.iter()
    }

    /// Points dont l'horodatage est ≥ `since`.
    pub fn since(&self, since: f64) -> impl Iterator<Item = &Sample> {
        let start = self.points.partition_point(|s| s.t < since);
        self.points[start..].iter()
    }

    /// Dernier point strictement avant `t` (valeur qui « tient » encore au début d'une fenêtre).
    pub fn before(&self, t: f64) -> Option<Sample> {
        let n = self.points.partition_point(|s| s.t < t);
        n.checked_sub(1).map(|i| self.points[i])
    }

    /// Ajoute une mesure reçue en direct. Une mesure plus ancienne que la dernière est
    /// ignorée (rejeu, message en retard). Rend `true` si elle a été ajoutée.
    pub fn push_live(&mut self, sample: Sample) -> bool {
        if self.points.last().is_some_and(|last| sample.t < last.t) {
            return false;
        }
        self.points.push(sample);
        if self.points.len() > MAX_POINTS {
            let extra = self.points.len() - MAX_POINTS;
            self.points.drain(..extra);
        }
        true
    }
}

/// Lecture d'historique d'un appareil : une série par grandeur.
#[derive(Debug, Clone, Default)]
pub struct DeviceSeries {
    series: BTreeMap<Metric, Series>,
    /// Début de la fenêtre lue (secondes Unix). `None` : rien n'a encore été lu.
    pub fetched_since: Option<f64>,
    /// Numéro de la requête en cours.
    pub loading: Option<u64>,
    /// Dernier échec de lecture, en français.
    pub error: Option<String>,
    /// Le serveur a tronqué la réponse : seuls les points les plus récents sont là.
    pub truncated: bool,
}

impl DeviceSeries {
    pub fn get(&self, metric: Metric) -> Option<&Series> {
        self.series.get(&metric)
    }

    pub fn len(&self, metric: Metric) -> usize {
        self.series.get(&metric).map_or(0, Series::len)
    }

    /// Remplace le contenu par une lecture du serveur.
    pub fn load(&mut self, since: f64, truncated: bool, points: &[HistoryPoint]) {
        self.series.clear();
        let mut sorted: Vec<&HistoryPoint> = points.iter().collect();
        sorted.sort_by(|a, b| a.at.total_cmp(&b.at));
        for p in sorted {
            self.series
                .entry(p.metric)
                .or_default()
                .points
                .push(Sample {
                    t: p.at,
                    value: p.value,
                });
        }
        self.fetched_since = Some(since);
        self.truncated = truncated;
        self.error = None;
        self.loading = None;
    }

    /// Complète une série déjà lue avec une mesure en direct. Sans lecture, ou pendant
    /// qu'elle est en cours, rien n'est ajouté : la lecture la contiendra.
    pub fn push_live(&mut self, metric: Metric, sample: Sample) {
        if self.fetched_since.is_some() && self.loading.is_none() {
            self.series.entry(metric).or_default().push_live(sample);
        }
    }

    /// Changements d'état (ouverture / fermeture) d'un capteur de porte : l'état d'un point
    /// qui diffère du précédent. Le premier point sert de référence.
    pub fn transitions(&self, metric: Metric) -> Vec<(f64, bool)> {
        let Some(series) = self.series.get(&metric) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut previous: Option<bool> = None;
        for s in series.iter() {
            let state = s.value != 0.0;
            if previous.is_some_and(|p| p != state) {
                out.push((s.t, state));
            }
            previous = Some(state);
        }
        out
    }
}

/// Fenêtre de temps affichée dans le graphique.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeWindow {
    Min15,
    Hour1,
    Hour24,
    /// Tout ce que le serveur garde (7 jours).
    All,
}

impl TimeWindow {
    pub const ALL: [TimeWindow; 4] = [
        TimeWindow::Min15,
        TimeWindow::Hour1,
        TimeWindow::Hour24,
        TimeWindow::All,
    ];
    /// Les trois périodes de la page Températures.
    pub const OVERVIEW: [TimeWindow; 3] = [TimeWindow::Hour1, TimeWindow::Hour24, TimeWindow::All];

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::Min15 => "15 min",
            TimeWindow::Hour1 => "1 h",
            TimeWindow::Hour24 => "24 h",
            TimeWindow::All => "7 jours",
        }
    }

    /// Libellé du sélecteur de la page Températures.
    pub fn overview_label(self) -> &'static str {
        match self {
            TimeWindow::All => "1 semaine",
            other => other.label(),
        }
    }

    /// Longueur de la fenêtre, en secondes.
    pub fn length_s(self) -> f64 {
        match self {
            TimeWindow::Min15 => 15.0 * 60.0,
            TimeWindow::Hour1 => 3600.0,
            TimeWindow::Hour24 => 24.0 * 3600.0,
            TimeWindow::All => HISTORY_MAX_WINDOW_S as f64,
        }
    }

    /// Horodatage de début de la fenêtre pour l'instant `now`.
    pub fn start(self, now: f64) -> f64 {
        now - self.length_s()
    }

    /// Début de la lecture à demander au serveur pour afficher cette fenêtre : les fenêtres
    /// courtes partagent une même lecture d'une heure, la longue lit tout.
    pub fn fetch_start(self, now: f64) -> f64 {
        match self {
            TimeWindow::Min15 | TimeWindow::Hour1 => now - TimeWindow::Hour1.length_s(),
            TimeWindow::Hour24 => now - TimeWindow::Hour24.length_s(),
            TimeWindow::All => now - TimeWindow::All.length_s(),
        }
    }
}

/// Faut-il lire (ou relire) l'historique pour afficher `window` ? Oui s'il n'a jamais été lu
/// ou si la lecture existante commence trop tard. Une lecture plus large que nécessaire suffit.
pub fn needs_fetch(fetched_since: Option<f64>, window: TimeWindow, now: f64) -> bool {
    match fetched_since {
        None => true,
        // Marge : l'horloge avance pendant que la lecture est affichée.
        Some(since) => since > window.start(now) + 300.0,
    }
}

/// Grandeurs à lire pour les graphiques d'un appareil, d'après ce qu'il sait faire.
pub fn metrics_for(caps: &protocol::events::Capabilities) -> Vec<Metric> {
    let mut m = Vec::new();
    if caps.temperature {
        m.push(Metric::Temperature);
    }
    if caps.humidity {
        m.push(Metric::Humidity);
    }
    if caps.contact {
        m.push(Metric::Open);
    }
    if caps.power {
        m.push(Metric::Power);
    }
    if caps.cover_position {
        m.push(Metric::Position);
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub count: usize,
}

/// Min / max / moyenne d'une suite de points ; `None` si elle est vide.
pub fn compute_stats<'a>(samples: impl IntoIterator<Item = &'a Sample>) -> Option<Stats> {
    let mut count = 0usize;
    let mut sum = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for s in samples {
        count += 1;
        sum += s.value;
        min = min.min(s.value);
        max = max.max(s.value);
    }
    (count > 0).then(|| Stats {
        min,
        max,
        mean: sum / count as f64,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, value: f64) -> Sample {
        Sample { t, value }
    }

    fn point(at: f64, metric: Metric, value: f64) -> HistoryPoint {
        HistoryPoint { at, metric, value }
    }

    #[test]
    fn load_sorts_groups_and_marks_the_window() {
        let mut d = DeviceSeries {
            loading: Some(3),
            ..Default::default()
        };
        d.load(
            100.0,
            true,
            &[
                point(30.0, Metric::Temperature, 3.0),
                point(10.0, Metric::Temperature, 1.0),
                point(20.0, Metric::Humidity, 50.0),
            ],
        );
        assert_eq!(
            (d.loading, d.fetched_since, d.truncated),
            (None, Some(100.0), true)
        );
        let t: Vec<f64> = d
            .get(Metric::Temperature)
            .unwrap()
            .iter()
            .map(|p| p.value)
            .collect();
        assert_eq!(t, vec![1.0, 3.0]);
        assert_eq!(d.len(Metric::Humidity), 1);
        assert_eq!(d.len(Metric::Power), 0);
    }

    #[test]
    fn live_points_are_added_only_after_a_load_and_never_out_of_order() {
        let mut d = DeviceSeries::default();
        d.push_live(Metric::Power, s(1.0, 5.0));
        assert_eq!(d.len(Metric::Power), 0, "pas de lecture : rien à compléter");
        d.load(0.0, false, &[point(10.0, Metric::Power, 1.0)]);
        d.push_live(Metric::Power, s(12.0, 2.0));
        d.push_live(Metric::Power, s(5.0, 9.0)); // plus ancien : ignoré
        d.push_live(Metric::Power, s(12.0, 2.0)); // même instant : accepté
        assert_eq!(d.len(Metric::Power), 3);
        d.loading = Some(1);
        d.push_live(Metric::Power, s(20.0, 3.0));
        assert_eq!(
            d.len(Metric::Power),
            3,
            "lecture en cours : la lecture contiendra le point"
        );
    }

    #[test]
    fn series_are_bounded() {
        let mut series = Series::default();
        for i in 0..(MAX_POINTS + 10) {
            assert!(series.push_live(s(i as f64, 0.0)));
        }
        assert_eq!(series.len(), MAX_POINTS);
        assert_eq!(series.first().unwrap().t, 10.0);
    }

    #[test]
    fn since_and_before_use_the_time_order() {
        let mut series = Series::default();
        for t in 0..10 {
            series.push_live(s(t as f64 * 100.0, t as f64));
        }
        let recent: Vec<f64> = series.since(500.0).map(|x| x.value).collect();
        assert_eq!(recent, vec![5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(series.before(500.0).map(|x| x.value), Some(4.0));
        assert_eq!(series.before(0.0), None);
    }

    #[test]
    fn door_transitions_ignore_the_baseline_and_repeats() {
        let mut d = DeviceSeries::default();
        d.load(
            0.0,
            false,
            &[
                point(1.0, Metric::Open, 1.0),
                point(2.0, Metric::Open, 1.0),
                point(3.0, Metric::Open, 0.0),
                point(4.0, Metric::Open, 1.0),
                point(5.0, Metric::Open, 0.0),
            ],
        );
        assert_eq!(
            d.transitions(Metric::Open),
            vec![(3.0, false), (4.0, true), (5.0, false)]
        );
        assert!(d.transitions(Metric::OnOff).is_empty());
    }

    #[test]
    fn fetch_decisions() {
        let now = 1_000_000.0;
        assert!(needs_fetch(None, TimeWindow::Min15, now));
        let hour = TimeWindow::Hour1.fetch_start(now);
        assert!(!needs_fetch(Some(hour), TimeWindow::Min15, now));
        assert!(!needs_fetch(Some(hour), TimeWindow::Hour1, now));
        assert!(needs_fetch(Some(hour), TimeWindow::All, now));
        assert!(needs_fetch(Some(hour), TimeWindow::Hour24, now));
        let day = TimeWindow::Hour24.fetch_start(now);
        assert!(!needs_fetch(Some(day), TimeWindow::Hour24, now));
        assert!(!needs_fetch(Some(day), TimeWindow::Hour1, now));
        assert!(needs_fetch(Some(day), TimeWindow::All, now));
        assert_eq!(TimeWindow::Hour24.length_s(), 86_400.0);
        assert_eq!(TimeWindow::All.overview_label(), "1 semaine");
        assert_eq!(TimeWindow::Hour24.overview_label(), "24 h");
        let all = TimeWindow::All.fetch_start(now);
        assert!(!needs_fetch(Some(all), TimeWindow::All, now + 200.0));
        assert!(!needs_fetch(Some(all), TimeWindow::Hour1, now));
        assert_eq!(TimeWindow::All.length_s(), HISTORY_MAX_WINDOW_S as f64);
    }

    #[test]
    fn metrics_follow_capabilities() {
        use protocol::events::Capabilities;
        let caps = Capabilities {
            temperature: true,
            humidity: true,
            battery: true,
            ..Default::default()
        };
        assert_eq!(
            metrics_for(&caps),
            vec![Metric::Temperature, Metric::Humidity]
        );
        let cover = Capabilities {
            cover: true,
            cover_position: false,
            ..Default::default()
        };
        assert!(metrics_for(&cover).is_empty());
    }

    #[test]
    fn stats_min_max_mean() {
        let data = [s(0.0, 21.5), s(1.0, 19.0), s(2.0, 23.5), s(3.0, 20.0)];
        let st = compute_stats(&data).unwrap();
        assert_eq!(st.min, 19.0);
        assert_eq!(st.max, 23.5);
        assert!((st.mean - 21.0).abs() < 1e-9);
        assert_eq!(st.count, 4);
    }

    #[test]
    fn stats_single_negative_and_empty() {
        let st = compute_stats(&[s(0.0, -4.25)]).unwrap();
        assert_eq!(
            (st.min, st.max, st.mean, st.count),
            (-4.25, -4.25, -4.25, 1)
        );
        let st = compute_stats(&[s(0.0, -2.0), s(1.0, 2.0)]).unwrap();
        assert_eq!(st.mean, 0.0);
        assert_eq!(compute_stats(&[]), None);
    }

    #[test]
    fn stats_on_time_window() {
        let mut series = Series::default();
        let now = 10_000.0;
        series.push_live(s(now - 7200.0, 100.0));
        series.push_live(s(now - 1800.0, 10.0));
        series.push_live(s(now - 60.0, 20.0));
        let st = compute_stats(series.since(TimeWindow::Hour1.start(now))).unwrap();
        assert_eq!((st.min, st.max, st.count), (10.0, 20.0, 2));
        let st = compute_stats(series.since(TimeWindow::Min15.start(now))).unwrap();
        assert_eq!(st.count, 1);
        let st = compute_stats(series.since(TimeWindow::All.start(now))).unwrap();
        assert_eq!((st.max, st.count), (100.0, 3));
    }
}
