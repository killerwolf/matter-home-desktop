//! Caméras enregistrées : données, saisie, validation, fichier `cameras.json`, et le carnet que
//! tient le hub (caméras + état de joignabilité).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use rand::Rng;
use serde::{Deserialize, Serialize};
use url::Url;

use protocol::wire::{
    is_camera_id, CameraProblem, CameraStatus, CameraView, StreamInfo, StreamKind, CAMERA_ID_MAX,
    CAMERA_ID_MIN, MAX_CAMERAS,
};

use crate::security::write_secret;

use super::probe::{Credentials, Target};

pub const FILE_NAME: &str = "cameras.json";
const FILE_VERSION: u32 = 1;
pub(super) const MAX_NAME: usize = 80;
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

/// Un refus, avec une majuscule initiale (la phrase s'affiche telle quelle dans la fenêtre).
pub(super) fn refuse<T>(text: impl Into<String>) -> Result<T, CameraError> {
    let text: String = text.into();
    let mut chars = text.chars();
    let sentence = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    };
    Err(CameraError(sentence))
}

/// Remet la première lettre en minuscule, pour insérer un refus dans une phrase.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Données enregistrées
// ---------------------------------------------------------------------------

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

    /// Les machines (nom ou adresse, sans le port) de ses deux adresses.
    fn hosts(&self) -> Vec<String> {
        [Some(&self.url), self.sub_url.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|u| host_of(u))
            .collect()
    }

    /// Les contrôles d'une caméra lue sur disque ou dans une sauvegarde. Les messages ne
    /// reprennent pas l'adresse.
    pub fn check(&self) -> Result<(), CameraError> {
        if !protocol::wire::is_camera_id(self.id) {
            return refuse("identifiant de caméra hors de la plage de tirage");
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
            camera.check().map_err(|e| {
                CameraError(format!(
                    "Caméra « {} » : {}",
                    camera.name,
                    lower_first(&e.0)
                ))
            })?;
            if !ids.insert(camera.id) {
                return refuse("identifiant de caméra en double");
            }
        }
        Ok(())
    }
}

/// Un nom libre à côté de `path` : `cameras.json.<what>`, puis `….<what>-2`, `-3`…, sans jamais
/// écraser un fichier existant (il peut contenir les seuls exemplaires de mots de passe).
fn free_aside(path: &Path, what: &str) -> std::path::PathBuf {
    let base = path.with_extension(format!("json.{what}"));
    if !base.exists() {
        return base;
    }
    (2..)
        .map(|n| path.with_extension(format!("json.{what}-{n}")))
        .find(|p| !p.exists())
        .expect("un nom libre")
}

/// Lit `cameras.json`. Absent : aucune caméra. Illisible : mis de côté sous
/// `cameras.json.illisible` (jamais écrasé : un suffixe s'ajoute) et aucune caméra. Les caméras
/// invalides ou en double sont écartées avec un avertissement (nom seulement dans le journal) ;
/// s'il y en a, **le fichier d'origine est d'abord copié** (0600) sous `cameras.json.ecartees`,
/// car le prochain enregistrement l'écraserait.
pub fn load(dir: &Path) -> Vec<StoredCamera> {
    let path = dir.join(FILE_NAME);
    let file: CameraFile = match crate::persistence::load_json(&path) {
        Ok(Some(file)) => file,
        Ok(None) => return Vec::new(),
        Err(e) => {
            let aside = free_aside(&path, "illisible");
            log::error!(
                "caméras illisibles, aucune caméra chargée ({e:#}) ; fichier conservé sous {}",
                aside.display()
            );
            let _ = std::fs::rename(&path, aside);
            return Vec::new();
        }
    };
    let total = file.cameras.len();
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
    if kept.len() != total {
        let copy = free_aside(&path, "ecartees");
        let copied = std::fs::copy(&path, &copy).inspect(|_| {
            #[cfg(unix)]
            {
                // Contient des mots de passe : 0600, quels que soient les droits de l'original.
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600));
            }
        });
        match copied {
            Ok(_) => log::warn!(
                "{} caméra(s) écartée(s) : fichier d'origine conservé sous {}",
                total - kept.len(),
                copy.display()
            ),
            Err(e) => log::error!("copie du fichier des caméras impossible : {e}"),
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
            None => match (url_password, existing) {
                (Some(p), _) => Some(p),
                (None, Some(old)) if old.password.is_some() => {
                    // Le mot de passe enregistré ne suit pas la caméra vers une autre machine :
                    // il faut le ressaisir.
                    let known = old.hosts();
                    let moved = [Some(&url), sub_url.as_ref()]
                        .into_iter()
                        .flatten()
                        .filter_map(|u| host_of(u))
                        .any(|h| !known.contains(&h));
                    if moved {
                        return refuse(
                            "l'adresse change de machine : saisissez de nouveau le mot de passe \
                             (il n'est pas réutilisé vers une autre adresse)",
                        );
                    }
                    old.password.clone()
                }
                (None, _) => None,
            },
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
pub type UrlCredentials = (String, Option<String>);

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

fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
}

/// Pour le formulaire : si `text` est une adresse `rtsp://` qui porte `identifiant:mot-de-passe@`,
/// rend l'adresse nettoyée et ces identifiants (décodés). `None` sinon.
pub fn split_credentials(text: &str) -> Option<(String, UrlCredentials)> {
    let (clean, credentials) = split_url(text, "adresse").ok()?;
    credentials.map(|c| (clean, c))
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
    /// Échecs de vérification consécutifs, par caméra (voir [`CameraBook::record_probe`]).
    failures: BTreeMap<u64, u32>,
}

