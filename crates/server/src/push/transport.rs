//! L'envoi vers le service de push de l'appareil (Apple pour un iPhone ou un iPad).
//!
//! - **Seuls les services connus**, en https : l'adresse d'abonnement vient d'un client ; sans
//!   cette liste, un client pourrait faire appeler au serveur n'importe quelle adresse de son
//!   réseau (SSRF). Voir [`service_of`].
//! - Même chemin que les descriptions (`descriptions/api.rs`) : le `curl` du système, un
//!   environnement vide, `--noproxy "*"`, https seulement. L'adresse (c'est elle qui permet
//!   d'écrire à l'appareil) et l'en-tête VAPID passent par l'entrée standard (`-K -`), jamais en
//!   argument ; le corps chiffré par un fichier temporaire 0600, effacé aussitôt.

use std::io::Write as _;
use std::process::Stdio;
use std::time::Duration;

use crate::descriptions::api::{curl_command, parse_http, quoted, TempBody, CURL_BEFORE_SEND};

/// Préfixe des corps en attente dans le dossier temporaire (effacés au démarrage s'ils traînent).
pub const BODY_PREFIX: &str = "mh-push-";

/// Les services de push acceptés : (hôte exact ou suffixe `.domaine`, nom affiché).
const SERVICES: &[(&str, &str)] = &[
    ("web.push.apple.com", "Apple"),
    (".push.apple.com", "Apple"),
    ("fcm.googleapis.com", "Google"),
    ("updates.push.services.mozilla.com", "Mozilla"),
    (".notify.windows.com", "Microsoft"),
];

/// Le service d'une adresse d'abonnement, si elle est acceptée : https, port par défaut, sans
/// identifiants, hôte connu (exact, ou sous-domaine pour les entrées en `.`), et rien qu'un
/// `curl -K` lirait de travers (guillemets, barres obliques inverses, caractères de contrôle).
pub fn service_of(endpoint: &str) -> Option<&'static str> {
    if endpoint.len() > 1024
        || endpoint
            .bytes()
            .any(|b| b <= 0x20 || b >= 0x7f || b == b'"' || b == b'\\')
    {
        return None;
    }
    let url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let url::Host::Domain(host) = url.host()? else {
        return None; // une adresse IP n'est jamais un service de push
    };
    let host = host.to_ascii_lowercase();
    SERVICES.iter().find_map(|(pattern, name)| {
        // `.push.apple.com` : un sous-domaine (au moins un caractère avant le point), jamais
        // `evilpush.apple.com` ni `push.apple.com.evil.example`.
        let ok = if pattern.starts_with('.') {
            host.len() > pattern.len() && host.ends_with(pattern)
        } else {
            host == *pattern
        };
        ok.then_some(*name)
    })
}

/// L'origine d'une adresse (`aud` du jeton VAPID) : `https://hôte`, sans barre finale.
pub fn origin_of(endpoint: &str) -> Option<String> {
    let url = url::Url::parse(endpoint).ok()?;
    Some(format!("https://{}", url.host_str()?))
}

/// Une requête vers le service de push.
#[derive(Debug, Clone)]
pub struct PushRequest {
    pub endpoint: String,
    /// `Authorization: vapid t=…, k=…`.
    pub authorization: String,
    pub ttl_s: u64,
    /// `high` (alarme) ou `normal`.
    pub urgency: &'static str,
    /// Le corps chiffré (`aes128gcm`).
    pub body: Vec<u8>,
}

/// La réponse du service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushReply {
    pub status: u16,
    pub retry_after: Option<Duration>,
}

/// Un échec avant toute réponse. `retryable` : la requête n'est sûrement pas partie.
#[derive(Debug, Clone)]
pub struct SendError {
    pub retryable: bool,
    pub why: String,
}

/// Qui envoie. Le serveur réel passe par `curl` ; les tests et la démonstration par un faux.
pub trait Transport: Send + Sync {
    fn post(&self, request: &PushRequest) -> Result<PushReply, SendError>;
}

/// Ne fait rien partir : mode démonstration (et par défaut sous `cargo test`).
pub struct NullTransport;

impl Transport for NullTransport {
    fn post(&self, _request: &PushRequest) -> Result<PushReply, SendError> {
        log::info!("notifications : démonstration, rien n'est envoyé");
        Ok(PushReply {
            status: 201,
            retry_after: None,
        })
    }
}

/// L'envoi réel, par `/usr/bin/curl`.
pub struct CurlTransport {
    pub timeout: Duration,
}

impl Default for CurlTransport {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
        }
    }
}

/// La configuration envoyée à `curl` par l'entrée standard.
pub fn curl_config(request: &PushRequest) -> Result<String, String> {
    Ok(format!(
        "url = {}\nheader = {}\nheader = \"Content-Encoding: aes128gcm\"\nheader = \"Content-Type: application/octet-stream\"\nheader = {}\nheader = {}\nheader = \"Expect:\"\n",
        quoted(&request.endpoint)?,
        quoted(&format!("Authorization: {}", request.authorization))?,
        quoted(&format!("TTL: {}", request.ttl_s))?,
        quoted(&format!("Urgency: {}", request.urgency))?,
    ))
}

/// Les arguments : https seulement (y compris une redirection, de toute façon non suivie), sortie
/// avec en-têtes (`-i`) pour lire le statut et `retry-after`.
pub fn curl_args(body: &std::path::Path, timeout: Duration) -> Vec<String> {
    [
        "-q",
        "-sS",
        "-i",
        "-m",
        &timeout.as_secs().max(1).to_string(),
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-g",
        "--noproxy",
        "*",
        "-X",
        "POST",
        "--data-binary",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([format!("@{}", body.display()), "-K".into(), "-".into()])
    .collect()
}

impl Transport for CurlTransport {
    fn post(&self, request: &PushRequest) -> Result<PushReply, SendError> {
        let local = |why: String| SendError {
            retryable: false,
            why,
        };
        // Vérifié aussi avant (`Push`), et encore ici : jamais d'autre adresse que celles-là.
        if service_of(&request.endpoint).is_none() {
            return Err(local("adresse de service de push refusée".into()));
        }
        let config = curl_config(request).map_err(local)?;
        let temp = TempBody::write_named(BODY_PREFIX, "bin", &request.body)
            .map_err(|e| local(format!("fichier temporaire : {}", e.kind())))?;
        let mut child = curl_command()
            .args(curl_args(temp.path(), self.timeout))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| local(format!("curl : {}", e.kind())))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(config.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(local(format!("curl : {}", e.kind())));
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|e| local(format!("curl : {}", e.kind())))?;
        if !output.status.success() {
            // `curl -sS` écrit une ligne courte ; on n'en garde pas l'adresse.
            let code = output.status.code();
            return Err(SendError {
                retryable: code.is_some_and(|c| CURL_BEFORE_SEND.contains(&c)),
                why: format!("échec réseau (curl {})", code.unwrap_or(-1)),
            });
        }
        let response = parse_http(&output.stdout).map_err(local)?;
        Ok(PushReply {
            status: response.status,
            retry_after: response.retry_after,
        })
    }
}
