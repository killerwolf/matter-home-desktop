//! La présence par le Wi-Fi de la box : Ostal Server se relie à la Bbox avec son mot de passe
//! d'administration, relève périodiquement les appareils actifs, et sait quel téléphone appartient
//! à quel compte. La décision (qui est à la maison, quel mode prendre) est dans `presence.rs` ; le
//! relevé et la règle de combinaison sont dans `hub/wifi.rs`.
//!
//! Fichiers, dans `serveur/` (jamais exporté) :
//! - `box.json` (0600) : l'adresse de la box, **son mot de passe** (jamais affiché, jamais
//!   journalisé, jamais exporté), l'interrupteur « Changer le mode de la maison selon la présence »
//!   (**désactivé** par défaut) et le délai avant de compter un départ ;
//! - `telephones.json` (0600) : quel compte a quel téléphone (le **nom** de l'appareil, jamais son
//!   adresse MAC ni son IP).
//!
//! Sûreté : tant que la box ne répond pas, ou refuse le mot de passe, rien n'est conclu : le mode
//! de la maison est figé, et un mot de passe refusé n'est plus rejoué (la box pourrait bloquer
//! l'accès) avant qu'on en donne un autre.

pub mod bbox;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::presence::Sighting;
use crate::security::write_secret;
use bbox::{BoxApi, BoxError, Host};

pub const CONFIG_FILE: &str = "box.json";
pub const PHONES_FILE: &str = "telephones.json";
/// Délai par défaut avant qu'un téléphone qu'on ne voit plus compte comme « parti ».
pub const DEFAULT_AWAY_DELAY_MIN: u32 = 10;
/// On ne relève pas la box plus souvent (une demande « Actualiser » rapprochée relit le dernier
/// relevé).
pub const MIN_POLL_GAP_S: f64 = 10.0;

/// L'état de la liaison avec la box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// Pas de box reliée.
    None,
    /// Le dernier relevé a réussi.
    Ok,
    /// La box ne répond pas (ou répond autre chose qu'une liste).
    Unreachable,
    /// La box a refusé le mot de passe.
    Refused,
}

/// Les réglages de la box. `Debug` n'en montre jamais le mot de passe.
#[derive(Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    url: String,
    #[serde(default)]
    password: String,
    /// « Changer le mode de la maison selon la présence » : désactivé tant qu'on ne l'a pas dit.
    #[serde(default)]
    auto_mode: bool,
    #[serde(default = "default_delay")]
    away_delay_min: u32,
}

fn default_delay() -> u32 {
    DEFAULT_AWAY_DELAY_MIN
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            url: String::new(),
            password: String::new(),
            auto_mode: false,
            away_delay_min: DEFAULT_AWAY_DELAY_MIN,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("url", &self.url)
            .field("password", &"<secret>")
            .field("auto_mode", &self.auto_mode)
            .finish()
    }
}

impl Config {
    fn linked(&self) -> bool {
        !self.url.is_empty() && !self.password.is_empty()
    }
}

/// Le téléphone d'un compte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneLink {
    pub user_id: String,
    /// Le nom de l'appareil sur la box (« iPhone-de-Sam »).
    pub host: String,
}

#[derive(Default, Serialize, Deserialize)]
struct PhonesFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    phones: Vec<PhoneLink>,
}

/// Pourquoi la box n'a pas pu être reliée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    BadUrl,
    Refused,
    Unreachable,
    Storage,
}

impl LinkError {
    /// La phrase qui le dit à l'administrateur.
    pub fn message(self) -> &'static str {
        match self {
            LinkError::BadUrl => "Adresse de la box invalide.",
            LinkError::Refused => "La box a refusé ce mot de passe.",
            LinkError::Unreachable => {
                "La box ne répond pas. Vérifiez que ce Mac est sur le Wi-Fi de la maison."
            }
            LinkError::Storage => "Le réglage n'a pas pu être enregistré.",
        }
    }
}

