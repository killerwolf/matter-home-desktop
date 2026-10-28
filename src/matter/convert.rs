//! Conversion des valeurs brutes des clusters de mesure.

use super::tlv::Value;

/// Valeur « nulle » d'un int16 nullable en Matter (0x8000), si l'appareil
/// ne l'encode pas directement comme un élément TLV Null.
const INT16_NULL: i64 = i16::MIN as i64;
const UINT16_NULL: u64 = u16::MAX as u64;

/// Temperature Measurement (0x0402) / MeasuredValue : int16 en centièmes de °C.
/// `2150` → `21.5`. Retourne `None` pour une valeur nulle ou hors plage.
pub fn temperature_from_raw(raw: i64) -> Option<f64> {
    // Plage du cluster : -273,15 °C à 327,67 °C.
    if raw == INT16_NULL || !(-27315..=32767).contains(&raw) {
        return None;
    }
    Some(raw as f64 / 100.0)
}

/// Relative Humidity Measurement (0x0405) / MeasuredValue : uint16 en centièmes de %.
/// `4550` → `45.5`. Retourne `None` pour une valeur nulle ou > 100 %.
pub fn humidity_from_raw(raw: u64) -> Option<f64> {
    if raw == UINT16_NULL || raw > 10000 {
        return None;
    }
    Some(raw as f64 / 100.0)
}

/// Décode la donnée TLV d'un attribut MeasuredValue de température.
pub fn temperature_from_tlv(v: &Value) -> Option<f64> {
    match v {
        Value::Signed(i) => temperature_from_raw(*i),
        Value::Unsigned(u) => i64::try_from(*u).ok().and_then(temperature_from_raw),
        _ => None,
    }
}

/// Décode la donnée TLV d'un attribut MeasuredValue d'humidité.
pub fn humidity_from_tlv(v: &Value) -> Option<f64> {
    match v {
        Value::Unsigned(u) => humidity_from_raw(*u),
        Value::Signed(i) if *i >= 0 => humidity_from_raw(*i as u64),
        _ => None,
    }
}

/// Formatage français : `21.5` → `"21,50 °C"`.
pub fn format_temperature(c: f64) -> String {
    format!("{:.2} °C", c).replace('.', ",")
}

pub fn format_humidity(p: f64) -> String {
    format!("{:.1} %", p).replace('.', ",")
}

/// Power Source (0x002F) / BatPercentRemaining : uint8 en demi-pourcents (200 = 100 %).
pub fn battery_percent_from_tlv(v: &Value) -> Option<f64> {
    v.as_u64().filter(|r| *r <= 200).map(|r| r as f64 / 2.0)
}

/// Power Source / BatChargeLevel : 0 = OK, 1 = faible, 2 = critique. `true` si pas OK.
pub fn battery_low_from_tlv(v: &Value) -> Option<bool> {
    v.as_u64().filter(|l| *l <= 2).map(|l| l > 0)
}

/// Electrical Power Measurement (0x0090) / ActivePower : int64 en mW → W.
pub fn power_w_from_tlv(v: &Value) -> Option<f64> {
    match v {
        Value::Signed(mw) => Some(*mw as f64 / 1000.0),
        Value::Unsigned(mw) => Some(*mw as f64 / 1000.0),
        _ => None,
    }
}

/// Electrical Energy Measurement (0x0091) / CumulativeEnergyImported :
/// structure EnergyMeasurementStruct dont le champ 0 est l'énergie en mWh → kWh.
pub fn energy_kwh_from_tlv(v: &Value) -> Option<f64> {
    match v.field(0)? {
        Value::Signed(mwh) => Some(*mwh as f64 / 1e6),
        Value::Unsigned(mwh) => Some(*mwh as f64 / 1e6),
        _ => None,
    }
}

