//! L'extension « Bbox » : la première box reliée à la présence Wi-Fi (voir `wifi/connector.rs`).
//! Elle se connecte avec le mot de passe d'administration et lit la liste des appareils
//! (`GET /api/v1/hosts`). Lecture seule : rien n'est jamais écrit sur la box. Ses réglages (adresse,
//! mot de passe) sont ceux de `extensions.json` ; ce qui est propre à la présence (téléphones
//! associés, interrupteur du mode automatique, délai) reste dans le cœur (`wifi/`).
//!
//! La forme de l'API vient du prototype `presence-wifi` : `POST /api/v1/login` (formulaire
//! `password=…&remember=0`, cookie de session), puis `GET /api/v1/hosts` →
//! `[{"hosts":{"list":[{hostname, ipaddress, macaddress, active (1/0), lastseen, …}]}}]`. Seuls le
//! nom (`hostname`), l'état (`active`) et `lastseen` sont gardés : jamais l'adresse MAC ni l'IP.
//!
//! Même chemin réseau que les notifications et les descriptions : le `curl` du système, un
//! environnement vide, sans proxy. L'adresse et le mot de passe passent par l'entrée standard
//! (`-K -`), jamais en argument (visible dans `ps`). La box a un certificat auto-signé : la
//! vérification TLS n'est désactivée (`insecure`) que pour l'adresse par défaut (`mabbox.bytel.fr`)
//! ou une adresse IP privée (RFC 1918, `fe80::/10`, `fd00::/8`). Toute autre adresse https garde la
//! vérification du certificat : le mot de passe d'administration ne part jamais vers un serveur
//! qu'on ne peut pas authentifier.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};

use super::{Changes, Descriptor, Extension, SecretField, Settings};
use crate::curl::Reply;
use crate::descriptions::api::quoted;
use crate::wifi::connector::clean_name;
pub use crate::wifi::connector::{BoxConnector, BoxError, Host};

/// L'adresse de la Bbox, résolue en local par la box elle-même.
pub const DEFAULT_URL: &str = "https://mabbox.bytel.fr";
/// Taille maximale d'une réponse de la box.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Qui lit la liste des appareils : la vraie box (`CurlBox`), une démonstration ou un faux (essais).
pub trait BoxApi: Send + Sync {
    /// La liste des appareils. Bloquant (quelques secondes au plus).
    fn hosts(&self, url: &str, password: &str) -> Result<Vec<Host>, BoxError>;

    /// Oublie la session ouverte (changement d'adresse ou de mot de passe).
    fn reset(&self) {}
}

/// L'adresse de la box, contrôlée et ramenée à `schéma://hôte[:port]` : http ou https, sans
/// identifiants, sans chemin, et rien qu'un fichier de configuration `curl` lirait de travers.
pub fn check_url(text: &str) -> Result<String, &'static str> {
    const BAD: &str = "Adresse de la box invalide.";
    let text = text.trim();
    if text.is_empty() || text.len() > 200 {
        return Err(BAD);
    }
    let url = url::Url::parse(text).map_err(|_| BAD)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(BAD);
    }
    let host = url.host_str().ok_or(BAD)?;
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_graphic() && b != b'"') {
        return Err(BAD);
    }
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(format!("{}://{host}{port}", url.scheme()))
}

/// Le certificat de la box est auto-signé : on ne s'en passe que pour l'adresse par défaut ou une
/// adresse IP privée (RFC 1918, lien local `fe80::/10`, `fd00::/8`). Ailleurs, il est vérifié.
fn accepts_self_signed(url: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("mabbox.bytel.fr"),
        Some(url::Host::Ipv4(ip)) => Ipv4Addr::is_private(&ip),
        Some(url::Host::Ipv6(ip)) => {
            let first = Ipv6Addr::segments(&ip)[0];
            first & 0xffc0 == 0xfe80 || first & 0xff00 == 0xfd00
        }
        None => false,
    }
}

/// La vraie box, par `curl`. Garde le cookie de session en mémoire (jamais sur disque).
pub struct CurlBox {
    timeout: Duration,
    /// (adresse, en-tête `Cookie`) de la session ouverte.
    session: Mutex<Option<(String, String)>>,
}

impl Default for CurlBox {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            session: Mutex::new(None),
        }
    }
}

