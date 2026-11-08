//! Formatage français des mesures (virgule décimale), utilisé par le client pour l'affichage
//! et par le serveur dans les textes des notifications d'automatisation.

/// Formatage français : `21.5` → `"21,50 °C"`.
pub fn format_temperature(c: f64) -> String {
    format!("{:.2} °C", c).replace('.', ",")
}

pub fn format_humidity(p: f64) -> String {
    format!("{:.1} %", p).replace('.', ",")
}

pub fn format_power(w: f64) -> String {
    if w.abs() >= 1000.0 {
        format!("{:.2} kW", w / 1000.0).replace('.', ",")
    } else {
        format!("{:.1} W", w).replace('.', ",")
    }
}

pub fn format_energy(kwh: f64) -> String {
    format!("{:.3} kWh", kwh).replace('.', ",")
}

pub fn format_position(open: f64) -> String {
    if open <= 0.0 {
        "Fermé".into()
    } else if open >= 100.0 {
        "Ouvert".into()
    } else {
        format!("Ouvert à {open:.0} %")
    }
}
