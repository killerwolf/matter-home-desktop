//! L'appel à l'API Messages d'Anthropic (ticket n° 19), sans SDK : un POST HTTPS fait par `curl`
//! (présent sur macOS, comme pour les webhooks des automatisations, et qui s'appuie sur les
//! certificats du système : le serveur n'embarque aucun client HTTP de plus).
//!
//! - **Ce qui part** : la capture JPEG (côté long ≤ 1024 px), le nom d'affichage de la caméra et
//!   la liste des classes. Jamais le flux, ni une adresse, ni un identifiant de caméra.
//! - **La clé** passe par l'entrée standard de `curl` (`-K -`), jamais en argument (un `ps` la
//!   montrerait) ni dans un fichier de plus ; le corps (l'image) est écrit dans un fichier
//!   temporaire 0600, effacé aussitôt. Rien de tout cela n'est journalisé.
//! - **Réponse** : sortie structurée `{description, niveau}`. On lit `stop_reason` avant le
//!   contenu : `refusal` n'est pas réessayé ; `max_tokens` est un échec.
//! - **Délais** : 30 s ; un seul nouvel essai (429, 5xx, réseau), après `retry-after` s'il est donné.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use super::settings::Model;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const VERSION: &str = "2023-06-01";
const BETA_FALLBACK: &str = "server-side-fallback-2026-07-01";
/// Côté long maximal de la capture envoyée.
pub const MAX_SIDE: u32 = 1024;
/// Taille maximale de la capture envoyée (une capture 1024 px fait ~100 Ko).
const MAX_JPEG_BYTES: usize = 4 * 1024 * 1024;
/// Attente maximale entre deux essais, même si `retry-after` demande plus.
const MAX_WAIT: Duration = Duration::from_secs(30);
pub const UNAVAILABLE: &str = "description indisponible";
pub const MAX_CHARS: usize = 200;

/// Où appeler l'API. Seuls les tests changent l'adresse (un faux serveur local) : elle n'est
/// jamais lue dans l'environnement ni dans un fichier, la clé ne partirait pas ailleurs.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    /// Attente avant le nouvel essai quand la réponse ne dit pas `retry-after`.
    pub retry_default: Duration,
    /// Délai d'un appel (les tests le raccourcissent).
    pub timeout: Duration,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            retry_default: Duration::from_secs(2),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Ce qu'on dit de l'événement au modèle.
#[derive(Debug, Clone)]
pub struct Context {
    pub camera_name: String,
    /// Classes vues (« personne », « chien »…).
    pub classes: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Described {
        description: String,
        niveau: u8,
    },
    /// Le modèle a refusé : « description indisponible », sans nouvel essai.
    Refused,
}

/// Le texte de la consigne. Le nom de la caméra est celui que l'utilisateur lui a donné,
/// nettoyé (une ligne, 60 caractères) ; rien d'autre sur la maison.
fn prompt(context: &Context) -> String {
    let name: String = context
        .camera_name
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    let classes = if context.classes.is_empty() {
        "objet".to_string()
    } else {
        context.classes.join(", ")
    };
    format!(
        "Cette image vient de la caméra de surveillance « {name} » d'une maison. Le système a \
         détecté : {classes}. Décris en une phrase courte, en français ({MAX_CHARS} caractères au \
         plus), ce qui se passe, sans deviner l'identité de quelqu'un. Donne aussi un niveau de \
         menace : 0 normal (habitant, livreur, animal, passage ordinaire), 1 suspect \
         (comportement inhabituel : rôde, observe la maison, essaie une porte ou une fenêtre, \
         visage dissimulé), 2 critique (intrusion, effraction, violence, fumée ou feu). Réponds \
         uniquement par l'objet JSON demandé."
    )
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "description": { "type": "string" },
            "niveau": { "type": "integer", "enum": [0, 1, 2] }
        },
        "required": ["description", "niveau"],
        "additionalProperties": false
    })
}

/// Le corps de la requête. Ni `temperature`, ni `top_p`, ni `top_k` (refusés par les modèles
/// actuels) ; pas de `thinking` (Opus 5.5 ne peut pas le couper), un effort « low » pour les trois.
pub fn build_body(model: Model, context: &Context, jpeg: &[u8]) -> Value {
    let mut body = json!({
        "model": model.id(),
        "max_tokens": 4096,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "image", "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": base64::engine::general_purpose::STANDARD.encode(jpeg),
                } },
                { "type": "text", "text": prompt(context) }
            ]
        }],
        "output_config": {
            "effort": "low",
            "format": { "type": "json_schema", "schema": schema() }
        }
    });
    if model.server_fallback() {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Le côté long d'un JPEG, lu dans son premier marqueur « SOF » (sans décoder l'image).
