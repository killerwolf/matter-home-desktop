//! Caméras RTSP : stockage, vérification et surveillance (ticket #4).
//!
//! # Ce que le serveur sait, ce que les clients savent
//!
//! Le serveur est le **seul** à parler aux caméras. Il garde pour chacune une adresse RTSP
//! (flux principal), une adresse facultative de flux réduit, un identifiant et un mot de passe
//! ([`StoredCamera`]). Les clients ne reçoivent que `protocol::wire::CameraView` : identifiant,
//! nom, état, codec et taille du flux. Ce type n'a pas de champ pour une adresse ou un secret.
//!
//! # Secrets
//!
//! - **Disque** : `cameras.json` dans le dossier de données (0700), créé en **0600** dès sa
//!   création et écrit de façon atomique (fichier temporaire, `fsync`, renommage), comme les clés
//!   de `controller.json`. Le mot de passe y est en clair : le fichier est un secret au même
//!   titre. Il est **inclus dans la sauvegarde `.matterhome`** (`backup.rs`), comme ces clés.
//! - **Adresse sans identifiants** : un utilisateur colle souvent `rtsp://user:mdp@ip:554/…`.
//!   L'adresse enregistrée n'a **jamais** de `user:mdp@` : les identifiants passent dans leurs
//!   champs (voir [`CameraDraft`]). `retina` refuse d'ailleurs une adresse qui en porte.
//! - **Mémoire et journaux** : `Debug` masque le mot de passe ([`StoredCamera`], [`CameraDraft`],
//!   [`Credentials`], [`Target`]), les messages d'erreur ne reprennent jamais ce que
//!   l'utilisateur a saisi, et rien de ce module n'écrit une adresse dans le journal (seulement
//!   le nom donné à la caméra et un identifiant). `masked_url` sert à afficher une adresse
//!   inconnue sans ses identifiants.
//!
//! # Connexions
//!
//! Le serveur ne garde **aucune connexion ouverte** vers une caméra. Il la vérifie par un
//! `DESCRIBE` bref (jamais de `SETUP` ni de `PLAY`), dans un thread à part avec son propre
//! runtime, une caméra à la fois, avec un délai strict ([`PROBE_TIMEOUT`]) : une caméra qui ne
//! répond pas ne ralentit ni ne bloque rien d'autre, et les appareils Matter l'ignorent. À
//! l'ajout ou la modification, la vérification est immédiate ; ensuite toutes les
//! [`MONITOR_EVERY`].
//!
//! # Pour #5 (lecture), #6 (plan) et #7 (liste)
//!
//! - **Lecture** : [`Hub::camera_target`](crate::hub::Hub::camera_target) rend l'adresse (sans
//!   identifiants) et les identifiants du flux voulu ([`StreamKind`]) : c'est le point d'entrée
//!   interne, il ne sort jamais du serveur. Par défaut on lit le flux réduit
//!   ([`StoredCamera::preferred_kind`]). Que le serveur relaie le flux ou que le client se connecte
//!   en direct n'est **pas décidé ici** (voir `docs/protocole.md`, « Caméras »).
//! - **Plan** : le placement (pièce, position, orientation, ouverture, portée) n'est pas dans la
//!   caméra mais dans le plan (`home::Item`) : à la suppression d'une caméra, `Hub::delete_camera`
//!   est l'endroit où la retirer aussi du plan (repère `#6`).
//! - **Liste** : `GET /api/cameras`, l'état complet et `cameras_changed` donnent tout ce qu'il faut.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};
use url::Url;

use protocol::wire::{
    CameraProblem, CameraStatus, CameraView, StreamInfo, CAMERA_ID_MAX, CAMERA_ID_MIN, MAX_CAMERAS,
};

use crate::hub::Hub;
use crate::security::write_secret;

/// Fichier des caméras, dans le dossier de données (exporté dans la sauvegarde).
pub const FILE_NAME: &str = "cameras.json";
const FILE_VERSION: u32 = 1;

/// Délai maximal d'une vérification (connexion comprise).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Intervalle entre deux vérifications de toutes les caméras.
pub const MONITOR_EVERY: Duration = Duration::from_secs(60);
/// Délai avant la première vérification au démarrage du serveur.
const FIRST_PROBE_DELAY: Duration = Duration::from_secs(2);

