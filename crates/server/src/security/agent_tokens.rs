//! Les jetons d'agent : l'accès d'un agent d'IA (Claude Code, Cursor…) au serveur MCP intégré
//! (`POST /mcp`, voir `net/mcp.rs` et `docs/agents-ia.md`). Chaque jeton appartient à une personne,
//! porte un libellé (« Claude Code sur le portable »), des droits cochés (`scopes`) et une durée.
//!
//! Comme une session ou un jeton de raccourci, un jeton est tiré au hasard (256 bits, préfixe
//! lisible `ostal_agent_`) et **seul son SHA-256 est enregistré** : une fuite du fichier ne donne
//! aucun jeton utilisable. Le jeton en clair n'existe qu'à sa création, il n'entre dans aucun
//! journal. Il ne vaut que sur `/mcp` : aucune route `/api/…` ne le reconnaît.
//!
//! Tout vit dans `serveur/agents.json` (0600, jamais exporté, à part de `jetons.json` dont le type
//! `Kind` ne tolère pas une variante inconnue), avec l'**interrupteur général** `enabled`, éteint
//! tant que le fichier ou le champ manque.
//!
//! **Ce fichier s'écrit par les fonctions libres de ce module, et seulement par elles** : la ligne
//! de commande (`ostal-server agent add|revoke|enable|disable`, jetons en lecture seule), les routes
//! de session de l'app web (`net/agents.rs` : « Mes agents », avec les droits cochés) et la fenêtre
//! d'Ostal Server (Réglages › Agents IA : interrupteur, « Tout couper », listes). **Chaque écriture
//! relit le fichier sur le disque** (jamais le cache de l'`AgentStore`), sous un verrou : un mutex
//! du processus et un verrou de fichier (`agents.lock`) pour la ligne de commande lancée pendant
//! que le serveur tourne. Le serveur relit le fichier dès qu'il change (date, taille, inode), donc
//! une révocation prend effet à la requête suivante, et aucune liste gardée en mémoire ne peut
//! ressusciter un jeton révoqué. Un fichier illisible vaut « interrupteur éteint, aucun jeton »
//! (jamais l'inverse), et n'est jamais écrasé.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{now, write_secret};

/// Le fichier des agents et de l'interrupteur.
pub const FILE: &str = "agents.json";
/// Début lisible de tout jeton d'agent.
pub const PREFIX: &str = "ostal_agent_";
/// Jetons d'agent au plus par personne.
pub const MAX_PER_USER: usize = 16;
/// Durée de validité par défaut, en jours.
pub const DEFAULT_DAYS: u32 = 90;
/// Durée de validité maximale, en jours.
pub const MAX_DAYS: u32 = 365;
/// Lectures au plus par minute et par jeton (§8 des specs).
pub const READS_PER_MINUTE: usize = 60;
/// Commandes d'appareil au plus par minute et par jeton (§8 des specs), en plus des 60 appels.
pub const COMMANDS_PER_MINUTE: usize = 20;
/// Appareils permis au plus par jeton. Une liste plus longue n'en permet **aucun** : on ne coupe
/// pas une liste au hasard, on la fait corriger.
pub const MAX_DEVICES_PER_AGENT: usize = 20;
const WINDOW: Duration = Duration::from_secs(60);

/// Ce qu'un jeton a le droit de faire. Un mot inconnu (fichier d'une version plus récente) ne
/// donne **aucun** droit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Lire les appareils et l'état de la maison.
    Read,
    /// Lire la présence et le mode, et ce qui en dépend (le comportement des caméras selon le mode) :
    /// « Vacances » dit que la maison est vide. Éteint par défaut ; jamais donné par la ligne de
    /// commande. Le droit `Mode` le suppose.
    Occupation,
    /// Commander des prises, des lampes et des volets (étapes E2 et E3), parmi les appareils permis du jeton
    /// (`devices`) et hors de la liste « jamais par un agent » (`never`). Jamais donné par la ligne
    /// de commande : on l'écrit à la main dans `agents.json`.
    Control,
    /// Changer le mode (`ostal_set_mode`, étape E3) : le mode demandé passe par une confirmation quand
    /// son effet l'exige. Suppose « occupation » ; jamais donné par la ligne de commande.
    Mode,
    #[serde(other)]
    Unknown,
}

impl Scope {
    pub fn word(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Occupation => "occupation",
            Scope::Control => "control",
            Scope::Mode => "mode",
            Scope::Unknown => "inconnu",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    user_id: String,
    label: String,
    scopes: Vec<Scope>,
    /// Les appareils que ce jeton peut commander (identifiants décimaux, 20 au plus), écrits à la
    /// main. Vide ou absent : aucun.
    #[serde(default)]
    devices: Vec<String>,
    /// SHA-256 du jeton, hexadécimal.
    hash: String,
    /// Secondes Unix.
    created: f64,
    /// Secondes Unix.
    expires: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    /// L'interrupteur général : éteint par défaut.
    #[serde(default)]
    enabled: bool,
    /// Compte les fois où l'interrupteur a été éteint (ligne de commande, « Tout couper ») : le
    /// serveur en cours y voit qu'il doit oublier les demandes à valider, même si l'interrupteur
    /// a été rallumé aussitôt.
    #[serde(default)]
    off_epoch: u64,
    /// La liste « jamais par un agent » : des appareils (identifiants décimaux) qu'aucun agent ne
    /// commande, quel que soit son jeton (congélateur, box, prise du Mac serveur). Écrite à la
    /// main. Une entrée illisible bloque **toute** commande.
    #[serde(default)]
    never: Vec<String>,
    /// La liste « sensible » : des appareils (identifiants décimaux) dont toute commande par un
    /// agent demande l'accord d'une personne (porte de garage, baie vitrée). Écrite à la main. Une
    /// entrée illisible rend **tout** appareil sensible (jamais l'inverse).
    #[serde(default)]
    sensitive: Vec<String>,
    #[serde(default)]
    agents: Vec<Entry>,
}

/// Un agent, sans son empreinte : ce que l'on peut montrer.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInfo {
    pub id: String,
    pub user_id: String,
    pub label: String,
    pub scopes: Vec<Scope>,
    /// Appareils permis (identifiants décimaux, tels qu'écrits dans `agents.json`).
    pub devices: Vec<String>,
    /// Secondes Unix.
    pub created: f64,
    /// Secondes Unix.
    pub expires: f64,
}

