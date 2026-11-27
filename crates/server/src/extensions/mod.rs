//! Les extensions : des fonctions facultatives **intégrées** à Ostal Server (pas de marketplace),
//! chacune **désactivée par défaut**, avec sa propre configuration. Aujourd'hui : la météo
//! extérieure (`weather.rs`). Une prochaine (Somfy) s'ajoute avec un `Descriptor` dans `CATALOG`.
//!
//! Stockage : `serveur/extensions.json` (0600, jamais exporté, écrit en une fois par
//! `write_secret`) :
//!
//! ```json
//! {"version": 1, "extensions": {"weather": {"enabled": true,
//!    "config": {"place": {"name": "Lyon", "latitude": 45.7, "longitude": 4.8}},
//!    "secrets": {}}}}
//! ```
//!
//! - `config` : réglages propres à l'extension, vérifiés et nettoyés par son `normalize` (une clé
//!   inconnue est refusée), **jamais de secret dedans** : l'API les renvoie tels quels.
//! - `secrets` : mots de passe et clés, déclarés dans `Descriptor::secrets` (une clé non déclarée est
//!   refusée). **Jamais renvoyés** : l'API ne dit que « défini » ou non ; jamais au journal (le
//!   journal d'audit dit seulement « secret modifié ») ; `Debug` les masque ; l'extension les lit
//!   côté serveur par `Extensions::secret`.
//!
//! Une extension absente du fichier est éteinte et sans réglage.

pub mod weather;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::security::write_secret;
use weather::{Place, WeatherApi};

pub const FILE: &str = "extensions.json";
/// Taille maximale d'un secret.
const MAX_SECRET: usize = 256;

/// Ce qu'on sait d'une extension intégrée, au compilateur.
pub struct Descriptor {
    /// Identifiant stable (kebab-case ou mot simple) : clé du fichier et de l'API.
    pub id: &'static str,
    pub name: &'static str,
    /// Une phrase pour la liste.
    pub description: &'static str,
    /// Ce qui sort d'Ostal quand elle est active (affiché avant de l'activer).
    pub disclosure: &'static str,
    /// Les noms de ses secrets (mot de passe, clé d'API…).
    pub secrets: &'static [&'static str],
    /// Vérifie et nettoie sa configuration (voir plus haut).
    pub normalize: fn(&Value) -> Result<Value, &'static str>,
}

/// Les extensions que cette version connaît.
pub const CATALOG: &[Descriptor] = &[weather::DESCRIPTOR];

fn descriptor(id: &str) -> Option<&'static Descriptor> {
    CATALOG.iter().find(|d| d.id == id)
}

/// Le nom affiché d'une extension (« Météo extérieure »), pour le journal d'audit.
pub fn name_of(id: &str) -> Option<&'static str> {
    descriptor(id).map(|d| d.name)
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("enabled", &self.enabled)
            .field("config", &self.config)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    extensions: BTreeMap<String, Entry>,
}

/// Une extension telle que l'API la montre. Jamais un secret.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub disclosure: &'static str,
    pub enabled: bool,
    pub config: Value,
    /// Pour chaque secret déclaré : défini ou non (jamais sa valeur).
    pub secrets: BTreeMap<String, bool>,
    /// L'état propre à l'extension (pour la météo : lieu, dernier relevé).
    pub status: Value,
}

/// Une modification demandée : tout est facultatif.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub enabled: Option<bool>,
    /// Remplace toute la configuration.
    pub config: Option<Value>,
    /// Secrets à changer : `"valeur"` pose, `null` ou `""` efface. Un secret absent ne change pas.
    #[serde(default)]
    pub secrets: BTreeMap<String, Option<String>>,
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Update")
            .field("enabled", &self.enabled)
            .field("config", &self.config)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateError {
    /// Identifiant inconnu.
    Unknown,
    /// Valeur refusée (la phrase le dit).
    Invalid(&'static str),
    Storage,
}

/// Ce qui a changé, pour le journal d'audit (jamais une valeur).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub switched: Option<bool>,
    pub config: bool,
    pub secrets: bool,
}