const MAX_NAME: usize = 80;
const MAX_URL: usize = 2_048;
const MAX_USERNAME: usize = 128;
const MAX_PASSWORD: usize = 256;

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

/// Une demande refusée, avec la phrase à montrer à l'utilisateur (français, jamais d'adresse
/// ni de mot de passe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraError(pub String);

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CameraError {}

fn refuse<T>(text: impl Into<String>) -> Result<T, CameraError> {
    Err(CameraError(text.into()))
}

// ---------------------------------------------------------------------------
// Données enregistrées
// ---------------------------------------------------------------------------

/// Flux d'une caméra : le principal (plein format) ou le réduit (mosaïque, lecture par défaut).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Main,
    Sub,
}

impl StreamKind {
    pub fn label(self) -> &'static str {
        match self {
            StreamKind::Main => "Flux principal",
            StreamKind::Sub => "Flux réduit",
        }
    }
}

/// Une caméra telle que le serveur la garde. **Contient un mot de passe** : ne jamais la
/// sérialiser vers un client ni la journaliser (son `Debug` le masque).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredCamera {
    /// Dans `CAMERA_ID_MIN..=CAMERA_ID_MAX` ; numérique sur disque (comme `maison.json`),
    /// en texte sur le réseau.
    pub id: u64,
    pub name: String,
    /// Adresse RTSP du flux principal, **sans** `user:mdp@`.
    pub url: String,
    /// Adresse du flux réduit, sans identifiants (facultative). Aqara G100 : `…/live/ch00_1`.
    #[serde(default)]
    pub sub_url: Option<String>,
    /// Identifiant, commun aux deux flux.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    // Extension : tout champ ajouté ici doit avoir `#[serde(default)]` (anciens fichiers, et
    // sauvegarde d'un autre Mac) et rester absent de `CameraView` s'il est sensible.
}

impl fmt::Debug for StoredCamera {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCamera")
            .field("id", &format_args!("{:#x}", self.id))
            .field("name", &self.name)
            .field("url", &"…")
            .field("sub_url", &self.sub_url.as_ref().map(|_| "…"))
            .field("username", &self.username.as_ref().map(|_| "…"))
            .field("password", &self.password.as_ref().map(|_| "…"))
            .finish()
    }
}

impl StoredCamera {
    pub fn has_sub_stream(&self) -> bool {
        self.sub_url.is_some()
    }

    /// Le flux à lire ou à surveiller par défaut : le réduit s'il existe (stable sur les
    /// Aqara G100, bien plus léger), sinon le principal.
    pub fn preferred_kind(&self) -> StreamKind {
        if self.has_sub_stream() {
            StreamKind::Sub
        } else {
            StreamKind::Main
        }
    }

    /// Adresse et identifiants du flux demandé, prêts pour `retina`. `None` si le flux réduit est
    /// demandé alors qu'il n'y en a pas, ou si l'adresse enregistrée est illisible.
    pub fn target(&self, kind: StreamKind) -> Option<Target> {
        let text = match kind {
            StreamKind::Main => Some(self.url.as_str()),
            StreamKind::Sub => self.sub_url.as_deref(),
        }?;
        let url = Url::parse(text).ok()?;
        let credentials = self.username.as_ref().map(|username| Credentials {
            username: username.clone(),
            password: self.password.clone().unwrap_or_default(),
        });
        Some(Target { url, credentials })
    }

    /// Les contrôles d'une caméra lue sur disque ou dans une sauvegarde. Les messages ne
    /// reprennent pas l'adresse.
    pub fn check(&self) -> Result<(), CameraError> {
        if !protocol::wire::is_camera_id(self.id) {
            return refuse("identifiant de caméra hors de la plage réservée");
        }
        check_text(&self.name, 1, MAX_NAME, "nom")?;
        check_stored_url(&self.url)?;
        if let Some(sub) = &self.sub_url {
            check_stored_url(sub)?;
        }
        if let Some(user) = &self.username {
            check_text(user, 1, MAX_USERNAME, "identifiant")?;
        }
        if let Some(password) = &self.password {
            if password.chars().count() > MAX_PASSWORD || password.chars().any(char::is_control) {
                return refuse("mot de passe de caméra invalide");
            }
        }
        if self.password.is_some() && self.username.is_none() {
            return refuse("mot de passe de caméra sans identifiant");
        }
        Ok(())
    }
}