impl AgentInfo {
    /// Ce jeton peut-il commander cet appareil ? Seulement s'il a le droit « commandes », que
    /// l'appareil est dans sa liste et que la liste tient en 20 entrées lisibles : une liste trop
    /// longue ou avec une entrée illisible ne permet **rien** (jamais le contraire).
    pub fn may_command(&self, node_id: u64) -> bool {
        self.has(Scope::Control)
            && self.devices.len() <= MAX_DEVICES_PER_AGENT
            && self
                .devices
                .iter()
                .all(|d| protocol::wire::parse_id(d).is_ok())
            && self
                .devices
                .iter()
                .any(|d| protocol::wire::parse_id(d).ok() == Some(node_id))
    }

    /// Le jeton a-t-il ce droit ? Le droit « mode » **suppose** « occupation » : pour changer le
    /// mode, l'agent doit le connaître (et la réponse le dit).
    pub fn has(&self, scope: Scope) -> bool {
        scope != Scope::Unknown
            && (self.scopes.contains(&scope)
                || (scope == Scope::Occupation && self.scopes.contains(&Scope::Mode)))
    }

    pub fn is_expired(&self) -> bool {
        self.expires <= now()
    }
}

impl From<&Entry> for AgentInfo {
    fn from(e: &Entry) -> Self {
        AgentInfo {
            id: e.id.clone(),
            user_id: e.user_id.clone(),
            label: e.label.clone(),
            scopes: e.scopes.clone(),
            devices: e.devices.clone(),
            created: e.created,
            expires: e.expires,
        }
    }
}

fn hash_of(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn new_id() -> String {
    let mut raw = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut raw);
    hex::encode(raw)
}

fn draw_token() -> String {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    format!("{PREFIX}{}", hex::encode(raw))
}

fn path_of(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

// ---------------------------------------------------------------------------
// Écriture : la ligne de commande seulement
// ---------------------------------------------------------------------------

/// Lit le fichier pour le modifier. Absent : un fichier vide. **Illisible : une erreur** (on
/// n'écrase jamais ce qu'on ne comprend pas).
fn read_for_edit(dir: &Path) -> Result<File> {
    let path = path_of(dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} est illisible, rien n'est modifié", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(e).with_context(|| format!("lecture de {}", path.display())),
    }
}

fn write(dir: &Path, file: &mut File) -> Result<()> {
    file.version = 1;
    write_secret(
        &path_of(dir),
        serde_json::to_string_pretty(file)?.as_bytes(),
    )
}

/// Un seul écrivain à la fois dans ce processus (`write_secret` passe toujours par le même
/// fichier temporaire : deux écritures en même temps s'écraseraient).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Le fichier de verrou : il ne contient rien, il ne sert qu'à `flock`.
const LOCK_FILE: &str = "agents.lock";

/// Lit, modifie et écrit **sous verrou** : mutex du processus, puis verrou du fichier
/// `agents.lock` (la ligne de commande est un autre processus). Si le fichier de verrou ne peut pas
/// être ouvert (dossier absent), l'écriture échouera de toute façon plus loin.
fn edit<T>(dir: &Path, change: impl FnOnce(&mut File) -> Result<T>) -> Result<T> {
    let _in_process = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let lock = {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(dir.join(LOCK_FILE)).ok()
    };
    if let Some(lock) = &lock {
        lock.lock()
            .with_context(|| format!("verrou de {}", dir.join(LOCK_FILE).display()))?;
    }
    let mut file = read_for_edit(dir)?;
    let before = serde_json::to_string(&file)?;
    let out = change(&mut file)?;
    // Rien de changé : le fichier n'est pas réécrit (ni créé), son horodatage ne bouge pas.
    if serde_json::to_string(&file)? != before {
        write(dir, &mut file)?;
    }
    // Le verrou tombe avec `lock` (fermeture du fichier).
    drop(lock);
    Ok(out)
}

/// Les droits d'un nouveau jeton, dans l'ordre fixe lecture, occupation, commandes, mode. « Lecture »
/// y est toujours ; « mode » suppose « occupation » ; un mot inconnu est refusé.
pub fn normalize_scopes(wanted: &[Scope]) -> Result<Vec<Scope>> {
    if wanted.contains(&Scope::Unknown) {
        bail!("droit inconnu");
    }
    let has = |s: Scope| wanted.contains(&s);
    let mut out = vec![Scope::Read];
    if has(Scope::Occupation) || has(Scope::Mode) {
        out.push(Scope::Occupation);
    }
    if has(Scope::Control) {
        out.push(Scope::Control);
    }
    if has(Scope::Mode) {
        out.push(Scope::Mode);
    }
    Ok(out)
}

/// Cet identifiant est-il dans une liste « jamais » ? Une entrée illisible rend **tous** les
/// appareils « jamais » (même règle que [`AgentStore::never_commanded`]).
fn listed_in(list: &[String], node_id: u64) -> bool {
    let mut broken = false;
    let mut listed = false;
    for text in list {
        match protocol::wire::parse_id(text) {
            Ok(id) => listed |= id == node_id,
            Err(_) => broken = true,
        }
    }
    broken || listed
}

/// Crée un jeton d'agent **en lecture seule** pour cette personne (`user_id` : identifiant
/// interne). Rend le jeton en clair, **une seule fois**. Les autres droits ne se donnent pas en
/// ligne de commande : sinon quiconque a un terminal sur le Mac les aurait.
pub fn add(dir: &Path, user_id: &str, label: &str, days: u32) -> Result<(AgentInfo, String)> {
    add_with(dir, user_id, label, days, &[Scope::Read], &[])
}