impl CurlBox {
    fn run(&self, config: &str) -> Result<Reply, BoxError> {
        crate::curl::run(config, self.timeout, "=http,https", MAX_BYTES)
            .map_err(|_| BoxError::Unreachable)
    }

    fn config(url: &str, path: &str, cookie: Option<&str>, form: Option<&str>) -> Option<String> {
        let mut text = format!("url = {}\n", quoted(&format!("{url}{path}")).ok()?);
        if url.starts_with("https://") && accepts_self_signed(url) {
            text.push_str("insecure\n");
        }
        if let Some(cookie) = cookie {
            text.push_str(&format!(
                "header = {}\n",
                quoted(&format!("Cookie: {cookie}")).ok()?
            ));
        }
        if let Some(form) = form {
            text.push_str(&format!("data = {}\n", quoted(form).ok()?));
        }
        text.push_str("header = \"Expect:\"\n");
        Some(text)
    }

    fn get_hosts(&self, url: &str, cookie: Option<&str>) -> Result<Reply, BoxError> {
        let config =
            Self::config(url, "/api/v1/hosts", cookie, None).ok_or(BoxError::Unreachable)?;
        self.run(&config)
    }

    /// Ouvre une session : le cookie (`nom=valeur; …`) ou `Refused`.
    fn login(&self, url: &str, password: &str) -> Result<String, BoxError> {
        let form = format!(
            "password={}&remember=0",
            utf8_percent_encode(password, NON_ALPHANUMERIC)
        );
        let config =
            Self::config(url, "/api/v1/login", None, Some(&form)).ok_or(BoxError::Unreachable)?;
        let reply = self.run(&config)?;
        match reply.status {
            200..=299 if !reply.cookies.is_empty() => Ok(reply.cookies.join("; ")),
            // Sans cookie, la box n'a rien ouvert : on ne devine pas.
            200..=299 => Err(BoxError::Refused),
            400 | 401 | 403 | 429 => Err(BoxError::Refused),
            _ => Err(BoxError::Unreachable),
        }
    }
}

impl BoxApi for CurlBox {
    fn hosts(&self, url: &str, password: &str) -> Result<Vec<Host>, BoxError> {
        let cookie = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(u, _)| u == url)
            .map(|(_, c)| c.clone());
        let mut reply = self.get_hosts(url, cookie.as_deref())?;
        if matches!(reply.status, 401 | 403) {
            // Session absente ou expirée : une seule reconnexion, jamais plus.
            let cookie = self.login(url, password)?;
            reply = self.get_hosts(url, Some(&cookie))?;
            if matches!(reply.status, 401 | 403) {
                return Err(BoxError::Refused);
            }
            if reply.status == 200 {
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((url.to_string(), cookie));
            }
        }
        if reply.status != 200 {
            return Err(BoxError::Unreachable);
        }
        parse_hosts(&reply.body).ok_or(BoxError::Unreachable)
    }

    fn reset(&self) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// La liste des appareils d'une réponse de la box. `None` : la forme n'est pas celle qu'on attend.
pub fn parse_hosts(body: &[u8]) -> Option<Vec<Host>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let list = value.get(0)?.get("hosts")?.get("list")?.as_array()?;
    let mut hosts: Vec<Host> = Vec::new();
    for item in list {
        let name = item
            .get("hostname")
            .and_then(Value::as_str)
            .map(clean_name)
            .unwrap_or_default();
        if name.is_empty() {
            continue; // sans nom, on ne sait pas de quel appareil il s'agit
        }
        let active = match item.get("active") {
            Some(Value::Number(n)) => n.as_i64() == Some(1),
            Some(Value::Bool(b)) => *b,
            _ => false,
        };
        // Un même nom vu deux fois (Wi-Fi et Ethernet) : actif si l'un des deux l'est.
        match hosts
            .iter_mut()
            .find(|h| h.name.eq_ignore_ascii_case(&name))
        {
            Some(h) => h.active |= active,
            None => hosts.push(Host { name, active }),
        }
    }
    Some(hosts)
}

// ---- l'extension « Bbox » ---------------------------------------------------------------------

pub const ID: &str = "bbox";
pub const PASSWORD: &str = "password";