/// Pourquoi un téléphone n'a pas pu être attribué.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignError {
    /// Ce nom n'est pas (ou plus) dans la liste des téléphones libres.
    NotFree,
    /// Un autre compte l'a déjà.
    Taken,
    Storage,
}

/// Le relevé en cours.
struct Inner {
    config: Config,
    phones: Vec<PhoneLink>,
    link: Link,
    /// La dernière liste lue.
    hosts: Vec<Host>,
    /// Date du dernier relevé réussi, et de la dernière tentative.
    last_ok: Option<f64>,
    last_poll: Option<f64>,
    /// Ce qu'on a vu de chaque téléphone relié (clé : nom en minuscules).
    sightings: HashMap<String, Sighting>,
}

/// Ce que le relevé a rendu.
pub enum Observed {
    /// Pas de box reliée, un relevé trop rapproché, ou un mot de passe refusé qu'on ne rejoue pas.
    Skipped,
    /// La box n'a pas répondu comme il faut : **rien n'est conclu**, le mode est figé.
    Frozen(Link),
    /// Une liste fraîche : les observations sont à jour.
    Fresh,
}

pub struct Wifi {
    dir: Option<PathBuf>,
    api: Box<dyn BoxApi>,
    inner: Mutex<Inner>,
    /// Un seul relevé à la fois.
    poll_lock: Mutex<()>,
}

/// Une vue pour l'administrateur : ce qu'on sait de la liaison.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub linked: bool,
    pub link: Link,
    pub auto_mode: bool,
    /// Appareils vus actifs au dernier relevé (tous types).
    pub active_hosts: usize,
    pub last_ok: Option<f64>,
    pub away_delay_min: u32,
}