/// Crée un jeton d'agent avec les droits demandés (app web « Mes agents »). **L'appelant a déjà
/// vérifié** que la personne a elle-même chacun de ces droits et que les appareils existent ; ici
/// on garde ce qui tient au fichier : durée, droits normalisés, appareils en identifiants décimaux
/// canoniques (20 au plus, seulement avec le droit « commandes », aucun dans la liste « jamais »),
/// 16 agents par personne, un libellé par personne.
pub fn add_with(
    dir: &Path,
    user_id: &str,
    label: &str,
    days: u32,
    scopes: &[Scope],
    devices: &[u64],
) -> Result<(AgentInfo, String)> {
    if days == 0 || days > MAX_DAYS {
        bail!("la durée va de 1 à {MAX_DAYS} jours");
    }
    let scopes = normalize_scopes(scopes)?;
    let mut devices: Vec<String> = devices.iter().map(u64::to_string).collect();
    devices.sort_by_key(|d| d.parse::<u64>().unwrap_or(0));
    devices.dedup();
    if !scopes.contains(&Scope::Control) && !devices.is_empty() {
        bail!("des appareils permis n'ont de sens qu'avec le droit « commandes »");
    }
    if devices.len() > MAX_DEVICES_PER_AGENT {
        bail!("{MAX_DEVICES_PER_AGENT} appareils permis au plus par agent");
    }
    edit(dir, |file| {
        if file.agents.iter().filter(|e| e.user_id == user_id).count() >= MAX_PER_USER {
            bail!("{MAX_PER_USER} agents au plus par personne : révoquez-en un d'abord");
        }
        let label = super::user_tokens::clean_label(label, "Agent");
        if file
            .agents
            .iter()
            .any(|e| e.user_id == user_id && e.label.to_lowercase() == label.to_lowercase())
        {
            bail!("cette personne a déjà un agent « {label} » : choisissez un autre libellé");
        }
        if devices.iter().any(|d| {
            d.parse::<u64>()
                .map_or(true, |id| listed_in(&file.never, id))
        }) {
            bail!("un de ces appareils est dans la liste « jamais par un agent »");
        }
        let token = draw_token();
        let created = now();
        let entry = Entry {
            id: new_id(),
            user_id: user_id.to_string(),
            label,
            scopes,
            devices,
            hash: hex::encode(hash_of(&token)),
            created,
            expires: created + f64::from(days) * 86_400.0,
        };
        let info = AgentInfo::from(&entry);
        file.agents.push(entry);
        Ok((info, token))
    })
}

/// Révoque un agent d'après son identifiant ou son libellé (insensible à la casse ; un libellé
/// porté par plusieurs agents est refusé : il faut alors l'identifiant). **Ligne de commande
/// seulement** : le libellé vaut pour toutes les personnes ; une route de session passe par
/// [`revoke_owned`].
pub fn revoke(dir: &Path, selector: &str) -> Result<AgentInfo> {
    edit(dir, |file| {
        let lower = selector.to_lowercase();
        let by_id: Vec<usize> = (0..file.agents.len())
            .filter(|&i| file.agents[i].id == selector)
            .collect();
        let hits = if by_id.is_empty() {
            (0..file.agents.len())
                .filter(|&i| file.agents[i].label.to_lowercase() == lower)
                .collect()
        } else {
            by_id
        };
        match hits.as_slice() {
            [] => bail!("aucun agent « {selector} »"),
            [i] => Ok(AgentInfo::from(&file.agents.remove(*i))),
            _ => {
                bail!("plusieurs agents s'appellent « {selector} » : donnez l'identifiant (agent list)")
            }
        }
    })
}

/// Révoque l'agent d'identifiant `agent_id` **s'il appartient à cette personne**. `None` : il n'y
/// en a pas (inconnu ou à quelqu'un d'autre : même réponse, une personne ne sonde pas les agents
/// des autres).
pub fn revoke_owned(dir: &Path, user_id: &str, agent_id: &str) -> Result<Option<AgentInfo>> {
    edit(dir, |file| {
        Ok(file
            .agents
            .iter()
            .position(|e| e.id == agent_id && e.user_id == user_id)
            .map(|i| AgentInfo::from(&file.agents.remove(i))))
    })
}

/// « Tout couper » : l'interrupteur s'éteint **et** tous les jetons sont révoqués, en une seule
/// écriture. Les listes « jamais » et « sensible » restent. Rend les agents retirés.
pub fn cut_all(dir: &Path) -> Result<Vec<AgentInfo>> {
    edit(dir, |file| {
        if file.enabled {
            file.off_epoch += 1;
        }
        file.enabled = false;
        Ok(file.agents.drain(..).map(|e| AgentInfo::from(&e)).collect())
    })
}

/// Révoque un agent par son identifiant, quelle que soit la personne (la fenêtre d'Ostal Server,
/// qui voit tous les agents). `None` : il n'existe plus.
pub fn revoke_id(dir: &Path, agent_id: &str) -> Result<Option<AgentInfo>> {
    edit(dir, |file| {
        Ok(file
            .agents
            .iter()
            .position(|e| e.id == agent_id)
            .map(|i| AgentInfo::from(&file.agents.remove(i))))
    })
}

/// Les listes « jamais par un agent » et « sensible » : au plus ce nombre d'appareils chacune.
pub const MAX_LISTED: usize = 200;

/// Remplace les deux listes. **Seulement des identifiants canoniques** (jamais d'entrée illisible :
/// une seule bloquerait toute commande, ou rendrait tout appareil sensible), triés, sans doublon.
/// Un appareil « jamais » qui figurerait aussi dans la liste « sensible » n'y reste pas (« jamais »
/// l'emporte). Rend `true` si le fichier a changé.
pub fn set_lists(dir: &Path, never: &[u64], sensitive: &[u64]) -> Result<bool> {
    if never.len() > MAX_LISTED || sensitive.len() > MAX_LISTED {
        bail!("{MAX_LISTED} appareils au plus par liste");
    }
    let canon = |ids: &[u64]| {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let never = canon(never);
    let sensitive: Vec<u64> = canon(sensitive)
        .into_iter()
        .filter(|id| !never.contains(id))
        .collect();
    let texts = |ids: &[u64]| ids.iter().map(u64::to_string).collect::<Vec<_>>();
    let (never, sensitive) = (texts(&never), texts(&sensitive));
    edit(dir, |file| {
        let changed = file.never != never || file.sensitive != sensitive;
        file.never = never;
        file.sensitive = sensitive;
        Ok(changed)
    })
}

/// Les deux listes telles qu'écrites dans le fichier (entrées illisibles comprises : on les
/// montre, on ne les cache pas).
pub fn lists(dir: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let file = read_for_edit(dir)?;
    Ok((file.never, file.sensitive))
}

/// L'interrupteur général et les agents, le plus récent d'abord. Jamais un jeton ni une empreinte.
pub fn list(dir: &Path) -> Result<(bool, Vec<AgentInfo>)> {
    let file = read_for_edit(dir)?;
    let mut agents: Vec<AgentInfo> = file.agents.iter().map(AgentInfo::from).collect();
    agents.sort_by(|a, b| b.created.total_cmp(&a.created));
    Ok((file.enabled, agents))
}

/// Allume ou éteint l'interrupteur général. Rend `true` si l'état a changé.
pub fn set_enabled(dir: &Path, enabled: bool) -> Result<bool> {
    edit(dir, |file| {
        let changed = file.enabled != enabled;
        if changed && !enabled {
            file.off_epoch += 1;
        }
        file.enabled = enabled;
        Ok(changed)
    })
}

// ---------------------------------------------------------------------------
// Lecture : le serveur
// ---------------------------------------------------------------------------

/// (date de modification, taille, inode) : change à chaque écriture (chacune renomme un fichier
/// neuf, donc l'inode change même si la date et la taille restent).
type Stamp = (SystemTime, u64, u64);

fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len(), meta.ino()))
}