/// Échecs consécutifs avant qu'une caméra jusque-là joignable soit déclarée injoignable : un
/// `DESCRIBE` raté (réseau Wi-Fi capricieux, caméra qui n'accepte que 2 ou 3 sessions) ne doit pas
/// faire clignoter l'état chez tous les clients.
pub const FAILURES_BEFORE_UNREACHABLE: u32 = 2;

/// Ce que l'enregistrement d'une vérification demande ensuite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    /// Ce que voient les clients a changé : diffuser.
    pub changed: bool,
    /// Premier échec d'une caméra joignable : revérifier tout de suite.
    pub recheck: bool,
}

/// Même état pour un client ? Pour une caméra injoignable, c'est le **motif** qui compte, pas la
/// phrase (`detail`), qui peut varier d'un essai à l'autre sans que rien n'ait changé.
fn same_status(a: &CameraStatus, b: &CameraStatus) -> bool {
    match (a, b) {
        (CameraStatus::Online, CameraStatus::Online)
        | (CameraStatus::Unknown, CameraStatus::Unknown) => true,
        (
            CameraStatus::Unreachable { problem: x, .. },
            CameraStatus::Unreachable { problem: y, .. },
        ) => x == y,
        _ => false,
    }
}

impl CameraBook {
    pub fn new(cameras: Vec<StoredCamera>) -> Self {
        let health = cameras.iter().map(|c| (c.id, Health::unknown())).collect();
        Self {
            cameras,
            health,
            failures: BTreeMap::new(),
        }
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

    /// Un identifiant libre : ni une caméra, ni un identifiant de `taken` (appareils connus : des
    /// appareils pontés tirés avant les caméras peuvent se trouver dans cette plage).
    fn fresh_id(&self, taken: &HashSet<u64>) -> u64 {
        let mut rng = rand::thread_rng();
        self.fresh_id_with(taken, || rng.gen_range(CAMERA_ID_MIN..=CAMERA_ID_MAX))
    }

    /// [`Self::fresh_id`] avec la source de tirages en paramètre (les tests la truquent).
    pub(super) fn fresh_id_with(&self, taken: &HashSet<u64>, mut draw: impl FnMut() -> u64) -> u64 {
        loop {
            let id = draw();
            // `u64::MAX` est l'identifiant « tous » de Matter : on l'évite par prudence.
            if is_camera_id(id) && id != u64::MAX && self.get(id).is_none() && !taken.contains(&id)
            {
                return id;
            }
        }
    }

    pub fn add(&mut self, draft: &CameraDraft) -> Result<u64, CameraError> {
        self.add_avoiding(draft, &HashSet::new())
    }

    /// Ajoute une caméra, sous un identifiant qui n'est pas dans `taken`.
    pub fn add_avoiding(
        &mut self,
        draft: &CameraDraft,
        taken: &HashSet<u64>,
    ) -> Result<u64, CameraError> {
        if self.cameras.len() >= MAX_CAMERAS {
            return refuse(format!("{MAX_CAMERAS} caméras au plus"));
        }
        let id = self.fresh_id(taken);
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
            self.failures.remove(&id);
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
        self.failures.remove(&id);
        if self.cameras.len() == before {
            return refuse("cette caméra n'existe plus");
        }
        Ok(())
    }

    /// L'état de joignabilité d'une caméra.
    pub fn status(&self, id: u64) -> Option<&CameraStatus> {
        self.health.get(&id).map(|h| &h.status)
    }

    /// Retient l'état d'une caméra. `true` si ce que voient les clients a changé : l'état (le motif
    /// d'une injoignabilité, pas sa phrase) ou le flux. Sinon, l'ancien état est gardé tel quel.
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
        let old = self.health.get(&id);
        let stream = stream.or_else(|| old.and_then(|h| h.stream.clone()));
        let changed = match old {
            Some(old) => !same_status(&old.status, &status) || old.stream != stream,
            None => true,
        };
        if changed {
            self.health.insert(id, Health { status, stream });
        }
        changed
    }

    /// Enregistre le résultat d'une vérification. Une caméra joignable ne devient injoignable
    /// qu'après [`FAILURES_BEFORE_UNREACHABLE`] échecs d'affilée ; le premier demande une
    /// revérification immédiate (`recheck`). Une caméra pas encore vue joignable est déclarée
    /// injoignable dès le premier échec (rien à ménager, et l'utilisateur attend la réponse).
    pub fn record_probe(
        &mut self,
        id: u64,
        outcome: Result<StreamInfo, (CameraProblem, String)>,
    ) -> Recorded {
        match outcome {
            Ok(info) => {
                self.failures.remove(&id);
                Recorded {
                    changed: self.set_health(id, CameraStatus::Online, Some(info)),
                    recheck: false,
                }
            }
            Err((problem, detail)) => {
                let n = self.failures.entry(id).or_insert(0);
                *n += 1;
                let was_online = self
                    .health
                    .get(&id)
                    .is_some_and(|h| h.status == CameraStatus::Online);
                if was_online && *n < FAILURES_BEFORE_UNREACHABLE {
                    return Recorded {
                        changed: false,
                        recheck: true,
                    };
                }
                let status = CameraStatus::Unreachable {
                    problem,
                    detail: Some(detail),
                };
                Recorded {
                    changed: self.set_health(id, status, None),
                    recheck: false,
                }
            }
        }
    }
}