/// Pourquoi une recherche de lieu n'a rien donné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    /// L'extension est éteinte : aucun appel ne part.
    Disabled,
    Failed,
}

pub struct Extensions {
    dir: Option<PathBuf>,
    inner: Mutex<File>,
    weather: weather::Weather,
}

impl Extensions {
    /// Ouvre `dir/extensions.json`. Absent ou illisible : tout est éteint (jamais une extension
    /// allumée par erreur).
    pub fn open(dir: &std::path::Path, weather_api: Arc<dyn WeatherApi>) -> Self {
        let file = std::fs::read(dir.join(FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .unwrap_or_default();
        Self {
            dir: Some(dir.to_path_buf()),
            weather: weather::Weather::new(Some(dir), weather_api),
            inner: Mutex::new(file),
        }
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory(weather_api: Arc<dyn WeatherApi>) -> Self {
        Self {
            dir: None,
            weather: weather::Weather::new(None, weather_api),
            inner: Mutex::new(File::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, File> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.lock().extensions.get(id).is_some_and(|e| e.enabled)
    }

    /// Un secret, pour l'extension qui s'en sert (côté serveur seulement : jamais renvoyé).
    pub fn secret(&self, id: &str, key: &str) -> Option<String> {
        let d = descriptor(id)?;
        if !d.secrets.contains(&key) {
            return None;
        }
        self.lock()
            .extensions
            .get(id)?
            .secrets
            .get(key)
            .filter(|v| !v.is_empty())
            .cloned()
    }

    /// La configuration d'une extension (objet vide si elle n'a rien).
    pub fn config(&self, id: &str) -> Value {
        self.lock()
            .extensions
            .get(id)
            .map(|e| e.config.clone())
            .filter(|c| c.is_object())
            .unwrap_or_else(|| json!({}))
    }

    fn weather_place(&self) -> Option<Place> {
        weather::place_of(&self.config(weather::ID))
    }

    fn view_of(&self, d: &'static Descriptor, entry: &Entry) -> View {
        let config = if entry.config.is_object() {
            entry.config.clone()
        } else {
            (d.normalize)(&json!({})).unwrap_or_else(|_| json!({}))
        };
        let status = match d.id {
            weather::ID => serde_json::to_value(
                self.weather
                    .status(entry.enabled, weather::place_of(&config).as_ref()),
            )
            .unwrap_or(Value::Null),
            _ => Value::Null,
        };
        View {
            id: d.id,
            name: d.name,
            description: d.description,
            disclosure: d.disclosure,
            enabled: entry.enabled,
            config,
            secrets: d
                .secrets
                .iter()
                .map(|k| {
                    (
                        (*k).to_string(),
                        entry.secrets.get(*k).is_some_and(|v| !v.is_empty()),
                    )
                })
                .collect(),
            status,
        }
    }

    /// Toutes les extensions connues, dans l'ordre du catalogue.
    pub fn list(&self) -> Vec<View> {
        let file = self.lock().clone();
        CATALOG
            .iter()
            .map(|d| {
                let entry = file.extensions.get(d.id).cloned().unwrap_or_default();
                self.view_of(d, &entry)
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<View> {
        let d = descriptor(id)?;
        let entry = self.lock().extensions.get(id).cloned().unwrap_or_default();
        Some(self.view_of(d, &entry))
    }

    /// Applique une modification. Rien n'est changé si une valeur est refusée ou si le disque
    /// refuse d'écrire.
    pub fn update(&self, id: &str, update: Update) -> Result<(View, Changes), UpdateError> {
        self.update_for(descriptor(id).ok_or(UpdateError::Unknown)?, update)
    }

    fn update_for(
        &self,
        d: &'static Descriptor,
        update: Update,
    ) -> Result<(View, Changes), UpdateError> {
        let id = d.id;
        let mut file = self.lock();
        let before = file.extensions.get(id).cloned().unwrap_or_default();
        let mut entry = before.clone();
        let mut changes = Changes::default();

        if let Some(config) = &update.config {
            let config = (d.normalize)(config).map_err(UpdateError::Invalid)?;
            if config != entry.config {
                changes.config = true;
                entry.config = config;
            }
        } else if !entry.config.is_object() {
            entry.config = (d.normalize)(&json!({})).map_err(UpdateError::Invalid)?;
        }
        for (key, value) in &update.secrets {
            if !d.secrets.contains(&key.as_str()) {
                return Err(UpdateError::Invalid("Secret inconnu."));
            }
            match value.as_deref().filter(|v| !v.is_empty()) {
                Some(v) => {
                    if v.len() > MAX_SECRET || v.chars().any(char::is_control) {
                        return Err(UpdateError::Invalid("Secret invalide."));
                    }
                    if entry.secrets.get(key).map(String::as_str) != Some(v) {
                        entry.secrets.insert(key.clone(), v.to_string());
                        changes.secrets = true;
                    }
                }
                None => {
                    changes.secrets |= entry.secrets.remove(key).is_some();
                }
            }
        }
        if let Some(on) = update.enabled {
            if on != entry.enabled {
                changes.switched = Some(on);
                entry.enabled = on;
            }
        }
        // Rien à changer : pas d'écriture.
        if changes != Changes::default() || !file.extensions.contains_key(id) {
            let mut next = file.clone();
            next.version = 1;
            next.extensions.insert(id.to_string(), entry.clone());
            if let Some(dir) = &self.dir {
                let bytes = serde_json::to_vec_pretty(&next).map_err(|_| UpdateError::Storage)?;
                write_secret(&dir.join(FILE), &bytes).map_err(|_| UpdateError::Storage)?;
            }
            *file = next;
        }
        drop(file);

        if id == weather::ID && (changes.switched.is_some() || changes.config) {
            self.weather
                .reset(entry.enabled, weather::place_of(&entry.config).as_ref());
        }
        Ok((self.view_of(d, &entry), changes))
    }

    // ---- météo -------------------------------------------------------------------------------

    /// Cherche un lieu (ville, code postal) chez Open-Meteo. Bloquant. Refusé tant que
    /// l'extension est éteinte : aucun appel ne part.
    pub fn weather_search(&self, query: &str) -> Result<Vec<Place>, SearchError> {
        if !self.is_enabled(weather::ID) {
            return Err(SearchError::Disabled);
        }
        self.weather
            .api()
            .search(query)
            .map_err(|_| SearchError::Failed)
    }

    /// Ce que montrent le plan et l'accueil.
    pub fn weather_status(&self) -> (bool, weather::Status) {
        let enabled = self.is_enabled(weather::ID);
        let place = self.weather_place();
        (enabled, self.weather.status(enabled, place.as_ref()))
    }

    /// Relève la météo si l'extension est active, qu'un lieu est choisi et qu'il est temps (ou
    /// `force`). Bloquant (quelques secondes au plus). Ne panique jamais : une erreur garde la
    /// dernière valeur. `true` : un appel est parti.
    pub fn poll(&self, now: f64, force: bool) -> bool {
        let Ok(_one_at_a_time) = self.weather.poll_lock.try_lock() else {
            return false;
        };
        if !self.is_enabled(weather::ID) {
            return false;
        }
        let Some(place) = self.weather_place() else {
            return false;
        };
        if !self.weather.due(now, force) {
            return false;
        }
        let result = self
            .weather
            .api()
            .current(place.latitude, place.longitude, now);
        // Éteinte pendant l'appel : on jette le résultat.
        if !self.is_enabled(weather::ID) {
            return true;
        }
        let failed = result.as_ref().err().copied();
        // Une ligne au journal seulement quand l'état de santé change (jamais le lieu).
        if self.weather.record(now, &place, result) == Some(true) {
            match failed {
                None => log::info!("météo : relevé rétabli"),
                Some(why) => log::warn!(
                    "météo : relevé impossible ({}), dernière valeur gardée",
                    match why {
                        weather::WeatherError::Unreachable => "pas de réponse d'Open-Meteo",
                        weather::WeatherError::BadReply => "réponse inattendue",
                    }
                ),
            }
        }
        true
    }

    /// L'arrêt du serveur : le crochet où une extension qui lance un sous-processus (Somfy et son
    /// pont Homebridge) l'arrête. La météo n'a rien à arrêter. Appelé après l'arrêt du fil des
    /// extensions.
    pub fn shutdown(&self) {}

    /// Pour les essais : remplace le client d'Open-Meteo.
    #[doc(hidden)]
    pub fn set_weather_api(&self, api: Arc<dyn WeatherApi>) {
        self.weather.set_api(api);
    }
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Extensions { .. }")
    }
}

#[cfg(test)]
mod tests {
    use super::weather::{DemoWeather, Reading, WeatherError};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Compte ses appels : la preuve qu'éteinte, l'extension n'appelle rien.
    #[derive(Default)]
    struct Counting {
        searches: AtomicUsize,
        reads: AtomicUsize,
        offline: std::sync::atomic::AtomicBool,
    }
    impl WeatherApi for Counting {
        fn search(&self, _q: &str) -> Result<Vec<Place>, WeatherError> {
            self.searches.fetch_add(1, Ordering::SeqCst);
            DemoWeather.search("")
        }
        fn current(&self, la: f64, lo: f64, now: f64) -> Result<Reading, WeatherError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.offline.load(Ordering::SeqCst) {
                return Err(WeatherError::Unreachable);
            }
            DemoWeather.current(la, lo, now)
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ostal-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lyon_config() -> Value {
        json!({"place": {"name": "Lyon", "latitude": 45.75, "longitude": 4.85}})
    }

    fn on() -> Update {
        Update {
            enabled: Some(true),
            ..Update::default()
        }
    }

    #[test]
    fn everything_is_off_by_default_and_calls_nothing() {
        let api = Arc::new(Counting::default());
        let ext = Extensions::in_memory(api.clone());
        let list = ext.list();
        assert_eq!(list.len(), CATALOG.len());
        assert!(list.iter().all(|v| !v.enabled));
        assert!(!ext.poll(1.0, true));
        assert_eq!(ext.weather_search("Lyon"), Err(SearchError::Disabled));
        // Même configurée, éteinte : aucun appel.
        ext.update(
            weather::ID,
            Update {
                config: Some(lyon_config()),
                ..Update::default()
            },
        )
        .unwrap();
        assert!(!ext.poll(1.0, true));
        assert_eq!(
            (
                api.searches.load(Ordering::SeqCst),
                api.reads.load(Ordering::SeqCst)
            ),
            (0, 0)
        );
    }

    #[test]
    fn enable_configure_poll_then_disable_stops_everything() {
        let dir = temp("cycle");
        let api = Arc::new(Counting::default());
        let ext = Extensions::open(&dir, api.clone());
        ext.update(weather::ID, on()).unwrap();
        // Sans lieu : rien à relever.
        assert!(!ext.poll(10.0, false));
        assert_eq!(ext.weather_search("Lyon").unwrap().len(), 1);
        ext.update(
            weather::ID,
            Update {
                config: Some(lyon_config()),
                ..Update::default()
            },
        )
        .unwrap();
        assert!(ext.poll(100.0, false));
        assert!(!ext.poll(101.0, false), "pas avant 15 minutes");
        let (enabled, status) = ext.weather_status();
        assert!(enabled);
        assert_eq!(status.reading.unwrap().temperature, Some(12.5));
        assert_eq!(api.reads.load(Ordering::SeqCst), 1);
        // Tout survit à un redémarrage.
        let again = Extensions::open(&dir, api.clone());
        assert!(again.is_enabled(weather::ID));
        assert_eq!(again.weather_status().1.reading.unwrap().at, 100.0);
        // Éteindre : le relevé s'efface, plus aucun appel.
        let (view, changes) = again
            .update(
                weather::ID,
                Update {
                    enabled: Some(false),
                    ..Update::default()
                },
            )
            .unwrap();
        assert_eq!(changes.switched, Some(false));
        assert!(!view.enabled);
        assert_eq!(view.status["reading"], Value::Null);
        assert!(!again.poll(100_000.0, true));
        assert_eq!(again.weather_search("x"), Err(SearchError::Disabled));
        assert_eq!(api.reads.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_keeps_the_last_value() {
        let api = Arc::new(Counting::default());
        let ext = Extensions::in_memory(api.clone());
        ext.update(weather::ID, on()).unwrap();
        ext.update(
            weather::ID,
            Update {
                config: Some(lyon_config()),
                ..Update::default()
            },
        )
        .unwrap();
        assert!(ext.poll(0.0, false));
        api.offline.store(true, Ordering::SeqCst);
        assert!(ext.poll(weather::POLL_EVERY_S + 1.0, false));
        let (_, status) = ext.weather_status();
        assert!(status.failing);
        assert_eq!(
            status.reading.unwrap().at,
            0.0,
            "l'heure du dernier bon relevé"
        );
    }

    #[test]
    fn refused_updates_change_nothing() {
        let ext = Extensions::in_memory(Arc::new(DemoWeather));
        let bad = Update {
            enabled: Some(true),
            config: Some(json!({"place": {"name": "X", "latitude": 200, "longitude": 0}})),
            ..Update::default()
        };
        assert_eq!(
            ext.update(weather::ID, bad).unwrap_err(),
            UpdateError::Invalid("Lieu invalide.")
        );
        assert!(!ext.is_enabled(weather::ID));
        assert_eq!(ext.update("nope", on()).unwrap_err(), UpdateError::Unknown);
        let mut secret = Update::default();
        secret
            .secrets
            .insert("mot-de-passe".into(), Some("x".into()));
        assert_eq!(
            ext.update(weather::ID, secret).unwrap_err(),
            UpdateError::Invalid("Secret inconnu.")
        );
    }

    fn accept_anything(v: &Value) -> Result<Value, &'static str> {
        Ok(v.clone())
    }

    static WITH_SECRET: Descriptor = Descriptor {
        id: "essai",
        name: "Essai",
        description: "",
        disclosure: "",
        secrets: &["password"],
        normalize: accept_anything,
    };

    #[test]
    fn secrets_are_stored_but_never_shown() {
        let dir = temp("secret");
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        let mut update = Update::default();
        update
            .secrets
            .insert("password".into(), Some("TRES-SECRET".into()));
        let (view, changes) = ext.update_for(&WITH_SECRET, update).unwrap();
        assert!(changes.secrets);
        assert!(view.secrets["password"]);
        let shown = serde_json::to_string(&view).unwrap();
        assert!(!shown.contains("TRES-SECRET"), "{shown}");
        // Sur disque, dans un fichier privé.
        let path = dir.join(FILE);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("TRES-SECRET"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Rejouer la même valeur ne change rien ; la valeur vide efface.
        let mut same = Update::default();
        same.secrets
            .insert("password".into(), Some("TRES-SECRET".into()));
        assert!(!ext.update_for(&WITH_SECRET, same).unwrap().1.secrets);
        let mut clear = Update::default();
        clear.secrets.insert("password".into(), None);
        let (view, changes) = ext.update_for(&WITH_SECRET, clear).unwrap();
        assert!(changes.secrets && !view.secrets["password"]);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("TRES-SECRET"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_never_shows_a_secret() {
        let mut entry = Entry::default();
        entry
            .secrets
            .insert("password".into(), "TRES-SECRET".into());
        assert!(!format!("{entry:?}").contains("TRES-SECRET"));
        let mut update = Update::default();
        update
            .secrets
            .insert("password".into(), Some("TRES-SECRET".into()));
        assert!(!format!("{update:?}").contains("TRES-SECRET"));
    }

    #[test]
    fn a_file_with_a_made_up_entry_does_not_turn_anything_on() {
        let dir = temp("garbage");
        std::fs::write(dir.join(FILE), b"{ pas du json").unwrap();
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        assert!(ext.list().iter().all(|v| !v.enabled));
        std::fs::write(
            dir.join(FILE),
            br#"{"version":1,"extensions":{"inconnue":{"enabled":true},"weather":{"enabled":true,"config":"n'importe quoi"}}}"#,
        )
        .unwrap();
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        // Une configuration illisible vaut « pas de lieu » : activée, mais rien à relever.
        assert!(ext.is_enabled(weather::ID));
        assert!(!ext.poll(1.0, true));
        assert_eq!(ext.get(weather::ID).unwrap().config, json!({"place": null}));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