pub static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    name: "Bbox",
    description: "Savoir quels téléphones sont sur le Wi-Fi de la maison, par la Bbox de Bouygues \
Telecom : c'est la source de la présence (qui est à la maison).",
    disclosure: "Une fois activée, Ostal Server se connecte à votre Bbox, sur le réseau de la \
maison (mabbox.bytel.fr ou l'adresse que vous donnez), avec son mot de passe d'administration, et \
lit la liste des appareils connectés toutes les 2 minutes environ. Ostal ne modifie rien sur la box, \
ne garde que les noms des appareils (jamais leur adresse MAC ni leur adresse IP) et rien ne sort \
vers Internet. Désactivée, ou sans mot de passe, plus aucun appel ne part.",
    secrets: &[SecretField {
        key: PASSWORD,
        label: "Mot de passe d'administration de la Bbox",
        help: "Celui de la page d'administration de la box (mabbox.bytel.fr), pas celui du Wi-Fi. \
Il est gardé par Ostal Server et n'est jamais réaffiché. S'il est refusé, Ostal n'essaie plus tant \
qu'il n'est pas changé (la box pourrait bloquer l'accès).",
    }],
    private_config: &["url"],
    normalize,
};

/// Valide la configuration `{"url": "https://mabbox.bytel.fr"}` ; l'adresse par défaut est celle de
/// la Bbox. Rien d'autre n'est gardé.
fn normalize(value: &Value) -> Result<Value, &'static str> {
    const BAD: &str = "Réglages de la Bbox invalides.";
    let obj = value.as_object().ok_or(BAD)?;
    if obj.keys().any(|k| k != "url") {
        return Err(BAD);
    }
    let url = match obj.get("url") {
        None | Some(Value::Null) => DEFAULT_URL.to_string(),
        Some(v) => match v.as_str().ok_or(BAD)?.trim() {
            "" => DEFAULT_URL.to_string(),
            text => check_url(text)?,
        },
    };
    Ok(json!({ "url": url }))
}

/// L'état de santé du dernier relevé.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Health {
    /// Pas encore de relevé depuis le dernier changement de réglage.
    Unknown,
    /// Le dernier relevé a réussi : appareils actifs, date.
    Ok(usize, f64),
    Unreachable,
}

struct State {
    enabled: bool,
    /// L'adresse et le mot de passe, quand l'extension est complète.
    creds: Option<(String, String)>,
    /// Les réglages d'avant les extensions, quand leur reprise a échoué au démarrage
    /// (`wifi/migration.rs`) : la box reste lue comme avant, en attendant. Jamais écrit nulle part.
    legacy: Option<(String, String)>,
    /// Change à chaque modification des réglages.
    generation: u64,
    /// La génération pour laquelle la box a refusé le mot de passe : on ne le rejoue pas.
    refused_at: Option<u64>,
    health: Health,
}

impl State {
    /// L'adresse et le mot de passe à utiliser, s'il y en a.
    fn current(&self) -> Option<(String, String)> {
        if self.enabled {
            self.creds.clone()
        } else if self.creds.is_none() {
            self.legacy.clone()
        } else {
            None
        }
    }
}

/// L'extension Bbox : relève la liste des appareils de la box pour le cœur de la présence
/// (`wifi/mod.rs`), qui la lit par le trait [`BoxConnector`]. Le relevé périodique est celui de la
/// présence (toutes les 2 minutes) : l'extension n'a pas de tour à elle.
pub struct Bbox {
    api: Arc<dyn BoxApi>,
    state: Mutex<State>,
    /// Un appel à la box à la fois.
    call: Mutex<()>,
}

impl Bbox {
    pub fn new(api: Arc<dyn BoxApi>) -> Self {
        Self {
            api,
            state: Mutex::new(State {
                enabled: false,
                creds: None,
                legacy: None,
                generation: 1,
                refused_at: None,
                health: Health::Unknown,
            }),
            call: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Retient les réglages (sans appel réseau) : nouvelle génération, ancien refus oublié.
    fn apply(&self, settings: &Settings, forget_legacy: bool) {
        let url = settings
            .config
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .unwrap_or(DEFAULT_URL)
            .to_string();
        let creds = settings
            .secret(PASSWORD)
            .map(|password| (url, password.to_string()));
        let mut st = self.lock();
        st.enabled = settings.enabled;
        st.creds = creds;
        if forget_legacy {
            st.legacy = None;
        }
        st.generation += 1;
        st.refused_at = None;
        st.health = Health::Unknown;
    }

    /// La reprise des anciens réglages a échoué : on lit la box comme avant, sans rien écrire.
    pub fn use_legacy(&self, url: &str, password: &str) {
        let mut st = self.lock();
        st.legacy = Some((url.to_string(), password.to_string()));
        st.generation += 1;
    }

    /// Essaie une adresse et un mot de passe **sans rien retenir** (une session neuve). Sert à
    /// contrôler un mot de passe avant de l'enregistrer.
    pub fn probe(&self, url: &str, password: &str) -> Result<Vec<Host>, BoxError> {
        let _one = self.call.lock().unwrap_or_else(|e| e.into_inner());
        self.api.reset();
        self.api.hosts(url, password)
    }
}

impl std::fmt::Debug for Bbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bbox { .. }")
    }
}

