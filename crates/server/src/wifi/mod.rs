//! La présence par le Wi-Fi de la box : Ostal Server relève périodiquement les appareils actifs de
//! la box de la maison et sait quel téléphone appartient à quel compte. La box elle-même (adresse,
//! mot de passe, dialogue avec la marque) vient d'une **extension** (aujourd'hui « Bbox »,
//! `extensions/bbox.rs`) que ce module lit par le trait [`connector::BoxConnector`] ; il ne connaît
//! aucune marque. La décision (qui est à la maison, quel mode prendre) est dans `presence.rs` ; le
//! relevé et la règle de combinaison sont dans `hub/wifi.rs`.
//!
//! Fichiers, dans `serveur/` (jamais exportés) :
//! - `box.json` (0600) : l'interrupteur « Changer le mode de la maison selon la présence »
//!   (**désactivé** par défaut) et le délai avant de compter un départ. Les anciennes versions y
//!   gardaient aussi l'adresse et le mot de passe de la box : `migration.rs` les reprend dans
//!   `extensions.json` au premier démarrage ;
//! - `telephones.json` (0600) : quel compte a quel téléphone (le **nom** de l'appareil, jamais son
//!   adresse MAC ni son IP).
//!
//! Sûreté : sans extension de box activée, ou tant que la box ne répond pas ou refuse le mot de
//! passe, rien n'est conclu : le mode de la maison est figé, et un mot de passe refusé n'est plus
//! rejoué (la box pourrait bloquer l'accès) avant qu'on en donne un autre.

pub mod connector;
pub mod journal;
pub mod migration;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::presence::Sighting;
use crate::security::write_secret;
use connector::{BoxConnector, BoxError, Host};

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

/// Les réglages de la présence (`box.json`). `Debug` n'en montre jamais le mot de passe.
#[derive(Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    version: u32,
    /// Ancien réglage de la box (avant les extensions), repris par `migration.rs`. Gardé tel quel à
    /// la réécriture tant que la reprise n'a pas réussi : changer l'interrupteur ne doit jamais
    /// faire perdre le mot de passe.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
            .field("password", &"<secret>")
            .field("auto_mode", &self.auto_mode)
            .finish_non_exhaustive()
    }
}

/// La phrase qui dit qu'un réglage n'a pas pu être enregistré.
pub const STORAGE_ERROR: &str = "Le réglage n'a pas pu être enregistré.";

/// Pourquoi la box n'a pas pu être reliée (`Hub::wifi_link`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    BadUrl,
    /// Mot de passe vide, trop long ou invalide : la box n'a pas été appelée.
    BadPassword,
    /// Trop d'essais après un refus : la box n'a pas été appelée.
    TooSoon(&'static str),
    Refused,
    Unreachable,
    Storage,
}

