//! Page Températures : toutes les courbes sur un seul graphique. Ce module est la partie sans
//! interface : quoi lire sur le serveur, comment réduire les points avant de les dessiner, où
//! couper une courbe (trou de données) et quelle valeur afficher au survol.
//!
//! Les points viennent du serveur, réduits par tranches de temps (`buckets`, minimum et maximum
//! de chaque tranche) ; ils sont complétés en direct, puis réduits encore au dessin.

use protocol::wire::{Metric, WireMode, HISTORY_MAX_BUCKETS, HISTORY_MAX_LIMIT};

use crate::series::{DeviceSeries, Series, TimeWindow};

/// Nombre de tranches de temps demandées au serveur et gardées au dessin : de quoi remplir un
/// graphique de plusieurs centaines de pixels sans en tracer davantage.
pub const CHART_BUCKETS: usize = 800;
const _: () = assert!(CHART_BUCKETS <= HISTORY_MAX_BUCKETS);

/// Plus petit trou de données (secondes) qui coupe une courbe.
const MIN_GAP_S: f64 = 20.0 * 60.0;

/// Lecture d'un appareil pour la page : ce qui est affiché et la fenêtre pour laquelle ces
/// points ont été lus (une autre fenêtre demande une autre lecture, de résolution adaptée).
#[derive(Debug, Clone, Default)]
pub struct TempFetch {
    pub data: DeviceSeries,
    pub window: Option<TimeWindow>,
}

impl TempFetch {
    /// Faut-il (re)lire l'historique pour afficher `window` ? Pas pendant une lecture ni après
    /// un échec (« Réessayer » efface l'erreur).
    pub fn needs_fetch(&self, window: TimeWindow) -> bool {
        self.data.loading.is_none()
            && self.data.error.is_none()
            && (self.data.fetched_since.is_none() || self.window != Some(window))
    }

    /// Une relecture complète est demandée (reconnexion : les points ne sont plus fiables).
    pub fn invalidate(&mut self) {
        self.data.fetched_since = None;
    }
}

/// Chemin de la requête d'historique de température d'un appareil pour une fenêtre.
pub fn history_path(node_id: u64, window: TimeWindow, now: f64) -> String {
    format!(
        "/api/devices/{node_id}/history?since={:.0}&until={now:.0}&metrics={}&limit={HISTORY_MAX_LIMIT}&buckets={CHART_BUCKETS}",
        window.start(now),
        Metric::Temperature.key(),
    )
}

/// Trou (secondes) au-delà duquel deux mesures successives ne sont plus reliées : le plus
/// grand de 20 minutes, de 2,5 fois l'intervalle de rapport de l'appareil (un thermomètre qui
/// ne rapporte qu'à l'heure n'est pas « en panne ») et de 3 tranches du graphique.
pub fn gap_threshold(mode: Option<&WireMode>, window: TimeWindow) -> f64 {
    let report = match mode {
        Some(WireMode::Subscription { max_interval_s }) => *max_interval_s as f64,
        Some(WireMode::Polling { every_s }) => *every_s as f64,
        None => 0.0,
    };
    let bucket = window.length_s() / CHART_BUCKETS as f64;
    MIN_GAP_S.max(2.5 * report).max(3.0 * bucket)
}

/// Points à dessiner : ceux de la fenêtre `[start, now]`, réduits à la largeur du graphique.
pub fn curve_points(series: &Series, start: f64, now: f64) -> Vec<(f64, f64)> {
    let raw: Vec<(f64, f64)> = series
        .since(start)
        .filter(|s| s.value.is_finite())
        .map(|s| (s.t, s.value))
        .collect();
    protocol::downsample::min_max(&raw, start, now, CHART_BUCKETS)
}

/// Coupe une courbe là où deux mesures successives sont séparées de plus de `gap` secondes :
/// pas de ligne à travers un trou de données.
pub fn split_gaps(points: &[(f64, f64)], gap: f64) -> Vec<Vec<(f64, f64)>> {
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    for &p in points {
        match segments.last_mut() {
            Some(seg) if seg.last().is_some_and(|l| p.0 - l.0 <= gap) => seg.push(p),
            _ => segments.push(vec![p]),
        }
    }
    segments
}

/// Valeur de la courbe à l'instant `t` : celle de la dernière mesure avant `t`, tant qu'elle
/// date de moins de `gap` secondes. `None` avant la première mesure ou dans un trou.
pub fn value_at(points: &[(f64, f64)], t: f64, gap: f64) -> Option<f64> {
    let n = points.partition_point(|p| p.0 <= t);
    let (at, value) = *points.get(n.checked_sub(1)?)?;
    (t - at <= gap).then_some(value)
}