fn check_stored_url(text: &str) -> Result<(), CameraError> {
    let url = parse_rtsp(text, "adresse")?;
    if !url.username().is_empty() || url.password().is_some() {
        return refuse("adresse de caméra avec identifiants : ils vont dans leurs champs");
    }
    Ok(())
}

/// Contenu de `cameras.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraFile {
    pub version: u32,
    #[serde(default)]
    pub cameras: Vec<StoredCamera>,
}

impl Default for CameraFile {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            cameras: Vec::new(),
        }
    }
}

impl CameraFile {
    /// Validation complète, pour une sauvegarde à importer : une caméra qui ne passe pas
    /// refuse la sauvegarde (le message dit laquelle, par son nom).
    pub fn validate(&self) -> Result<(), CameraError> {
        if self.version != FILE_VERSION {
            return refuse("version du fichier des caméras non prise en charge");
        }
        if self.cameras.len() > MAX_CAMERAS {
            return refuse("trop de caméras");
        }
        let mut ids = HashSet::new();
        for camera in &self.cameras {
            camera
                .check()
                .map_err(|e| CameraError(format!("caméra « {} » : {e}", camera.name)))?;
            if !ids.insert(camera.id) {
                return refuse("identifiant de caméra en double");
            }
        }
        Ok(())
    }
}

/// Lit `cameras.json`. Absent : aucune caméra. Illisible : mis de côté sous
/// `cameras.json.illisible` (jamais écrasé sans trace) et aucune caméra. Les caméras
/// invalides ou en double sont écartées avec un avertissement (nom seulement dans le journal).
pub fn load(dir: &Path) -> Vec<StoredCamera> {
    let path = dir.join(FILE_NAME);
    let file: CameraFile = match crate::persistence::load_json(&path) {
        Ok(Some(file)) => file,
        Ok(None) => return Vec::new(),
        Err(e) => {
            let aside = path.with_extension("json.illisible");
            log::error!(
                "caméras illisibles, aucune caméra chargée ({e:#}) ; fichier conservé sous {}",
                aside.display()
            );
            let _ = std::fs::rename(&path, aside);
            return Vec::new();
        }
    };
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for camera in file.cameras {
        if let Err(e) = camera.check() {
            log::warn!("caméra « {} » ignorée : {e}", camera.name);
        } else if !seen.insert(camera.id) {
            log::warn!("caméra « {} » ignorée : identifiant en double", camera.name);
        } else if kept.len() >= MAX_CAMERAS {
            log::warn!("caméra « {} » ignorée : trop de caméras", camera.name);
        } else {
            kept.push(camera);
        }
    }
    kept
}

/// Enregistre `cameras.json` : 0600 dès la création, atomique, synchronisé sur disque.
pub fn save(dir: &Path, cameras: &[StoredCamera]) -> Result<()> {
    let file = CameraFile {
        version: FILE_VERSION,
        cameras: cameras.to_vec(),
    };
    let text = serde_json::to_string_pretty(&file)?;
    write_secret(&dir.join(FILE_NAME), text.as_bytes()).context("écriture de cameras.json")
}

// ---------------------------------------------------------------------------
// Saisie : ce que l'utilisateur tape dans la fenêtre du serveur
// ---------------------------------------------------------------------------

/// Le formulaire de la fenêtre « Caméras ». `Debug` masque le mot de passe.
#[derive(Clone, Default, PartialEq)]
pub struct CameraDraft {
    pub name: String,
    pub url: String,
    /// Vide : pas de flux réduit.
    pub sub_url: String,
    /// Vide : pas d'identifiant (caméra sans authentification).
    pub username: String,
    /// `None` : garder le mot de passe enregistré (modification d'une caméra existante).
    /// `Some("")` : l'effacer. `Some(texte)` : le remplacer.
    pub password: Option<String>,
}