impl LinkError {
    /// La phrase qui le dit à l'administrateur.
    pub fn message(self) -> &'static str {
        match self {
            LinkError::BadUrl => "Adresse de la box invalide.",
            LinkError::BadPassword => {
                "Mot de passe trop long ou invalide (la box n'a pas été appelée)."
            }
            LinkError::TooSoon(why) => why,
            LinkError::Refused => "La box a refusé ce mot de passe.",
            LinkError::Unreachable => {
                "La box ne répond pas. Vérifiez que ce Mac est sur le Wi-Fi de la maison."
            }
            LinkError::Storage => STORAGE_ERROR,
        }
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
    /// Le connecteur (identifiant, génération) dont viennent `hosts` et `sightings` : s'il change
    /// (autre box, autre mot de passe, extension éteinte), on repart de zéro.
    bound: Option<(&'static str, u64)>,
    /// La reprise des anciens réglages de la box a échoué au démarrage : dit dans le journal.
    notice: Option<String>,
}

/// Ce que le relevé a rendu.
pub enum Observed {
    /// Pas d'extension de box activée, un relevé trop rapproché, ou un mot de passe refusé qu'on ne
    /// rejoue pas.
    Skipped,
    /// La box n'a pas répondu comme il faut : **rien n'est conclu**, le mode est figé.
    Frozen(Link),
    /// Une liste fraîche : les observations sont à jour.
    Fresh,
}

pub struct Wifi {
    dir: Option<PathBuf>,
    /// Les connecteurs de box que cette version connaît (`Extensions::box_connectors`) : le
    /// premier qui est prêt (extension activée et complète) sert.
    connectors: Vec<Arc<dyn BoxConnector>>,
    inner: Mutex<Inner>,
    /// Un seul relevé à la fois.
    poll_lock: Mutex<()>,
    /// Les copies de l'ancien `box.json` ont déjà été supprimées (ou cherchées) depuis le démarrage.
    copies_checked: std::sync::atomic::AtomicBool,
    /// Ce que le journal a déjà dit (une ligne seulement quand quelque chose change).
    journal: Mutex<journal::Journal>,
}

/// Une vue pour l'administrateur : ce qu'on sait de la liaison.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Une extension de box est activée et complète.
    pub linked: bool,
    /// Le nom de cette box (« Bbox »), quand `linked`.
    pub connector: Option<&'static str>,
    pub link: Link,
    pub auto_mode: bool,
    /// Appareils vus actifs au dernier relevé (tous types).
    pub active_hosts: usize,
    pub last_ok: Option<f64>,
    pub away_delay_min: u32,
}

impl Wifi {
    /// Ouvre `dir/box.json` et `dir/telephones.json` (absents : aucun téléphone, interrupteur
    /// désactivé). Un fichier illisible est mis de côté : on repart avec l'interrupteur éteint,
    /// jamais avec le mode automatique. À ouvrir **après** `migration::run` (voir `Hub::new`).
    pub fn open(dir: &Path, connectors: Vec<Arc<dyn BoxConnector>>) -> Self {
        let config = load::<Config>(&dir.join(CONFIG_FILE)).unwrap_or_default();
        let phones = load::<PhonesFile>(&dir.join(PHONES_FILE))
            .map(|f| f.phones)
            .unwrap_or_default();
        Self::build(Some(dir.to_path_buf()), connectors, config, phones)
    }

    /// Sans fichier (démonstration, dossier inutilisable).
    pub fn in_memory(connectors: Vec<Arc<dyn BoxConnector>>) -> Self {
        Self::build(None, connectors, Config::default(), Vec::new())
    }

