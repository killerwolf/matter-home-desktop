//! Extension « Volets Somfy » : les volets roulants (RTS) du Kit de connectivité Somfy, pilotés par
//! le cloud de Somfy (l'API de l'app TaHoma, voir `overkiz.rs`), sans Homebridge ni pont Apple Maison.
//!
//! - **Désactivée par défaut** ; tant qu'elle l'est (ou sans identifiant ni mot de passe), aucun
//!   appel ne part. L'éteindre, ou effacer le mot de passe, retire les volets d'Ostal, vide la file
//!   des ordres et oublie la session.
//! - Le compte : l'identifiant (e-mail, `config.email`, privé) et le mot de passe (un secret, jamais
//!   relu ni journalisé ; il ne vit en mémoire que dans `Credentials`).
//! - **Peu d'appels** : la liste des appareils est lue quand on active l'extension, quand l'identifiant
//!   ou le mot de passe change et sur demande (« Actualiser ») ; jamais en boucle. Un volet RTS n'a
//!   pas de retour d'état : rien à relever. Chaque ordre est un appel, envoyé par **un seul fil**,
//!   un ordre à la fois et avec un écart entre deux (pas de rafale).
//! - **Identifiants refusés** : un refus de Somfy bloque tout appel jusqu'à ce que l'identifiant ou
//!   le mot de passe change (ou qu'on rallume l'extension) : réessayer un mauvais mot de passe
//!   risque de faire bloquer le compte. Hors ligne : nouvel essai de la liste après 1, 2, 4… minutes
//!   (15 au plus) ; un **ordre** raté n'est jamais rejoué tout seul (un volet qui bouge en retard est
//!   pire qu'un volet qui n'a pas bougé).
//! - **État** : l'état affiché d'un volet est **le dernier ordre qu'Ostal lui a donné**
//!   (ouvert ou fermé), pas son état réel : si on le bouge à la télécommande, Ostal ne le sait pas.
//! - Les appareils d'Ostal (`NodeInfo`) sont construits par le hub à partir de [`Somfy::exposed`] ;
//!   leurs identifiants sont stables (gardés dans `somfy.json`, 0600, jamais exporté) et dans la plage
//!   [`protocol::events::EXTENSION_ID_MIN`].

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::overkiz::{Credentials, Order, OverkizApi, OverkizError, RemoteDevice};
use super::{Changes, Descriptor, Extension, SecretField, Settings};
use crate::security::write_secret;
use protocol::events::{EXTENSION_ID_MAX, EXTENSION_ID_MIN};

pub const ID: &str = "somfy";
pub const STATE_FILE: &str = "somfy.json";
pub const PASSWORD: &str = "password";
/// La classe Overkiz des volets roulants : cochés d'office.
const SHUTTER_KIND: &str = "RollerShutter";
/// Écart minimal entre deux ordres envoyés au cloud.
const ORDER_GAP: Duration = Duration::from_millis(700);
/// Ordres en attente au plus (au-delà : « occupé »).
const MAX_QUEUE: usize = 16;
/// Écart minimal entre deux lectures de la liste demandées à la main.
const MIN_REFRESH_GAP_S: f64 = 20.0;
const RETRY_MIN_S: f64 = 60.0;
const RETRY_MAX_S: f64 = 15.0 * 60.0;
const MAX_EMAIL: usize = 254;
const MAX_EXPOSED: usize = 200;
const MAX_KEY: usize = 200;

pub static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    name: "Volets Somfy",
    description: "Ouvrir et fermer vos volets roulants Somfy (Kit de connectivité) depuis Ostal : \
tuiles, plan de la maison, automatisations.",
    disclosure: "Une fois activée, Ostal se connecte au cloud de Somfy (ha101-1.overkiz.com, sur \
Internet) avec l'identifiant et le mot de passe de votre compte TaHoma : le mot de passe part chez \
Somfy à chaque connexion, et chaque ordre (ouvrir, fermer) y passe aussi. Ostal lit la liste de vos \
appareils quand vous activez l'extension, quand vous changez de compte et quand vous demandez \
« Actualiser ». Rien d'autre ne part. Désactivée, ou sans mot de passe, plus aucun appel.",
    secrets: &[SecretField {
        key: PASSWORD,
        label: "Mot de passe TaHoma",
        help: "Celui du compte de l'app TaHoma by Somfy, qui pilote votre Kit de connectivité. Les \
volets doivent déjà être ajoutés au Kit dans cette app. Le Kit n'a pas d'API locale : les ordres \
passent par le cloud de Somfy, Internet est donc nécessaire. Un volet RTS ne renvoie pas son état : \
Ostal affiche le dernier ordre qu'il lui a donné.",
    }],
    private_config: &["email", "exposed"],
    normalize,
};

