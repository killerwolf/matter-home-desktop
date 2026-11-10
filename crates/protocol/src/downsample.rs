//! Réduction d'une série de mesures par tranches de temps, en gardant le minimum et le
//! maximum de chaque tranche : une pointe de chauffe ou un creux nocturne ne disparaît jamais,
//! et aucune valeur n'est inventée (pas de moyenne). Utilisée par le serveur (paramètre
//! `buckets` de l'historique) et par le client (dessin des courbes).

/// Réduit `points` (couples `(instant, valeur)` triés par instant) à au plus `2 × buckets + 2`
/// points : la fenêtre `[since, until]` est découpée en `buckets` tranches égales ; chaque
/// tranche garde son minimum et son maximum, dans l'ordre du temps. Le premier et le dernier
/// point de la série sont toujours conservés. Une série déjà assez courte est rendue telle
/// quelle. Les points hors fenêtre sont rattachés à la première ou à la dernière tranche.
pub fn min_max(points: &[(f64, f64)], since: f64, until: f64, buckets: usize) -> Vec<(f64, f64)> {
    let buckets = buckets.max(1);
    if points.len() <= 2 * buckets + 2
        || until.partial_cmp(&since) != Some(std::cmp::Ordering::Greater)
    {
        return points.to_vec();
    }
    let width = (until - since) / buckets as f64;
    let index = |t: f64| -> usize {
        let i = ((t - since) / width).floor();
        if i < 0.0 {
            0
        } else {
            (i as usize).min(buckets - 1)
        }
    };
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(2 * buckets + 2);
    // `lo` / `hi` : positions (dans `points`) du minimum et du maximum de la tranche en cours.
    let mut start = 0;
    while start < points.len() {
        let bucket = index(points[start].0);
        let mut end = start + 1;
        while end < points.len() && index(points[end].0) == bucket {
            end += 1;
        }
        let (mut lo, mut hi) = (start, start);
        for i in start..end {
            if points[i].1 < points[lo].1 {
                lo = i;
            }
            if points[i].1 > points[hi].1 {
                hi = i;
            }
        }
        let mut keep = [lo, hi];
        keep.sort_unstable();
        out.push(points[keep[0]]);
        if keep[1] != keep[0] {
            out.push(points[keep[1]]);
        }
        start = end;
    }
    // Les extrémités de la série : l'état de départ et la valeur actuelle.
    if out.first() != points.first() {
        out.insert(0, points[0]);
    }
    if out.last() != points.last() {
        out.push(points[points.len() - 1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|i| (i as f64, (i % 7) as f64)).collect()
    }

    #[test]
    fn short_series_is_unchanged() {
        let p = ramp(10);
        assert_eq!(min_max(&p, 0.0, 10.0, 5), p);
        assert_eq!(min_max(&[], 0.0, 10.0, 5), Vec::<(f64, f64)>::new());
    }

    #[test]
    fn long_series_is_bounded_and_keeps_extremes() {
        // 100 000 points sur une semaine, un pic isolé et un creux isolé.
        let n = 100_000;
        let mut p: Vec<(f64, f64)> = (0..n).map(|i| (i as f64 * 6.0, 20.0)).collect();
        p[12_345].1 = 31.5;
        p[77_777].1 = -4.25;
        let out = min_max(&p, 0.0, n as f64 * 6.0, 500);
        assert!(out.len() <= 2 * 500 + 2, "{} points", out.len());
        assert!(out.iter().any(|s| s.1 == 31.5), "le pic est perdu");
        assert!(out.iter().any(|s| s.1 == -4.25), "le creux est perdu");
        assert_eq!(out.first(), p.first());
        assert_eq!(out.last(), p.last());
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "ordre du temps");
    }

    #[test]
    fn covers_the_whole_window() {
        // Contrairement à une coupe « les plus récents », toute la fenêtre est représentée.
        let p: Vec<(f64, f64)> = (0..60_000)
            .map(|i| (i as f64 * 10.0, (i % 50) as f64))
            .collect();
        let out = min_max(&p, 0.0, 600_000.0, 400);
        assert!(out.first().unwrap().0 < 10.0);
        assert!(out.last().unwrap().0 > 599_000.0);
    }

    #[test]
    fn gaps_stay_gaps() {
        // Deux paquets séparés par un trou : aucun point n'est créé dans le trou.
        let mut p: Vec<(f64, f64)> = (0..1000).map(|i| (i as f64, 1.0)).collect();
        p.extend((0..1000).map(|i| (100_000.0 + i as f64, 2.0)));
        let out = min_max(&p, 0.0, 101_000.0, 100);
        assert!(out.iter().all(|s| s.0 < 1000.0 || s.0 >= 100_000.0));
    }

    #[test]
    fn out_of_window_points_do_not_panic() {
        let p: Vec<(f64, f64)> = (0..1000).map(|i| (-50.0 + i as f64, i as f64)).collect();
        let out = min_max(&p, 0.0, 500.0, 10);
        assert!(out.len() <= 22);
        assert_eq!(min_max(&p, 5.0, 5.0, 10), p);
    }
}
