//! Page Températures : toutes les courbes sur un seul graphique. Ce module est la partie sans
//! interface : quoi lire sur le serveur, comment réduire les points avant de les dessiner, où
//! couper une courbe (trou de données) et quelle valeur afficher au survol.
//!
//! Les points viennent du serveur, réduits par tranches de temps (`buckets`, minimum et maximum
//! de chaque tranche) ; ils sont complétés en direct, puis réduits encore au dessin.

use protocol::wire::{Metric, WireMode, HISTORY_MAX_BUCKETS, HISTORY_MAX_LIMIT};

use crate::series::{DeviceSeries, Series, TimeWindow};

/// Bornes du nombre de tranches de temps demandées au serveur : environ une par pixel logique
/// du graphique.
pub const MIN_BUCKETS: usize = 400;
pub const MAX_BUCKETS: usize = 1600;
const _: () = assert!(MAX_BUCKETS <= HISTORY_MAX_BUCKETS);

/// Nombre de tranches pour un graphique de `width` pixels logiques : arrondi à la centaine
/// supérieure, borné.
pub fn buckets_for_width(width: f32) -> usize {
    let n = (width.max(0.0) as usize).div_ceil(200) * 200;
    n.clamp(MIN_BUCKETS, MAX_BUCKETS)
}

/// Plus petit trou de données (secondes) qui coupe une courbe.
const MIN_GAP_S: f64 = 20.0 * 60.0;

/// Lecture d'un appareil pour la page : ce qui est affiché et la fenêtre pour laquelle ces
/// points ont été lus (une autre fenêtre demande une autre lecture, de résolution adaptée).
#[derive(Debug, Clone, Default)]
pub struct TempFetch {
    pub data: DeviceSeries,
    pub window: Option<TimeWindow>,
    /// Nombre de tranches de la lecture affichée.
    pub buckets: usize,
}

impl TempFetch {
    /// Faut-il (re)lire l'historique pour afficher `window` avec `buckets` tranches ? Pas
    /// pendant une lecture ni après un échec (« Réessayer » efface l'erreur). Un graphique plus
    /// large demande une lecture plus fine ; un graphique deux fois plus étroit en demande une
    /// plus légère ; entre les deux, pas de relecture à chaque redimensionnement.
    pub fn needs_fetch(&self, window: TimeWindow, buckets: usize) -> bool {
        self.data.loading.is_none()
            && self.data.error.is_none()
            && (self.data.fetched_since.is_none()
                || self.window != Some(window)
                || buckets > self.buckets
                || buckets * 2 <= self.buckets)
    }

    /// Une relecture complète est demandée (reconnexion : les points ne sont plus fiables).
    /// Une lecture en vol est abandonnée : sa réponse ne peut plus annuler l'invalidation.
    pub fn invalidate(&mut self) {
        self.data.invalidate();
    }
}

/// Chemin de la requête d'historique de température d'un appareil pour une fenêtre.
pub fn history_path(node_id: u64, window: TimeWindow, now: f64, buckets: usize) -> String {
    format!(
        "/api/devices/{node_id}/history?since={:.0}&until={now:.0}&metrics={}&limit={HISTORY_MAX_LIMIT}&buckets={buckets}",
        window.start(now),
        Metric::Temperature.key(),
    )
}

/// Trou (secondes) au-delà duquel deux mesures successives ne sont plus reliées : le plus
/// grand de 20 minutes, de 2,5 fois l'intervalle de rapport de l'appareil (un thermomètre qui
/// ne rapporte qu'à l'heure n'est pas « en panne ») et de 3 tranches du graphique.
pub fn gap_threshold(mode: Option<&WireMode>, window: TimeWindow, buckets: usize) -> f64 {
    let report = match mode {
        Some(WireMode::Subscription { max_interval_s }) => *max_interval_s as f64,
        Some(WireMode::Polling { every_s }) => *every_s as f64,
        None => 0.0,
    };
    let bucket = window.length_s() / buckets as f64;
    MIN_GAP_S.max(2.5 * report).max(3.0 * bucket)
}