struct Cache {
    file: File,
    stamp: Option<Stamp>,
}

fn load(dir: &Path) -> Cache {
    let path = path_of(dir);
    // La date avant la lecture : un changement entre les deux sera revu à la requête suivante.
    let stamp = stamp_of(&path);
    let file = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::error!("agents.json illisible, accès des agents coupé ({e})");
            File::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => {
            log::error!("agents.json non lu, accès des agents coupé ({e})");
            File::default()
        }
    };
    Cache { file, stamp }
}

/// Les agents vus par le serveur : `agents.json` relu dès qu'il change, jamais écrit, et le
/// limiteur de lectures par jeton (en mémoire, remis à zéro au redémarrage).
pub struct AgentStore {
    dir: PathBuf,
    cache: Mutex<Cache>,
    reads: Mutex<HashMap<String, VecDeque<Instant>>>,
    commands: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Dernier appel et appels du jour, par jeton : **en mémoire**, remis à zéro au redémarrage.
    usage: Mutex<HashMap<String, Usage>>,
    /// Dernier `off_epoch` pris en compte par [`AgentStore::switched_off_since_last_check`].
    seen_off_epoch: std::sync::atomic::AtomicU64,
}

/// L'usage d'un agent depuis le démarrage du serveur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    /// Secondes Unix du dernier appel.
    pub last: f64,
    /// Appels depuis minuit (heure locale), refusés par le débit compris.
    pub today: u32,
    day: chrono::NaiveDate,
}

impl AgentStore {
    pub fn open(dir: &Path) -> Self {
        let cache = load(dir);
        let epoch = cache.file.off_epoch;
        Self {
            dir: dir.to_path_buf(),
            cache: Mutex::new(cache),
            seen_off_epoch: std::sync::atomic::AtomicU64::new(epoch),
            reads: Mutex::new(HashMap::new()),
            commands: Mutex::new(HashMap::new()),
            usage: Mutex::new(HashMap::new()),
        }
    }

    fn with_file<T>(&self, f: impl FnOnce(&File) -> T) -> T {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if stamp_of(&path_of(&self.dir)) != cache.stamp {
            *cache = load(&self.dir);
        }
        f(&cache.file)
    }

    /// L'interrupteur général : tant qu'il est éteint, `/mcp` n'existe pas.
    pub fn enabled(&self) -> bool {
        self.with_file(|file| file.enabled)
    }

    /// `true` une fois, si l'interrupteur a été éteint depuis le dernier appel (même rallumé
    /// depuis) : le serveur vide alors les demandes à valider.
    pub fn switched_off_since_last_check(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.with_file(|file| file.off_epoch);
        self.seen_off_epoch.swap(now, SeqCst) != now
    }

    /// Les agents de cette personne, le plus récent d'abord (expirés compris : on les montre pour
    /// qu'elle les retire). Jamais un jeton ni une empreinte.
    pub fn agents_of(&self, user_id: &str) -> Vec<AgentInfo> {
        let mut agents = self.all_agents();
        agents.retain(|a| a.user_id == user_id);
        agents
    }

    /// Tous les agents, le plus récent d'abord (la fenêtre d'Ostal Server).
    pub fn all_agents(&self) -> Vec<AgentInfo> {
        let mut agents =
            self.with_file(|file| file.agents.iter().map(AgentInfo::from).collect::<Vec<_>>());
        agents.sort_by(|a, b| b.created.total_cmp(&a.created));
        agents
    }

    /// Les listes « jamais » et « sensible » **lisibles** (les entrées illisibles ne sont pas
    /// rendues ; leur effet, lui, reste : voir `never_commanded` et `is_sensitive`).
    pub fn lists(&self) -> (Vec<u64>, Vec<u64>) {
        self.with_file(|file| {
            let ids = |list: &[String]| {
                list.iter()
                    .filter_map(|t| protocol::wire::parse_id(t).ok())
                    .collect::<Vec<u64>>()
            };
            (ids(&file.never), ids(&file.sensitive))
        })
    }

    /// Une liste a-t-elle une entrée illisible (écrite à la main) ? Elle bloque alors tout ou rend
    /// tout sensible : la fenêtre le dit.
    pub fn lists_are_broken(&self) -> bool {
        self.with_file(|file| {
            file.never
                .iter()
                .chain(file.sensitive.iter())
                .any(|t| protocol::wire::parse_id(t).is_err())
        })
    }

    /// Le dernier appel et les appels du jour de cet agent depuis le démarrage.
    pub fn usage(&self, agent_id: &str) -> Option<Usage> {
        let map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        map.get(agent_id).copied().map(|mut u| {
            if u.day != chrono::Local::now().date_naive() {
                u.today = 0;
            }
            u
        })
    }

    fn note_use(&self, agent_id: &str) {
        let today = chrono::Local::now().date_naive();
        let mut map = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 256 && !map.contains_key(agent_id) {
            map.clear();
        }
        let u = map.entry(agent_id.to_string()).or_insert(Usage {
            last: 0.0,
            today: 0,
            day: today,
        });
        if u.day != today {
            u.day = today;
            u.today = 0;
        }
        u.today = u.today.saturating_add(1);
        u.last = now();
    }

    /// À qui est ce jeton ? `None` s'il est inconnu, révoqué ou expiré. Comparaison en temps
    /// constant, sur tous les jetons (pas de sortie anticipée qui trahirait un préfixe).
    pub fn verify(&self, token: &str) -> Option<AgentInfo> {
        if !token.starts_with(PREFIX) {
            return None;
        }
        let wanted = hash_of(token);
        let found = self.with_file(|file| {
            let mut found: Option<AgentInfo> = None;
            for entry in &file.agents {
                let same = match hex::decode(&entry.hash) {
                    Ok(bytes) if bytes.len() == 32 => bool::from(bytes.ct_eq(wanted.as_slice())),
                    _ => false,
                };
                if same {
                    found = Some(AgentInfo::from(entry));
                }
            }
            found
        })?;
        (!found.is_expired()).then_some(found)
    }

    /// Un appareil est-il dans la liste « jamais par un agent » ? **Oui aussi** quand la liste a une
    /// entrée illisible (un identifiant mal recopié ne doit jamais laisser un congélateur sans
    /// protection) : alors aucun appareil ne se commande.
    pub fn never_commanded(&self, node_id: u64) -> bool {
        self.with_file(|file| {
            let mut broken = false;
            let mut listed = false;
            for text in &file.never {
                match protocol::wire::parse_id(text) {
                    Ok(id) => listed |= id == node_id,
                    Err(_) => broken = true,
                }
            }
            if broken {
                log::error!("agents.json : la liste « never » a une entrée illisible, aucune commande d'agent n'est permise");
            }
            broken || listed
        })
    }