impl Wifi {
    /// Ouvre `dir/box.json` et `dir/telephones.json` (absents : pas de box, aucun téléphone,
    /// interrupteur désactivé). Un fichier illisible est mis de côté : on repart sans box, jamais
    /// avec le mode automatique.
    pub fn open(dir: &Path, api: Box<dyn BoxApi>) -> Self {
        let config = load::<Config>(&dir.join(CONFIG_FILE)).unwrap_or_default();
        let phones = load::<PhonesFile>(&dir.join(PHONES_FILE))
            .map(|f| f.phones)
            .unwrap_or_default();
        Self::build(Some(dir.to_path_buf()), api, config, phones)
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory(api: Box<dyn BoxApi>) -> Self {
        Self::build(None, api, Config::default(), Vec::new())
    }

    fn build(
        dir: Option<PathBuf>,
        api: Box<dyn BoxApi>,
        config: Config,
        phones: Vec<PhoneLink>,
    ) -> Self {
        let link = if config.linked() {
            Link::Unreachable
        } else {
            Link::None
        };
        Self {
            dir,
            api,
            inner: Mutex::new(Inner {
                config,
                phones,
                link,
                hosts: Vec::new(),
                last_ok: None,
                last_poll: None,
                sightings: HashMap::new(),
            }),
            poll_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_config(&self, config: &Config) -> Result<()> {
        if let Some(dir) = &self.dir {
            write_secret(
                &dir.join(CONFIG_FILE),
                serde_json::to_string_pretty(config)?.as_bytes(),
            )?;
        }
        Ok(())
    }

    fn save_phones(&self, phones: &[PhoneLink]) -> Result<()> {
        if let Some(dir) = &self.dir {
            let file = PhonesFile {
                version: 1,
                phones: phones.to_vec(),
            };
            write_secret(
                &dir.join(PHONES_FILE),
                serde_json::to_string_pretty(&file)?.as_bytes(),
            )?;
        }
        Ok(())
    }

    // ---- réglages ------------------------------------------------------------------------------

    pub fn status(&self) -> Status {
        let st = self.lock();
        Status {
            linked: st.config.linked(),
            link: st.link,
            auto_mode: st.config.auto_mode && st.config.linked(),
            active_hosts: st.hosts.iter().filter(|h| h.active).count(),
            last_ok: st.last_ok,
            away_delay_min: st.config.away_delay_min,
        }
    }

    /// L'adresse de la box reliée (pour la montrer à l'administrateur ; jamais le mot de passe).
    pub fn url(&self) -> Option<String> {
        let st = self.lock();
        st.config.linked().then(|| st.config.url.clone())
    }

    /// Relie la box : l'adresse est contrôlée, le mot de passe est **essayé** (une liste doit
    /// revenir) avant d'être gardé. Rien n'est enregistré si la box refuse ou ne répond pas.
    pub fn link(&self, url: &str, password: &str) -> Result<(), LinkError> {
        let url = bbox::check_url(url).map_err(|_| LinkError::BadUrl)?;
        if password.is_empty() || password.len() > 200 || password.chars().any(char::is_control) {
            return Err(LinkError::Refused);
        }
        let _poll = self.poll_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.api.reset();
        let hosts = self.api.hosts(&url, password).map_err(|e| match e {
            BoxError::Refused => LinkError::Refused,
            BoxError::Unreachable => LinkError::Unreachable,
        })?;
        let mut st = self.lock();
        let mut config = st.config.clone();
        config.url = url;
        config.password = password.to_string();
        config.version = 1;
        self.save_config(&config).map_err(|e| {
            log::error!("réglages de la box non enregistrés : {e:#}");
            LinkError::Storage
        })?;
        st.config = config;
        st.link = Link::Ok;
        st.hosts = hosts;
        st.last_ok = Some(crate::security::now());
        st.last_poll = st.last_ok;
        st.sightings.clear();
        Ok(())
    }

    /// Délie la box : le mot de passe est effacé et le mode automatique **désactivé**. Les
    /// téléphones attribués restent.
    pub fn unlink(&self) -> Result<(), LinkError> {
        let _poll = self.poll_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.api.reset();
        let mut st = self.lock();
        let mut config = st.config.clone();
        config.url.clear();
        config.password.clear();
        config.auto_mode = false;
        self.save_config(&config).map_err(|e| {
            log::error!("réglages de la box non enregistrés : {e:#}");
            LinkError::Storage
        })?;
        st.config = config;
        st.link = Link::None;
        st.hosts.clear();
        st.last_ok = None;
        st.sightings.clear();
        Ok(())
    }

    /// L'interrupteur « Changer le mode de la maison selon la présence ». On ne l'active que si la
    /// box est reliée.
    pub fn set_auto_mode(&self, on: bool) -> Result<(), &'static str> {
        let mut st = self.lock();
        if on && !st.config.linked() {
            return Err("Reliez d'abord la box de la maison.");
        }
        let mut config = st.config.clone();
        config.auto_mode = on;
        self.save_config(&config).map_err(|e| {
            log::error!("réglage non enregistré : {e:#}");
            LinkError::Storage.message()
        })?;
        st.config = config;
        Ok(())
    }

    /// Le délai avant de compter un départ vu seulement sur le Wi-Fi, en minutes (0 à 240).
    pub fn set_away_delay_min(&self, minutes: u32) -> Result<(), &'static str> {
        let mut st = self.lock();
        let mut config = st.config.clone();
        config.away_delay_min = minutes.min(240);
        self.save_config(&config).map_err(|e| {
            log::error!("réglage non enregistré : {e:#}");
            LinkError::Storage.message()
        })?;
        st.config = config;
        Ok(())
    }

    /// L'interrupteur est-il activé (et la box reliée) ?
    pub fn auto_mode(&self) -> bool {
        let st = self.lock();
        st.config.auto_mode && st.config.linked()
    }

    // ---- relevé --------------------------------------------------------------------------------

    /// Relève la box à la date `now` (secondes Unix) et met à jour ce qu'on a vu de chaque
    /// téléphone relié. Bloquant.
    ///
    /// - Pas de box, ou mot de passe déjà refusé : rien (on ne rejoue pas un mot de passe refusé) ;
    /// - la box ne répond pas, refuse, ou n'annonce aucun appareil actif (une liste vide serait lue
    ///   « personne à la maison ») : `Frozen`, et les absences en cours de comptage repartent de zéro ;
    /// - sinon la liste est gardée.
    ///
    /// `force` : ignore l'écart minimal entre deux relevés.
    pub fn observe(&self, now: f64, force: bool) -> Observed {
        let Ok(_poll) = self.poll_lock.try_lock() else {
            return Observed::Skipped;
        };
        let (url, password) = {
            let st = self.lock();
            if !st.config.linked() || st.link == Link::Refused {
                return Observed::Skipped;
            }
            if !force
                && st
                    .last_poll
                    .is_some_and(|at| (now - at).abs() < MIN_POLL_GAP_S)
            {
                return Observed::Skipped;
            }
            (st.config.url.clone(), st.config.password.clone())
        };
        let result = self.api.hosts(&url, &password);
        let mut st = self.lock();
        // La box a pu être déliée ou son mot de passe changé pendant la requête.
        if !st.config.linked() || st.config.url != url || st.config.password != password {
            return Observed::Skipped;
        }
        st.last_poll = Some(now);
        let failure = match result {
            Ok(hosts) if hosts.iter().any(|h| h.active) => {
                st.link = Link::Ok;
                st.last_ok = Some(now);
                let phones: Vec<String> = st.phones.iter().map(|p| p.host.to_lowercase()).collect();
                let seen: HashMap<String, bool> = hosts
                    .iter()
                    .map(|h| (h.name.to_lowercase(), h.active))
                    .collect();
                st.hosts = hosts;
                st.sightings.retain(|k, _| phones.contains(k));
                for key in phones {
                    let active = seen.get(&key).copied().unwrap_or(false);
                    let s = st.sightings.entry(key).or_default();
                    if active {
                        s.active_since.get_or_insert(now);
                        s.inactive_since = None;
                    } else {
                        s.inactive_since.get_or_insert(now);
                        s.active_since = None;
                    }
                }
                return Observed::Fresh;
            }
            Ok(_) => {
                log::warn!("box : aucun appareil actif dans la liste, relevé ignoré");
                Link::Unreachable
            }
            Err(BoxError::Refused) => {
                log::warn!(
                    "box : mot de passe refusé, plus de nouvel essai avant d'en donner un autre"
                );
                Link::Refused
            }
            Err(BoxError::Unreachable) => {
                log::warn!("box : pas de réponse exploitable");
                Link::Unreachable
            }
        };
        st.link = failure;
        // Une absence qu'on comptait n'est plus sûre : elle repart de zéro au retour de la box.
        for s in st.sightings.values_mut() {
            s.inactive_since = None;
        }
        Observed::Frozen(failure)
    }

    /// Change le mot de passe de la box (et relance les relevés si le précédent était refusé).
    pub fn change_password(&self, password: &str) -> Result<(), LinkError> {
        let url = self.lock().config.url.clone();
        if url.is_empty() {
            return Err(LinkError::Unreachable);
        }
        self.link(&url, password)
    }

    /// Rend, pour chaque téléphone relié, ce qu'on en a vu (clé : identifiant du compte).
    pub fn sightings(&self) -> Vec<(String, Sighting)> {
        let st = self.lock();
        st.phones
            .iter()
            .map(|p| {
                (
                    p.user_id.clone(),
                    st.sightings
                        .get(&p.host.to_lowercase())
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    /// Le délai avant de compter un départ, en secondes.
    pub fn away_delay_s(&self) -> f64 {
        f64::from(self.lock().config.away_delay_min) * 60.0
    }

    // ---- téléphones ----------------------------------------------------------------------------

    pub fn phones(&self) -> Vec<PhoneLink> {
        self.lock().phones.clone()
    }

    /// Le nom du téléphone de ce compte.
    pub fn phone_of(&self, user_id: &str) -> Option<String> {
        self.lock()
            .phones
            .iter()
            .find(|p| p.user_id == user_id)
            .map(|p| p.host.clone())
    }

    /// Ce téléphone est-il vu actif au dernier relevé ?
    pub fn is_seen(&self, host: &str) -> bool {
        self.lock()
            .hosts
            .iter()
            .any(|h| h.active && h.name.eq_ignore_ascii_case(host))
    }

    /// Les noms des appareils actifs, sans propriétaire. `phones_only` : seulement ceux qui ont un
    /// nom de téléphone (ce que voit un membre ; l'administrateur voit tout).
    pub fn free_hosts(&self, phones_only: bool) -> Vec<String> {
        let st = self.lock();
        let mut names: Vec<String> = st
            .hosts
            .iter()
            .filter(|h| h.active)
            .filter(|h| !phones_only || bbox::looks_like_phone(&h.name))
            .filter(|h| {
                !st.phones
                    .iter()
                    .any(|p| p.host.eq_ignore_ascii_case(&h.name))
            })
            .map(|h| h.name.clone())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Attribue ce téléphone à ce compte (celui qu'il avait est remplacé), ou le retire (`None`).
    /// Le nom doit être dans les appareils libres vus au dernier relevé. Rend `true` si quelque
    /// chose a changé.
    pub fn assign(
        &self,
        user_id: &str,
        host: Option<&str>,
        phones_only: bool,
    ) -> Result<bool, AssignError> {
        let mut st = self.lock();
        let mut phones = st.phones.clone();
        let before = phones.clone();
        let current = phones.iter().find(|p| p.user_id == user_id).cloned();
        match host {
            None => phones.retain(|p| p.user_id != user_id),
            Some(name) => {
                let name = bbox::clean_name(name);
                if current
                    .as_ref()
                    .is_some_and(|p| p.host.eq_ignore_ascii_case(&name))
                {
                    return Ok(false);
                }
                if phones.iter().any(|p| p.host.eq_ignore_ascii_case(&name)) {
                    return Err(AssignError::Taken);
                }
                let free = st
                    .hosts
                    .iter()
                    .find(|h| h.active && h.name.eq_ignore_ascii_case(&name))
                    .filter(|h| !phones_only || bbox::looks_like_phone(&h.name))
                    .map(|h| h.name.clone())
                    .ok_or(AssignError::NotFree)?;
                phones.retain(|p| p.user_id != user_id);
                phones.push(PhoneLink {
                    user_id: user_id.to_string(),
                    host: free,
                });
            }
        }
        if phones == before {
            return Ok(false);
        }
        self.save_phones(&phones).map_err(|e| {
            log::error!("téléphones non enregistrés : {e:#}");
            AssignError::Storage
        })?;
        // Les relevés de l'ancien téléphone ne valent plus pour personne.
        if let Some(old) = current {
            let key = old.host.to_lowercase();
            if !phones.iter().any(|p| p.host.to_lowercase() == key) {
                st.sightings.remove(&key);
            }
        }
        st.phones = phones;
        Ok(true)
    }
}

/// Lit un fichier JSON ; illisible : mis de côté (`.illisible`), `None`.
fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    match crate::persistence::load_json::<T>(path) {
        Ok(value) => value,
        Err(e) => {
            let aside = path.with_extension("json.illisible");
            log::error!(
                "{} illisible, ignoré ({e:#}) ; fichier conservé sous {}",
                path.display(),
                aside.display()
            );
            let _ = std::fs::rename(path, aside);
            None
        }
    }
}

/// Une box de démonstration : quelques téléphones, sans réseau (`--demo` ne sort jamais de la boucle
/// locale).
pub struct DemoBox;

impl BoxApi for DemoBox {
    fn hosts(&self, _url: &str, _password: &str) -> Result<Vec<Host>, BoxError> {
        let host = |name: &str, active: bool| Host {
            name: name.to_string(),
            active,
        };
        Ok(vec![
            host("iPhone-de-Hamdi", true),
            host("iPhone-de-Sam", true),
            host("Galaxy-de-Lea", false),
            host("Redmi-de-Adam", true),
            host("iPhone-7", true),
            host("android-3f2a", true),
            host("MacBook-Pro", true),
        ])
    }
}
