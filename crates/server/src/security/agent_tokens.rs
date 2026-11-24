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
//! **Seule la ligne de commande écrit ce fichier** (`ostal-server agent add|revoke|enable|disable`,
//! fonctions libres de ce module). Le serveur ne fait que le lire : il le relit dès qu'il change
//! (date, taille, inode), donc une révocation lancée pendant qu'il tourne prend effet à la requête
//! suivante, et il ne peut jamais ressusciter un jeton qui vient d'être révoqué en réécrivant une
//! liste gardée en mémoire. Un fichier illisible vaut « interrupteur éteint, aucun jeton » (jamais
//! l'inverse).

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
const WINDOW: Duration = Duration::from_secs(60);

/// Ce qu'un jeton a le droit de faire. Un mot inconnu (fichier d'une version plus récente) ne
/// donne **aucun** droit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Lire les appareils et l'état de la maison.
    Read,
    /// Lire la présence et le mode (« Vacances » dit que la maison est vide). Étape E1b.
    Occupation,
    /// Commander des appareils. Étape E2.
    Control,
    /// Changer le mode. Étape E3.
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
    /// Secondes Unix.
    pub created: f64,
    /// Secondes Unix.
    pub expires: f64,
}

impl AgentInfo {
    pub fn has(&self, scope: Scope) -> bool {
        scope != Scope::Unknown && self.scopes.contains(&scope)
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

/// Crée un jeton d'agent **en lecture seule** pour cette personne (`user_id` : identifiant
/// interne). Rend le jeton en clair, **une seule fois**. Les autres droits ne se donnent pas en
/// ligne de commande : sinon quiconque a un terminal sur le Mac les aurait.
pub fn add(dir: &Path, user_id: &str, label: &str, days: u32) -> Result<(AgentInfo, String)> {
    if days == 0 || days > MAX_DAYS {
        bail!("la durée va de 1 à {MAX_DAYS} jours");
    }
    let mut file = read_for_edit(dir)?;
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
    let token = draw_token();
    let created = now();
    let entry = Entry {
        id: new_id(),
        user_id: user_id.to_string(),
        label,
        scopes: vec![Scope::Read],
        hash: hex::encode(hash_of(&token)),
        created,
        expires: created + f64::from(days) * 86_400.0,
    };
    let info = AgentInfo::from(&entry);
    file.agents.push(entry);
    write(dir, &mut file)?;
    Ok((info, token))
}

/// Révoque un agent d'après son identifiant ou son libellé (insensible à la casse ; un libellé
/// porté par plusieurs agents est refusé : il faut alors l'identifiant).
pub fn revoke(dir: &Path, selector: &str) -> Result<AgentInfo> {
    let mut file = read_for_edit(dir)?;
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
        [i] => {
            let gone = file.agents.remove(*i);
            write(dir, &mut file)?;
            Ok(AgentInfo::from(&gone))
        }
        _ => {
            bail!("plusieurs agents s'appellent « {selector} » : donnez l'identifiant (agent list)")
        }
    }
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
    let mut file = read_for_edit(dir)?;
    if file.enabled == enabled {
        return Ok(false);
    }
    file.enabled = enabled;
    write(dir, &mut file)?;
    Ok(true)
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
}

impl AgentStore {
    pub fn open(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            cache: Mutex::new(load(dir)),
            reads: Mutex::new(HashMap::new()),
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

    /// Une lecture de plus pour ce jeton. `Err(secondes)` : la minute est pleine, réessayer après
    /// ce délai.
    pub fn check_read(&self, agent_id: &str) -> Result<(), u64> {
        self.check_read_at(agent_id, Instant::now())
    }

    fn check_read_at(&self, agent_id: &str, at: Instant) -> Result<(), u64> {
        let mut reads = self.reads.lock().unwrap_or_else(|e| e.into_inner());
        if reads.len() > 256 {
            reads.retain(|_, q| q.back().is_some_and(|t| at.duration_since(*t) < WINDOW));
        }
        let queue = reads.entry(agent_id.to_string()).or_default();
        while queue
            .front()
            .is_some_and(|t| at.duration_since(*t) >= WINDOW)
        {
            queue.pop_front();
        }
        if queue.len() >= READS_PER_MINUTE {
            let wait = queue
                .front()
                .map_or(WINDOW, |t| WINDOW.saturating_sub(at.duration_since(*t)));
            return Err(wait.as_secs().max(1));
        }
        queue.push_back(at);
        Ok(())
    }
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
}