/// Valide la configuration `{"email": "…", "exposed": null | ["<oid>", …]}` ; rien d'autre n'est
/// gardé. `exposed: null` : les volets roulants du compte (le choix d'office) ; une liste : ces
/// appareils-là, et eux seuls.
fn normalize(value: &Value) -> Result<Value, &'static str> {
    const BAD: &str = "Réglages Somfy invalides.";
    let obj = value.as_object().ok_or(BAD)?;
    if obj.keys().any(|k| k != "email" && k != "exposed") {
        return Err(BAD);
    }
    let email = match obj.get("email") {
        None | Some(Value::Null) => String::new(),
        Some(v) => v.as_str().ok_or(BAD)?.trim().to_string(),
    };
    if email.chars().count() > MAX_EMAIL || email.chars().any(char::is_control) {
        return Err("Identifiant invalide.");
    }
    let exposed = match obj.get("exposed") {
        None | Some(Value::Null) => Value::Null,
        Some(list) => {
            let list = list.as_array().ok_or(BAD)?;
            if list.len() > MAX_EXPOSED {
                return Err("Trop d'appareils choisis.");
            }
            let mut keys: Vec<String> = Vec::new();
            for key in list {
                let key = key.as_str().ok_or(BAD)?;
                if key.is_empty() || key.len() > MAX_KEY || key.chars().any(char::is_control) {
                    return Err(BAD);
                }
                if !keys.iter().any(|k| k == key) {
                    keys.push(key.to_string());
                }
            }
            json!(keys)
        }
    };
    Ok(json!({"email": email, "exposed": exposed}))
}

fn email_of(config: &Value) -> String {
    config
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn exposed_of(config: &Value) -> Option<Vec<String>> {
    config.get("exposed")?.as_array().map(|list| {
        list.iter()
            .filter_map(|k| k.as_str().map(str::to_owned))
            .collect()
    })
}

// ---- ce qui est gardé sur disque -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    oid: String,
    url: String,
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    commandable: bool,
}

