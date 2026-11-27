//! Extension « Météo extérieure » : la température, l'humidité et la pression dehors, relevées
//! toutes les 15 minutes auprès d'Open-Meteo (gratuit, sans clé).
//!
//! C'est la première sortie d'Ostal vers Internet : l'extension est **désactivée par défaut**, et
//! tant qu'elle l'est, aucun appel ne part. Une fois activée, deux appels seulement :
//! - le géocodage (`geocoding-api.open-meteo.com/v1/search`) quand on cherche le lieu : le texte
//!   saisi (ville ou code postal) part chez Open-Meteo ;
//! - le relevé (`api.open-meteo.com/v1/forecast`) : la latitude et la longitude du lieu choisi.
//!
//! Le HTTP passe par le `curl` du système, comme la box (`wifi/bbox.rs`) : environnement vide,
//! `--proto =https`, délai, taille bornée. Hors ligne ou en erreur, on **garde la dernière valeur**
//! avec son heure : jamais de panique, jamais de valeur inventée.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Descriptor;
use crate::descriptions::api::quoted;
use crate::security::write_secret;
use crate::wifi::bbox::{curl_run, BoxError};

pub const ID: &str = "weather";
pub const STATE_FILE: &str = "meteo.json";
/// Écart entre deux relevés réussis.
pub const POLL_EVERY_S: f64 = 15.0 * 60.0;
/// Après un échec, on réessaie plus tôt.
pub const RETRY_AFTER_S: f64 = 5.0 * 60.0;
const MAX_BYTES: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

pub const DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    name: "Météo extérieure",
    description: "Température, humidité et pression dehors, affichées sur le plan et l'accueil.",
    disclosure: "Une fois activée, Ostal interroge Open-Meteo (sur Internet) pour ce lieu : le \
texte cherché (ville ou code postal), puis la latitude et la longitude du lieu choisi, toutes \
les 15 minutes. Rien d'autre ne part. Désactivée, plus aucun appel.",
    secrets: &[],
    normalize,
};

/// Un lieu choisi : de quoi interroger Open-Meteo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// « Lyon, Rhône, France » : ce que l'interface affiche.
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// Un relevé. Une grandeur absente de la réponse reste `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// °C.
    pub temperature: Option<f64>,
    /// % d'humidité relative.
    pub humidity: Option<f64>,
    /// hPa, au niveau de la station (`surface_pressure`).
    pub pressure: Option<f64>,
    /// Quand Ostal l'a relevé (secondes Unix).
    pub at: f64,
}

/// Pourquoi un appel n'a rien donné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherError {
    /// Pas de réponse (hors ligne, délai, erreur HTTP).
    Unreachable,
    /// Réponse qui n'a pas la forme attendue.
    BadReply,
}

/// Qui parle à Open-Meteo : le vrai (`CurlWeather`), la démonstration ou un faux (essais).
pub trait WeatherApi: Send + Sync {
    /// Les lieux qui correspondent à ce texte (ville, code postal). Bloquant.
    fn search(&self, query: &str) -> Result<Vec<Place>, WeatherError>;
    /// Le relevé actuel à ce lieu. Bloquant.
    fn current(&self, latitude: f64, longitude: f64, now: f64) -> Result<Reading, WeatherError>;
}

/// Valide la configuration `{"place": {"name", "latitude", "longitude"} | null}` : rien d'autre
/// n'est gardé.
fn normalize(value: &Value) -> Result<Value, &'static str> {
    const BAD: &str = "Lieu invalide.";
    let obj = value.as_object().ok_or(BAD)?;
    if obj.keys().any(|k| k != "place") {
        return Err(BAD);
    }
    match obj.get("place") {
        None | Some(Value::Null) => Ok(json!({ "place": null })),
        Some(place) => {
            let latitude = place.get("latitude").and_then(Value::as_f64).ok_or(BAD)?;
            let longitude = place.get("longitude").and_then(Value::as_f64).ok_or(BAD)?;
            let name = place.get("name").and_then(Value::as_str).ok_or(BAD)?;
            let place = checked_place(name, latitude, longitude).ok_or(BAD)?;
            Ok(json!({ "place": place }))
        }
    }
}

