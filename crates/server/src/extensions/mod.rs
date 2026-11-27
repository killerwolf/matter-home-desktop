//! Les extensions : des fonctions facultatives **intégrées** à Ostal Server (pas de marketplace),
//! chacune **désactivée par défaut**, avec sa propre configuration. Aujourd'hui : la météo
//! extérieure (`weather.rs`). Pour en ajouter une (Somfy…) :
//! 1. écrire un `static DESCRIPTOR: Descriptor` (identifiant, textes français, secrets, champs de
//!    configuration privés, `normalize`) et l'ajouter à `CATALOG` ;
//! 2. implémenter le trait [`Extension`] (état, démarrage, changement de réglage, tour périodique,
//!    arrêt) et l'enregistrer dans `Extensions::build`.
//!
//! Le socle ne connaît aucune extension en particulier : il appelle le trait.
//!
//! Stockage : `serveur/extensions.json` (0600, jamais exporté, écrit en une fois par
//! `write_secret`) :
//!
//! ```json
//! {"version": 1, "extensions": {"weather": {"enabled": true,
//!    "config": {"place": {"name": "Lyon", "latitude": 45.75, "longitude": 4.85}},
//!    "secrets": {}}}}
//! ```
//!
//! - `config` : réglages propres à l'extension, vérifiés et nettoyés par son `normalize` (une clé
//!   inconnue est refusée), **jamais de secret dedans**. Les clés listées dans
//!   `Descriptor::private_config` (un identifiant e-mail, par exemple) ne sont montrées qu'aux
//!   comptes qui ont le droit « extensions ».
//! - `secrets` : mots de passe et clés, déclarés dans `Descriptor::secrets` (une clé non déclarée est
//!   refusée). **Jamais renvoyés** : l'API ne dit que « défini » ou non ; jamais au journal (le
//!   journal d'audit dit seulement « secret modifié ») ; `Debug` les masque ; l'extension les lit
//!   côté serveur par `Extensions::secret`.
//!
//! Une extension absente du fichier est éteinte et sans réglage. Un fichier **illisible** est mis
//! de côté (`extensions.json.illisible-…`, jamais effacé) et tout repart éteint ; si même cela
//! échoue, les écritures sont refusées plutôt que d'écraser les secrets. Les champs inconnus
//! (d'une version plus récente) sont conservés à la réécriture.

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

/// Un secret déclaré par une extension, avec ses mots pour l'interface.
pub struct SecretField {
    /// Le nom dans le fichier et dans l'API.
    pub key: &'static str,
    /// « Mot de passe TaHoma » (français).
    pub label: &'static str,
    /// Une phrase d'aide sous le champ.
    pub help: &'static str,
}

/// Ce qu'on sait d'une extension intégrée, au compilateur.
pub struct Descriptor {
    /// Identifiant stable (mot simple, minuscules) : clé du fichier et de l'API.
    pub id: &'static str,
    pub name: &'static str,
    /// Une phrase pour la liste.
    pub description: &'static str,
    /// Ce qui sort d'Ostal quand elle est active (affiché avant de l'activer).
    pub disclosure: &'static str,
    pub secrets: &'static [SecretField],
    /// Clés de premier niveau de `config` qui ne sont pas montrées aux comptes sans le droit
    /// « extensions » (un identifiant e-mail, par exemple).
    pub private_config: &'static [&'static str],
    /// Vérifie et nettoie sa configuration (voir plus haut).
    pub normalize: fn(&Value) -> Result<Value, &'static str>,
}

/// Les extensions que cette version connaît.
pub static CATALOG: &[&Descriptor] = &[&weather::DESCRIPTOR];