    fn build(
        dir: Option<PathBuf>,
        connectors: Vec<Arc<dyn BoxConnector>>,
        config: Config,
        phones: Vec<PhoneLink>,
    ) -> Self {
        Self {
            dir,
            connectors,
            inner: Mutex::new(Inner {
                config,
                phones,
                link: Link::Unreachable,
                hosts: Vec::new(),
                last_ok: None,
                last_poll: None,
                sightings: HashMap::new(),
                bound: None,
                notice: None,
            }),
            poll_lock: Mutex::new(()),
            copies_checked: std::sync::atomic::AtomicBool::new(false),
            journal: Mutex::new(journal::Journal::default()),
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

    /// Le connecteur de box qui sert : le premier dont l'extension est activée et complète.
    fn active(&self) -> Option<&Arc<dyn BoxConnector>> {
        self.connectors.iter().find(|c| c.ready())
    }

    /// Ce qu'on a relevé vient-il bien du connecteur actif, dans ses réglages d'aujourd'hui ?
    fn fresh(&self, st: &Inner) -> bool {
        self.active()
            .is_some_and(|c| st.bound == Some((c.id(), c.generation())))
    }

    pub fn status(&self) -> Status {
        let st = self.lock();
        let active = self.active();
        let fresh = self.fresh(&st);
        Status {
            linked: active.is_some(),
            connector: active.map(|c| c.name()),
            link: match active {
                None => Link::None,
                // Les réglages ont changé depuis le dernier relevé : rien n'est confirmé.
                Some(_) if !fresh => Link::Unreachable,
                Some(_) => st.link,
            },
            auto_mode: st.config.auto_mode,
            active_hosts: if fresh {
                st.hosts.iter().filter(|h| h.active).count()
            } else {
                0
            },
            last_ok: if fresh { st.last_ok } else { None },
            away_delay_min: st.config.away_delay_min,
        }
    }

    /// La mémoire du journal de présence (voir [`journal::Journal`]).
    pub fn journal(&self) -> MutexGuard<'_, journal::Journal> {
        self.journal.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// L'ancienne box de `box.json` (adresse, mot de passe), quand la reprise dans l'extension n'a
    /// pas pu se faire : la box reste alors lue comme avant (`Bbox::use_legacy`).
    pub fn legacy_box(&self) -> Option<(String, String)> {
        let st = self.lock();
        (!st.config.url.is_empty() && !st.config.password.is_empty())
            .then(|| (st.config.url.clone(), st.config.password.clone()))
    }

    /// Un relevé vient de réussir : la reprise des anciens réglages (`migration.rs`) est confirmée,
    /// les copies de l'ancien `box.json` (qui portent l'ancien mot de passe) sont supprimées. Une
    /// fois par démarrage, et jamais quand la reprise a échoué (la box est alors lue d'après
    /// `box.json`, qu'il faut garder).
    pub fn confirm_takeover(&self) {
        use std::sync::atomic::Ordering;
        if self.copies_checked.swap(true, Ordering::SeqCst) || self.legacy_box().is_some() {
            return;
        }
        if let Some(dir) = &self.dir {
            let n = migration::remove_copies(dir);
            if n > 0 {
                log::info!("extension Bbox : reprise confirmée par un relevé, {n} copie(s) de l'ancien box.json supprimée(s)");
            }
        }
    }

    /// Une phrase pour le journal : pourquoi la box n'est pas reliée alors qu'elle devrait l'être
    /// (la reprise des anciens réglages a échoué).
    pub fn set_notice(&self, notice: Option<String>) {
        self.lock().notice = notice;
    }

    pub fn notice(&self) -> Option<String> {
        self.lock().notice.clone()
    }

    /// Les comptes dont le téléphone associé n'est dans aucun relevé de la box (même inactif) :
    /// son nom d'hôte a peut-être changé.
    pub fn unlisted_phones(&self) -> Vec<String> {
        let st = self.lock();
        st.phones
            .iter()
            .filter(|p| {
                !st.hosts
                    .iter()
                    .any(|h| h.name.eq_ignore_ascii_case(&p.host))
            })
            .map(|p| p.user_id.clone())
            .collect()
    }

    /// L'interrupteur « Changer le mode de la maison selon la présence ». Il peut être allumé sans
    /// box : la règle ne s'appuie alors que sur les raccourcis « Je pars » et « Je rentre ».
    pub fn set_auto_mode(&self, on: bool) -> Result<(), &'static str> {
        let mut st = self.lock();
        let mut config = st.config.clone();
        config.auto_mode = on;
        self.save_config(&config).map_err(|e| {
            log::error!("réglage non enregistré : {e:#}");
            STORAGE_ERROR
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
            STORAGE_ERROR
        })?;
        st.config = config;
        Ok(())
    }

    /// L'interrupteur est-il activé ? (Avec ou sans box.)
    pub fn auto_mode(&self) -> bool {
        self.lock().config.auto_mode
    }

    // ---- relevé --------------------------------------------------------------------------------

    /// Relève la box à la date `now` (secondes Unix) et met à jour ce qu'on a vu de chaque
    /// téléphone relié. Bloquant.
    ///
    /// - Aucune extension de box prête, ou mot de passe déjà refusé : rien (on ne rejoue pas un mot
    ///   de passe refusé) ;
    /// - les réglages de l'extension ont changé depuis le dernier relevé : on repart de zéro (liste,
    ///   absences en cours de comptage) ;
    /// - la box ne répond pas, refuse, ou n'annonce aucun appareil actif (une liste vide serait lue
    ///   « personne à la maison ») : `Frozen`, et les absences en cours de comptage repartent de zéro ;
    /// - sinon la liste est gardée.
    ///
    /// `force` : ignore l'écart minimal entre deux relevés.
    pub fn observe(&self, now: f64, force: bool) -> Observed {
        let Ok(_poll) = self.poll_lock.try_lock() else {
            return Observed::Skipped;
        };
        let Some(connector) = self.active().cloned() else {
            // Plus d'extension de box : on n'a plus rien de valable en mémoire.
            let mut st = self.lock();
            if st.bound.take().is_some() {
                st.hosts.clear();
                st.sightings.clear();
                st.last_ok = None;
                st.last_poll = None;
            }
            st.link = Link::None;
            return Observed::Skipped;
        };
        let key = (connector.id(), connector.generation());
        {
            let mut st = self.lock();
            if st.bound != Some(key) {
                // Une autre box, un autre mot de passe : l'ancien relevé ne vaut plus.
                st.bound = Some(key);
                st.hosts.clear();
                st.sightings.clear();
                st.last_ok = None;
                st.last_poll = None;
                st.link = Link::Unreachable;
            }
            if st.link == Link::Refused {
                return Observed::Skipped;
            }
            if !force
                && st
                    .last_poll
                    .is_some_and(|at| (now - at).abs() < MIN_POLL_GAP_S)
            {
                return Observed::Skipped;
            }
        }
        let result = connector.hosts();
        let mut st = self.lock();
        // Les réglages ont pu changer (extension éteinte, mot de passe changé) pendant la requête.
        if st.bound != Some(key) || connector.generation() != key.1 {
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
        let st = self.lock();
        self.fresh(&st)
            && st
                .hosts
                .iter()
                .any(|h| h.active && h.name.eq_ignore_ascii_case(host))
    }

    /// Les noms des appareils connus de la box, sans propriétaire : ceux qui sont sur le Wi-Fi en
    /// ce moment d'abord, puis les autres (un téléphone parti reste reliable à distance).
    /// `phones_only` : seulement ceux qui ont un nom de téléphone (ce que voit un membre ;
    /// l'administrateur voit tout).
    pub fn free_hosts(&self, phones_only: bool) -> Vec<String> {
        let st = self.lock();
        let known: &[Host] = if self.fresh(&st) { &st.hosts } else { &[] };
        let mut hosts: Vec<&connector::Host> = known
            .iter()
            .filter(|h| !phones_only || connector::looks_like_phone(&h.name))
            .filter(|h| {
                !st.phones
                    .iter()
                    .any(|p| p.host.eq_ignore_ascii_case(&h.name))
            })
            .collect();
        hosts.sort_by_key(|h| (!h.active, h.name.to_lowercase()));
        hosts.into_iter().map(|h| h.name.clone()).collect()
    }

    /// Attribue ce téléphone à ce compte (celui qu'il avait est remplacé), ou le retire (`None`).
    /// Le nom doit être parmi les appareils libres connus de la box au dernier relevé. Rend `true` si quelque
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
                let name = connector::clean_name(name);
                if current
                    .as_ref()
                    .is_some_and(|p| p.host.eq_ignore_ascii_case(&name))
                {
                    return Ok(false);
                }
                if phones.iter().any(|p| p.host.eq_ignore_ascii_case(&name)) {
                    return Err(AssignError::Taken);
                }
                let known: &[Host] = if self.fresh(&st) { &st.hosts } else { &[] };
                let free = known
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case(&name))
                    .filter(|h| !phones_only || connector::looks_like_phone(&h.name))
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