impl Extension for Bbox {
    fn status(&self, enabled: bool, _config: &Value) -> Value {
        let st = self.lock();
        let (state, devices, last_ok) = if !enabled {
            if st.creds.is_none() && st.legacy.is_some() {
                ("legacy", None, None)
            } else {
                ("off", None, None)
            }
        } else if st.creds.is_none() {
            ("unconfigured", None, None)
        } else if st.refused_at == Some(st.generation) {
            ("refused", None, None)
        } else {
            match st.health {
                Health::Unknown => ("waiting", None, None),
                Health::Ok(n, at) => ("ok", Some(n), Some(at)),
                Health::Unreachable => ("unreachable", None, None),
            }
        };
        json!({"state": state, "devices": devices, "last_ok": last_ok})
    }

    fn start(&self, settings: &Settings) {
        self.apply(settings, false);
    }

    fn on_change(&self, settings: &Settings, _changes: &Changes) {
        // Une autre adresse ou un autre mot de passe : la session ouverte ne vaut plus.
        self.api.reset();
        self.apply(settings, true);
    }
}

impl BoxConnector for Bbox {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        DESCRIPTOR.name
    }

    fn ready(&self) -> bool {
        self.lock().current().is_some()
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    fn hosts(&self) -> Result<Vec<Host>, BoxError> {
        let (url, password, generation) = {
            let st = self.lock();
            let Some((url, password)) = st.current() else {
                return Err(BoxError::Unreachable);
            };
            // Un mot de passe refusé n'est pas rejoué avant que les réglages changent.
            if st.refused_at == Some(st.generation) {
                return Err(BoxError::Refused);
            }
            (url, password, st.generation)
        };
        let _one = self.call.lock().unwrap_or_else(|e| e.into_inner());
        let result = self.api.hosts(&url, &password);
        let mut st = self.lock();
        // Les réglages ont changé pendant l'appel : ce relevé ne dit rien de la nouvelle box.
        if st.generation == generation {
            match &result {
                Ok(hosts) => {
                    let active = hosts.iter().filter(|h| h.active).count();
                    st.health = Health::Ok(active, crate::security::now());
                }
                Err(BoxError::Refused) => {
                    st.refused_at = Some(generation);
                    log::warn!(
                        "bbox : mot de passe refusé, plus de nouvel essai avant d'en donner un autre"
                    );
                }
                Err(BoxError::Unreachable) => st.health = Health::Unreachable,
            }
        }
        result
    }
}