/// Nom affiché dans la légende : « Salon (Séjour) » quand la pièce est connue et différente.
pub fn display_label(name: &str, room: Option<&str>) -> String {
    match room {
        Some(room) if !room.trim().is_empty() && !name.contains(room) => {
            format!("{name} ({room})")
        }
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::Sample;
    use protocol::wire::HistoryPoint;

    fn temp(at: f64, value: f64) -> HistoryPoint {
        HistoryPoint {
            at,
            metric: Metric::Temperature,
            value,
        }
    }

    #[test]
    fn path_asks_for_temperature_with_buckets_over_the_window() {
        let p = history_path(18, TimeWindow::All, 1_000_000.0);
        assert!(p.starts_with("/api/devices/18/history?since=395200&until=1000000&"));
        assert!(p.contains("metrics=temperature"));
        assert!(p.contains("&buckets=800"));
        assert!(p.contains("&limit=20000"));
        let day = history_path(18, TimeWindow::Hour24, 1_000_000.0);
        assert!(day.contains("since=913600"));
    }

    #[test]
    fn a_new_window_or_a_resync_triggers_a_read_but_an_error_does_not_loop() {
        let mut f = TempFetch::default();
        assert!(f.needs_fetch(TimeWindow::Hour1));
        f.data.loading = Some(1);
        assert!(!f.needs_fetch(TimeWindow::Hour1), "lecture en cours");
        f.data.load(0.0, false, &[temp(1.0, 20.0)]);
        f.window = Some(TimeWindow::Hour1);
        assert!(!f.needs_fetch(TimeWindow::Hour1));
        assert!(
            f.needs_fetch(TimeWindow::All),
            "autre fenêtre : autre résolution"
        );
        f.invalidate();
        assert!(f.needs_fetch(TimeWindow::Hour1));
        f.data.error = Some("hors ligne".into());
        assert!(
            !f.needs_fetch(TimeWindow::Hour1),
            "pas de relecture en boucle"
        );
    }

    #[test]
    fn gap_follows_the_report_interval_and_the_window() {
        let sub = WireMode::Subscription {
            max_interval_s: 3600,
        };
        let poll = WireMode::Polling { every_s: 30 };
        assert_eq!(gap_threshold(None, TimeWindow::Hour1), 1200.0);
        assert_eq!(gap_threshold(Some(&poll), TimeWindow::Hour1), 1200.0);
        assert_eq!(gap_threshold(Some(&sub), TimeWindow::Hour1), 9000.0);
        // Une semaine : trois tranches de 12,6 minutes l'emportent sur 20 minutes.
        assert!(gap_threshold(None, TimeWindow::All) > 2200.0);
    }

    #[test]
    fn lines_are_cut_at_gaps_not_drawn_across_them() {
        let pts = [
            (0.0, 1.0),
            (60.0, 2.0),
            (120.0, 3.0),
            (5000.0, 4.0),
            (5060.0, 5.0),
        ];
        let segs = split_gaps(&pts, 1200.0);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].len(), 3);
        assert_eq!(segs[1], vec![(5000.0, 4.0), (5060.0, 5.0)]);
        assert!(split_gaps(&[], 1200.0).is_empty());
        assert_eq!(split_gaps(&pts[..1], 1200.0).len(), 1);
    }

    #[test]
    fn hover_value_is_the_last_sample_unless_in_a_gap() {
        let pts = [(100.0, 20.0), (200.0, 21.0), (5000.0, 25.0)];
        assert_eq!(
            value_at(&pts, 50.0, 1200.0),
            None,
            "avant la première mesure"
        );
        assert_eq!(value_at(&pts, 150.0, 1200.0), Some(20.0));
        assert_eq!(value_at(&pts, 200.0, 1200.0), Some(21.0));
        assert_eq!(value_at(&pts, 2000.0, 1200.0), None, "trou");
        assert_eq!(value_at(&pts, 6000.0, 1200.0), Some(25.0));
        assert_eq!(value_at(&[], 1.0, 1200.0), None);
    }

    #[test]
    fn curves_are_reduced_to_the_chart_width_and_keep_the_peaks() {
        let mut data = DeviceSeries::default();
        let week = 7.0 * 24.0 * 3600.0;
        let points: Vec<HistoryPoint> = (0..60_000)
            .map(|i| {
                temp(
                    i as f64 * week / 60_000.0,
                    if i == 31_337 { 40.0 } else { 20.0 },
                )
            })
            .collect();
        data.load(0.0, false, &points);
        // Une mesure en direct après la lecture s'ajoute à la fin.
        data.push_live(
            Metric::Temperature,
            Sample {
                t: week + 1.0,
                value: 22.0,
            },
        );
        let series = data.get(Metric::Temperature).unwrap();
        let drawn = curve_points(series, 0.0, week + 2.0);
        assert!(drawn.len() <= 2 * CHART_BUCKETS + 2, "{}", drawn.len());
        assert!(drawn.iter().any(|p| p.1 == 40.0));
        assert_eq!(drawn.last().unwrap().1, 22.0);
        // La fenêtre coupe : seule la dernière heure.
        let hour = curve_points(series, week - 3600.0, week + 2.0);
        assert!(hour.iter().all(|p| p.0 >= week - 3600.0));
    }

    #[test]
    fn legend_label_adds_the_room_only_when_it_tells_something() {
        assert_eq!(display_label("Sonde A", Some("Salon")), "Sonde A (Salon)");
        assert_eq!(display_label("Salon", Some("Salon")), "Salon");
        assert_eq!(display_label("Sonde A", None), "Sonde A");
        assert_eq!(display_label("Sonde A", Some("  ")), "Sonde A");
    }
}