/// Points à dessiner : ceux de la fenêtre `[start, now]`, réduits à la largeur du graphique.
pub fn curve_points(series: &Series, start: f64, now: f64, buckets: usize) -> Vec<(f64, f64)> {
    let raw: Vec<(f64, f64)> = series
        .since(start)
        .filter(|s| s.value.is_finite())
        .map(|s| (s.t, s.value))
        .collect();
    protocol::downsample::min_max(&raw, start, now, buckets)
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

/// Nom court d'un thermomètre pour l'en-tête et la légende : la pièce quand elle est connue,
/// sinon le nom de l'appareil sans sa parenthèse ni son complément (« Chambre — capteur endormi
/// (démo) » devient « Chambre »).
pub fn short_label(name: &str, room: Option<&str>) -> String {
    if let Some(room) = room.map(str::trim).filter(|r| !r.is_empty()) {
        return room.to_string();
    }
    let cut = [" (", " — "]
        .iter()
        .filter_map(|sep| name.find(sep))
        .min()
        .unwrap_or(name.len());
    let short = name[..cut].trim();
    if short.is_empty() { name } else { short }.to_string()
}

/// Plus bas et plus haut d'une courbe sur la période affichée (points finis seulement).
pub fn period_range(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    points
        .iter()
        .map(|p| p.1)
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

/// Écart (s) entre les repères de l'axe du temps : des heures rondes, et peu de repères.
pub fn axis_step(span_s: f64) -> f64 {
    if span_s <= 2.0 * 3600.0 {
        15.0 * 60.0
    } else if span_s <= 12.0 * 3600.0 {
        3600.0
    } else if span_s <= 36.0 * 3600.0 {
        6.0 * 3600.0
    } else {
        86_400.0
    }
}

/// Repères de l'axe entre `start` et `end` (secondes Unix) : les multiples du pas à l'heure
/// locale (`offset_s` = écart du fuseau à UTC), avec leur pas.
pub fn round_marks(start: f64, end: f64, offset_s: i64) -> Vec<(f64, f64)> {
    let step = axis_step(end - start);
    let offset = offset_s as f64;
    let mut t = ((start + offset) / step).ceil() * step - offset;
    let mut marks = Vec::new();
    while t <= end {
        marks.push((t, step));
        t += step;
    }
    marks
}

/// Texte d'un repère : « 18 h », « 0 h » ; « 14:15 » pour un pas de quinze minutes ; « lun. 04/11 »
/// pour un pas d'un jour.
pub fn axis_label(t: f64, step: f64, offset_s: i64) -> String {
    use chrono::{Datelike, Timelike};
    let Some(d) = chrono::DateTime::from_timestamp(t as i64 + offset_s, 0) else {
        return String::new();
    };
    if step >= 86_400.0 {
        const DAYS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];
        format!(
            "{} {:02}/{:02}",
            DAYS[d.weekday().num_days_from_monday() as usize],
            d.day(),
            d.month()
        )
    } else if step >= 3600.0 {
        format!("{} h", d.hour())
    } else {
        format!("{:02}:{:02}", d.hour(), d.minute())
    }
}

/// Écart du fuseau local à UTC à l'instant `t`.
pub fn local_offset_s(t: f64) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::Local
        .timestamp_opt(t as i64, 0)
        .single()
        .map_or(0, |d| i64::from(d.offset().fix().local_minus_utc()))
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn short_labels_prefer_the_room() {
        assert_eq!(short_label("Salon (démo)", Some("Salon")), "Salon");
        assert_eq!(
            short_label("Chambre — capteur endormi (démo)", None),
            "Chambre"
        );
        assert_eq!(short_label("Sonde", Some("  ")), "Sonde");
        assert_eq!(short_label("(démo)", None), "(démo)");
    }

    #[test]
    fn period_range_is_min_and_max_of_the_points() {
        assert_eq!(period_range(&[]), None);
        assert_eq!(
            period_range(&[(0.0, 19.0), (1.0, 21.4), (2.0, 18.1), (3.0, f64::NAN)]),
            Some((18.1, 21.4))
        );
    }

    #[test]
    fn marks_fall_on_round_local_hours() {
        // 24 h : un repère toutes les 6 h, à 0 h, 6 h, 12 h, 18 h heure locale (UTC+1 ici).
        let offset = 3600;
        let end = 1_700_000_000.0;
        let marks = round_marks(end - 86_400.0, end, offset);
        assert!(marks.len() == 4 || marks.len() == 5);
        for (t, step) in &marks {
            assert_eq!(*step, 6.0 * 3600.0);
            let local = *t + 3600.0;
            assert_eq!(local % (6.0 * 3600.0), 0.0, "{t} n'est pas une heure ronde");
        }
        let labels: Vec<String> = marks.iter().map(|m| axis_label(m.0, m.1, offset)).collect();
        assert!(labels
            .iter()
            .all(|l| ["0 h", "6 h", "12 h", "18 h"].contains(&l.as_str())));
        // 1 h : tous les quarts d'heure ; 7 jours : à minuit.
        let hour = round_marks(end - 3600.0, end, 0);
        assert!(hour.iter().all(|m| m.0 % 900.0 == 0.0 && m.1 == 900.0));
        assert_eq!(axis_label(hour[0].0, 900.0, 0).len(), 5);
        let week = round_marks(end - 7.0 * 86_400.0, end, 0);
        assert_eq!(week.len(), 7);
        assert!(week.iter().all(|m| m.0 % 86_400.0 == 0.0));
        assert_eq!(axis_label(week[0].0, 86_400.0, 0), "mer. 08/11");
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
        let p = history_path(18, TimeWindow::All, 1_000_000.0, 800);
        assert!(p.starts_with("/api/devices/18/history?since=395200&until=1000000&"));
        assert!(p.contains("metrics=temperature"));
        assert!(p.contains("&buckets=800"));
        assert!(p.contains("&limit=20000"));
        let day = history_path(18, TimeWindow::Hour24, 1_000_000.0, 800);
        assert!(day.contains("since=913600"));
    }

    #[test]
    fn a_new_window_or_a_resync_triggers_a_read_but_an_error_does_not_loop() {
        let mut f = TempFetch::default();
        assert!(f.needs_fetch(TimeWindow::Hour1, 800));
        f.data.loading = Some(1);
        assert!(!f.needs_fetch(TimeWindow::Hour1, 800), "lecture en cours");
        f.data.load(0.0, false, &[temp(1.0, 20.0)]);
        f.window = Some(TimeWindow::Hour1);
        f.buckets = 800;
        assert!(!f.needs_fetch(TimeWindow::Hour1, 800));
        assert!(
            f.needs_fetch(TimeWindow::All, 800),
            "autre fenêtre : autre résolution"
        );
        // Redimensionnement : relecture seulement si le graphique grandit ou devient deux fois
        // plus étroit.
        assert!(!f.needs_fetch(TimeWindow::Hour1, 600));
        assert!(f.needs_fetch(TimeWindow::Hour1, 1000));
        assert!(f.needs_fetch(TimeWindow::Hour1, 400));
        f.invalidate();
        assert!(f.needs_fetch(TimeWindow::Hour1, 800));
        f.data.error = Some("hors ligne".into());
        assert!(
            !f.needs_fetch(TimeWindow::Hour1, 800),
            "pas de relecture en boucle"
        );
        // Une invalidation efface l'erreur et abandonne la lecture en vol.
        f.data.loading = Some(9);
        f.invalidate();
        assert_eq!((f.data.loading, f.data.error.is_none()), (None, true));
    }

    #[test]
    fn gap_follows_the_report_interval_and_the_window() {
        let sub = WireMode::Subscription {
            max_interval_s: 3600,
        };
        let poll = WireMode::Polling { every_s: 30 };
        assert_eq!(gap_threshold(None, TimeWindow::Hour1, 800), 1200.0);
        assert_eq!(gap_threshold(Some(&poll), TimeWindow::Hour1, 800), 1200.0);
        assert_eq!(gap_threshold(Some(&sub), TimeWindow::Hour1, 800), 9000.0);
        // Une semaine : trois tranches de 12,6 minutes l'emportent sur 20 minutes.
        assert!(gap_threshold(None, TimeWindow::All, 800) > 2200.0);
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
        let drawn = curve_points(series, 0.0, week + 2.0, 800);
        assert!(drawn.len() <= 2 * 800 + 2, "{}", drawn.len());
        assert!(drawn.iter().any(|p| p.1 == 40.0));
        assert_eq!(drawn.last().unwrap().1, 22.0);
        // La fenêtre coupe : seule la dernière heure.
        let hour = curve_points(series, week - 3600.0, week + 2.0, 800);
        assert!(hour.iter().all(|p| p.0 >= week - 3600.0));
    }

    #[test]
    fn bucket_count_follows_the_plot_width() {
        assert_eq!(buckets_for_width(100.0), MIN_BUCKETS);
        assert_eq!(buckets_for_width(1160.0), 1200);
        assert_eq!(buckets_for_width(1201.0), 1400);
        assert_eq!(buckets_for_width(5000.0), MAX_BUCKETS);
        assert_eq!(buckets_for_width(f32::NAN), MIN_BUCKETS);
    }

    #[test]
    fn legend_label_adds_the_room_only_when_it_tells_something() {
        assert_eq!(display_label("Sonde A", Some("Salon")), "Sonde A (Salon)");
        assert_eq!(display_label("Salon", Some("Salon")), "Salon");
        assert_eq!(display_label("Sonde A", None), "Sonde A");
        assert_eq!(display_label("Sonde A", Some("  ")), "Sonde A");
    }
}