/// Boolean State (0x0045) / StateValue : `true` = contact (fermé). Retourne « ouvert ».
pub fn open_from_tlv(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(contact) => Some(!contact),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_conversion() {
        assert_eq!(battery_percent_from_tlv(&Value::Unsigned(200)), Some(100.0));
        assert_eq!(battery_percent_from_tlv(&Value::Unsigned(37)), Some(18.5));
        assert_eq!(battery_percent_from_tlv(&Value::Unsigned(255)), None);
        assert_eq!(battery_percent_from_tlv(&Value::Null), None);
        assert_eq!(battery_low_from_tlv(&Value::Unsigned(0)), Some(false));
        assert_eq!(battery_low_from_tlv(&Value::Unsigned(2)), Some(true));
        assert_eq!(battery_low_from_tlv(&Value::Unsigned(7)), None);
    }

    #[test]
    fn power_and_energy_conversion() {
        assert_eq!(power_w_from_tlv(&Value::Signed(12_345)), Some(12.345));
        assert_eq!(power_w_from_tlv(&Value::Signed(-500)), Some(-0.5));
        assert_eq!(power_w_from_tlv(&Value::Null), None);
        let energy = Value::Container(vec![(Some(0), Value::Signed(1_234_567)), (Some(1), Value::Unsigned(0))]);
        assert_eq!(energy_kwh_from_tlv(&energy), Some(1.234567));
        assert_eq!(energy_kwh_from_tlv(&Value::Null), None);
        assert_eq!(format_power(12.34), "12,3 W");
        assert_eq!(format_power(2500.0), "2,50 kW");
        assert_eq!(format_energy(1.5), "1,500 kWh");
    }

    #[test]
    fn contact_conversion() {
        assert_eq!(open_from_tlv(&Value::Bool(true)), Some(false));
        assert_eq!(open_from_tlv(&Value::Bool(false)), Some(true));
        assert_eq!(open_from_tlv(&Value::Unsigned(1)), None);
    }

    #[test]
    fn temperature_conversion() {
        assert_eq!(temperature_from_raw(2150), Some(21.5));
        assert_eq!(temperature_from_raw(0), Some(0.0));
        assert_eq!(temperature_from_raw(-5), Some(-0.05));
        assert_eq!(temperature_from_raw(-1234), Some(-12.34));
        assert_eq!(temperature_from_raw(32767), Some(327.67));
        assert_eq!(temperature_from_raw(-27315), Some(-273.15));
    }

    #[test]
    fn temperature_null_and_out_of_range() {
        assert_eq!(temperature_from_raw(-32768), None); // 0x8000 = null
        assert_eq!(temperature_from_raw(-27316), None);
        assert_eq!(temperature_from_raw(40000), None);
    }

    #[test]
    fn temperature_from_tlv_values() {
        assert_eq!(temperature_from_tlv(&Value::Signed(2150)), Some(21.5));
        assert_eq!(temperature_from_tlv(&Value::Signed(-50)), Some(-0.5));
        assert_eq!(temperature_from_tlv(&Value::Unsigned(1999)), Some(19.99));
        assert_eq!(temperature_from_tlv(&Value::Null), None);
        assert_eq!(temperature_from_tlv(&Value::Bool(true)), None);
    }

    #[test]
    fn humidity_conversion() {
        assert_eq!(humidity_from_raw(4550), Some(45.5));
        assert_eq!(humidity_from_raw(10000), Some(100.0));
        assert_eq!(humidity_from_raw(10001), None);
        assert_eq!(humidity_from_raw(0xFFFF), None);
        assert_eq!(humidity_from_tlv(&Value::Unsigned(3300)), Some(33.0));
        assert_eq!(humidity_from_tlv(&Value::Null), None);
    }

    #[test]
    fn french_formatting() {
        assert_eq!(format_temperature(21.5), "21,50 °C");
        assert_eq!(format_temperature(-0.05), "-0,05 °C");
        assert_eq!(format_humidity(45.55), "45,5 %");
    }
}