/// Le comportement d'une extension. Les méthodes reçoivent l'interrupteur et la configuration
/// courants ; les secrets se lisent par `Extensions::secret`. Aucune ne doit paniquer.
pub trait Extension: Send + Sync {
    /// L'état propre à l'extension, montré dans `View::status` (`null` si rien).
    fn status(&self, _enabled: bool, _config: &Value) -> Value {
        Value::Null
    }
    /// Au démarrage du serveur, avant tout tour : remettre l'état d'accord avec la configuration
    /// gardée (et, pour une extension à sous-processus, le lancer si elle est active).
    fn start(&self, _enabled: bool, _config: &Value) {}
    /// Après un changement d'interrupteur, de configuration ou de secret (sous le verrou des
    /// modifications : deux changements ne s'entrecroisent pas). Pas d'appel réseau long ici.
    fn on_change(&self, _enabled: bool, _config: &Value) {}
    /// Un tour périodique (toutes les 30 s environ, fil dédié) : relever si c'est l'heure. Peut
    /// bloquer quelques secondes. `true` : un appel est parti.
    fn tick(&self, _now: f64, _enabled: bool, _config: &Value) -> bool {
        false
    }
    /// L'arrêt du serveur : arrêter ce qu'elle a lancé (sous-processus).
    fn shutdown(&self) {}
    /// Pour les essais : remplace le client réseau de la météo. Sans effet ailleurs.
    #[doc(hidden)]
    fn set_weather_api(&self, _api: Arc<dyn WeatherApi>) {}
}

fn descriptor(id: &str) -> Option<&'static Descriptor> {
    CATALOG.iter().copied().find(|d| d.id == id)
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
    /// Champs d'une version plus récente : gardés tels quels.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
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
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// Un secret tel que l'API le montre : ses mots et s'il est défini, jamais sa valeur.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SecretView {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub set: bool,
}

/// Une extension telle que l'API la montre. Jamais un secret.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub disclosure: &'static str,
    pub enabled: bool,
    /// Sans les champs privés pour un compte qui n'a pas le droit « extensions ».
    pub config: Value,
    pub secrets: Vec<SecretView>,
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
    /// Le fichier n'a pas pu être lu ni mis de côté : on n'y écrit rien.
    read_only: bool,
    /// Une modification à la fois, `on_change` compris (sans bloquer les lectures).
    update_lock: Mutex<()>,
    registry: Vec<(&'static Descriptor, Arc<dyn Extension>)>,
    weather: Arc<weather::Weather>,
}

/// Lit `dir/extensions.json`. Absent : vide. Illisible : journalisé, mis de côté, vide ; si même
/// la mise de côté échoue, `read_only`.
fn load(dir: &std::path::Path) -> (File, bool) {
    let path = dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (File::default(), false),
        Err(e) => {
            log::error!(
                "extensions : {FILE} illisible ({}), réglages en lecture seule",
                e.kind()
            );
            return (File::default(), true);
        }
    };
    match serde_json::from_slice::<File>(&bytes) {
        Ok(file) => (file, false),
        Err(_) => {
            let aside = dir.join(format!(
                "{FILE}.illisible-{}",
                crate::security::now() as u64
            ));
            match std::fs::rename(&path, &aside) {
                Ok(()) => {
                    log::error!(
                        "extensions : {FILE} illisible, mis de côté ({}) ; toutes les extensions repartent éteintes",
                        aside.display()
                    );
                    (File::default(), false)
                }
                Err(e) => {
                    log::error!(
                        "extensions : {FILE} illisible et impossible à mettre de côté ({}) : réglages en lecture seule",
                        e.kind()
                    );
                    (File::default(), true)
                }
            }
        }
    }
}