pub fn jpeg_long_side(jpeg: &[u8]) -> Option<u32> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return None;
    }
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            return None;
        }
        let marker = jpeg[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        let len = usize::from(jpeg[at + 2]) << 8 | usize::from(jpeg[at + 3]);
        // SOF0 à SOF15, sauf DHT (C4), JPG (C8) et DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            let sof = jpeg.get(at + 5..at + 9)?;
            let height = u32::from(sof[0]) << 8 | u32::from(sof[1]);
            let width = u32::from(sof[2]) << 8 | u32::from(sof[3]);
            return Some(width.max(height));
        }
        at += 2 + len;
    }
    None
}

/// Lit la réponse : `stop_reason` d'abord, puis le premier bloc de texte (les blocs de réflexion
/// sont ignorés), qui est du JSON `{description, niveau}`.
pub fn parse_response(body: &str) -> Result<Outcome, String> {
    let value: Value = serde_json::from_str(body).map_err(|_| "réponse illisible".to_string())?;
    match value.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => return Ok(Outcome::Refused),
        Some("max_tokens") => return Err("réponse tronquée (max_tokens)".into()),
        _ => {}
    }
    let text = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .ok_or("pas de texte dans la réponse")?;
    let answer: Value =
        serde_json::from_str(text).map_err(|_| "la réponse n'est pas du JSON".to_string())?;
    let description = answer
        .get("description")
        .and_then(Value::as_str)
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty())
        .ok_or("description absente")?;
    let niveau = answer
        .get("niveau")
        .and_then(Value::as_u64)
        .filter(|n| *n <= 2)
        .ok_or("niveau absent ou hors de 0 à 2")? as u8;
    Ok(Outcome::Described {
        description: truncate(&description),
        niveau,
    })
}

/// Au plus [`MAX_CHARS`] caractères, coupés proprement (« … » si coupé).
fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Une réponse HTTP (aussi lue par les notifications web push, `push/transport.rs`).
#[derive(Debug)]
pub(crate) struct Response {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: String,
}

/// Sépare la sortie de `curl -i` : en-têtes (les réponses intermédiaires 1xx, « 100 Continue », sont
/// passées) et corps. Pas de proxy (voir `curl_command`) : pas de « Connection established ».
pub(crate) fn parse_http(output: &[u8]) -> Result<Response, String> {
    let mut rest = output;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("réponse HTTP illisible")?;
        let head = String::from_utf8_lossy(&rest[..end]).into_owned();
        let body = &rest[end + 4..];
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or("réponse HTTP illisible")?;
        if (100..200).contains(&status) {
            rest = body;
            continue;
        }
        let retry_after = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        return Ok(Response {
            status,
            retry_after,
            body: String::from_utf8_lossy(body).into_owned(),
        });
    }
}

/// Préfixe des corps de requête des descriptions dans le dossier temporaire.
pub(crate) const BODY_PREFIX: &str = "mh-description-";

/// Un fichier temporaire 0600, effacé à la fin.
pub(crate) struct TempBody(PathBuf);

impl TempBody {
    fn write(bytes: &[u8]) -> std::io::Result<TempBody> {
        Self::write_named(BODY_PREFIX, "json", bytes)
    }