/// Une box de démonstration : quelques téléphones, sans réseau (`--demo` ne sort jamais de la
/// boucle locale).
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Une API de la box des essais : compte ses appels (la preuve qu'éteinte, ou refusée, elle n'en
    /// reçoit aucun) et répond ce qu'on lui dit.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        resets: AtomicUsize,
        refuse: std::sync::atomic::AtomicBool,
        down: std::sync::atomic::AtomicBool,
    }

    impl BoxApi for Counting {
        fn hosts(&self, _url: &str, _password: &str) -> Result<Vec<Host>, BoxError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.refuse.load(Ordering::SeqCst) {
                return Err(BoxError::Refused);
            }
            if self.down.load(Ordering::SeqCst) {
                return Err(BoxError::Unreachable);
            }
            Ok(vec![
                Host {
                    name: "iPhone-de-Sam".into(),
                    active: true,
                },
                Host {
                    name: "Galaxy-de-Lea".into(),
                    active: false,
                },
            ])
        }

        fn reset(&self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn settings_of<'a>(
        enabled: bool,
        config: &'a Value,
        secrets: &'a std::collections::BTreeMap<String, String>,
    ) -> Settings<'a> {
        Settings::new(enabled, config, secrets)
    }

    fn secrets(password: &str) -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        if !password.is_empty() {
            map.insert(PASSWORD.to_string(), password.to_string());
        }
        map
    }

    fn state_of(bbox: &Bbox, enabled: bool) -> String {
        bbox.status(enabled, &json!({}))["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn the_extension_is_a_box_connector_that_is_ready_only_when_enabled_and_complete() {
        let api = Arc::new(Counting::default());
        let bbox = Bbox::new(api.clone());
        let config = json!({"url": "https://mabbox.bytel.fr"});
        assert!(!bbox.ready());
        assert_eq!(state_of(&bbox, false), "off");
        // Activée sans mot de passe : rien à relever, aucun appel.
        bbox.on_change(
            &settings_of(true, &config, &secrets("")),
            &Changes::default(),
        );
        assert!(!bbox.ready());
        assert_eq!(state_of(&bbox, true), "unconfigured");
        assert_eq!(bbox.hosts(), Err(BoxError::Unreachable));
        assert_eq!(api.calls.load(Ordering::SeqCst), 0);
        // Complète : prête, le premier relevé dit « ok » avec le nombre d'appareils actifs.
        let before = bbox.generation();
        bbox.on_change(
            &settings_of(true, &config, &secrets("mdp")),
            &Changes::default(),
        );
        assert!(bbox.generation() > before);
        assert!(bbox.ready());
        assert_eq!(state_of(&bbox, true), "waiting");
        assert_eq!(bbox.hosts().unwrap().len(), 2);
        let status = bbox.status(true, &config);
        assert_eq!(
            (status["state"].as_str(), status["devices"].as_u64()),
            (Some("ok"), Some(1))
        );
        assert!(status["last_ok"].as_f64().is_some());
        // Éteinte : plus prête, plus aucun appel.
        bbox.on_change(
            &settings_of(false, &config, &secrets("mdp")),
            &Changes::default(),
        );
        assert!(!bbox.ready());
        assert_eq!(state_of(&bbox, false), "off");
        assert_eq!(api.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            api.resets.load(Ordering::SeqCst),
            3,
            "la session est oubliée à chaque changement"
        );
    }

    #[test]
    fn a_refused_password_is_not_replayed_until_the_settings_change() {
        let api = Arc::new(Counting::default());
        api.refuse.store(true, Ordering::SeqCst);
        let bbox = Bbox::new(api.clone());
        let config = json!({"url": "https://mabbox.bytel.fr"});
        bbox.start(&settings_of(true, &config, &secrets("mauvais")));
        assert_eq!(bbox.hosts(), Err(BoxError::Refused));
        assert_eq!(state_of(&bbox, true), "refused");
        for _ in 0..5 {
            assert_eq!(bbox.hosts(), Err(BoxError::Refused));
        }
        assert_eq!(api.calls.load(Ordering::SeqCst), 1, "un seul essai");
        // Un autre mot de passe : la box est de nouveau interrogée.
        api.refuse.store(false, Ordering::SeqCst);
        bbox.on_change(
            &settings_of(true, &config, &secrets("bon")),
            &Changes::default(),
        );
        assert_eq!(state_of(&bbox, true), "waiting");
        assert!(bbox.hosts().is_ok());
        assert_eq!(state_of(&bbox, true), "ok");
        // Injoignable : dit, mais pas bloqué (la box peut revenir).
        api.down.store(true, Ordering::SeqCst);
        assert_eq!(bbox.hosts(), Err(BoxError::Unreachable));
        assert_eq!(state_of(&bbox, true), "unreachable");
        api.down.store(false, Ordering::SeqCst);
        assert!(bbox.hosts().is_ok());
    }

    #[test]
    fn the_status_never_shows_the_address_or_the_password() {
        let api = Arc::new(Counting::default());
        let bbox = Bbox::new(api);
        let config = json!({"url": "http://192.168.1.254"});
        bbox.start(&settings_of(true, &config, &secrets("MOT-DE-PASSE-BOX")));
        bbox.hosts().unwrap();
        let text = bbox.status(true, &config).to_string();
        assert!(
            !text.contains("192.168") && !text.contains("MOT-DE-PASSE"),
            "{text}"
        );
        assert!(!format!("{bbox:?}").contains("MOT-DE-PASSE"));
        // L'adresse est un champ privé : un compte sans le droit « extensions » ne la voit pas.
        assert_eq!(DESCRIPTOR.private_config, &["url"]);
    }

    #[test]
    fn the_configuration_defaults_to_the_bbox_address_and_refuses_anything_else() {
        assert_eq!(normalize(&json!({})).unwrap(), json!({"url": DEFAULT_URL}));
        assert_eq!(
            normalize(&json!({"url": null})).unwrap(),
            json!({"url": DEFAULT_URL})
        );
        assert_eq!(
            normalize(&json!({"url": ""})).unwrap(),
            json!({"url": DEFAULT_URL})
        );
        assert_eq!(
            normalize(&json!({"url": " http://192.168.1.254/ "})).unwrap(),
            json!({"url": "http://192.168.1.254"})
        );
        for bad in [
            json!({"url": "ftp://box"}),
            json!({"url": 3}),
            json!({"password": "x"}),
            json!([]),
        ] {
            assert!(normalize(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_legacy_fallback_is_used_only_while_nothing_is_set_and_any_change_drops_it() {
        let api = Arc::new(Counting::default());
        let bbox = Bbox::new(api.clone());
        bbox.use_legacy("http://192.168.1.254", "ancien");
        assert!(bbox.ready());
        assert_eq!(state_of(&bbox, false), "legacy");
        assert!(bbox.hosts().is_ok());
        // La personne règle l'extension : le secours n'a plus cours.
        let config = json!({"url": "http://192.168.1.254"});
        bbox.on_change(
            &settings_of(false, &config, &secrets("")),
            &Changes::default(),
        );
        assert!(!bbox.ready());
        assert_eq!(state_of(&bbox, false), "off");
    }

    #[test]
    fn the_address_is_checked_and_normalised() {
        assert_eq!(
            check_url(" https://mabbox.bytel.fr/ ").unwrap(),
            "https://mabbox.bytel.fr"
        );
        assert_eq!(
            check_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        for bad in [
            "",
            "ftp://box",
            "mabbox.bytel.fr",
            "https://u:p@box",
            "https://box/api",
            "https://box/?a=1",
            "https://box/#x",
            "file:///etc/passwd",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn tls_is_only_unchecked_for_the_default_address_or_a_private_ip() {
        let insecure = |url: &str| {
            CurlBox::config(url, "/api/v1/hosts", None, None)
                .unwrap()
                .contains("insecure\n")
        };
        for ok in [
            "https://mabbox.bytel.fr",
            "https://MaBBox.Bytel.fr",
            "https://192.168.1.254",
            "https://10.0.0.1:8443",
            "https://172.16.5.1",
            "https://172.31.255.1",
            "https://[fe80::1]",
            "https://[fd12:3456::1]",
        ] {
            assert!(insecure(ok), "{ok}");
        }
        for checked in [
            "https://box.example.com",
            "https://mabbox.bytel.fr.evil.example",
            "https://8.8.8.8",
            "https://172.32.0.1",
            "https://100.64.0.1",
            "https://[2001:db8::1]",
            "https://[fc00::1]",
            "http://192.168.1.254",
        ] {
            assert!(!insecure(checked), "{checked}");
        }
    }

    #[test]
    fn the_box_list_keeps_names_and_state_only() {
        let body = br#"[{"hosts":{"list":[
            {"hostname":"iPhone-de-Sam","ipaddress":"192.168.1.20","macaddress":"AA:BB:CC:DD:EE:01","active":1,"lastseen":0,"link":"WiFi 5"},
            {"hostname":"Galaxy-de-Lea","ipaddress":"192.168.1.21","macaddress":"AA:BB:CC:DD:EE:02","active":0,"lastseen":900},
            {"hostname":"","active":1},
            {"hostname":"iphone-de-sam","active":0},
            {"hostname":"Mac\u0007mini","active":true}
        ]}}]"#;
        let hosts = parse_hosts(body).unwrap();
        assert_eq!(
            hosts,
            vec![
                Host {
                    name: "iPhone-de-Sam".into(),
                    active: true
                },
                Host {
                    name: "Galaxy-de-Lea".into(),
                    active: false
                },
                Host {
                    name: "Macmini".into(),
                    active: true
                },
            ]
        );
        assert!(parse_hosts(b"{}").is_none());
        assert!(parse_hosts(b"pas du json").is_none());
        assert!(parse_hosts(br#"[{"hosts":{}}]"#).is_none());
    }
}