impl fmt::Debug for CameraDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CameraDraft")
            .field("name", &self.name)
            .field("url", &"…")
            .field("sub_url", &"…")
            .field("username", &"…")
            .field("password", &self.password.as_ref().map(|_| "…"))
            .finish()
    }
}

impl CameraDraft {
    /// Contrôle la saisie et la met en forme enregistrable, sous l'identifiant `id`.
    /// `existing` : la caméra modifiée, dont on garde le mot de passe si `password` est `None`.
    ///
    /// Les identifiants peuvent aussi être collés dans l'adresse (`rtsp://user:mdp@ip/…`) : ils
    /// sont retirés de l'adresse et rangés dans leurs champs. Un champ rempli l'emporte sur
    /// l'adresse ; deux adresses aux identifiants différents sont refusées.
    pub fn build(
        &self,
        id: u64,
        existing: Option<&StoredCamera>,
    ) -> Result<StoredCamera, CameraError> {
        let name = self.name.trim().to_string();
        check_text(&name, 1, MAX_NAME, "nom")?;

        let (url, main_creds) = split_url(&self.url, "adresse du flux principal")?;
        let (sub_url, sub_creds) = match self.sub_url.trim() {
            "" => (None, None),
            text => {
                let (u, c) = split_url(text, "adresse du flux réduit")?;
                (Some(u), c)
            }
        };
        let from_url = match (main_creds, sub_creds) {
            (Some(a), Some(b)) if a != b => {
                return refuse("les deux adresses contiennent des identifiants différents");
            }
            (a, b) => a.or(b),
        };

        let mut username = self.username.trim().to_string();
        if username.is_empty() {
            username = from_url
                .as_ref()
                .map(|(u, _)| u.clone())
                .unwrap_or_default();
        }
        let url_password = from_url.as_ref().and_then(|(_, p)| p.clone());
        let password = match &self.password {
            Some(p) if !p.is_empty() => Some(p.clone()),
            Some(_) => url_password,
            None => url_password.or_else(|| existing.and_then(|c| c.password.clone())),
        };
        let username = Some(username).filter(|u| !u.is_empty());
        if let Some(u) = &username {
            check_text(u, 1, MAX_USERNAME, "identifiant")?;
        }
        if let Some(p) = &password {
            if p.chars().count() > MAX_PASSWORD || p.chars().any(char::is_control) {
                return refuse(
                    "mot de passe invalide (256 caractères au plus, sans retour à la ligne)",
                );
            }
        }
        if password.is_some() && username.is_none() {
            return refuse("un mot de passe demande un identifiant");
        }
        Ok(StoredCamera {
            id,
            name,
            url,
            sub_url,
            username,
            password,
        })
    }
}

fn check_text(text: &str, min: usize, max: usize, what: &str) -> Result<(), CameraError> {
    let n = text.chars().count();
    if n < min {
        return refuse(format!("{what} manquant"));
    }
    if n > max || text.chars().any(char::is_control) {
        return refuse(format!("{what} invalide ({max} caractères au plus)"));
    }
    Ok(())
}

/// Lit une adresse `rtsp://`. Aucun message ne reprend le texte saisi : il peut contenir un
/// mot de passe.
fn parse_rtsp(text: &str, what: &str) -> Result<Url, CameraError> {
    let text = text.trim();
    if text.is_empty() {
        return refuse(format!("{what} manquante"));
    }
    if text.chars().count() > MAX_URL {
        return refuse(format!("{what} trop longue"));
    }
    let url = Url::parse(text).map_err(|_| {
        CameraError(format!(
            "{what} illisible (exemple : rtsp://192.168.1.20:554/live/ch00_1)"
        ))
    })?;
    match url.scheme() {
        "rtsp" => {}
        "rtsps" => {
            return refuse(format!(
                "{what} : rtsps:// (chiffré) n'est pas pris en charge"
            ))
        }
        _ => return refuse(format!("{what} : elle doit commencer par rtsp://")),
    }
    if url.host_str().is_none_or(str::is_empty) {
        return refuse(format!("{what} : le nom ou l'adresse de la caméra manque"));
    }
    Ok(url)
}