    /// Un appareil est-il « sensible » : toute commande d'un agent demande l'accord d'une personne ?
    /// **Oui pour tout appareil** quand la liste a une entrée illisible (on durcit, on n'assouplit
    /// jamais).
    pub fn is_sensitive(&self, node_id: u64) -> bool {
        self.with_file(|file| {
            let mut broken = false;
            let mut listed = false;
            for text in &file.sensitive {
                match protocol::wire::parse_id(text) {
                    Ok(id) => listed |= id == node_id,
                    Err(_) => broken = true,
                }
            }
            if broken {
                log::error!("agents.json : la liste « sensitive » a une entrée illisible, toute commande d'agent demande un accord");
            }
            broken || listed
        })
    }

    /// Cet agent existe-t-il encore (ni révoqué ni expiré) ? Pour ne plus proposer à une personne la
    /// demande d'un agent qu'on lui a retiré.
    pub fn live_agent(&self, agent_id: &str) -> Option<AgentInfo> {
        let found = self.with_file(|file| {
            file.agents
                .iter()
                .find(|e| e.id == agent_id)
                .map(AgentInfo::from)
        })?;
        (!found.is_expired()).then_some(found)
    }

    /// Une lecture de plus pour ce jeton. `Err(secondes)` : la minute est pleine, réessayer après
    /// ce délai.
    pub fn check_read(&self, agent_id: &str) -> Result<(), u64> {
        self.note_use(agent_id);
        self.check_read_at(agent_id, Instant::now())
    }

    fn check_read_at(&self, agent_id: &str, at: Instant) -> Result<(), u64> {
        check_window(&self.reads, agent_id, at, READS_PER_MINUTE)
    }

    /// Une commande d'appareil de plus pour ce jeton (20 par minute, en plus des appels
    /// comptés par `check_read`). Même retour.
    pub fn check_command(&self, agent_id: &str) -> Result<(), u64> {
        self.check_command_at(agent_id, Instant::now())
    }

    fn check_command_at(&self, agent_id: &str, at: Instant) -> Result<(), u64> {
        check_window(&self.commands, agent_id, at, COMMANDS_PER_MINUTE)
    }

    /// `n` commandes de plus d'un coup (une pièce entière : un appareil, une commande), **tout ou
    /// rien** : si la minute ne les contient pas toutes, aucune n'est comptée.
    pub fn check_commands(&self, agent_id: &str, n: usize) -> Result<(), u64> {
        check_window_n(
            &self.commands,
            agent_id,
            Instant::now(),
            COMMANDS_PER_MINUTE,
            n,
        )
    }

    /// Rend `n` commandes comptées par [`AgentStore::check_commands`] et finalement pas envoyées
    /// (la demande attendait une personne, ou l'autorisation était refusée).
    pub fn refund_commands(&self, agent_id: &str, n: usize) {
        let mut map = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(queue) = map.get_mut(agent_id) {
            for _ in 0..n {
                queue.pop_back();
            }
        }
    }
}

/// Fenêtre glissante d'une minute, par jeton.
fn check_window(
    map: &Mutex<HashMap<String, VecDeque<Instant>>>,
    agent_id: &str,
    at: Instant,
    limit: usize,
) -> Result<(), u64> {
    check_window_n(map, agent_id, at, limit, 1)
}

