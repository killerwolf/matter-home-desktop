//! Le cloud Somfy (API « Overkiz » de l'app TaHoma), par le `curl` du système.
//!
//! Les adresses, la connexion et les commandes viennent de la bibliothèque `overkiz-client` 1.0.23
//! (celle de `homebridge-tahoma`), service `somfy_europe` :
//! - `src/Client.ts`, `getApiClient` : l'hôte `ha101-1.overkiz.com` ;
//! - `src/ApiClient.ts`, `CloudApiClient` : base `https://<hôte>/enduser-mobile-web/enduserAPI`,
//!   connexion `POST /login` en formulaire (`userId`, `userPassword`), la session est le cookie
//!   `JSESSIONID` ; un 401 en cours de route veut dire « session expirée » (on se reconnecte une
//!   fois) ; un 4xx à la connexion veut dire « identifiants refusés » (la bibliothèque s'interdit
//!   alors de réessayer pendant 60 s, puis 4 fois plus longtemps : voir `somfy.rs`) ;
//! - `src/Client.ts`, `getDevices` : `GET /setup/devices` ;
//! - `src/Client.ts`, `execute` : `POST /exec/apply` avec `{label, actions: [{deviceURL, commands:
//!   [{type: 1, name, parameters: []}]}]}` ; la réponse porte `execId`. (`/exec/apply/highPriority`
//!   n'est pas utilisé.)
//!
//! Aucun appel d'état (`/events`) : un volet RTS n'en a pas, et chaque appel de trop use la limite
//! de requêtes du compte. Le mot de passe ne sort que par l'entrée standard de `curl`
//! (`curl::Call`), jamais en argument ni au journal ; `Credentials` masque son `Debug`.

use std::sync::Mutex;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};

use crate::curl::{self, Call};

/// L'hôte du service `somfy_europe`.
pub const HOST: &str = "ha101-1.overkiz.com";
const TIMEOUT: Duration = Duration::from_secs(12);
/// La liste des appareils d'une maison bien garnie reste bien en dessous.
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_LABEL: usize = 80;

/// Ce que le compte permet de faire d'un volet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Open,
    Close,
}

impl Order {
    /// Le nom de la commande Overkiz.
    pub fn command(self) -> &'static str {
        match self {
            Order::Open => "open",
            Order::Close => "close",
        }
    }
}

/// L'identifiant et le mot de passe du compte TaHoma.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

/// Un appareil du compte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDevice {
    /// Identifiant propre de l'appareil dans le compte (`oid`) : ce qu'Ostal garde pour le choix.
    pub oid: String,
    /// `deviceURL` (`rts://1234-5678-9012/12345678`) : il contient le code du Kit, il ne s'affiche
    /// et ne se journalise jamais.
    pub url: String,
    /// Le nom donné dans l'app TaHoma.
    pub name: String,
    /// La classe d'interface Overkiz (`RollerShutter`, `Pod`, `Light`…).
    pub kind: String,
    /// L'appareil sait `open` et `close`.
    pub commandable: bool,
}

/// Pourquoi un appel n'a rien donné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverkizError {
    /// Somfy refuse l'identifiant ou le mot de passe : ne jamais réessayer tel quel.
    Rejected,
    /// Pas de réponse (hors ligne, délai, service en panne, limite de requêtes).
    Unreachable,
    /// Réponse qui n'a pas la forme attendue.
    BadReply,
}

/// Qui parle au cloud Somfy : le vrai (`CurlOverkiz`), la démonstration ou un faux (essais).
/// Bloquant ; la session (le cookie) reste à l'intérieur.
pub trait OverkizApi: Send + Sync {
    fn devices(&self, creds: &Credentials) -> Result<Vec<RemoteDevice>, OverkizError>;
    fn order(
        &self,
        creds: &Credentials,
        device_url: &str,
        label: &str,
        order: Order,
    ) -> Result<(), OverkizError>;
    /// Oublie la session (extension éteinte, identifiants changés).
    fn forget_session(&self);
}

// ---- analyse des réponses --------------------------------------------------------------------

fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// `true` pour le composant principal d'un appareil (`…/12345678` ou `…#1`) : les `#2`, `#3`… sont
/// ses capteurs, qu'Ostal ne montre pas.
fn is_main_component(url: &str) -> bool {
    match url.split_once('#') {
        None => true,
        Some((_, n)) => n == "1",
    }
}

/// Les appareils d'une réponse `GET /setup/devices`. `None` : pas la forme attendue.
pub fn parse_devices(body: &[u8]) -> Option<Vec<RemoteDevice>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let mut out = Vec::new();
    for item in value.as_array()? {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        let Some(url) = text(item.get("deviceURL")).filter(|u| !u.is_empty()) else {
            continue;
        };
        if !is_main_component(&url) {
            continue;
        }
        // Sans `oid` (le Kit lui-même, par exemple), pas d'identifiant à garder : écarté. L'adresse
        // ne sert jamais d'identifiant, elle contient le code du Kit.
        let Some(oid) = text(item.get("oid")).filter(|o| !o.is_empty()) else {
            continue;
        };
        let definition = item.get("definition");
        let commands = definition
            .and_then(|d| d.get("commands"))
            .and_then(Value::as_array);
        let knows = |name: &str| {
            commands.is_some_and(|list| {
                list.iter()
                    .any(|c| c.get("commandName").and_then(Value::as_str) == Some(name))
            })
        };
        let name = text(item.get("label"))
            .map(|l| clean(&l, MAX_LABEL))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "Appareil Somfy".to_string());
        let kind = definition
            .and_then(|d| d.get("uiClass"))
            .and_then(Value::as_str)
            .map(|k| clean(k, 40))
            .unwrap_or_default();
        out.push(RemoteDevice {
            oid,
            kind,
            commandable: knows("open") && knows("close"),
            name,
            url,
        });
    }
    Some(out)
}

/// Le corps de `POST /exec/apply` pour un ordre à un appareil.
pub fn execution_body(label: &str, device_url: &str, order: Order) -> String {
    json!({
        "label": clean(label, MAX_LABEL),
        "actions": [{
            "deviceURL": device_url,
            "commands": [{"type": 1, "name": order.command(), "parameters": []}],
        }],
    })
    .to_string()
}