fn checked_place(name: &str, latitude: f64, longitude: f64) -> Option<Place> {
    if !(latitude.is_finite() && longitude.is_finite())
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return None;
    }
    let name = clean(name, 120);
    if name.is_empty() {
        return None;
    }
    Some(Place {
        name,
        // Quatre décimales : une dizaine de mètres, assez pour la météo, sans plus de précision
        // que nécessaire.
        latitude: round4(latitude),
        longitude: round4(longitude),
    })
}

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// Le lieu de la configuration, s'il y en a un.
pub fn place_of(config: &Value) -> Option<Place> {
    serde_json::from_value(config.get("place")?.clone()).ok()
}

// ---- analyse des réponses --------------------------------------------------------------------

/// Les lieux d'une réponse de géocodage. `results` absent : aucun lieu (liste vide). Un lieu sans
/// nom ou aux coordonnées impossibles est écarté.
pub fn parse_search(body: &[u8]) -> Option<Vec<Place>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if !value.is_object() {
        return None;
    }
    let Some(list) = value.get("results") else {
        return Some(Vec::new());
    };
    let mut places = Vec::new();
    for item in list.as_array()? {
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .map(|t| clean(t, 80))
                .filter(|t| !t.is_empty())
        };
        let (Some(name), Some(lat), Some(lon)) = (
            text("name"),
            item.get("latitude").and_then(Value::as_f64),
            item.get("longitude").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let mut label = vec![name];
        for key in ["admin1", "country"] {
            if let Some(part) = text(key) {
                if !label.contains(&part) {
                    label.push(part);
                }
            }
        }
        if let Some(place) = checked_place(&label.join(", "), lat, lon) {
            places.push(place);
        }
    }
    Some(places)
}

/// Le relevé d'une réponse de prévision (`current`). `None` : pas la forme attendue, ou aucune
/// grandeur plausible.
pub fn parse_current(body: &[u8], now: f64) -> Option<Reading> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let current = value.get("current")?.as_object()?;
    let pick = |key: &str, lo: f64, hi: f64| {
        current
            .get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && (lo..=hi).contains(v))
    };
    let reading = Reading {
        temperature: pick("temperature_2m", -90.0, 65.0),
        humidity: pick("relative_humidity_2m", 0.0, 100.0),
        pressure: pick("surface_pressure", 300.0, 1100.0),
        at: now,
    };
    (reading.temperature.is_some() || reading.humidity.is_some() || reading.pressure.is_some())
        .then_some(reading)
}

// ---- le vrai Open-Meteo, par curl ------------------------------------------------------------

#[derive(Default)]
pub struct CurlWeather;

impl CurlWeather {
    fn get(url: &str) -> Result<Vec<u8>, WeatherError> {
        let config = format!(
            "url = {}\nheader = \"Accept: application/json\"\nheader = \"Expect:\"\n",
            quoted(url).map_err(|_| WeatherError::Unreachable)?
        );
        let reply = curl_run(&config, TIMEOUT, "=https", MAX_BYTES).map_err(|e| match e {
            BoxError::Unreachable | BoxError::Refused => WeatherError::Unreachable,
        })?;
        if reply.status != 200 {
            return Err(WeatherError::Unreachable);
        }
        Ok(reply.body)
    }
}