fn check_window_n(
    map: &Mutex<HashMap<String, VecDeque<Instant>>>,
    agent_id: &str,
    at: Instant,
    limit: usize,
    n: usize,
) -> Result<(), u64> {
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() > 256 {
        map.retain(|_, q| q.back().is_some_and(|t| at.duration_since(*t) < WINDOW));
    }
    let queue = map.entry(agent_id.to_string()).or_default();
    while queue
        .front()
        .is_some_and(|t| at.duration_since(*t) >= WINDOW)
    {
        queue.pop_front();
    }
    if queue.len() + n > limit {
        let wait = queue
            .front()
            .map_or(WINDOW, |t| WINDOW.saturating_sub(at.duration_since(*t)));
        return Err(wait.as_secs().max(1));
    }
    for _ in 0..n {
        queue.push_back(at);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ostal-agents-{tag}-{}-{}",
            std::process::id(),
            new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nothing_is_on_by_default() {
        let dir = temp("default");
        let store = AgentStore::open(&dir);
        assert!(!store.enabled());
        assert!(store.verify("ostal_agent_".to_string().as_str()).is_none());
    }

    #[test]
    fn a_new_token_is_read_only_and_only_its_hash_is_stored() {
        let dir = temp("add");
        let (info, token) = add(&dir, "u1", "Claude Code", 30).unwrap();
        assert!(token.starts_with(PREFIX));
        assert_eq!(token.len(), PREFIX.len() + 64);
        assert_eq!(info.scopes, vec![Scope::Read]);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(
            !text.contains(&token),
            "le jeton ne s'écrit jamais en clair"
        );
        assert!(text.contains(&hex::encode(hash_of(&token))));
        let store = AgentStore::open(&dir);
        let found = store.verify(&token).expect("jeton connu");
        assert_eq!(found.id, info.id);
        assert!(found.has(Scope::Read) && !found.has(Scope::Control));
        assert!(store.verify(&format!("{token}0")).is_none());
        assert!(store.verify("ostal_agent_zz").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("mode");
        add(&dir, "u1", "A", 30).unwrap();
        let mode = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_revocation_made_elsewhere_is_seen_at_once_and_never_undone() {
        let dir = temp("revoke");
        let (a, ta) = add(&dir, "u1", "A", 30).unwrap();
        let (_b, tb) = add(&dir, "u1", "B", 30).unwrap();
        let store = AgentStore::open(&dir);
        assert!(store.verify(&ta).is_some() && store.verify(&tb).is_some());
        revoke(&dir, &a.id).unwrap();
        assert!(store.verify(&ta).is_none());
        assert!(store.verify(&tb).is_some());
        // Un agent ajouté ensuite est vu aussi, et le premier ne revient pas.
        let (_c, tc) = add(&dir, "u1", "C", 30).unwrap();
        assert!(store.verify(&tc).is_some());
        assert!(store.verify(&ta).is_none());
    }

    #[test]
    fn an_expired_token_is_refused() {
        let dir = temp("expired");
        let (_, token) = add(&dir, "u1", "A", 30).unwrap();
        let mut file = read_for_edit(&dir).unwrap();
        file.agents[0].expires = now() - 1.0;
        write(&dir, &mut file).unwrap();
        assert!(AgentStore::open(&dir).verify(&token).is_none());
    }

    #[test]
    fn the_switch_follows_the_file_and_an_unreadable_file_cuts_everything() {
        let dir = temp("switch");
        let (_, token) = add(&dir, "u1", "A", 30).unwrap();
        let store = AgentStore::open(&dir);
        assert!(!store.enabled());
        assert!(set_enabled(&dir, true).unwrap());
        assert!(!set_enabled(&dir, true).unwrap(), "déjà allumé");
        assert!(store.enabled());
        assert!(store.verify(&token).is_some());
        std::fs::write(dir.join(FILE), "{ pas du JSON").unwrap();
        assert!(!store.enabled());
        assert!(store.verify(&token).is_none());
        // Et la ligne de commande n'écrase pas ce qu'elle ne comprend pas.
        assert!(add(&dir, "u1", "B", 30).is_err());
        assert!(set_enabled(&dir, true).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE)).unwrap(),
            "{ pas du JSON"
        );
    }

    #[test]
    fn an_unknown_scope_gives_no_right() {
        let dir = temp("scope");
        let (_, token) = add(&dir, "u1", "A", 30).unwrap();
        let mut file = read_for_edit(&dir).unwrap();
        file.agents[0].scopes = vec![Scope::Unknown];
        write(&dir, &mut file).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE))
            .unwrap()
            .replace("\"unknown\"", "\"teleportation\"");
        std::fs::write(dir.join(FILE), text).unwrap();
        let found = AgentStore::open(&dir).verify(&token).unwrap();
        assert!(!found.has(Scope::Read) && !found.has(Scope::Unknown));
    }

    #[test]
    fn labels_are_unique_per_person_and_limited() {
        let dir = temp("labels");
        add(&dir, "u1", "Claude Code", 30).unwrap();
        assert!(add(&dir, "u1", "claude code", 30).is_err());
        assert!(add(&dir, "u2", "claude code", 30).is_ok());
        assert!(add(&dir, "u1", "X", 0).is_err());
        assert!(add(&dir, "u1", "X", MAX_DAYS + 1).is_err());
        for i in 0..MAX_PER_USER - 1 {
            add(&dir, "u1", &format!("agent {i}"), 30).unwrap();
        }
        assert!(add(&dir, "u1", "de trop", 30).is_err());
    }

    #[test]
    fn revoke_by_label_refuses_what_is_ambiguous() {
        let dir = temp("ambiguous");
        add(&dir, "u1", "Portable", 30).unwrap();
        add(&dir, "u2", "Portable", 30).unwrap();
        assert!(revoke(&dir, "portable").is_err());
        assert!(revoke(&dir, "inconnu").is_err());
        let (_, agents) = list(&dir).unwrap();
        assert_eq!(agents.len(), 2);
        revoke(&dir, &agents[0].id).unwrap();
        assert!(revoke(&dir, "portable").is_ok());
    }

    #[test]
    fn reads_are_limited_per_token_over_a_sliding_minute() {
        let dir = temp("limit");
        let store = AgentStore::open(&dir);
        let t0 = Instant::now();
        for _ in 0..READS_PER_MINUTE {
            assert!(store.check_read_at("a", t0).is_ok());
        }
        let wait = store.check_read_at("a", t0).unwrap_err();
        assert!((1..=60).contains(&wait));
        // Un autre jeton n'est pas touché ; une minute plus tard le premier repart.
        assert!(store.check_read_at("b", t0).is_ok());
        assert!(store.check_read_at("a", t0 + WINDOW).is_ok());
    }

    fn hand_edit(dir: &Path, f: impl FnOnce(&mut serde_json::Value)) {
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap();
        f(&mut value);
        std::fs::write(dir.join(FILE), value.to_string()).unwrap();
    }

    #[test]
    fn commanding_needs_the_right_and_a_listed_device_in_a_list_of_twenty_at_most() {
        let dir = temp("devices");
        let (_, token) = add(&dir, "u1", "A", 30).unwrap();
        let store = AgentStore::open(&dir);
        let agent = store.verify(&token).unwrap();
        assert!(agent.devices.is_empty() && !agent.may_command(1));
        hand_edit(&dir, |v| {
            v["agents"][0]["scopes"] = serde_json::json!(["read", "control"]);
            v["agents"][0]["devices"] = serde_json::json!(["5", "12"]);
        });
        let agent = store.verify(&token).unwrap();
        assert!(agent.may_command(5) && agent.may_command(12) && !agent.may_command(6));
        // Les appareils sans le droit : rien.
        hand_edit(&dir, |v| {
            v["agents"][0]["scopes"] = serde_json::json!(["read"])
        });
        assert!(!store.verify(&token).unwrap().may_command(5));
        // Vingt et un : aucun. Vingt : tous.
        let twenty: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
        hand_edit(&dir, |v| {
            v["agents"][0]["scopes"] = serde_json::json!(["read", "control"]);
            v["agents"][0]["devices"] = serde_json::json!(twenty);
        });
        assert!(store.verify(&token).unwrap().may_command(7));
        hand_edit(&dir, |v| {
            let list = v["agents"][0]["devices"].as_array_mut().unwrap();
            list.push(serde_json::json!("21"));
        });
        assert!(!store.verify(&token).unwrap().may_command(7));
        // Une entrée illisible : aucun non plus.
        hand_edit(&dir, |v| {
            v["agents"][0]["devices"] = serde_json::json!(["5", "cinq"]);
        });
        assert!(!store.verify(&token).unwrap().may_command(5));
    }

    #[test]
    fn the_never_list_blocks_and_a_broken_entry_blocks_everything() {
        let dir = temp("never");
        add(&dir, "u1", "A", 30).unwrap();
        let store = AgentStore::open(&dir);
        assert!(
            !store.never_commanded(5),
            "vide : personne n'est protégé en plus"
        );
        hand_edit(&dir, |v| v["never"] = serde_json::json!(["5"]));
        assert!(store.never_commanded(5) && !store.never_commanded(6));
        hand_edit(&dir, |v| v["never"] = serde_json::json!(["5", "0x6"]));
        assert!(store.never_commanded(5) && store.never_commanded(6) && store.never_commanded(7));
        hand_edit(&dir, |v| v["never"] = serde_json::json!(["007"]));
        assert!(
            store.never_commanded(7),
            "un zéro initial n'est pas un identifiant"
        );
    }

    #[test]
    fn the_command_line_keeps_the_lists_written_by_hand() {
        let dir = temp("keep");
        add(&dir, "u1", "A", 30).unwrap();
        hand_edit(&dir, |v| {
            v["never"] = serde_json::json!(["5"]);
            v["agents"][0]["devices"] = serde_json::json!(["9"]);
            v["agents"][0]["scopes"] = serde_json::json!(["read", "control"]);
        });
        add(&dir, "u1", "B", 30).unwrap();
        set_enabled(&dir, true).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap();
        assert_eq!(value["never"], serde_json::json!(["5"]));
        let a = &value["agents"][0];
        assert_eq!(a["devices"], serde_json::json!(["9"]));
        assert_eq!(a["scopes"], serde_json::json!(["read", "control"]));
        assert_eq!(value["agents"][1]["scopes"], serde_json::json!(["read"]));
        assert_eq!(value["agents"][1]["devices"], serde_json::json!([]));
    }

    #[test]
    fn commands_have_their_own_limit_of_twenty_a_minute() {
        let dir = temp("cmd-limit");
        let store = AgentStore::open(&dir);
        let t0 = Instant::now();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(store.check_command_at("a", t0).is_ok());
        }
        assert!((1..=60).contains(&store.check_command_at("a", t0).unwrap_err()));
        assert!(store.check_command_at("b", t0).is_ok());
        assert!(store.check_command_at("a", t0 + WINDOW).is_ok());
        // Les lectures ont leur compteur à part.
        assert!(store.check_read_at("a", t0).is_ok());
    }

    #[test]
    fn the_sensitive_list_is_hand_written_survives_the_command_line_and_fails_safe() {
        let dir = temp("sensitive");
        add(&dir, "u1", "A", 30).unwrap();
        // Rien d'écrit : aucun appareil n'est sensible.
        assert!(!AgentStore::open(&dir).is_sensitive(7));
        let mut file = read_for_edit(&dir).unwrap();
        file.sensitive = vec!["7".into(), "13828100".into()];
        write(&dir, &mut file).unwrap();
        let store = AgentStore::open(&dir);
        assert!(store.is_sensitive(7) && store.is_sensitive(13_828_100));
        assert!(!store.is_sensitive(8));
        // La ligne de commande réécrit le fichier sans perdre la liste.
        add(&dir, "u1", "B", 30).unwrap();
        set_enabled(&dir, true).unwrap();
        assert_eq!(read_for_edit(&dir).unwrap().sensitive.len(), 2);
        assert!(store.is_sensitive(7));
        // Une entrée illisible (zéro initial, faute de frappe) rend **tout** sensible.
        let mut file = read_for_edit(&dir).unwrap();
        file.sensitive.push("007".into());
        write(&dir, &mut file).unwrap();
        assert!(store.is_sensitive(8) && store.is_sensitive(123));
        // La liste « jamais » reste une autre liste.
        assert!(!store.never_commanded(7));
    }

    #[test]
    fn a_batch_of_commands_is_counted_all_or_nothing() {
        let dir = temp("batch");
        let store = AgentStore::open(&dir);
        assert!(store.check_commands("a", COMMANDS_PER_MINUTE - 1).is_ok());
        // Il reste une place : un lot de deux ne passe pas, et ne mange rien.
        assert!(store.check_commands("a", 2).is_err());
        assert!(store.check_command("a").is_ok());
        assert!(store.check_command("a").is_err());
        // Un autre jeton n'est pas touché.
        assert!(store.check_commands("b", COMMANDS_PER_MINUTE).is_ok());
        assert!(store.check_commands("b", 1).is_err());
        // Ce qui n'est finalement pas envoyé se rend.
        store.refund_commands("b", 3);
        assert!(store.check_commands("b", 3).is_ok());
        assert!(store.check_commands("b", 1).is_err());
        store.refund_commands("inconnu", 5);
    }

    #[test]
    fn a_live_agent_is_found_by_id_until_it_is_revoked_or_expires() {
        let dir = temp("live");
        let (a, _) = add(&dir, "u1", "A", 30).unwrap();
        let store = AgentStore::open(&dir);
        assert_eq!(store.live_agent(&a.id).map(|x| x.label), Some("A".into()));
        assert!(store.live_agent("inconnu").is_none());
        revoke(&dir, &a.id).unwrap();
        assert!(store.live_agent(&a.id).is_none());
        let (b, _) = add(&dir, "u1", "B", 30).unwrap();
        let mut file = read_for_edit(&dir).unwrap();
        file.agents[0].expires = now() - 1.0;
        write(&dir, &mut file).unwrap();
        assert!(store.live_agent(&b.id).is_none());
    }

    #[test]
    fn scopes_are_normalized_and_an_unknown_word_is_refused() {
        assert_eq!(
            normalize_scopes(&[Scope::Mode]).unwrap(),
            vec![Scope::Read, Scope::Occupation, Scope::Mode]
        );
        assert_eq!(
            normalize_scopes(&[Scope::Control, Scope::Read, Scope::Control]).unwrap(),
            vec![Scope::Read, Scope::Control]
        );
        assert_eq!(normalize_scopes(&[]).unwrap(), vec![Scope::Read]);
        assert!(normalize_scopes(&[Scope::Read, Scope::Unknown]).is_err());
    }

    #[test]
    fn add_with_gives_the_checked_rights_and_only_them() {
        let dir = temp("add-with");
        let (agent, token) = add_with(
            &dir,
            "u1",
            "Claude Code",
            30,
            &[Scope::Control, Scope::Mode],
            &[13828102, 13828101, 13828102],
        )
        .unwrap();
        assert!(token.starts_with(PREFIX));
        assert_eq!(
            agent.scopes,
            vec![Scope::Read, Scope::Occupation, Scope::Control, Scope::Mode]
        );
        assert_eq!(agent.devices, vec!["13828101", "13828102"]);
        let store = AgentStore::open(&dir);
        let seen = store.verify(&token).unwrap();
        assert!(seen.may_command(13828101) && !seen.may_command(7));
        // Des appareils sans le droit de commander : refusé, et rien n'est écrit.
        assert!(add_with(&dir, "u1", "Autre", 30, &[Scope::Read], &[1]).is_err());
        assert_eq!(list(&dir).unwrap().1.len(), 1);
        // Plus de 20 appareils : refusé.
        let many: Vec<u64> = (1..=21).collect();
        assert!(add_with(&dir, "u1", "Trop", 30, &[Scope::Control], &many).is_err());
        // Durée hors bornes : refusé.
        assert!(add_with(&dir, "u1", "Durée", 0, &[Scope::Read], &[]).is_err());
        assert!(add_with(&dir, "u1", "Durée", MAX_DAYS + 1, &[Scope::Read], &[]).is_err());
        // Le jeton en clair n'est nulle part dans le fichier.
        let text = std::fs::read_to_string(path_of(&dir)).unwrap();
        assert!(!text.contains(&token));
    }

    #[test]
    fn a_device_of_the_never_list_is_refused_and_a_broken_never_list_refuses_all() {
        let dir = temp("add-never");
        set_lists(&dir, &[10], &[]).unwrap();
        assert!(add_with(&dir, "u1", "A", 30, &[Scope::Control], &[10]).is_err());
        assert!(add_with(&dir, "u1", "A", 30, &[Scope::Control], &[11]).is_ok());
        hand_edit(&dir, |v| v["never"] = serde_json::json!(["010"]));
        assert!(add_with(&dir, "u1", "B", 30, &[Scope::Control], &[11]).is_err());
    }

    #[test]
    fn a_person_revokes_only_her_own_agents() {
        let dir = temp("revoke-owned");
        let (mine, _) = add(&dir, "u1", "Portable", 30).unwrap();
        let (theirs, _) = add(&dir, "u2", "Portable", 30).unwrap();
        // L'identifiant d'un autre : même réponse qu'un identifiant inconnu, et rien ne bouge.
        assert!(revoke_owned(&dir, "u1", &theirs.id).unwrap().is_none());
        assert!(revoke_owned(&dir, "u1", "inconnu").unwrap().is_none());
        // Le libellé ne désigne rien : seul l'identifiant vaut.
        assert!(revoke_owned(&dir, "u1", "Portable").unwrap().is_none());
        assert_eq!(list(&dir).unwrap().1.len(), 2);
        assert_eq!(
            revoke_owned(&dir, "u1", &mine.id).unwrap().unwrap().id,
            mine.id
        );
        let left = list(&dir).unwrap().1;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, theirs.id);
    }

    #[test]
    fn cut_all_switches_off_and_revokes_everything_but_keeps_the_lists() {
        let dir = temp("cut-all");
        add(&dir, "u1", "A", 30).unwrap();
        add(&dir, "u2", "B", 30).unwrap();
        set_enabled(&dir, true).unwrap();
        set_lists(&dir, &[1, 2], &[3]).unwrap();
        let store = AgentStore::open(&dir);
        assert!(store.enabled());
        let cut = cut_all(&dir).unwrap();
        assert_eq!(cut.len(), 2);
        assert!(!store.enabled());
        assert!(store.all_agents().is_empty());
        assert_eq!(store.lists(), (vec![1, 2], vec![3]));
    }

    #[test]
    fn lists_are_written_canonical_sorted_and_never_wins_over_sensitive() {
        let dir = temp("lists");
        assert!(set_lists(&dir, &[30, 10, 10], &[20, 30]).unwrap());
        let text = std::fs::read_to_string(path_of(&dir)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["never"], serde_json::json!(["10", "30"]));
        assert_eq!(v["sensitive"], serde_json::json!(["20"]));
        // Les mêmes listes : rien ne change.
        assert!(!set_lists(&dir, &[10, 30], &[20]).unwrap());
        let store = AgentStore::open(&dir);
        assert!(store.never_commanded(10) && !store.never_commanded(20));
        assert!(store.is_sensitive(20) && !store.is_sensitive(10));
        assert!(!store.lists_are_broken());
        let too_many: Vec<u64> = (0..=MAX_LISTED as u64).collect();
        assert!(set_lists(&dir, &too_many, &[]).is_err());
        hand_edit(&dir, |v| v["sensitive"] = serde_json::json!(["x"]));
        assert!(store.lists_are_broken());
        // On réécrit les listes : l'entrée illisible disparaît.
        set_lists(&dir, &[10], &[20]).unwrap();
        assert!(!store.lists_are_broken());
    }

    #[test]
    fn an_unreadable_file_is_never_overwritten_by_a_change() {
        let dir = temp("broken-file");
        std::fs::write(path_of(&dir), "{ pas du json").unwrap();
        assert!(set_enabled(&dir, true).is_err());
        assert!(add_with(&dir, "u1", "A", 30, &[Scope::Read], &[]).is_err());
        assert!(cut_all(&dir).is_err());
        assert_eq!(
            std::fs::read_to_string(path_of(&dir)).unwrap(),
            "{ pas du json"
        );
    }

    #[test]
    fn an_unchanged_switch_does_not_even_create_the_file() {
        let dir = temp("no-write");
        assert!(!set_enabled(&dir, false).unwrap());
        assert!(!path_of(&dir).exists());
        assert!(set_enabled(&dir, true).unwrap());
        assert!(!set_enabled(&dir, true).unwrap());
    }

    #[test]
    fn writers_in_parallel_never_lose_an_agent_or_clash_on_the_temporary_file() {
        let dir = temp("parallel");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for j in 0..4 {
                        add(&dir, "u1", &format!("agent {i}-{j}"), 30)
                            .map(|_| ())
                            .ok();
                        // Ne dépasse jamais 16 par personne : on change de personne.
                        add(&dir, &format!("p{i}"), &format!("x{j}"), 30).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let (_, agents) = list(&dir).unwrap();
        // 8 personnes x 4 agents, plus 16 au plus pour u1.
        assert_eq!(agents.iter().filter(|a| a.user_id != "u1").count(), 32);
        assert_eq!(agents.iter().filter(|a| a.user_id == "u1").count(), 16);
    }

    #[test]
    fn usage_counts_calls_since_the_start_per_agent() {
        let dir = temp("usage");
        let store = AgentStore::open(&dir);
        assert!(store.usage("a").is_none());
        store.check_read("a").unwrap();
        store.check_read("a").unwrap();
        store.check_read("b").unwrap();
        let a = store.usage("a").unwrap();
        assert_eq!(a.today, 2);
        assert!(a.last > 0.0);
        assert_eq!(store.usage("b").unwrap().today, 1);
    }

    #[test]
    fn the_agents_of_a_person_are_hers_alone() {
        let dir = temp("agents-of");
        add(&dir, "u1", "A", 30).unwrap();
        add(&dir, "u2", "B", 30).unwrap();
        let store = AgentStore::open(&dir);
        assert_eq!(store.agents_of("u1").len(), 1);
        assert_eq!(store.agents_of("u1")[0].label, "A");
        assert!(store.agents_of("u3").is_empty());
        assert_eq!(store.all_agents().len(), 2);
    }
}
