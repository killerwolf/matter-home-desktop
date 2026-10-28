//! Historique des mesures en mémoire : tampon circulaire, fenêtres d'affichage,
//! statistiques et export CSV.

use std::collections::VecDeque;

/// Nombre maximal de points conservés par série (≈ 41 h à un point toutes les 30 s).
pub const DEFAULT_CAPACITY: usize = 5000;

/// Une mesure horodatée (secondes Unix, valeur dans l'unité de la série).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t: f64,
    pub value: f64,
}

/// Tampon circulaire : au-delà de la capacité, le point le plus ancien est éliminé.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    data: VecDeque<Sample>,
    capacity: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "la capacité doit être > 0");
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, sample: Sample) {
        if self.data.len() == self.capacity {
            self.data.pop_front();
        }
        self.data.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn last(&self) -> Option<Sample> {
        self.data.back().copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sample> {
        self.data.iter()
    }

    /// Points dont l'horodatage est ≥ `since` (les points sont ajoutés dans l'ordre).
    pub fn since(&self, since: f64) -> impl Iterator<Item = &Sample> {
        self.data.iter().filter(move |s| s.t >= since)
    }
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// Fenêtre de temps affichée dans le graphique.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeWindow {
    Min15,
    Hour1,
    Session,
}

impl TimeWindow {
    pub const ALL: [TimeWindow; 3] = [TimeWindow::Min15, TimeWindow::Hour1, TimeWindow::Session];

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::Min15 => "15 min",
            TimeWindow::Hour1 => "1 h",
            TimeWindow::Session => "Toute la session",
        }
    }

    /// Horodatage de début de la fenêtre pour l'instant `now`.
    pub fn start(self, now: f64) -> f64 {
        match self {
            TimeWindow::Min15 => now - 15.0 * 60.0,
            TimeWindow::Hour1 => now - 3600.0,
            TimeWindow::Session => f64::NEG_INFINITY,
        }
    }
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

/// Âge maximal de la dernière humidité connue pour l'associer à une température.
const HUMIDITY_MATCH_S: f64 = 120.0;

/// Export CSV : une ligne par mesure de température, avec la dernière humidité
/// connue à cet instant (si elle date de moins de 2 min).
pub fn to_csv(temperature: &RingBuffer, humidity: Option<&RingBuffer>) -> String {
    let hum: Vec<&Sample> = humidity.map(|h| h.iter().collect()).unwrap_or_default();
    let mut next = 0;
    let mut current: Option<&Sample> = None;
    let mut out = String::from("timestamp_unix,datetime_local,temperature_c,humidity_pct\n");
    for s in temperature.iter() {
        while next < hum.len() && hum[next].t <= s.t + 1.0 {
            current = Some(hum[next]);
            next += 1;
        }
        let dt = chrono::DateTime::from_timestamp(s.t as i64, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let hum = current
            .filter(|h| s.t - h.t <= HUMIDITY_MATCH_S)
            .map(|h| format!("{:.2}", h.value))
            .unwrap_or_default();
        out.push_str(&format!("{:.0},{},{:.2},{}\n", s.t, dt, s.value, hum));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, value: f64) -> Sample {
        Sample { t, value }
    }

    #[test]
    fn ring_buffer_keeps_last_points() {
        let mut rb = RingBuffer::new(3);
        assert!(rb.is_empty());
        for i in 0..5 {
            rb.push(s(i as f64, i as f64 * 10.0));
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.capacity(), 3);
        let values: Vec<f64> = rb.iter().map(|x| x.value).collect();
        assert_eq!(values, vec![20.0, 30.0, 40.0]);
        assert_eq!(rb.last(), Some(s(4.0, 40.0)));
    }

    #[test]
    fn ring_buffer_below_capacity() {
        let mut rb = RingBuffer::new(10);
        rb.push(s(1.0, 1.0));
        rb.push(s(2.0, 2.0));
        assert_eq!(rb.len(), 2);
        assert_eq!(rb.iter().next(), Some(&s(1.0, 1.0)));
    }

    #[test]
    #[should_panic]
    fn ring_buffer_zero_capacity_panics() {
        let _ = RingBuffer::new(0);
    }

    #[test]
    fn ring_buffer_since_filters_window() {
        let mut rb = RingBuffer::new(100);
        for t in 0..10 {
            rb.push(s(t as f64 * 100.0, t as f64));
        }
        let recent: Vec<f64> = rb.since(500.0).map(|x| x.value).collect();
        assert_eq!(recent, vec![5.0, 6.0, 7.0, 8.0, 9.0]);
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
    fn stats_single_and_negative_values() {
        let st = compute_stats(&[s(0.0, -4.25)]).unwrap();
        assert_eq!((st.min, st.max, st.mean, st.count), (-4.25, -4.25, -4.25, 1));

        let st = compute_stats(&[s(0.0, -2.0), s(1.0, 2.0)]).unwrap();
        assert_eq!(st.mean, 0.0);
    }

    #[test]
    fn stats_empty_is_none() {
        assert_eq!(compute_stats(&[]), None);
    }

    #[test]
    fn stats_on_time_window() {
        let mut rb = RingBuffer::new(100);
        let now = 10_000.0;
        rb.push(s(now - 7200.0, 100.0)); // hors de la fenêtre 1 h
        rb.push(s(now - 1800.0, 10.0));
        rb.push(s(now - 60.0, 20.0));
        let st = compute_stats(rb.since(TimeWindow::Hour1.start(now))).unwrap();
        assert_eq!((st.min, st.max, st.count), (10.0, 20.0, 2));
        let st = compute_stats(rb.since(TimeWindow::Min15.start(now))).unwrap();
        assert_eq!(st.count, 1);
        let st = compute_stats(rb.since(TimeWindow::Session.start(now))).unwrap();
        assert_eq!((st.max, st.count), (100.0, 3));
    }

    #[test]
    fn csv_export_format() {
        let mut t = RingBuffer::new(10);
        let mut h = RingBuffer::new(10);
        t.push(s(1_700_000_000.0, 21.5));
        h.push(s(1_700_000_000.2, 45.0));
        t.push(s(1_700_000_030.0, 21.75));
        t.push(s(1_700_000_600.0, 22.0)); // humidité trop ancienne
        let csv = to_csv(&t, Some(&h));
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "timestamp_unix,datetime_local,temperature_c,humidity_pct");
        assert!(lines[1].starts_with("1700000000,"));
        assert!(lines[1].ends_with(",21.50,45.00"));
        assert!(lines[2].ends_with(",21.75,45.00"));
        assert!(lines[3].ends_with(",22.00,"));
        assert!(to_csv(&t, None).lines().nth(1).unwrap().ends_with(",21.50,"));
    }
}