/// La réponse de `POST /exec/apply` : un `execId`.
pub fn parse_execution(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("execId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// La réponse de Somfy dit « trop de requêtes » (panne passagère, pas un refus du compte).
fn too_many_requests(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .to_ascii_lowercase()
        .contains("too many")
}

/// Le corps du formulaire de connexion : les deux valeurs en pourcentage, de sorte qu'un mot de
/// passe avec `"`, `\`, `&` ou un accent passe tel quel.
pub fn login_body(creds: &Credentials) -> String {
    format!(
        "userId={}&userPassword={}",
        utf8_percent_encode(&creds.user, NON_ALPHANUMERIC),
        utf8_percent_encode(&creds.password, NON_ALPHANUMERIC)
    )
}

// ---- le vrai cloud, par curl -----------------------------------------------------------------

pub struct CurlOverkiz {
    /// `https://ha101-1.overkiz.com/enduser-mobile-web/enduserAPI` (une adresse locale en essai).
    base: String,
    /// Schémas permis pour `curl` : `=https`, jamais autre chose en vrai.
    proto: &'static str,
    /// Le cookie de session, en mémoire seulement ; le verrou sérialise aussi les appels (jamais
    /// deux connexions en même temps).
    /// Avec l'identifiant qui l'a obtenu : un autre compte ne s'en sert jamais.
    session: Mutex<Option<(String, String)>>,
}

impl Default for CurlOverkiz {
    fn default() -> Self {
        Self {
            base: format!("https://{HOST}/enduser-mobile-web/enduserAPI"),
            proto: "=https",
            session: Mutex::new(None),
        }
    }
}

impl CurlOverkiz {
    /// Pour les essais seulement : un serveur sur la boucle locale.
    #[cfg(test)]
    pub fn local(base: String) -> Self {
        Self {
            base,
            proto: "=http",
            session: Mutex::new(None),
        }
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[String],
        body: Option<&str>,
    ) -> Result<curl::Reply, OverkizError> {
        let url = format!("{}{path}", self.base);
        let config = Call {
            method,
            url: &url,
            headers,
            body,
        }
        .config()
        .ok_or(OverkizError::Unreachable)?;
        curl::run(&config, TIMEOUT, self.proto, MAX_BYTES).map_err(|_| OverkizError::Unreachable)
    }

    /// Ouvre une session. 4xx : identifiants refusés ; autre échec : injoignable.
    fn login(&self, creds: &Credentials) -> Result<String, OverkizError> {
        let body = login_body(creds);
        let headers = [
            "Accept: application/json".to_string(),
            "Content-Type: application/x-www-form-urlencoded".to_string(),
        ];
        let reply = self.send("POST", "/login", &headers, Some(&body))?;
        match reply.status {
            200 => reply
                .cookies
                .iter()
                .find(|c| c.starts_with("JSESSIONID="))
                .cloned()
                .ok_or(OverkizError::BadReply),
            // Trop de requêtes : une panne passagère, pas un mauvais mot de passe. (Le texte exact
            // de Somfy n'est pas dans `overkiz-client` : on cherche « too many » sans casse.)
            429 => Err(OverkizError::Unreachable),
            400..=499 if too_many_requests(&reply.body) => Err(OverkizError::Unreachable),
            400..=499 => Err(OverkizError::Rejected),
            _ => Err(OverkizError::Unreachable),
        }
    }

    /// Un appel avec la session ; un 401 (session expirée) se reconnecte **une fois**.
    fn call(
        &self,
        creds: &Credentials,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<Vec<u8>, OverkizError> {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        // Le cookie d'un ancien compte est jeté tout de suite.
        if session
            .as_ref()
            .is_some_and(|(user, _)| *user != creds.user)
        {
            *session = None;
        }
        for attempt in 0..2 {
            if session.is_none() {
                *session = Some((creds.user.clone(), self.login(creds)?));
            }
            let cookie = session.as_ref().map(|(_, c)| c.clone()).unwrap_or_default();
            let headers = [
                "Accept: application/json".to_string(),
                "Content-Type: application/json".to_string(),
                format!("Cookie: {cookie}"),
            ];
            let reply = self.send(method, path, &headers, body)?;
            match reply.status {
                200 => return Ok(reply.body),
                401 => {
                    *session = None;
                    // Un 401 juste après une connexion réussie : ce n'est pas le mot de passe.
                    if attempt == 1 {
                        return Err(OverkizError::Unreachable);
                    }
                }
                _ => {
                    return Err(OverkizError::Unreachable);
                }
            }
        }
        Err(OverkizError::Unreachable)
    }
}

impl OverkizApi for CurlOverkiz {
    fn devices(&self, creds: &Credentials) -> Result<Vec<RemoteDevice>, OverkizError> {
        let body = self.call(creds, "GET", "/setup/devices", None)?;
        parse_devices(&body).ok_or(OverkizError::BadReply)
    }

    fn order(
        &self,
        creds: &Credentials,
        device_url: &str,
        label: &str,
        order: Order,
    ) -> Result<(), OverkizError> {
        let body = execution_body(label, device_url, order);
        let reply = self.call(creds, "POST", "/exec/apply", Some(&body))?;
        parse_execution(&reply)
            .map(|_| ())
            .ok_or(OverkizError::BadReply)
    }

    fn forget_session(&self) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ---- la démonstration ------------------------------------------------------------------------

/// La démonstration : jamais de réseau, trois appareils fictifs.
pub struct DemoOverkiz;

impl OverkizApi for DemoOverkiz {
    fn devices(&self, _creds: &Credentials) -> Result<Vec<RemoteDevice>, OverkizError> {
        let device = |n: u32, name: &str, kind: &str, commandable: bool| RemoteDevice {
            oid: format!("00000000-0000-4000-8000-00000000000{n}"),
            url: format!("rts://0000-0000-0000/1000000{n}"),
            name: name.into(),
            kind: kind.into(),
            commandable,
        };
        Ok(vec![
            device(1, "Volet de la véranda", "RollerShutter", true),
            device(2, "Volet du garage", "RollerShutter", true),
            device(3, "Voyant du Kit", "Light", false),
        ])
    }

    fn order(
        &self,
        _creds: &Credentials,
        _url: &str,
        _label: &str,
        _order: Order,
    ) -> Result<(), OverkizError> {
        Ok(())
    }

    fn forget_session(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const DEVICES: &str = include_str!("fixtures/overkiz-devices.json");

    #[test]
    fn parses_the_recorded_device_list() {
        let list = parse_devices(DEVICES.as_bytes()).unwrap();
        // Le composant `#2` (un capteur) est écarté, comme le Kit (sans `oid`).
        let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Porte", "Garage", "Voyant du Kit"]);
        let porte = &list[0];
        assert_eq!(porte.kind, "RollerShutter");
        assert!(porte.commandable);
        assert_eq!(porte.oid, "5a1ae8c2-0000-4000-8000-000000000001");
        assert_eq!(porte.url, "rts://1234-5678-9012/16756321");
        // Un nom d'appareil avec un caractère de contrôle est nettoyé.
        assert_eq!(list[1].name, "Garage");
        // Ni `open` ni `close` : pas commandable.
        assert!(!list[2].commandable);
        assert_eq!(list[2].kind, "Light");
        assert!(parse_devices(b"{}").is_none());
        assert!(parse_devices(b"nope").is_none());
        assert_eq!(parse_devices(b"[]"), Some(Vec::new()));
    }

    #[test]
    fn the_order_body_is_the_overkiz_execution() {
        let body: Value = serde_json::from_str(&execution_body(
            "Ostal",
            "rts://1234-5678-9012/1",
            Order::Close,
        ))
        .unwrap();
        assert_eq!(
            body,
            json!({"label": "Ostal", "actions": [{
                "deviceURL": "rts://1234-5678-9012/1",
                "commands": [{"type": 1, "name": "close", "parameters": []}]}]})
        );
        assert_eq!(Order::Open.command(), "open");
        assert_eq!(
            parse_execution(br#"{"execId":"abc-123"}"#).as_deref(),
            Some("abc-123")
        );
        assert!(parse_execution(br#"{"error":"x"}"#).is_none());
    }

    #[test]
    fn the_login_form_survives_any_password() {
        let creds = Credentials {
            user: "sam@example.org".into(),
            password: "a\"b\\c&d=é".into(),
        };
        let body = login_body(&creds);
        assert!(!body.contains('"') && !body.contains('\\'));
        assert_eq!(
            body,
            "userId=sam%40example%2Eorg&userPassword=a%22b%5Cc%26d%3D%C3%A9"
        );
        assert_eq!(format!("{creds:?}"), "Credentials { .. }");
    }

    /// Un serveur HTTP minimal sur la boucle locale (aucun appel hors de cette machine) : il note
    /// ce qu'il reçoit et répond avec les réponses données, dans l'ordre.
    fn serve(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            listener.set_nonblocking(true).unwrap();
            for reply in replies {
                // Jamais d'attente sans fin : 10 s au plus pour qu'un client se présente.
                let started = std::time::Instant::now();
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(_) if started.elapsed() < Duration::from_secs(10) => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return seen,
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = vec![0u8; 16 * 1024];
                let mut got = 0;
                loop {
                    let n = stream.read(&mut buf[got..]).unwrap_or(0);
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).into_owned();
                    let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                        let want = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        body.len() >= want
                    });
                    if n == 0 || complete {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&buf[..got]).into_owned());
                stream.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (base, handle)
    }

    const LOGIN_OK: &str = "HTTP/1.1 200 OK\r\nSet-Cookie: JSESSIONID=SESSION-1; Path=/; HttpOnly\r\nContent-Type: application/json\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"success\":true}";
    const EXPIRED: &str =
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    const EXEC_OK: &str =
        "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"execId\":\"e-1\"}";

    #[test]
    fn a_password_with_quotes_reaches_the_server_intact_and_never_the_arguments() {
        // Une connexion, puis une commande dont le corps JSON contient des guillemets : tout passe
        // par l'entrée standard de `curl`, échappé.
        let (base, server) = serve(vec![LOGIN_OK, EXEC_OK]);
        let api = CurlOverkiz::local(base);
        let creds = Credentials {
            user: "sam@example.org".into(),
            password: "pa\"ss\\w;rd é".into(),
        };
        api.order(
            &creds,
            "rts://1234-5678-9012/1",
            "Volet \"Porte\"",
            Order::Open,
        )
        .unwrap();
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("POST /login "), "{}", seen[0]);
        assert!(
            seen[0].ends_with("userId=sam%40example%2Eorg&userPassword=pa%22ss%5Cw%3Brd%20%C3%A9"),
            "{}",
            seen[0]
        );
        assert!(seen[1].starts_with("POST /exec/apply "), "{}", seen[1]);
        assert!(
            seen[1].contains("Cookie: JSESSIONID=SESSION-1"),
            "{}",
            seen[1]
        );
        let body = seen[1].split_once("\r\n\r\n").unwrap().1;
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["label"], "Volet \"Porte\"");
        assert_eq!(body["actions"][0]["commands"][0]["name"], "open");
    }

    #[test]
    fn an_expired_session_logs_in_once_more_then_retries_once() {
        // login, appel refusé (401), nouvelle connexion, appel qui passe.
        let (base, server) = serve(vec![LOGIN_OK, EXPIRED, LOGIN_OK, EXEC_OK]);
        let api = CurlOverkiz::local(base);
        let creds = Credentials {
            user: "a".into(),
            password: "b".into(),
        };
        api.order(&creds, "rts://x/1", "Ostal", Order::Close)
            .unwrap();
        assert_eq!(server.join().unwrap().len(), 4);
    }

    #[test]
    fn too_many_requests_at_login_is_an_outage_not_a_refusal() {
        let (base, server) = serve(vec![
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 40\r\nConnection: close\r\n\r\n{\"error\":\"Too many requests, try later\"}",
        ]);
        let api = CurlOverkiz::local(base);
        let creds = Credentials {
            user: "a".into(),
            password: "b".into(),
        };
        assert_eq!(api.devices(&creds), Err(OverkizError::Unreachable));
        server.join().unwrap();
    }

    #[test]
    fn a_session_is_never_used_for_another_account() {
        // Deux comptes à la suite : la seconde commande se reconnecte avec son propre compte.
        let (base, server) = serve(vec![LOGIN_OK, EXEC_OK, LOGIN_OK, EXEC_OK]);
        let api = CurlOverkiz::local(base);
        let account = |user: &str| Credentials {
            user: user.into(),
            password: "b".into(),
        };
        api.order(&account("ancien"), "rts://x/1", "Ostal", Order::Open)
            .unwrap();
        api.order(&account("nouveau"), "rts://x/1", "Ostal", Order::Open)
            .unwrap();
        let seen = server.join().unwrap();
        assert!(seen[2].starts_with("POST /login "), "{}", seen[2]);
        assert!(seen[2].contains("userId=nouveau"), "{}", seen[2]);
    }

    #[test]
    fn refused_credentials_are_reported_as_such() {
        let (base, server) = serve(vec![
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        ]);
        let api = CurlOverkiz::local(base);
        let creds = Credentials {
            user: "a".into(),
            password: "b".into(),
        };
        assert_eq!(api.devices(&creds), Err(OverkizError::Rejected));
        assert_eq!(
            server.join().unwrap().len(),
            1,
            "un seul essai de connexion"
        );
    }

    #[test]
    fn config_values_with_control_characters_are_never_sent() {
        assert_eq!(curl::quote("a\"b\\c").as_deref(), Some("\"a\\\"b\\\\c\""));
        assert!(curl::quote("a\u{0}b").is_none());
        assert!(curl::quote("a\u{7f}b").is_none());
        // Un saut de ligne reste sur la même ligne de configuration.
        assert_eq!(curl::quote("a\nb").as_deref(), Some("\"a\\nb\""));
    }
}