impl WeatherApi for CurlWeather {
    fn search(&self, query: &str) -> Result<Vec<Place>, WeatherError> {
        let query = clean(query, 100);
        if query.chars().count() < 2 {
            return Ok(Vec::new());
        }
        let url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?count=8&language=fr&format=json&name={}",
            utf8_percent_encode(&query, NON_ALPHANUMERIC)
        );
        parse_search(&Self::get(&url)?).ok_or(WeatherError::BadReply)
    }

    fn current(&self, latitude: f64, longitude: f64, now: f64) -> Result<Reading, WeatherError> {
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={latitude:.4}&longitude={longitude:.4}\
&current=temperature_2m,relative_humidity_2m,surface_pressure&timeformat=unixtime"
        );
        parse_current(&Self::get(&url)?, now).ok_or(WeatherError::BadReply)
    }
}

/// La démonstration : jamais de réseau, des valeurs fixes.
pub struct DemoWeather;

impl WeatherApi for DemoWeather {
    fn search(&self, _query: &str) -> Result<Vec<Place>, WeatherError> {
        Ok(vec![Place {
            name: "Ville d'essai, France".into(),
            latitude: 45.0,
            longitude: 5.0,
        }])
    }

    fn current(&self, _lat: f64, _lon: f64, now: f64) -> Result<Reading, WeatherError> {
        Ok(Reading {
            temperature: Some(12.5),
            humidity: Some(70.0),
            pressure: Some(1015.0),
            at: now,
        })
    }
}

// ---- l'état : dernier relevé, échec, prochain tour -------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    reading: Option<Reading>,
    /// Le lieu du relevé : un relevé d'un autre lieu ne se relit jamais.
    #[serde(default)]
    place: Option<Place>,
}

#[derive(Default)]
struct Inner {
    reading: Option<Reading>,
    place: Option<Place>,
    /// Le dernier relevé a échoué : la valeur gardée est donc périmée.
    failing: bool,
    last_attempt: Option<f64>,
}

/// Ce que l'API montre de la météo.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub place: Option<String>,
    pub reading: Option<Reading>,
    /// Le dernier relevé a échoué : `reading` est la dernière valeur connue, à son heure.
    pub failing: bool,
}

pub struct Weather {
    dir: Option<PathBuf>,
    api: RwLock<Arc<dyn WeatherApi>>,
    inner: Mutex<Inner>,
    /// Un seul relevé à la fois.
    pub(super) poll_lock: Mutex<()>,
}