/// Adresse et identifiants extraits (déjà décodés).
type UrlCredentials = (String, Option<String>);

/// L'adresse sans identifiants (ni ancre), et les identifiants qu'elle portait.
fn split_url(text: &str, what: &str) -> Result<(String, Option<UrlCredentials>), CameraError> {
    let mut url = parse_rtsp(text, what)?;
    let credentials = (!url.username().is_empty() || url.password().is_some()).then(|| {
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        (decode(url.username()), url.password().map(decode))
    });
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    Ok((url.to_string(), credentials))
}

/// L'adresse d'un flux sans ses identifiants, pour l'afficher sans risque. Un texte qui n'est pas
/// une adresse n'est pas repris du tout.
pub fn masked_url(text: &str) -> String {
    match Url::parse(text.trim()) {
        Ok(mut url) => {
            if !url.username().is_empty() || url.password().is_some() {
                let _ = url.set_username("***");
                let _ = url.set_password(None);
            }
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "<adresse illisible>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Vérification d'un flux (DESCRIBE)
// ---------------------------------------------------------------------------

/// Identifiants d'une caméra. `Debug` masque tout.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials { … }")
    }
}

/// Ce qu'il faut pour parler à un flux : l'adresse (sans identifiants) et les identifiants.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub url: Url,
    pub credentials: Option<Credentials>,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Target { … }")
    }
}

/// Une vérification échouée : le type de problème et une phrase pour l'utilisateur (sans
/// adresse ni identifiant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub problem: CameraProblem,
    pub message: String,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProbeError {}

fn probe_error(problem: CameraProblem, message: &str) -> ProbeError {
    ProbeError {
        problem,
        message: message.to_string(),
    }
}

/// Classe une erreur de `retina` (opaque) d'après son texte. Le texte brut ne sort jamais : il
/// contient les adresses de la connexion.
fn classify(error: &retina::Error) -> ProbeError {
    let text = error.to_string();
    if text.contains("401 response") || text.contains("403 response") {
        probe_error(
            CameraProblem::Unauthorized,
            "La caméra refuse l'identifiant ou le mot de passe.",
        )
    } else if text.contains("404 response") {
        probe_error(
            CameraProblem::NotFound,
            "La caméra ne connaît pas cette adresse de flux (vérifiez le chemin, par exemple /live/ch00_1).",
        )
    } else if text.contains("Unable to connect") {
        probe_error(
            CameraProblem::Unreachable,
            "La caméra ne répond pas (éteinte, hors réseau, RTSP désactivé ou mauvais port).",
        )
    } else if text.contains("Error reading from RTSP peer")
        || text.contains("Error writing to RTSP peer")
    {
        probe_error(
            CameraProblem::Unreachable,
            "La connexion avec la caméra s'est interrompue.",
        )
    } else if text.contains("RTSP framing error") || text.contains("Unparseable") {
        probe_error(
            CameraProblem::Protocol,
            "La caméra a répondu, mais pas comme une caméra RTSP.",
        )
    } else if text.contains("response to DESCRIBE") {
        probe_error(
            CameraProblem::Failed,
            "La caméra a refusé de décrire ce flux.",
        )
    } else {
        probe_error(CameraProblem::Failed, "Échec de la connexion à la caméra.")
    }
}