impl From<RemoteDevice> for Cached {
    fn from(d: RemoteDevice) -> Self {
        Self {
            oid: d.oid,
            url: d.url,
            name: d.name,
            kind: d.kind,
            commandable: d.commandable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct LastOrder {
    open: bool,
    at: f64,
}

/// `somfy.json` : les identifiants d'Ostal (jamais réutilisés), la dernière liste connue du compte
/// et le dernier ordre de chaque volet. Les adresses des appareils contiennent le code du Kit :
/// fichier privé (0600), hors de l'export, jamais montré.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    version: u32,
    /// `oid` → identifiant d'Ostal.
    #[serde(default)]
    ids: BTreeMap<String, u64>,
    #[serde(default)]
    devices: Vec<Cached>,
    #[serde(default)]
    synced_at: Option<f64>,
    #[serde(default)]
    orders: BTreeMap<String, LastOrder>,
}

// ---- ce que voit le reste du serveur ---------------------------------------------------------

/// Un volet exposé dans Ostal.
#[derive(Debug, Clone, PartialEq)]
pub struct Exposed {
    pub id: u64,
    pub name: String,
    /// Le dernier ordre donné par Ostal (`true` : ouvrir) et son heure ; `None` : jamais.
    pub last_order: Option<(bool, f64)>,
}

/// Un appareil du compte, pour la page de réglages (droit « extensions »). `key` est l'`oid`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceChoice {
    pub key: String,
    pub name: String,
    /// La classe Overkiz (`RollerShutter`…).
    pub kind: String,
    /// Sait ouvrir et fermer : sinon, on ne peut pas le choisir.
    pub commandable: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choices {
    pub devices: Vec<DeviceChoice>,
    pub synced_at: Option<f64>,
}

/// L'état de santé de la connexion au cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    /// Pas encore d'appel réussi ni échoué.
    Unknown,
    Ok,
    /// Somfy refuse l'identifiant ou le mot de passe : plus aucun appel tant qu'ils ne changent pas.
    Rejected,
    /// Pas de réponse du cloud.
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderError {
    /// Éteinte, ou sans identifiant ni mot de passe.
    Disabled,
    /// Pas un volet exposé.
    Unknown,
    /// Somfy a refusé le compte : corrigez-le d'abord.
    Rejected,
    /// Trop d'ordres en attente.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshError {
    Disabled,
    Rejected,
    TooSoon,
}

struct Job {
    oid: String,
    url: String,
    name: String,
    order: Order,
    /// Ce que l'ordre devient dans Ostal (« dernier ordre ») une fois parti.
    state: LastOrder,
}

struct Inner {
    saved: Saved,
    enabled: bool,
    /// Le compte, quand l'extension est allumée et complète.
    creds: Option<Credentials>,
    exposed: Option<Vec<String>>,
    health: Health,
    /// Une lecture de la liste est à faire.
    sync_due: bool,
    retry_at: f64,
    failures: u32,
    last_attempt: f64,
    /// Il faut oublier la session (compte changé, extension éteinte) : fait par le fil des
    /// extensions, jamais sous le verrou des réglages.
    forget_session: bool,
    /// Les ordres envoyés dans Ostal et pas encore partis : montrés comme déjà donnés.
    pending: BTreeMap<String, LastOrder>,
    queue: VecDeque<Job>,
    worker: Option<JoinHandle<()>>,
    worker_running: bool,
    stopping: bool,
    /// Dernier ordre raté (phrase française, sans adresse ni code), pour l'état de l'extension.
    last_error: Option<&'static str>,
    /// Phrases pour le journal d'Ostal (`Hub::sync_extension_devices`).
    notices: Vec<String>,
}

pub struct Somfy {
    dir: Option<PathBuf>,
    api: Arc<dyn OverkizApi>,
    inner: Mutex<Inner>,
    /// Une lecture de la liste à la fois.
    sync_lock: Mutex<()>,
    /// Ce qu'Ostal montre a changé : le hub le relit.
    changed: AtomicBool,
}

impl Somfy {
    pub fn new(dir: Option<&Path>, api: Arc<dyn OverkizApi>) -> Self {
        let saved: Saved = dir
            .and_then(|d| std::fs::read(d.join(STATE_FILE)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            dir: dir.map(Path::to_path_buf),
            api,
            inner: Mutex::new(Inner {
                saved,
                enabled: false,
                creds: None,
                exposed: None,
                health: Health::Unknown,
                sync_due: false,
                retry_at: 0.0,
                failures: 0,
                last_attempt: 0.0,
                forget_session: false,
                pending: BTreeMap::new(),
                queue: VecDeque::new(),
                worker: None,
                worker_running: false,
                stopping: false,
                last_error: None,
                notices: Vec::new(),
            }),
            sync_lock: Mutex::new(()),
            changed: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mark_changed(&self) {
        self.changed.store(true, Ordering::SeqCst);
    }

    /// Ce qu'Ostal montre a-t-il changé depuis la dernière fois ? (Remet à zéro.)
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::SeqCst)
    }

    /// Les phrases à mettre au journal d'Ostal (ordre raté…), une fois chacune.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().notices)
    }

    /// Écrit `somfy.json` (sous le verrou de l'état : les écritures ne s'entrecroisent pas).
    fn save(&self, inner: &Inner) {
        let Some(dir) = &self.dir else { return };
        let saved = Saved {
            version: 1,
            ids: inner.saved.ids.clone(),
            devices: inner.saved.devices.clone(),
            synced_at: inner.saved.synced_at,
            orders: inner.saved.orders.clone(),
        };
        let result = serde_json::to_vec(&saved)
            .map_err(anyhow::Error::from)
            .and_then(|b| write_secret(&dir.join(STATE_FILE), &b));
        if let Err(e) = result {
            log::warn!("somfy : état non gardé sur disque : {e:#}");
        }
    }

    /// Un appareil du compte est-il exposé dans Ostal ?
    fn is_exposed(inner: &Inner, device: &Cached) -> bool {
        device.commandable
            && match &inner.exposed {
                Some(list) => list.contains(&device.oid),
                None => device.kind == SHUTTER_KIND,
            }
    }

    /// L'identifiant d'Ostal d'un appareil (tiré une fois, gardé pour toujours).
    fn id_of(inner: &mut Inner, oid: &str) -> Option<u64> {
        if let Some(id) = inner.saved.ids.get(oid) {
            return Some(*id);
        }
        let next = inner
            .saved
            .ids
            .values()
            .copied()
            .max()
            .map_or(EXTENSION_ID_MIN + 1, |m| m + 1);
        if next > EXTENSION_ID_MAX {
            return None;
        }
        inner.saved.ids.insert(oid.to_string(), next);
        Some(next)
    }

    fn live(inner: &Inner) -> bool {
        inner.enabled && inner.creds.is_some()
    }

    /// Les volets exposés dans Ostal : rien tant que l'extension est éteinte ou incomplète.
    pub fn exposed(&self) -> Vec<Exposed> {
        let mut inner = self.lock();
        if !Self::live(&inner) {
            return Vec::new();
        }
        let devices: Vec<Cached> = inner
            .saved
            .devices
            .iter()
            .filter(|d| Self::is_exposed(&inner, d))
            .cloned()
            .collect();
        let mut fresh = false;
        let mut out = Vec::new();
        for d in devices {
            let known = inner.saved.ids.contains_key(&d.oid);
            let Some(id) = Self::id_of(&mut inner, &d.oid) else {
                continue;
            };
            fresh |= !known;
            let last = inner
                .pending
                .get(&d.oid)
                .or_else(|| inner.saved.orders.get(&d.oid))
                .copied();
            out.push(Exposed {
                id,
                name: d.name,
                last_order: last.map(|o| (o.open, o.at)),
            });
        }
        if fresh {
            self.save(&inner);
        }
        out
    }

    /// Le compte est refusé : les volets sont montrés « injoignables » avec la raison.
    pub fn rejected(&self) -> bool {
        let inner = self.lock();
        Self::live(&inner) && inner.health == Health::Rejected
    }

    /// Les appareils du compte (dernière liste connue), pour la page de réglages.
    pub fn choices(&self) -> Choices {
        let inner = self.lock();
        Choices {
            devices: inner
                .saved
                .devices
                .iter()
                .map(|d| DeviceChoice {
                    key: d.oid.clone(),
                    name: d.name.clone(),
                    kind: d.kind.clone(),
                    commandable: d.commandable,
                    selected: Self::is_exposed(&inner, d),
                })
                .collect(),
            synced_at: inner.saved.synced_at,
        }
    }

    // ---- réglages -------------------------------------------------------------------------

    fn creds_of(settings: &Settings) -> Option<Credentials> {
        let user = email_of(settings.config);
        let password = settings.secret(PASSWORD)?.to_string();
        (!user.is_empty()).then_some(Credentials { user, password })
    }

    /// Prend les réglages en compte. `fresh` : le compte, ou l'état allumé/éteint, a changé : on
    /// oublie une éventuelle interdiction d'appeler et on relit la liste.
    fn apply(&self, settings: &Settings, changes: Option<&Changes>) {
        let creds = if settings.enabled {
            Self::creds_of(settings)
        } else {
            None
        };
        let mut inner = self.lock();
        let was_live = Self::live(&inner);
        let account_changed = inner.creds != creds;
        inner.enabled = settings.enabled;
        inner.exposed = exposed_of(settings.config);
        inner.creds = creds;
        if !Self::live(&inner) {
            // Éteinte ou incomplète : plus rien ne part, plus rien n'est montré.
            inner.queue.clear();
            inner.pending.clear();
            inner.sync_due = false;
            inner.health = Health::Unknown;
            inner.failures = 0;
            inner.last_error = None;
            inner.forget_session |= was_live;
        } else if changes.is_none() {
            // Démarrage du serveur : la dernière liste connue suffit, aucun appel ; sans liste
            // (jamais lue), on la lit.
            inner.sync_due = inner.saved.devices.is_empty();
        } else if account_changed || changes.is_some_and(|c| c.switched.is_some()) || !was_live {
            inner.health = Health::Unknown;
            inner.failures = 0;
            inner.retry_at = 0.0;
            inner.last_error = None;
            inner.sync_due = true;
            // Une ancienne session ne sert plus : on repart d'une connexion.
            inner.forget_session |= account_changed && was_live;
        } else if inner.saved.devices.is_empty() && inner.health != Health::Rejected {
            inner.sync_due = true;
        }
        drop(inner);
        self.mark_changed();
    }

    // ---- la liste des appareils -----------------------------------------------------------

    /// Une lecture de la liste si elle est due et permise. Bloquant. `true` : un appel est parti.
    fn poll(&self, now: f64) -> bool {
        let _one_at_a_time = match self.sync_lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        let creds = {
            let mut inner = self.lock();
            if inner.forget_session {
                inner.forget_session = false;
                drop(inner);
                self.api.forget_session();
                inner = self.lock();
            }
            if !Self::live(&inner)
                || !inner.sync_due
                || inner.health == Health::Rejected
                || now < inner.retry_at
            {
                return false;
            }
            inner.last_attempt = now;
            match inner.creds.clone() {
                Some(c) => c,
                None => return false,
            }
        };
        let result = self.api.devices(&creds);
        self.record_devices(&creds, now, result);
        true
    }

    fn record_devices(
        &self,
        creds: &Credentials,
        now: f64,
        result: Result<Vec<RemoteDevice>, OverkizError>,
    ) {
        let mut inner = self.lock();
        // Le compte a changé pendant l'appel : ce résultat ne vaut plus.
        if inner.creds.as_ref() != Some(creds) {
            return;
        }
        match result {
            Ok(list) => {
                inner.saved.devices = list.into_iter().map(Cached::from).collect();
                inner.saved.synced_at = Some(now);
                inner.health = Health::Ok;
                inner.failures = 0;
                inner.sync_due = false;
                // Des identifiants pour tout ce qui est exposé, gardés pour toujours.
                let wanted: Vec<String> = inner
                    .saved
                    .devices
                    .iter()
                    .filter(|d| Self::is_exposed(&inner, d))
                    .map(|d| d.oid.clone())
                    .collect();
                for oid in wanted {
                    Self::id_of(&mut inner, &oid);
                }
                self.save(&inner);
                log::info!(
                    "somfy : {} appareil(s) dans le compte",
                    inner.saved.devices.len()
                );
            }
            Err(OverkizError::Rejected) => {
                inner.health = Health::Rejected;
                inner.sync_due = false;
                inner.queue.clear();
                inner.pending.clear();
                log::warn!(
                    "somfy : identifiant ou mot de passe refusés par Somfy, plus aucun appel"
                );
            }
            Err(e) => {
                inner.health = Health::Offline;
                inner.failures = inner.failures.saturating_add(1);
                let wait = (RETRY_MIN_S * 2f64.powi(inner.failures as i32 - 1)).min(RETRY_MAX_S);
                inner.retry_at = now + wait;
                log::warn!(
                    "somfy : liste des appareils impossible ({}), nouvel essai dans {} s",
                    match e {
                        OverkizError::BadReply => "réponse inattendue",
                        _ => "pas de réponse de Somfy",
                    },
                    wait as u64
                );
            }
        }
        drop(inner);
        self.mark_changed();
    }

    /// « Actualiser » : lire la liste maintenant (au plus une fois toutes les 20 s).
    pub fn request_refresh(&self, now: f64) -> Result<(), RefreshError> {
        let mut inner = self.lock();
        if !Self::live(&inner) {
            return Err(RefreshError::Disabled);
        }
        if inner.health == Health::Rejected {
            return Err(RefreshError::Rejected);
        }
        if now >= inner.last_attempt && now - inner.last_attempt < MIN_REFRESH_GAP_S {
            return Err(RefreshError::TooSoon);
        }
        inner.sync_due = true;
        inner.retry_at = 0.0;
        Ok(())
    }

    // ---- les ordres -----------------------------------------------------------------------

    /// Un ordre à un volet : mis en file (jamais d'appel ici), montré aussitôt comme donné, envoyé
    /// par le fil des ordres. `true` : ouvrir.
    pub fn order(this: &Arc<Self>, id: u64, order: Order, now: f64) -> Result<(), OrderError> {
        let mut inner = this.lock();
        if !Self::live(&inner) || inner.stopping {
            return Err(OrderError::Disabled);
        }
        if inner.health == Health::Rejected {
            return Err(OrderError::Rejected);
        }
        let device = inner
            .saved
            .devices
            .iter()
            .find(|d| inner.saved.ids.get(&d.oid) == Some(&id) && Self::is_exposed(&inner, d))
            .cloned()
            .ok_or(OrderError::Unknown)?;
        // Un ordre plus récent pour le même volet remplace celui qui attend.
        inner.queue.retain(|j| j.oid != device.oid);
        if inner.queue.len() >= MAX_QUEUE {
            return Err(OrderError::Busy);
        }
        let state = LastOrder {
            open: order == Order::Open,
            at: now,
        };
        inner.pending.insert(device.oid.clone(), state);
        inner.queue.push_back(Job {
            oid: device.oid,
            url: device.url,
            name: device.name,
            order,
            state,
        });
        if !inner.worker_running {
            inner.worker_running = true;
            let old = inner.worker.take();
            let me = Arc::clone(this);
            match std::thread::Builder::new()
                .name("somfy".into())
                .spawn(move || me.work())
            {
                Ok(handle) => {
                    drop(inner);
                    if let Some(old) = old {
                        let _ = old.join();
                    }
                    this.lock().worker = Some(handle);
                }
                Err(_) => {
                    inner.worker_running = false;
                    inner.queue.clear();
                    inner.pending.clear();
                    return Err(OrderError::Busy);
                }
            }
        }
        this.mark_changed();
        Ok(())
    }

    /// Le fil des ordres : un à la fois, avec un écart, jusqu'à ce que la file soit vide.
    fn work(self: Arc<Self>) {
        loop {
            let (job, creds) = {
                let mut inner = self.lock();
                let next = if inner.stopping {
                    None
                } else {
                    inner.queue.pop_front().zip(inner.creds.clone())
                };
                match next {
                    Some(pair) => pair,
                    None => {
                        inner.worker_running = false;
                        return;
                    }
                }
            };
            let label = format!("Ostal : {}", job.name);
            let result = self.api.order(&creds, &job.url, &label, job.order);
            self.record_order(&creds, job, result);
            // Pas de rafale : un écart avant l'ordre suivant, interrompu par l'arrêt.
            let mut waited = Duration::ZERO;
            while waited < ORDER_GAP {
                {
                    let inner = self.lock();
                    if inner.stopping || inner.queue.is_empty() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
                waited += Duration::from_millis(50);
            }
        }
    }

    fn record_order(&self, creds: &Credentials, job: Job, result: Result<(), OverkizError>) {
        let mut inner = self.lock();
        // Éteinte (ou compte changé) pendant l'appel : rien à noter.
        if inner.creds.as_ref() != Some(creds) {
            return;
        }
        // Un ordre plus récent pour ce volet peut attendre : il garde sa place dans `pending`.
        if inner.pending.get(&job.oid) == Some(&job.state) {
            inner.pending.remove(&job.oid);
        }
        match result {
            Ok(()) => {
                inner.saved.orders.insert(job.oid.clone(), job.state);
                self.save(&inner);
                inner.health = Health::Ok;
                inner.last_error = None;
            }
            Err(e) => {
                // L'ordre n'est pas parti : l'état d'avant revient (le dernier ordre réussi), et on
                // le dit. Jamais rejoué.
                let (why, line) = match e {
                    OverkizError::Rejected => {
                        inner.health = Health::Rejected;
                        inner.queue.clear();
                        inner.pending.clear();
                        (
                            "Somfy refuse l'identifiant ou le mot de passe.",
                            "Somfy refuse l'identifiant ou le mot de passe : plus aucun ordre ne part.",
                        )
                    }
                    OverkizError::BadReply => (
                        "Réponse inattendue de Somfy.",
                        "réponse inattendue de Somfy, l'ordre n'est pas parti.",
                    ),
                    OverkizError::Unreachable => (
                        "Somfy ne répond pas.",
                        "Somfy ne répond pas, l'ordre n'est pas parti.",
                    ),
                };
                inner.last_error = Some(why);
                inner.notices.push(format!("{} : {line}", job.name));
                log::warn!("somfy : ordre non transmis ({why})");
            }
        }
        drop(inner);
        self.mark_changed();
    }

    // ---- état montré à tous ---------------------------------------------------------------

    fn view(&self, enabled: bool) -> Value {
        let inner = self.lock();
        let live = Self::live(&inner);
        let state = if !enabled {
            "off"
        } else if !live {
            "unconfigured"
        } else {
            match inner.health {
                Health::Rejected => "rejected",
                Health::Offline => "offline",
                Health::Ok => "ok",
                Health::Unknown => "waiting",
            }
        };
        let devices = if live {
            inner
                .saved
                .devices
                .iter()
                .filter(|d| Self::is_exposed(&inner, d))
                .count()
        } else {
            0
        };
        json!({
            "state": state,
            "devices": devices,
            "synced_at": if live { inner.saved.synced_at } else { None },
            "last_error": if live { inner.last_error } else { None },
        })
    }
}

impl Extension for Somfy {
    fn status(&self, enabled: bool, _config: &Value) -> Value {
        // Ni l'identifiant ni une adresse : tous les comptes voient ce résultat.
        self.view(enabled)
    }

    fn start(&self, settings: &Settings) {
        self.apply(settings, None);
    }

    fn on_change(&self, settings: &Settings, changes: &Changes) {
        // Sous le verrou des réglages : on note, rien de long (les appels partent plus tard, du fil
        // des extensions).
        self.apply(settings, Some(changes));
    }

    fn tick(&self, now: f64, _settings: &Settings) -> bool {
        self.poll(now)
    }

    fn shutdown(&self) {
        let worker = {
            let mut inner = self.lock();
            inner.stopping = true;
            inner.queue.clear();
            inner.worker.take()
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        self.api.forget_session();
    }
}

#[cfg(test)]
mod tests {
    use super::super::overkiz::parse_devices;
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const DEVICES: &str = include_str!("fixtures/overkiz-devices.json");

    /// Un faux cloud : la liste enregistrée, des ordres notés, des pannes à la demande.
    #[derive(Default)]
    struct Fake {
        lists: AtomicUsize,
        orders: Mutex<Vec<(String, Order, String)>>,
        fail: Mutex<Option<OverkizError>>,
        forgets: AtomicUsize,
        logins_seen: Mutex<Vec<String>>,
    }

    impl OverkizApi for Fake {
        fn devices(&self, creds: &Credentials) -> Result<Vec<RemoteDevice>, OverkizError> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            self.logins_seen.lock().unwrap().push(creds.user.clone());
            if let Some(e) = *self.fail.lock().unwrap() {
                return Err(e);
            }
            Ok(parse_devices(DEVICES.as_bytes()).unwrap())
        }
        fn order(
            &self,
            _creds: &Credentials,
            url: &str,
            label: &str,
            order: Order,
        ) -> Result<(), OverkizError> {
            if let Some(e) = *self.fail.lock().unwrap() {
                return Err(e);
            }
            self.orders
                .lock()
                .unwrap()
                .push((url.to_string(), order, label.to_string()));
            Ok(())
        }
        fn forget_session(&self) {
            self.forgets.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ostal-somfy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Setup {
        enabled: bool,
        config: Value,
        secrets: BTreeMap<String, String>,
    }

    impl Setup {
        fn on() -> Self {
            Self {
                enabled: true,
                config: json!({"email": "sam@example.org", "exposed": null}),
                secrets: BTreeMap::from([(PASSWORD.to_string(), "pa\"ss\\".to_string())]),
            }
        }
        fn settings(&self) -> Settings<'_> {
            Settings::new(self.enabled, &self.config, &self.secrets)
        }
    }

    fn wait_idle(somfy: &Somfy) {
        for _ in 0..200 {
            if !somfy.lock().worker_running {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("le fil des ordres ne s'arrête pas");
    }

    fn enabled(fake: &Arc<Fake>, dir: Option<&Path>) -> (Arc<Somfy>, Setup) {
        let somfy = Arc::new(Somfy::new(dir, fake.clone()));
        let setup = Setup::on();
        somfy.start(&setup.settings());
        (somfy, setup)
    }

    #[test]
    fn nothing_is_called_while_off_or_incomplete() {
        let fake = Arc::new(Fake::default());
        let somfy = Arc::new(Somfy::new(None, fake.clone()));
        // Jamais configurée.
        somfy.start(&Setup::on().settings());
        let mut off = Setup::on();
        off.enabled = false;
        somfy.on_change(&off.settings(), &Changes::default());
        assert!(!somfy.tick(1.0, &off.settings()));
        // Allumée sans mot de passe : rien non plus, et rien n'est exposé.
        let mut no_password = Setup::on();
        no_password.secrets.clear();
        somfy.on_change(&no_password.settings(), &Changes::default());
        assert!(!somfy.tick(2.0, &no_password.settings()));
        // Sans identifiant.
        let mut no_email = Setup::on();
        no_email.config = json!({"email": "", "exposed": null});
        somfy.on_change(&no_email.settings(), &Changes::default());
        assert!(!somfy.tick(3.0, &no_email.settings()));
        assert_eq!(fake.lists.load(Ordering::SeqCst), 0);
        assert!(somfy.exposed().is_empty());
        assert_eq!(
            somfy.status(true, &no_email.config)["state"],
            "unconfigured"
        );
        assert_eq!(somfy.status(false, &no_email.config)["state"], "off");
    }

    #[test]
    fn enabling_reads_the_list_once_and_exposes_only_the_shutters() {
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, None);
        assert!(somfy.take_changed());
        assert!(somfy.tick(10.0, &setup.settings()));
        assert!(!somfy.tick(11.0, &setup.settings()), "une seule lecture");
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        let names: Vec<_> = somfy.exposed().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Porte", "Garage"], "ni le voyant, ni le Kit");
        let choices = somfy.choices();
        assert_eq!(choices.devices.len(), 3);
        assert!(choices.devices[0].selected && !choices.devices[2].selected);
        assert!(!choices.devices[2].commandable);
        // L'état montré à tous ne dit ni l'identifiant ni une adresse.
        let status = somfy.status(true, &setup.config).to_string();
        assert!(status.contains("\"ok\"") && status.contains("\"devices\":2"));
        assert!(
            !status.contains("sam@") && !status.contains("1234-5678") && !status.contains("rts")
        );
    }

    #[test]
    fn a_manual_choice_replaces_the_default_and_never_picks_what_cannot_open() {
        let fake = Arc::new(Fake::default());
        let (somfy, mut setup) = enabled(&fake, None);
        somfy.tick(10.0, &setup.settings());
        setup.config = json!({"email": "sam@example.org", "exposed": [
            "5a1ae8c2-0000-4000-8000-000000000002", // Garage
            "5a1ae8c2-0000-4000-8000-000000000004", // le voyant : ne sait pas ouvrir
        ]});
        somfy.on_change(
            &setup.settings(),
            &Changes {
                config: true,
                ..Changes::default()
            },
        );
        // Un simple choix ne rappelle pas le cloud.
        assert!(!somfy.tick(100.0, &setup.settings()));
        let names: Vec<_> = somfy.exposed().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Garage"]);
    }

    #[test]
    fn ids_are_stable_in_the_extension_range_and_survive_a_restart_offline() {
        let dir = temp("ids");
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, Some(&dir));
        somfy.tick(10.0, &setup.settings());
        let first = somfy.exposed();
        assert!(first
            .iter()
            .all(|e| protocol::events::is_extension_id(e.id)));
        assert_ne!(first[0].id, first[1].id);
        // Redémarrage : même liste, mêmes identifiants, sans aucun appel au cloud.
        let again_fake = Arc::new(Fake::default());
        let (again, again_setup) = enabled(&again_fake, Some(&dir));
        assert!(!again.tick(20.0, &again_setup.settings()));
        assert_eq!(again_fake.lists.load(Ordering::SeqCst), 0);
        assert_eq!(again.exposed(), first);
        // Le fichier est privé.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(STATE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_order_is_queued_shown_at_once_sent_by_the_worker_and_remembered() {
        let dir = temp("order");
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, Some(&dir));
        somfy.tick(10.0, &setup.settings());
        let porte = somfy.exposed()[0].id;
        assert_eq!(somfy.exposed()[0].last_order, None);
        Somfy::order(&somfy, porte, Order::Open, 50.0).unwrap();
        wait_idle(&somfy);
        let sent = fake.orders.lock().unwrap().clone();
        assert_eq!(
            sent,
            vec![(
                "rts://1234-5678-9012/16756321".into(),
                Order::Open,
                "Ostal : Porte".into()
            )]
        );
        assert_eq!(somfy.exposed()[0].last_order, Some((true, 50.0)));
        // Après un redémarrage, le dernier ordre est toujours là (sans appel).
        let (again, _) = enabled(&Arc::new(Fake::default()), Some(&dir));
        assert_eq!(again.exposed()[0].last_order, Some((true, 50.0)));
        // Ni éteinte ni inconnue : refusé.
        assert_eq!(
            Somfy::order(&somfy, 12345, Order::Open, 60.0),
            Err(OrderError::Unknown)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_order_goes_back_to_the_previous_state_and_is_never_replayed() {
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, None);
        somfy.tick(10.0, &setup.settings());
        let porte = somfy.exposed()[0].id;
        Somfy::order(&somfy, porte, Order::Open, 50.0).unwrap();
        wait_idle(&somfy);
        *fake.fail.lock().unwrap() = Some(OverkizError::Unreachable);
        Somfy::order(&somfy, porte, Order::Close, 60.0).unwrap();
        wait_idle(&somfy);
        assert_eq!(
            somfy.exposed()[0].last_order,
            Some((true, 50.0)),
            "l'ordre n'est pas parti"
        );
        let notices = somfy.take_notices();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].starts_with("Porte"), "{notices:?}");
        assert!(somfy.status(true, &setup.config)["last_error"]
            .as_str()
            .unwrap()
            .contains("ne répond pas"));
        // Le réseau revient : rien n'est rejoué tout seul.
        *fake.fail.lock().unwrap() = None;
        assert!(!somfy.tick(70.0, &setup.settings()));
        assert_eq!(fake.orders.lock().unwrap().len(), 1);
        // Un ordre qui réussit efface l'erreur.
        Somfy::order(&somfy, porte, Order::Close, 80.0).unwrap();
        wait_idle(&somfy);
        assert_eq!(somfy.status(true, &setup.config)["last_error"], Value::Null);
        assert_eq!(somfy.exposed()[0].last_order, Some((false, 80.0)));
    }

    #[test]
    fn orders_are_sent_one_at_a_time_and_a_newer_one_replaces_a_waiting_one() {
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, None);
        somfy.tick(10.0, &setup.settings());
        let ids: Vec<u64> = somfy.exposed().iter().map(|e| e.id).collect();
        // Trois ordres d'affilée : ouvrir et fermer la porte (le dernier gagne), ouvrir le garage.
        Somfy::order(&somfy, ids[0], Order::Open, 50.0).unwrap();
        Somfy::order(&somfy, ids[1], Order::Open, 50.1).unwrap();
        Somfy::order(&somfy, ids[0], Order::Close, 50.2).unwrap();
        wait_idle(&somfy);
        let sent = fake.orders.lock().unwrap().clone();
        // Au plus un ordre par volet est parti après le premier ; jamais deux en même temps.
        assert!(sent.len() <= 3 && sent.len() >= 2, "{sent:?}");
        assert_eq!(somfy.exposed()[0].last_order, Some((false, 50.2)));
        assert_eq!(somfy.exposed()[1].last_order, Some((true, 50.1)));
    }

    #[test]
    fn refused_credentials_stop_every_call_until_they_change() {
        let fake = Arc::new(Fake::default());
        *fake.fail.lock().unwrap() = Some(OverkizError::Rejected);
        let (somfy, mut setup) = enabled(&fake, None);
        assert!(somfy.tick(10.0, &setup.settings()));
        assert_eq!(somfy.status(true, &setup.config)["state"], "rejected");
        assert!(somfy.rejected());
        // Ni relecture automatique, ni « Actualiser », ni ordre : jamais deux fois le même essai.
        for t in [11.0, 1000.0, 100_000.0] {
            assert!(!somfy.tick(t, &setup.settings()));
        }
        assert_eq!(
            somfy.request_refresh(100_000.0),
            Err(RefreshError::Rejected)
        );
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        // Un autre mot de passe : on réessaie, une seule fois.
        *fake.fail.lock().unwrap() = None;
        setup.secrets.insert(PASSWORD.into(), "nouveau".into());
        somfy.on_change(
            &setup.settings(),
            &Changes {
                secrets: true,
                ..Changes::default()
            },
        );
        assert!(!somfy.rejected());
        assert!(somfy.tick(20.0, &setup.settings()));
        assert_eq!(somfy.status(true, &setup.config)["state"], "ok");
        assert_eq!(fake.lists.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn offline_retries_with_a_growing_delay_and_keeps_the_known_list() {
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, None);
        somfy.tick(0.0, &setup.settings());
        let before = somfy.exposed();
        assert_eq!(before.len(), 2);
        // La liste est relue sur demande, mais le cloud ne répond pas.
        *fake.fail.lock().unwrap() = Some(OverkizError::Unreachable);
        somfy.request_refresh(1000.0).unwrap();
        assert!(somfy.tick(1000.0, &setup.settings()));
        assert_eq!(somfy.status(true, &setup.config)["state"], "offline");
        assert_eq!(somfy.exposed(), before, "la dernière liste reste");
        // 1 minute, puis 2, puis 4 : jamais plus tôt.
        assert!(!somfy.tick(1030.0, &setup.settings()));
        assert!(somfy.tick(1061.0, &setup.settings()));
        assert!(!somfy.tick(1061.0 + 100.0, &setup.settings()));
        assert!(somfy.tick(1061.0 + 121.0, &setup.settings()));
        assert_eq!(fake.lists.load(Ordering::SeqCst), 4);
        // « Actualiser » en rafale est refusé.
        assert_eq!(
            somfy.request_refresh(1061.0 + 125.0),
            Err(RefreshError::TooSoon)
        );
    }

    #[test]
    fn switching_off_or_clearing_the_password_removes_the_shutters_and_stops_the_calls() {
        let fake = Arc::new(Fake::default());
        let (somfy, mut setup) = enabled(&fake, None);
        somfy.tick(10.0, &setup.settings());
        let porte = somfy.exposed()[0].id;
        assert_eq!(somfy.exposed().len(), 2);
        // Mot de passe effacé : même effet que l'extinction.
        let secrets = std::mem::take(&mut setup.secrets);
        somfy.on_change(
            &setup.settings(),
            &Changes {
                secrets: true,
                ..Changes::default()
            },
        );
        assert!(somfy.exposed().is_empty());
        assert_eq!(
            Somfy::order(&somfy, porte, Order::Open, 20.0),
            Err(OrderError::Disabled)
        );
        assert_eq!(somfy.request_refresh(1e6), Err(RefreshError::Disabled));
        // La session est oubliée par le fil des extensions (jamais sous le verrou des réglages).
        assert_eq!(fake.forgets.load(Ordering::SeqCst), 0);
        assert!(!somfy.tick(30.0, &setup.settings()));
        assert_eq!(fake.forgets.load(Ordering::SeqCst), 1);
        // Remis : les mêmes volets, les mêmes identifiants.
        setup.secrets = secrets;
        somfy.on_change(
            &setup.settings(),
            &Changes {
                secrets: true,
                ..Changes::default()
            },
        );
        somfy.tick(40.0, &setup.settings());
        assert_eq!(somfy.exposed()[0].id, porte);
        // Éteinte.
        setup.enabled = false;
        somfy.on_change(
            &setup.settings(),
            &Changes {
                switched: Some(false),
                ..Changes::default()
            },
        );
        assert!(somfy.exposed().is_empty());
        let lists = fake.lists.load(Ordering::SeqCst);
        assert!(!somfy.tick(50.0, &setup.settings()));
        assert_eq!(fake.lists.load(Ordering::SeqCst), lists);
        somfy.shutdown();
    }

    #[test]
    fn shutdown_waits_for_the_worker_and_drops_the_waiting_orders() {
        let fake = Arc::new(Fake::default());
        let (somfy, setup) = enabled(&fake, None);
        somfy.tick(10.0, &setup.settings());
        let ids: Vec<u64> = somfy.exposed().iter().map(|e| e.id).collect();
        Somfy::order(&somfy, ids[0], Order::Open, 50.0).unwrap();
        somfy.shutdown();
        assert!(!somfy.lock().worker_running);
        assert_eq!(
            Somfy::order(&somfy, ids[1], Order::Open, 51.0),
            Err(OrderError::Disabled)
        );
    }

    #[test]
    fn config_is_checked_and_cleaned() {
        let ok =
            normalize(&json!({"email": "  sam@example.org ", "exposed": ["a", "a", "b"]})).unwrap();
        assert_eq!(
            ok,
            json!({"email": "sam@example.org", "exposed": ["a", "b"]})
        );
        assert_eq!(
            normalize(&json!({})).unwrap(),
            json!({"email": "", "exposed": null})
        );
        for bad in [
            json!([]),
            json!({"email": 3}),
            json!({"password": "x"}),
            json!({"email": "a\u{7}b"}),
            json!({"exposed": "a"}),
            json!({"exposed": [1]}),
            json!({"exposed": [""]}),
        ] {
            assert!(normalize(&bad).is_err(), "{bad}");
        }
    }
}