impl Weather {
    pub fn new(dir: Option<&Path>, api: Arc<dyn WeatherApi>) -> Self {
        let saved: Saved = dir
            .and_then(|d| std::fs::read(d.join(STATE_FILE)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            dir: dir.map(Path::to_path_buf),
            api: RwLock::new(api),
            inner: Mutex::new(Inner {
                reading: saved.reading,
                place: saved.place,
                ..Inner::default()
            }),
            poll_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn api(&self) -> Arc<dyn WeatherApi> {
        self.api.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_api(&self, api: Arc<dyn WeatherApi>) {
        *self.api.write().unwrap_or_else(|e| e.into_inner()) = api;
    }

    /// Ce que l'API en montre : rien tant que l'extension est éteinte ou sans lieu.
    pub fn status(&self, enabled: bool, place: Option<&Place>) -> Status {
        let inner = self.lock();
        let Some(place) = place.filter(|_| enabled) else {
            return Status {
                place: None,
                reading: None,
                failing: false,
            };
        };
        Status {
            place: Some(place.name.clone()),
            reading: inner
                .reading
                .clone()
                .filter(|_| inner.place.as_ref() == Some(place)),
            failing: inner.failing,
        }
    }

    /// La configuration a changé (activée, éteinte, autre lieu) : on oublie ce qui ne vaut plus et
    /// on relève au prochain tour.
    pub fn reset(&self, enabled: bool, place: Option<&Place>) {
        let mut inner = self.lock();
        if !enabled || inner.place.as_ref() != place {
            inner.reading = None;
        }
        inner.place = place.cloned();
        inner.failing = false;
        inner.last_attempt = None;
        let saved = Saved {
            reading: inner.reading.clone(),
            place: inner.place.clone(),
        };
        drop(inner);
        self.save(&saved);
    }

    /// Est-il temps de relever ?
    pub fn due(&self, now: f64, force: bool) -> bool {
        let inner = self.lock();
        match inner.last_attempt {
            None => true,
            Some(_) if force => true,
            Some(at) => {
                let gap = if inner.failing {
                    RETRY_AFTER_S
                } else {
                    POLL_EVERY_S
                };
                // Une horloge qui recule ne bloque pas le relevé.
                now < at || now - at >= gap
            }
        }
    }

    /// Note le résultat d'une tentative. Une erreur garde la dernière valeur. `None` : le lieu a
    /// changé pendant l'appel, ce résultat ne vaut plus. `Some(true)` : l'état de santé a changé
    /// (pour le journal).
    pub fn record(
        &self,
        now: f64,
        place: &Place,
        result: Result<Reading, WeatherError>,
    ) -> Option<bool> {
        let mut inner = self.lock();
        if inner.place.as_ref() != Some(place) {
            return None;
        }
        inner.last_attempt = Some(now);
        let was_failing = inner.failing;
        let ok = result.is_ok();
        match result {
            Ok(reading) => {
                inner.reading = Some(reading);
                inner.failing = false;
            }
            Err(_) => inner.failing = true,
        }
        let saved = Saved {
            reading: inner.reading.clone(),
            place: inner.place.clone(),
        };
        drop(inner);
        if ok {
            self.save(&saved);
        }
        Some(was_failing == ok)
    }

    fn save(&self, saved: &Saved) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(STATE_FILE);
        let result = if saved.reading.is_none() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        } else {
            serde_json::to_vec(saved)
                .map_err(anyhow::Error::from)
                .and_then(|b| write_secret(&path, &b))
        };
        if let Err(e) = result {
            log::warn!("météo : relevé non gardé sur disque : {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORECAST: &str = include_str!("fixtures/forecast.json");
    const GEOCODING: &str = include_str!("fixtures/geocoding.json");
    const EMPTY: &str = include_str!("fixtures/geocoding-empty.json");

    #[test]
    fn parses_the_recorded_forecast() {
        let r = parse_current(FORECAST.as_bytes(), 42.0).unwrap();
        assert_eq!(r.temperature, Some(7.4));
        assert_eq!(r.humidity, Some(82.0));
        assert_eq!(r.pressure, Some(1013.6));
        assert_eq!(r.at, 42.0);
    }

    #[test]
    fn forecast_with_missing_or_absurd_values() {
        let body =
            br#"{"current":{"temperature_2m":null,"relative_humidity_2m":55,"surface_pressure":5}}"#;
        let r = parse_current(body, 1.0).unwrap();
        assert_eq!(
            (r.temperature, r.humidity, r.pressure),
            (None, Some(55.0), None)
        );
        // Rien d'exploitable, ou pas la bonne forme : jamais de relevé.
        assert!(parse_current(br#"{"current":{"temperature_2m":null}}"#, 1.0).is_none());
        assert!(parse_current(br#"{"error":true,"reason":"x"}"#, 1.0).is_none());
        assert!(parse_current(b"<html>", 1.0).is_none());
        assert!(parse_current(b"[]", 1.0).is_none());
    }

    #[test]
    fn parses_the_recorded_geocoding() {
        let places = parse_search(GEOCODING.as_bytes()).unwrap();
        // La 3e ville a une latitude impossible : écartée ; la 1re garde région et pays.
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].name, "Lyon, Auvergne-Rhône-Alpes, France");
        assert_eq!((places[0].latitude, places[0].longitude), (45.7485, 4.8467));
        assert_eq!(places[1].name, "Lyon, Géorgie, États-Unis");
    }

    #[test]
    fn geocoding_without_result_is_an_empty_list() {
        assert_eq!(parse_search(EMPTY.as_bytes()), Some(Vec::new()));
        assert!(parse_search(b"nope").is_none());
        assert!(parse_search(br#"{"results":3}"#).is_none());
    }

    #[test]
    fn config_is_checked_and_cleaned() {
        let ok = normalize(
            &json!({"place": {"name": " Lyon\u{7} ", "latitude": 45.74846, "longitude": 4.84671}}),
        )
        .unwrap();
        assert_eq!(ok["place"]["name"], "Lyon");
        assert_eq!(ok["place"]["latitude"], 45.7485);
        assert_eq!(normalize(&json!({})).unwrap(), json!({"place": null}));
        for bad in [
            json!({"place": {"name": "X", "latitude": 91, "longitude": 0}}),
            json!({"place": {"name": "", "latitude": 1, "longitude": 0}}),
            json!({"place": {"name": "X", "latitude": 1}}),
            json!({"place": null, "extra": 1}),
            json!("Lyon"),
        ] {
            assert!(normalize(&bad).is_err(), "{bad}");
        }
    }

    fn lyon() -> Place {
        Place {
            name: "Lyon".into(),
            latitude: 45.7485,
            longitude: 4.8467,
        }
    }

    #[test]
    fn keeps_the_last_value_when_the_call_fails() {
        let w = Weather::new(None, Arc::new(DemoWeather));
        w.reset(true, Some(&lyon()));
        assert!(w.due(0.0, false));
        let good = parse_current(FORECAST.as_bytes(), 100.0).unwrap();
        assert_eq!(w.record(100.0, &lyon(), Ok(good.clone())), Some(false));
        assert!(!w.due(200.0, false));
        assert!(w.due(100.0 + POLL_EVERY_S, false));
        // Hors ligne : la valeur reste, avec son heure, marquée périmée ; réessai plus tôt.
        assert_eq!(
            w.record(1000.0, &lyon(), Err(WeatherError::Unreachable)),
            Some(true)
        );
        let status = w.status(true, Some(&lyon()));
        assert!(status.failing);
        assert_eq!(status.reading, Some(good));
        assert!(!w.due(1000.0 + RETRY_AFTER_S - 1.0, false));
        assert!(w.due(1000.0 + RETRY_AFTER_S, false));
        // Une horloge qui recule ne bloque rien.
        assert!(w.due(500.0, false));
    }

    #[test]
    fn a_reading_never_outlives_its_place_or_the_switch() {
        let w = Weather::new(None, Arc::new(DemoWeather));
        w.reset(true, Some(&lyon()));
        let good = parse_current(FORECAST.as_bytes(), 1.0).unwrap();
        w.record(1.0, &lyon(), Ok(good));
        assert!(w.status(true, Some(&lyon())).reading.is_some());
        // Éteinte : rien à montrer, et plus rien en mémoire.
        assert_eq!(w.status(false, Some(&lyon())).reading, None);
        w.reset(false, Some(&lyon()));
        w.reset(true, Some(&lyon()));
        assert_eq!(w.status(true, Some(&lyon())).reading, None);
        // Un relevé revenu après un changement de lieu est jeté.
        let other = Place {
            name: "Paris".into(),
            ..lyon()
        };
        w.reset(true, Some(&other));
        assert_eq!(w.record(2.0, &lyon(), Err(WeatherError::BadReply)), None);
    }

    #[test]
    fn the_reading_survives_a_restart_in_a_temp_dir() {
        let dir = std::env::temp_dir().join(format!("ostal-meteo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = Weather::new(Some(&dir), Arc::new(DemoWeather));
        w.reset(true, Some(&lyon()));
        let good = parse_current(FORECAST.as_bytes(), 5.0).unwrap();
        w.record(5.0, &lyon(), Ok(good.clone()));
        let again = Weather::new(Some(&dir), Arc::new(DemoWeather));
        assert_eq!(again.status(true, Some(&lyon())).reading, Some(good));
        // Éteindre efface le fichier.
        again.reset(false, Some(&lyon()));
        assert!(!dir.join(STATE_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
