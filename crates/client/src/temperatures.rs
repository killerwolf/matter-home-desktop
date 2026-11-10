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

/// Un morceau continu de la ligne : la moyenne par tranche, et pour chaque tranche le minimum et
/// le maximum (la bande pâle sous la ligne, pour que les pics restent visibles).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineSegment {
    /// `(instant, moyenne)`.
    pub mean: Vec<(f64, f64)>,
    /// `(instant, minimum, maximum)`.
    pub band: Vec<(f64, f64, f64)>,
}

/// La **ligne** d'une courbe : un point par tranche d'environ trois pixels, à la moyenne des
/// mesures de la tranche (une seule densité sur toute la période : les points minimum et maximum
/// de `curve_points` donnaient des barres épaisses là où les mesures sont serrées et un trait fin
/// ailleurs). Elle est coupée sur les **tranches vides** : seule une plage de tranches sans
/// aucune mesure, plus longue que `gap_s`, est un vrai trou. Le décalage d'un instant moyen d'une
/// tranche à l'autre n'en fait jamais un.
pub fn line_segments(
    series: &Series,
    start: f64,
    now: f64,
    buckets: usize,
    gap_s: f64,
) -> Vec<LineSegment> {
    let n = (buckets / 3).max(1);
    let span = now - start;
    if span <= 0.0 {
        return Vec::new();
    }
    let width = span / n as f64;
    #[derive(Clone, Copy)]
    struct Cell {
        t: f64,
        v: f64,
        lo: f64,
        hi: f64,
        count: u32,
    }
    let mut cells = vec![
        Cell {
            t: 0.0,
            v: 0.0,
            lo: f64::INFINITY,
            hi: f64::NEG_INFINITY,
            count: 0,
        };
        n
    ];
    for s in series.since(start).filter(|s| s.value.is_finite()) {
        let i = (((s.t - start) / span) * n as f64)
            .floor()
            .clamp(0.0, (n - 1) as f64) as usize;
        let c = &mut cells[i];
        c.t += s.t;
        c.v += s.value;
        c.lo = c.lo.min(s.value);
        c.hi = c.hi.max(s.value);
        c.count += 1;
    }
    let mut segments: Vec<LineSegment> = Vec::new();
    let mut empty_run = 0_usize;
    for c in cells {
        if c.count == 0 {
            empty_run += 1;
            continue;
        }
        let k = f64::from(c.count);
        if segments.is_empty() || empty_run as f64 * width > gap_s {
            segments.push(LineSegment::default());
        }
        empty_run = 0;
        if let Some(seg) = segments.last_mut() {
            seg.mean.push((c.t / k, c.v / k));
            seg.band.push((c.t / k, c.lo, c.hi));
        }
    }
    segments
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
/// locale, avec leur pas. `offset_at(t)` donne l'écart du fuseau à UTC à l'instant `t` : il est
/// relu pour chaque repère, de sorte qu'un changement d'heure en milieu de période tombe juste.
pub fn round_marks(start: f64, end: f64, offset_at: &dyn Fn(f64) -> i64) -> Vec<(f64, f64)> {
    let step = axis_step(end - start);
    let first_offset = offset_at(start) as f64;
    let mut k = ((start + first_offset) / step).ceil();
    let mut marks = Vec::new();
    loop {
        // Heure locale voulue, convertie avec l'écart en vigueur à l'instant trouvé. Une heure
        // locale qui n'existe pas (saut du changement d'heure) n'a pas de repère.
        let local = k * step;
        let first = local - offset_at(local - first_offset) as f64;
        let second = local - offset_at(first) as f64;
        let found = [second, first]
            .into_iter()
            .find(|t| *t + offset_at(*t) as f64 == local);
        if found.unwrap_or(first) > end {
            break;
        }
        if let Some(t) = found.filter(|t| *t >= start) {
            marks.push((t, step));
        }
        k += 1.0;
    }
    marks
}

/// Repères de l'axe vertical (°C) entre `lo` et `hi` : un tous les 0,5, 1, 2, 5 ou 10 degrés, au
/// plus petit pas qui laisse sept repères au plus. Le pas sert de « taille » du repère : une
/// valeur inférieure à 1 appelle une décimale (voir `y_label`).
pub fn y_marks(lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let span = (hi - lo).abs();
    if !span.is_finite() || span == 0.0 {
        return Vec::new();
    }
    let step = [0.5, 1.0, 2.0, 5.0, 10.0, 20.0]
        .into_iter()
        .find(|s| span / s <= 7.0)
        .unwrap_or(50.0);
    let mut v = (lo.min(hi) / step).ceil() * step;
    let mut marks = Vec::new();
    while v <= hi.max(lo) {
        marks.push((v, step));
        v += step;
    }
    marks
}

/// « 21 °C », ou « 20,5 °C » quand le pas des repères est inférieur à un degré.
pub fn y_label(value: f64, step: f64) -> String {
    if step < 1.0 {
        format!("{value:.1} °C").replace('.', ",")
    } else {
        format!("{value:.0} °C")
    }
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

    fn series_of(points: &[(f64, f64)]) -> DeviceSeries {
        use protocol::wire::HistoryPoint;
        let mut data = DeviceSeries::default();
        let pts: Vec<HistoryPoint> = points
            .iter()
            .map(|&(at, value)| HistoryPoint {
                at,
                metric: Metric::Temperature,
                value,
            })
            .collect();
        data.load(0.0, false, &pts);
        data
    }

    #[test]
    fn the_line_has_one_density_over_the_whole_period_and_keeps_real_gaps() {
        // 24 h : une mesure toutes les 5 minutes les 22 premières heures, puis toutes les 2
        // secondes sur la dernière demi-heure.
        let (start, now) = (0.0, 86_400.0);
        let mut raw = Vec::new();
        let mut t = 0.0;
        while t < 79_200.0 {
            raw.push((t, 20.0 + (t / 300.0) % 2.0));
            t += 300.0;
        }
        let mut t = 84_600.0;
        while t < now {
            raw.push((t, 21.0 + (t % 3.0)));
            t += 2.0;
        }
        let data = series_of(&raw);
        let series = data.get(Metric::Temperature).unwrap();
        let buckets = 900;
        let gap = gap_threshold(None, TimeWindow::Hour24, buckets);
        let segments = line_segments(series, start, now, buckets, gap);
        // Pas plus d'un point par tranche, donc la zone dense n'est pas plus épaisse ; un trou
        // de 1 h 30 sans mesure (de 79 200 s à 84 600 s) coupe la ligne.
        let bucket = (now - start) / 300.0;
        let mut per_bucket = std::collections::HashMap::new();
        for p in segments.iter().flat_map(|s| &s.mean) {
            *per_bucket.entry((p.0 / bucket) as usize).or_insert(0) += 1;
        }
        assert!(per_bucket.values().all(|n| *n == 1));
        assert_eq!(segments.len(), 2, "un trou réel coupe la ligne en deux");
        for seg in &segments {
            assert_eq!(seg.mean.len(), seg.band.len());
            assert!(seg.band.iter().all(|b| b.1 <= b.2));
        }
        // La bande suit les extrêmes de chaque tranche : la zone dense va de 21 à 24 degrés.
        let dense = segments.last().unwrap();
        assert!(dense.band.iter().all(|b| b.1 >= 21.0 && b.2 < 24.0));
        // Période vide : rien.
        assert!(line_segments(series, 10.0, 10.0, 900, gap).is_empty());
    }

    #[test]
    fn a_week_of_continuous_data_is_one_segment_and_a_real_gap_makes_two() {
        // 7 jours, 900 tranches : la tranche de la ligne dure 33,6 minutes (le même seuil de
        // trou qu'avant en vaut 33,6) ; une mesure toutes les 10 minutes, dont l'instant moyen
        // d'une tranche à l'autre varie, ne doit jamais couper la ligne.
        let (start, now) = (0.0, 7.0 * 86_400.0);
        let mut continuous = Vec::new();
        let mut t = 100.0;
        while t < now {
            continuous.push((t, 18.0 + (t / 3600.0_f64).sin()));
            t += 600.0;
        }
        let gap = gap_threshold(None, TimeWindow::All, 900);
        let data = series_of(&continuous);
        let one = line_segments(data.get(Metric::Temperature).unwrap(), start, now, 900, gap);
        assert_eq!(one.len(), 1, "des données continues sont une seule ligne");

        // Un vrai trou de 3 h : deux morceaux.
        let with_gap: Vec<(f64, f64)> = continuous
            .iter()
            .copied()
            .filter(|p| !(300_000.0..310_800.0).contains(&p.0))
            .collect();
        let data = series_of(&with_gap);
        let two = line_segments(data.get(Metric::Temperature).unwrap(), start, now, 900, gap);
        assert_eq!(two.len(), 2, "un trou de trois heures coupe la ligne");
        // Sur une heure avec un rapport toutes les 5 minutes, aucune coupure.
        let hourly: Vec<(f64, f64)> = (0..12).map(|i| (f64::from(i) * 300.0, 20.0)).collect();
        let data = series_of(&hourly);
        let gap = gap_threshold(None, TimeWindow::Hour1, 900);
        let one = line_segments(
            data.get(Metric::Temperature).unwrap(),
            0.0,
            3600.0,
            900,
            gap,
        );
        assert_eq!(one.len(), 1);
    }

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
    fn marks_follow_a_clock_change_in_the_middle_of_the_period() {
        // Le fuseau passe de UTC+1 à UTC+2 à mi-période : chaque repère est une heure ronde
        // locale avec l'écart en vigueur à cet instant.
        let end = 1_700_000_000.0;
        let change = end - 12.0 * 3600.0;
        let offset_at = |t: f64| if t < change { 3600 } else { 7200 };
        let marks = round_marks(end - 86_400.0, end, &offset_at);
        assert!(marks.len() >= 4);
        for (t, step) in &marks {
            let local = *t + offset_at(*t) as f64;
            assert_eq!(local % *step, 0.0, "{t} n'est pas une heure ronde locale");
        }
        // Avec un écart fixe d'une heure, les mêmes instants ne seraient pas tous ronds.
        let after: Vec<_> = marks.iter().filter(|m| m.0 >= change).collect();
        assert!(after.iter().all(|m| (m.0 + 7200.0) % (6.0 * 3600.0) == 0.0));
    }

    #[test]
    fn y_marks_are_one_or_two_degrees_apart_with_decimals_only_when_needed() {
        let marks = y_marks(18.2, 21.7);
        assert!(marks.iter().all(|m| m.0 == m.0.round() || m.1 < 1.0));
        assert!(marks.len() >= 3 && marks.len() <= 8, "{marks:?}");
        // Vue d'une heure, étroite : pas de demi-degré, chaque libellé est différent.
        let narrow = y_marks(20.8, 21.4);
        assert_eq!(narrow.first().map(|m| m.1), Some(0.5));
        let labels: Vec<String> = narrow.iter().map(|m| y_label(m.0, m.1)).collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(labels, {
            let mut l = labels.clone();
            l.dedup();
            l
        });
        assert_eq!(labels.len(), unique.len());
        assert_eq!(y_label(21.0, 1.0), "21 °C");
        assert_eq!(y_label(20.5, 0.5), "20,5 °C");
        assert!(y_marks(5.0, 5.0).is_empty());
        let wide = y_marks(10.0, 30.0);
        assert!(wide.iter().all(|m| m.1 >= 5.0) && wide.len() <= 8);
    }

    #[test]
    fn marks_fall_on_round_local_hours() {
        // 24 h : un repère toutes les 6 h, à 0 h, 6 h, 12 h, 18 h heure locale (UTC+1 ici).
        let offset = 3600;
        let end = 1_700_000_000.0;
        let marks = round_marks(end - 86_400.0, end, &|_| offset);
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
        let hour = round_marks(end - 3600.0, end, &|_| 0);
        assert!(hour.iter().all(|m| m.0 % 900.0 == 0.0 && m.1 == 900.0));
        assert_eq!(axis_label(hour[0].0, 900.0, 0).len(), 5);
        let week = round_marks(end - 7.0 * 86_400.0, end, &|_| 0);
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
