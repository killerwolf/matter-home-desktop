//! Parler à la box (Bbox) : se connecter avec le mot de passe d'administration et lire la liste des
//! appareils (`GET /api/v1/hosts`). Lecture seule : rien n'est jamais écrit sur la box.
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

use std::sync::Mutex;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;

use crate::curl::Reply;
use crate::descriptions::api::quoted;

/// L'adresse de la Bbox, résolue en local par la box elle-même.
pub const DEFAULT_URL: &str = "https://mabbox.bytel.fr";
/// Taille maximale d'une réponse de la box.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Un appareil vu par la box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Le nom DHCP (« iPhone-de-Sam »), tel que la box l'écrit.
    pub name: String,
    /// Connecté en ce moment.
    pub active: bool,
}

/// Pourquoi la box n'a pas répondu comme il faut. Dans tous les cas, **rien n'est conclu** : le
/// mode de la maison reste figé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxError {
    /// Pas de réponse, réponse illisible ou erreur de la box.
    Unreachable,
    /// La box a refusé le mot de passe.
    Refused,
}

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

/// Un nom d'appareil propre : sans caractères de contrôle, 64 caractères au plus.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(64)
        .collect()
}

/// Ce nom ressemble-t-il à un téléphone (iPhone ou Android) ? La box ne dit pas le type d'un
/// appareil : on s'en tient au nom. Un membre de la maison ne voit que ces noms-là ; l'administrateur
/// peut attribuer n'importe quel appareil vu.
pub fn looks_like_phone(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "iphone",
        "android",
        "galaxy",
        "pixel",
        "redmi",
        "xiaomi",
        "poco",
        "oneplus",
        "oppo",
        "realme",
        "huawei",
        "honor",
        "samsung",
        "motorola",
        "moto-",
        "fairphone",
        "xperia",
        "nokia",
        "vivo",
        "phone",
        "telephone",
        "tel-",
        "smartphone",
    ];
    let lower = name.to_lowercase();
    WORDS.iter().any(|w| lower.contains(w)) || lower.starts_with("sm-")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn phones_are_recognised_by_name() {
        for yes in [
            "iPhone-de-Sam",
            "Galaxy-de-Lea",
            "Redmi-de-Adam",
            "android-3f2a",
            "SM-G991B",
            "Pixel-7",
        ] {
            assert!(looks_like_phone(yes), "{yes}");
        }
        for no in ["MacBook-Pro", "LG-webOS-TV", "ESP_3F2A10", "Bbox-4K", "PS5"] {
            assert!(!looks_like_phone(no), "{no}");
        }
    }
}