    /// `prefix` : voir `clean_leftover_bodies`, qui efface ceux qu'un arrêt brutal a laissés.
    pub(crate) fn write_named(
        prefix: &str,
        extension: &str,
        bytes: &[u8],
    ) -> std::io::Result<TempBody> {
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}-{:016x}.{extension}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        let guard = TempBody(path);
        file.write_all(bytes)?;
        Ok(guard)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempBody {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Une valeur entre guillemets pour un fichier de configuration de `curl` (déjà validée : ni
/// guillemet ni barre oblique inverse, ni caractère de contrôle).
pub(crate) fn quoted(value: &str) -> Result<String, String> {
    if value
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\')
    {
        return Err("valeur refusée".into());
    }
    Ok(format!("\"{value}\""))
}

/// Les arguments de `curl` : `-q` en premier ignore `~/.curlrc`, seuls http et https (y compris
/// après redirection, qui n'est de toute façon pas suivie), `-g` désactive les `{}` et `[]`.
fn curl_args(body: &Path, timeout: Duration) -> Vec<String> {
    [
        "-q",
        "-sS",
        "-i",
        "-m",
        "{timeout}",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "-g",
        "--noproxy",
        "*",
        "-X",
        "POST",
        "--data-binary",
    ]
    .map(|a| a.replace("{timeout}", &timeout.as_secs().max(1).to_string()))
    .into_iter()
    .chain([format!("@{}", body.display()), "-K".into(), "-".into()])
    .collect()
}

/// La configuration envoyée à `curl` par l'entrée standard : adresse et en-têtes (dont la clé).
fn curl_config(base_url: &str, key: &str, model: Model) -> Result<String, String> {
    let mut config = format!(
        "url = {}\nheader = {}\nheader = {}\nheader = \"content-type: application/json\"\nheader = \"Expect:\"\n",
        quoted(&format!("{}/v1/messages", base_url.trim_end_matches('/')))?,
        quoted(&format!("x-api-key: {key}"))?,
        quoted(&format!("anthropic-version: {VERSION}"))?,
    );
    if model.server_fallback() {
        config.push_str(&format!(
            "header = {}\n",
            quoted(&format!("anthropic-beta: {BETA_FALLBACK}"))?
        ));
    }
    Ok(config)
}

/// Un échec d'une tentative : `retryable` seulement quand la requête n'a sûrement pas été traitée
/// (connexion impossible), jamais après un délai dépassé ou une coupure en cours de réponse
/// (l'API a pu la facturer : pas de double facturation).
struct PostError {
    retryable: bool,
    why: String,
}

impl PostError {
    fn local(why: impl Into<String>) -> Self {
        Self {
            retryable: false,
            why: why.into(),
        }
    }
}

/// Codes de sortie de `curl` d'avant l'envoi de la requête : adresse du proxy ou de l'hôte
/// introuvable (5, 6), connexion refusée ou impossible (7), poignée de main TLS (35).
pub(crate) const CURL_BEFORE_SEND: [i32; 4] = [5, 6, 7, 35];

/// Le programme et son environnement : le `curl` du système (pas celui du `PATH`), un
/// environnement **vide** : ni proxy hérité (`https_proxy`, `ALL_PROXY`…), ni variable de
/// configuration, et `--noproxy "*"` en plus. Sans proxy, la sortie de `curl -i` ne commence jamais
/// par « Connection established » (voir `parse_http`).
pub(crate) fn curl_command() -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command.env_clear();
    command
}

/// Une tentative.
fn post_once(
    endpoint: &Endpoint,
    key: &str,
    model: Model,
    body: &[u8],
) -> Result<Response, PostError> {
    if !protocol::automation::is_webhook_url(&endpoint.base_url) {
        return Err(PostError::local("adresse de l'API invalide"));
    }
    let temp = TempBody::write(body)
        .map_err(|e| PostError::local(format!("fichier temporaire : {}", e.kind())))?;
    let config = curl_config(&endpoint.base_url, key, model).map_err(PostError::local)?;
    let mut child = curl_command()
        .args(curl_args(temp.path(), endpoint.timeout))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PostError::local(format!("curl : {}", e.kind())))?;
    if let Some(mut stdin) = child.stdin.take() {
        // La configuration est courte : elle tient dans le tube sans attendre `curl`.
        if let Err(e) = stdin.write_all(config.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PostError::local(format!("curl : {}", e.kind())));
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| PostError::local(format!("curl : {}", e.kind())))?;
    if !output.status.success() {
        // `curl -sS` écrit une ligne sans la clé (« Could not resolve host… »).
        let why = String::from_utf8_lossy(&output.stderr);
        let why = why.lines().next().unwrap_or("échec réseau").trim();
        return Err(PostError {
            retryable: output
                .status
                .code()
                .is_some_and(|c| CURL_BEFORE_SEND.contains(&c)),
            why: why.chars().take(160).collect(),
        });
    }
    parse_http(&output.stdout).map_err(PostError::local)
}

/// Le message d'erreur de l'API, court (sans rien d'autre de la réponse).
fn api_error(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let e = v.get("error")?;
            let kind = e.get("type").and_then(Value::as_str).unwrap_or("erreur");
            let message = e.get("message").and_then(Value::as_str).unwrap_or("");
            Some(format!("{kind} : {message}"))
        })
        .map_or_else(String::new, |m| m.chars().take(200).collect())
}

/// Décrit une capture. Bloquant (jusqu'à 2 × 30 s et une attente) : à n'appeler que depuis le fil
/// des descriptions.
pub fn describe(
    endpoint: &Endpoint,
    key: &str,
    model: Model,
    context: &Context,
    jpeg: &[u8],
) -> Result<Outcome, String> {
    match jpeg_long_side(jpeg) {
        Some(side) if side <= MAX_SIDE && jpeg.len() <= MAX_JPEG_BYTES => {}
        Some(side) => {
            return Err(format!(
                "capture trop grande ({side} px, {} octets)",
                jpeg.len()
            ))
        }
        None => return Err("capture illisible (pas un JPEG)".into()),
    }
    let body = serde_json::to_vec(&build_body(model, context, jpeg)).map_err(|e| e.to_string())?;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let (why, wait) = match post_once(endpoint, key, model, &body) {
            Ok(r) if (200..300).contains(&r.status) => return parse_response(&r.body),
            Ok(r) if r.status == 429 || r.status >= 500 => (
                format!("HTTP {} {}", r.status, api_error(&r.body)),
                r.retry_after.unwrap_or(endpoint.retry_default),
            ),
            Ok(r) => return Err(format!("HTTP {} {}", r.status, api_error(&r.body))),
            Err(e) if e.retryable => (e.why, endpoint.retry_default),
            Err(e) => return Err(e.why.trim().to_string()),
        };
        if attempt >= 2 {
            return Err(why.trim().to_string());
        }
        log::warn!(
            "description : {} ; nouvel essai dans {} s",
            why.trim(),
            wait.min(MAX_WAIT).as_secs()
        );
        std::thread::sleep(wait.min(MAX_WAIT));
    }
}

#[cfg(test)]
mod tests;