/// Décrit un flux : un `DESCRIBE`, lu par `retina` (authentification Basic ou Digest comprise),
/// puis la connexion est lâchée. Ni `SETUP` ni `PLAY` : la caméra ne démarre aucun flux.
///
/// Rend le codec et, quand la caméra les annonce dans sa description (SPS H.264 ou H.265), les
/// dimensions du premier flux vidéo.
pub async fn probe(target: &Target, timeout: Duration) -> Result<StreamInfo, ProbeError> {
    let mut options = retina::client::SessionOptions::default()
        .user_agent(format!("MatterHomeServer/{}", env!("CARGO_PKG_VERSION")));
    if let Some(c) = &target.credentials {
        options = options.creds(Some(retina::client::Credentials {
            username: c.username.clone(),
            password: c.password.clone(),
        }));
    }
    let described = tokio::time::timeout(
        timeout,
        retina::client::Session::describe(target.url.clone(), options),
    )
    .await
    .map_err(|_| {
        probe_error(
            CameraProblem::Unreachable,
            "La caméra n'a pas répondu à temps.",
        )
    })?
    .map_err(|e| classify(&e))?;
    let stream = described
        .streams()
        .iter()
        .find(|s| s.media() == "video")
        .ok_or_else(|| {
            probe_error(
                CameraProblem::Protocol,
                "Aucun flux vidéo dans la réponse de la caméra.",
            )
        })?;
    let (width, height) = match stream.parameters() {
        Some(retina::codec::ParametersRef::Video(v)) => {
            let (w, h) = v.pixel_dimensions();
            (Some(w), Some(h))
        }
        _ => (None, None),
    };
    Ok(StreamInfo {
        codec: stream.encoding_name().to_ascii_lowercase(),
        width,
        height,
    })
}

/// [`probe`] pour un appelant sans runtime (la fenêtre du serveur) : bloque jusqu'au résultat
/// ou au délai.
pub fn probe_blocking(target: &Target, timeout: Duration) -> Result<StreamInfo, ProbeError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| probe_error(CameraProblem::Failed, "Vérification impossible."))?;
    runtime.block_on(probe(target, timeout))
}

/// « H.264 · 1280 × 720 » : le résultat d'une vérification, pour l'utilisateur.
pub fn describe_stream(info: &StreamInfo) -> String {
    let codec = match info.codec.as_str() {
        "h264" => "H.264".to_string(),
        "h265" => "H.265".to_string(),
        "jpeg" => "MJPEG".to_string(),
        other => other.to_ascii_uppercase(),
    };
    match (info.width, info.height) {
        (Some(w), Some(h)) => format!("{codec} · {w} × {h}"),
        _ => format!("{codec} · résolution annoncée au premier flux"),
    }
}

/// Résultat du bouton « Tester » pour un flux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTest {
    pub kind: StreamKind,
    pub outcome: Result<StreamInfo, ProbeError>,
}

impl StreamTest {
    /// « Flux principal : H.264 · 1280 × 720 » ou « Flux réduit : La caméra refuse… ».
    pub fn describe(&self) -> String {
        match &self.outcome {
            Ok(info) => format!("{} : {}", self.kind.label(), describe_stream(info)),
            Err(e) => format!("{} : {}", self.kind.label(), e.message),
        }
    }
}