impl Extensions {
    /// Ouvre `dir/extensions.json`. Absent ou illisible : tout est éteint (jamais une extension
    /// allumée par erreur).
    pub fn open(dir: &std::path::Path, weather_api: Arc<dyn WeatherApi>) -> Self {
        let (file, read_only) = load(dir);
        Self::build(Some(dir), file, read_only, weather_api)
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory(weather_api: Arc<dyn WeatherApi>) -> Self {
        Self::build(None, File::default(), false, weather_api)
    }

    fn build(
        dir: Option<&std::path::Path>,
        file: File,
        read_only: bool,
        weather_api: Arc<dyn WeatherApi>,
    ) -> Self {
        let weather = Arc::new(weather::Weather::new(dir, weather_api));
        let registry: Vec<(&'static Descriptor, Arc<dyn Extension>)> =
            vec![(&weather::DESCRIPTOR, weather.clone())];
        let this = Self {
            dir: dir.map(PathBuf::from),
            inner: Mutex::new(file),
            read_only,
            update_lock: Mutex::new(()),
            registry,
            weather,
        };
        // Chaque extension remet son état d'accord avec la configuration gardée.
        for (d, ext) in &this.registry {
            let (enabled, config) = this.snapshot(d.id);
            ext.start(enabled, &config);
        }
        this
    }

    fn lock(&self) -> MutexGuard<'_, File> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn instance(&self, id: &str) -> Option<&Arc<dyn Extension>> {
        self.registry
            .iter()
            .find(|(d, _)| d.id == id)
            .map(|(_, e)| e)
    }

    /// L'interrupteur et la configuration (objet, éventuellement vide) d'une extension.
    fn snapshot(&self, id: &str) -> (bool, Value) {
        let file = self.lock();
        let entry = file.extensions.get(id);
        let enabled = entry.is_some_and(|e| e.enabled);
        let config = entry
            .map(|e| e.config.clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        (enabled, config)
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.snapshot(id).0
    }

    /// Un secret, pour l'extension qui s'en sert (côté serveur seulement : jamais renvoyé).
    pub fn secret(&self, id: &str, key: &str) -> Option<String> {
        let d = descriptor(id)?;
        if !d.secrets.iter().any(|f| f.key == key) {
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

    /// La configuration d'une extension (objet vide si elle n'a rien), champs privés compris : pour
    /// le serveur lui-même.
    pub fn config(&self, id: &str) -> Value {
        self.snapshot(id).1
    }

    fn view_of(&self, d: &'static Descriptor, entry: &Entry, manage: bool) -> View {
        let mut config = if entry.config.is_object() {
            entry.config.clone()
        } else {
            (d.normalize)(&json!({})).unwrap_or_else(|_| json!({}))
        };
        let status = self
            .instance(d.id)
            .map(|e| e.status(entry.enabled, &config))
            .unwrap_or(Value::Null);
        if !manage {
            if let Some(map) = config.as_object_mut() {
                for key in d.private_config {
                    map.remove(*key);
                }
            }
        }
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
                .map(|f| SecretView {
                    key: f.key,
                    label: f.label,
                    help: f.help,
                    set: entry.secrets.get(f.key).is_some_and(|v| !v.is_empty()),
                })
                .collect(),
            status,
        }
    }

    /// Toutes les extensions connues, dans l'ordre du catalogue. `manage` : la personne a le droit
    /// « extensions » (sinon, les champs de configuration privés sont retirés).
    pub fn list(&self, manage: bool) -> Vec<View> {
        let file = self.lock().clone();
        CATALOG
            .iter()
            .map(|d| {
                let entry = file.extensions.get(d.id).cloned().unwrap_or_default();
                self.view_of(d, &entry, manage)
            })
            .collect()
    }

    pub fn get(&self, id: &str, manage: bool) -> Option<View> {
        let d = descriptor(id)?;
        let entry = self.lock().extensions.get(id).cloned().unwrap_or_default();
        Some(self.view_of(d, &entry, manage))
    }

    /// Applique une modification. Rien n'est changé si une valeur est refusée ou si le disque
    /// refuse d'écrire. Une modification à la fois, `on_change` de l'extension compris.
    pub fn update(&self, id: &str, update: Update) -> Result<(View, Changes), UpdateError> {
        self.update_for(descriptor(id).ok_or(UpdateError::Unknown)?, update)
    }

    fn update_for(
        &self,
        d: &'static Descriptor,
        update: Update,
    ) -> Result<(View, Changes), UpdateError> {
        let id = d.id;
        let _one_at_a_time = self.update_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.read_only {
            return Err(UpdateError::Storage);
        }
        let before = self.lock().extensions.get(id).cloned().unwrap_or_default();
        let mut entry = before;
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
            if !d.secrets.iter().any(|f| f.key == key) {
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
        let known = self.lock().extensions.contains_key(id);
        if changes != Changes::default() || !known {
            let mut next = self.lock().clone();
            next.version = 1;
            next.extensions.insert(id.to_string(), entry.clone());
            if let Some(dir) = &self.dir {
                let bytes = serde_json::to_vec_pretty(&next).map_err(|_| UpdateError::Storage)?;
                write_secret(&dir.join(FILE), &bytes).map_err(|_| UpdateError::Storage)?;
            }
            *self.lock() = next;
        }
        // Toujours sous `update_lock` : la configuration et l'état de l'extension restent d'accord.
        if changes != Changes::default() {
            if let Some(ext) = self.instance(id) {
                ext.on_change(entry.enabled, &entry.config);
            }
        }
        Ok((self.view_of(d, &entry, true), changes))
    }

    /// Un tour périodique de chaque extension. Bloquant (quelques secondes au plus). `true` : un
    /// appel est parti. Éteintes, elles ne font rien.
    pub fn tick(&self, now: f64) -> bool {
        let mut called = false;
        for (d, ext) in &self.registry {
            let (enabled, config) = self.snapshot(d.id);
            called |= ext.tick(now, enabled, &config);
        }
        called
    }

    /// L'arrêt du serveur : chaque extension arrête ce qu'elle a lancé. Appelé après l'arrêt du fil
    /// des extensions.
    pub fn shutdown(&self) {
        for (_, ext) in &self.registry {
            ext.shutdown();
        }
    }

    // ---- météo -------------------------------------------------------------------------------

    fn weather_place(&self) -> Option<Place> {
        weather::place_of(&self.config(weather::ID))
    }

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
        (enabled, self.weather.view(enabled, place.as_ref()))
    }

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

    fn set_place(ext: &Extensions) {
        ext.update(
            weather::ID,
            Update {
                config: Some(lyon_config()),
                ..Update::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn everything_is_off_by_default_and_calls_nothing() {
        let api = Arc::new(Counting::default());
        let ext = Extensions::in_memory(api.clone());
        let list = ext.list(true);
        assert_eq!(list.len(), CATALOG.len());
        assert!(list.iter().all(|v| !v.enabled));
        assert!(!ext.tick(1.0));
        assert_eq!(ext.weather_search("Lyon"), Err(SearchError::Disabled));
        // Même configurée, éteinte : aucun appel.
        set_place(&ext);
        assert!(!ext.tick(1.0));
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
        assert!(!ext.tick(10.0));
        assert_eq!(ext.weather_search("Lyon").unwrap().len(), 1);
        set_place(&ext);
        assert!(ext.tick(100.0));
        assert!(!ext.tick(101.0), "pas avant 15 minutes");
        let (enabled, status) = ext.weather_status();
        assert!(enabled);
        assert_eq!(status.reading.unwrap().temperature, Some(12.5));
        assert_eq!(api.reads.load(Ordering::SeqCst), 1);
        // Tout survit à un redémarrage, sans nouvel appel.
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
        assert!(!again.tick(100_000.0));
        assert_eq!(again.weather_search("x"), Err(SearchError::Disabled));
        assert_eq!(api.reads.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_after_an_offline_activation_polls_once_then_waits() {
        // Activée avec son lieu hors ligne (aucun meteo.json), puis le serveur redémarre.
        let dir = temp("offline");
        let api = Arc::new(Counting::default());
        api.offline.store(true, Ordering::SeqCst);
        let ext = Extensions::open(&dir, api.clone());
        ext.update(weather::ID, on()).unwrap();
        set_place(&ext);
        assert!(ext.tick(10.0));
        assert!(ext.weather_status().1.failing);

        let again = Extensions::open(&dir, api.clone());
        api.offline.store(false, Ordering::SeqCst);
        assert!(again.tick(20.0), "un relevé aussitôt");
        assert!(again.weather_status().1.reading.is_some());
        let calls = api.reads.load(Ordering::SeqCst);
        // Toutes les 30 s pendant presque 15 minutes : aucun appel de plus.
        for i in 1..30 {
            assert!(!again.tick(20.0 + 30.0 * f64::from(i)));
        }
        assert_eq!(api.reads.load(Ordering::SeqCst), calls);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_keeps_the_last_value() {
        let api = Arc::new(Counting::default());
        let ext = Extensions::in_memory(api.clone());
        ext.update(weather::ID, on()).unwrap();
        set_place(&ext);
        assert!(ext.tick(0.0));
        api.offline.store(true, Ordering::SeqCst);
        assert!(ext.tick(weather::POLL_EVERY_S + 1.0));
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
        secrets: &[SecretField {
            key: "password",
            label: "Mot de passe",
            help: "Celui du compte.",
        }],
        private_config: &["email"],
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
        assert!(view.secrets[0].set);
        assert_eq!(view.secrets[0].label, "Mot de passe");
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
        assert!(changes.secrets && !view.secrets[0].set);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("TRES-SECRET"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_config_is_hidden_from_accounts_without_the_right() {
        let ext = Extensions::in_memory(Arc::new(DemoWeather));
        let entry = Entry {
            config: json!({"email": "sam@example.org", "autre": 1}),
            ..Entry::default()
        };
        let managed = ext.view_of(&WITH_SECRET, &entry, true);
        assert_eq!(managed.config["email"], "sam@example.org");
        let plain = ext.view_of(&WITH_SECRET, &entry, false);
        assert!(plain.config.get("email").is_none());
        assert_eq!(plain.config["autre"], 1);
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
    fn an_unreadable_file_is_set_aside_not_overwritten() {
        let dir = temp("garbage");
        std::fs::write(dir.join(FILE), b"{ pas du json AVEC-UN-SECRET").unwrap();
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        assert!(ext.list(true).iter().all(|v| !v.enabled));
        // Le contenu est gardé à côté, rien n'est écrasé, et on peut de nouveau régler.
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".illisible-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert!(std::fs::read_to_string(aside[0].path())
            .unwrap()
            .contains("AVEC-UN-SECRET"));
        ext.update(weather::ID, on()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_set_aside_makes_settings_read_only() {
        // Un dossier à la place du fichier : illisible, et impossible à renommer vers un nom qui
        // existe déjà comme dossier non vide.
        let dir = temp("readonly");
        let ext = Extensions::build(Some(&dir), File::default(), true, Arc::new(DemoWeather));
        assert_eq!(
            ext.update(weather::ID, on()).unwrap_err(),
            UpdateError::Storage
        );
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_and_unknown_extensions_survive_a_rewrite() {
        let dir = temp("future");
        std::fs::write(
            dir.join(FILE),
            br#"{"version":2,"futur":{"x":1},"extensions":{"inconnue":{"enabled":true,"secrets":{"k":"v"},"neuf":7},"weather":{"enabled":false,"champ":"nouveau"}}}"#,
        )
        .unwrap();
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        ext.update(weather::ID, on()).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        for kept in ["futur", "inconnue", "\"neuf\"", "\"k\"", "\"champ\""] {
            assert!(text.contains(kept), "{kept} perdu :\n{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_a_garbled_entry_does_not_turn_anything_on() {
        let dir = temp("garbled");
        std::fs::write(
            dir.join(FILE),
            br#"{"version":1,"extensions":{"inconnue":{"enabled":true},"weather":{"enabled":true,"config":"n'importe quoi"}}}"#,
        )
        .unwrap();
        let ext = Extensions::open(&dir, Arc::new(DemoWeather));
        // Une configuration illisible vaut « pas de lieu » : activée, mais rien à relever.
        assert!(ext.is_enabled(weather::ID));
        assert!(!ext.tick(1.0));
        assert_eq!(
            ext.get(weather::ID, true).unwrap().config,
            json!({"place": null})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
