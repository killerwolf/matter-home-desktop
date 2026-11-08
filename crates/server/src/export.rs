//! Export CSV d'un appareil, produit à la demande par le serveur.

use protocol::wire::{HistoryPoint, Metric};

/// Âge maximal de la dernière humidité connue pour l'associer à une température.
const HUMIDITY_MATCH_S: f64 = 120.0;

/// Une ligne par mesure de température, avec la dernière humidité connue à cet instant
/// (si elle date de moins de 2 min). `datetime_local` est l'heure locale du serveur.
/// Les points doivent être triés par instant.
pub fn temperature_csv(points: &[HistoryPoint]) -> String {
    let temperature: Vec<&HistoryPoint> = points
        .iter()
        .filter(|p| p.metric == Metric::Temperature)
        .collect();
    let humidity: Vec<&HistoryPoint> = points
        .iter()
        .filter(|p| p.metric == Metric::Humidity)
        .collect();
    let mut next = 0;
    let mut current: Option<&HistoryPoint> = None;
    let mut out = String::from("timestamp_unix,datetime_local,temperature_c,humidity_pct\n");
    for s in temperature {
        while next < humidity.len() && humidity[next].at <= s.at + 1.0 {
            current = Some(humidity[next]);
            next += 1;
        }
        let dt = chrono::DateTime::from_timestamp(s.at as i64, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let hum = current
            .filter(|h| s.at - h.at <= HUMIDITY_MATCH_S)
            .map(|h| format!("{:.2}", h.value))
            .unwrap_or_default();
        out.push_str(&format!("{:.0},{},{:.2},{}\n", s.at, dt, s.value, hum));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(at: f64, metric: Metric, value: f64) -> HistoryPoint {
        HistoryPoint { at, metric, value }
    }

    #[test]
    fn csv_pairs_temperature_with_recent_humidity() {
        let points = [
            p(1000.0, Metric::Temperature, 21.5),
            p(1001.0, Metric::Humidity, 45.0),
            p(1010.0, Metric::Temperature, 21.6),
            p(1500.0, Metric::Temperature, 21.7),
            p(1500.0, Metric::Power, 3.0),
        ];
        let csv = temperature_csv(&points);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            "timestamp_unix,datetime_local,temperature_c,humidity_pct"
        );
        assert!(lines[1].ends_with(",21.50,45.00"), "{}", lines[1]);
        assert!(lines[2].ends_with(",21.60,45.00"), "{}", lines[2]);
        // L'humidité a plus de 2 minutes : colonne vide.
        assert!(lines[3].ends_with(",21.70,"), "{}", lines[3]);
    }
}