/// Teste les flux d'une caméra, l'un après l'autre (jamais deux connexions à la fois : les
/// caméras en acceptent peu).
pub fn test_camera(camera: &StoredCamera) -> Vec<StreamTest> {
    [StreamKind::Main, StreamKind::Sub]
        .into_iter()
        .filter_map(|kind| {
            let target = camera.target(kind)?;
            Some(StreamTest {
                kind,
                outcome: probe_blocking(&target, PROBE_TIMEOUT),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Le carnet : caméras et état, tenus par le hub
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Health {
    status: CameraStatus,
    stream: Option<StreamInfo>,
}

impl Health {
    fn unknown() -> Self {
        Self {
            status: CameraStatus::Unknown,
            stream: None,
        }
    }
}

/// Ce que la fenêtre du serveur montre d'une caméra : tout sauf le mot de passe.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraAdmin {
    pub id: u64,
    pub name: String,
    pub url: String,
    pub sub_url: Option<String>,
    pub username: Option<String>,
    pub has_password: bool,
    pub status: CameraStatus,
    pub stream: Option<StreamInfo>,
}

/// Les caméras et leur état de joignabilité (qui n'est pas enregistré).
#[derive(Debug, Clone, Default)]
pub struct CameraBook {
    cameras: Vec<StoredCamera>,
    health: BTreeMap<u64, Health>,
}

impl CameraBook {
    pub fn new(cameras: Vec<StoredCamera>) -> Self {
        let health = cameras.iter().map(|c| (c.id, Health::unknown())).collect();
        Self { cameras, health }
    }

    /// Le mode démo : des caméras factices avec un état fixe, sans réseau.
    pub fn demo() -> Self {
        let camera = |n: u64, name: &str, sub: bool| StoredCamera {
            id: CAMERA_ID_MIN + n,
            name: name.into(),
            // Adresses de documentation (RFC 5737) : jamais routées.
            url: format!("rtsp://192.0.2.{}:554/live/ch00_0", 10 + n),
            sub_url: sub.then(|| format!("rtsp://192.0.2.{}:554/live/ch00_1", 10 + n)),
            username: Some("demo".into()),
            password: Some("demo-camera-mdp".into()),
        };
        let mut book = Self::new(vec![
            camera(1, "Entrée", true),
            camera(2, "Jardin", false),
            camera(3, "Garage", true),
        ]);
        let h264 = |w, h| StreamInfo {
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
        };
        book.set_health(
            CAMERA_ID_MIN + 1,
            CameraStatus::Online,
            Some(h264(1280, 720)),
        );
        book.set_health(
            CAMERA_ID_MIN + 2,
            CameraStatus::Online,
            Some(h264(2304, 1296)),
        );
        book.set_health(
            CAMERA_ID_MIN + 3,
            CameraStatus::Unreachable {
                problem: CameraProblem::Unreachable,
                detail: Some(
                    "La caméra ne répond pas (éteinte, hors réseau, RTSP désactivé ou mauvais port)."
                        .into(),
                ),
            },
            None,
        );
        book
    }

    pub fn cameras(&self) -> &[StoredCamera] {
        &self.cameras
    }

    pub fn get(&self, id: u64) -> Option<&StoredCamera> {
        self.cameras.iter().find(|c| c.id == id)
    }

    pub fn ids(&self) -> Vec<u64> {
        self.cameras.iter().map(|c| c.id).collect()
    }

    /// Ce que voient les clients, trié par nom (puis par identifiant).
    pub fn views(&self) -> Vec<CameraView> {
        let mut views: Vec<CameraView> = self
            .cameras
            .iter()
            .map(|c| {
                let health = self
                    .health
                    .get(&c.id)
                    .cloned()
                    .unwrap_or_else(Health::unknown);
                CameraView {
                    id: c.id,
                    name: c.name.clone(),
                    status: health.status,
                    has_sub_stream: c.has_sub_stream(),
                    stream: health.stream,
                }
            })
            .collect();
        views.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        views
    }

    /// La liste pour la fenêtre du serveur (sans mot de passe), triée comme celle des clients.
    pub fn admin_list(&self) -> Vec<CameraAdmin> {
        self.views()
            .into_iter()
            .filter_map(|view| {
                let c = self.get(view.id)?;
                Some(CameraAdmin {
                    id: c.id,
                    name: c.name.clone(),
                    url: c.url.clone(),
                    sub_url: c.sub_url.clone(),
                    username: c.username.clone(),
                    has_password: c.password.is_some(),
                    status: view.status,
                    stream: view.stream,
                })
            })
            .collect()
    }

    fn fresh_id(&self) -> u64 {
        let mut rng = rand::thread_rng();
        loop {
            let id = rng.gen_range(CAMERA_ID_MIN..=CAMERA_ID_MAX);
            // `u64::MAX` est l'identifiant « tous » de Matter : on l'évite par prudence.
            if id != u64::MAX && self.get(id).is_none() {
                return id;
            }
        }
    }

    pub fn add(&mut self, draft: &CameraDraft) -> Result<u64, CameraError> {
        if self.cameras.len() >= MAX_CAMERAS {
            return refuse(format!("{MAX_CAMERAS} caméras au plus"));
        }
        let id = self.fresh_id();
        let camera = draft.build(id, None)?;
        self.cameras.push(camera);
        self.health.insert(id, Health::unknown());
        Ok(id)
    }

    /// Modifie une caméra. Son état repart de « inconnu » : l'adresse a peut-être changé.
    pub fn update(&mut self, id: u64, draft: &CameraDraft) -> Result<(), CameraError> {
        let Some(old) = self.get(id) else {
            return refuse("cette caméra n'existe plus");
        };
        let new = draft.build(id, Some(old))?;
        // Renommer ne change rien à la joignabilité ; une autre adresse ou d'autres identifiants,
        // si : l'ancien résultat ne vaut plus.
        let same_connection = new.url == old.url
            && new.sub_url == old.sub_url
            && new.username == old.username
            && new.password == old.password;
        if !same_connection {
            self.health.insert(id, Health::unknown());
        }
        if let Some(slot) = self.cameras.iter_mut().find(|c| c.id == id) {
            *slot = new;
        }
        Ok(())
    }

    pub fn remove(&mut self, id: u64) -> Result<(), CameraError> {
        let before = self.cameras.len();
        self.cameras.retain(|c| c.id != id);
        self.health.remove(&id);
        if self.cameras.len() == before {
            return refuse("cette caméra n'existe plus");
        }
        Ok(())
    }

    /// Retient l'état d'une caméra. `true` si ce que voient les clients a changé.
    pub fn set_health(
        &mut self,
        id: u64,
        status: CameraStatus,
        stream: Option<StreamInfo>,
    ) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        // Un échec garde le dernier flux connu : codec et taille ne changent pas parce que la
        // caméra a été débranchée.
        let previous = self.health.get(&id).and_then(|h| h.stream.clone());
        let stream = stream.or(previous);
        let new = Health { status, stream };
        let changed = self.health.get(&id) != Some(&new);
        self.health.insert(id, new);
        changed
    }
}

// ---------------------------------------------------------------------------
// Surveillance
// ---------------------------------------------------------------------------

/// Demande au surveillant de vérifier tout de suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Camera(u64),
    All,
}

/// Le fil de surveillance. [`CameraMonitor::stop`] l'arrête (une vérification en cours est
/// abandonnée sur-le-champ).
pub struct CameraMonitor {
    stop: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
}

impl CameraMonitor {
    /// Arrête et attend le fil. `true` s'il s'est arrêté sans panique.
    pub fn stop(mut self) -> bool {
        self.stop.notify_one();
        self.thread.take().is_none_or(|t| t.join().is_ok())
    }
}

/// Lance la surveillance : vérifie chaque caméra peu après le démarrage puis toutes les
/// [`MONITOR_EVERY`], et sur demande (ajout, modification). Un seul `DESCRIBE` à la fois.
pub fn start_monitor(hub: Arc<Hub>) -> std::io::Result<CameraMonitor> {
    let (tx, rx) = mpsc::unbounded_channel();
    hub.set_camera_wake(Some(tx));
    let stop = Arc::new(Notify::new());
    let thread = {
        let (hub, stop) = (hub.clone(), stop.clone());
        std::thread::Builder::new()
            .name("cameras".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("surveillance des caméras impossible : {e}");
                        return;
                    }
                };
                runtime.block_on(monitor_loop(&hub, rx, &stop));
                hub.set_camera_wake(None);
            })?
    };
    Ok(CameraMonitor {
        stop,
        thread: Some(thread),
    })
}

async fn monitor_loop(hub: &Hub, mut rx: mpsc::UnboundedReceiver<Wake>, stop: &Notify) {
    let mut next_full = tokio::time::Instant::now() + FIRST_PROBE_DELAY;
    loop {
        let mut all = false;
        let mut some: Vec<u64> = Vec::new();
        let mut take = |wake: Wake| match wake {
            Wake::All => all = true,
            Wake::Camera(id) => some.push(id),
        };
        tokio::select! {
            () = stop.notified() => return,
            message = rx.recv() => match message {
                Some(wake) => take(wake),
                None => return,
            },
            () = tokio::time::sleep_until(next_full) => take(Wake::All),
        }
        while let Ok(wake) = rx.try_recv() {
            take(wake);
        }
        let ids = if all {
            next_full = tokio::time::Instant::now() + MONITOR_EVERY;
            hub.camera_ids()
        } else {
            some.sort_unstable();
            some.dedup();
            some
        };
        for id in ids {
            let Some((kind, target)) = hub.camera_probe_target(id) else {
                continue;
            };
            let outcome = tokio::select! {
                () = stop.notified() => return,
                outcome = probe(&target, PROBE_TIMEOUT) => outcome,
            };
            hub.record_camera_probe(id, kind, &target, outcome);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
