//! Persistance minimale : identifiants du contrôleur et liste des nœuds appairés,
//! dans un seul fichier JSON du dossier de données de l'application.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use protocol::home::Symbol;

pub const FILE_NAME: &str = "controller.json";

/// Sous-dossier du dossier de données réservé au serveur réseau : compte, sessions,
/// certificat TLS et sa clé. Il n'est ni exporté ni accepté à l'import, et l'import le
/// laisse en place (voir `backup::apply_pending`).
pub const SERVER_DIR: &str = "serveur";

/// Identité du contrôleur sur sa fabric (clés en clair, comme les fichiers PEM de matc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerIdentity {
    pub fabric_id: u64,
    pub controller_node_id: u64,
    /// Clé privée P-256 de l'autorité racine (RCAC), hexadécimal.
    pub ca_private_key: String,
    /// Certificat X.509 DER de la racine, hexadécimal.
    pub ca_cert_der: String,
    /// Clé privée P-256 du contrôleur, hexadécimal.
    pub controller_private_key: String,
    /// Certificat opérationnel X.509 DER du contrôleur, hexadécimal.
    pub controller_cert_der: String,
}

/// Endpoints portant les clusters suivis, pour un appareil (nœud ou appareil ponté).
/// Aplati dans le JSON du nœud : le format des anciennes versions reste lisible.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrackedEndpoints {
    /// Endpoint portant le cluster Temperature Measurement (0x0402), s'il existe.
    #[serde(default)]
    pub temperature_endpoint: Option<u16>,
    /// Endpoint portant le cluster Relative Humidity Measurement (0x0405), s'il existe.
    #[serde(default)]
    pub humidity_endpoint: Option<u16>,
    /// Endpoint du cluster Boolean State (0x0045) : détecteur d'ouverture.
    #[serde(default)]
    pub contact_endpoint: Option<u16>,
    /// Endpoint du cluster Power Source (0x002F) alimenté par pile.
    #[serde(default)]
    pub battery_endpoint: Option<u16>,
    /// Endpoint du cluster Electrical Power Measurement (0x0090).
    #[serde(default)]
    pub power_endpoint: Option<u16>,
    /// Endpoint du cluster Electrical Energy Measurement (0x0091).
    #[serde(default)]
    pub energy_endpoint: Option<u16>,
    /// Endpoint du cluster On/Off (0x0006).
    #[serde(default)]
    pub on_off_endpoint: Option<u16>,
    /// Endpoint du cluster Window Covering (0x0102) : volet, store.
    #[serde(default)]
    pub cover_endpoint: Option<u16>,
    /// Le volet connaît sa position (FeatureMap « PositionAwareLift »).
    #[serde(default)]
    pub cover_position: bool,
}

/// Appareil exposé par un pont (Aggregator) : un endpoint du pont avec son propre nom.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredBridgedDevice {
    /// Identifiant propre, stable, dans [`BRIDGED_ID_MIN`]..=[`BRIDGED_ID_MAX`].
    pub device_id: u64,
    /// Endpoint du pont portant Bridged Device Basic Information (0x0039).
    pub endpoint: u16,
    /// Nom annoncé par le pont (NodeLabel, sinon ProductName).
    pub name: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub device_types: Vec<String>,
    #[serde(flatten)]
    pub endpoints: TrackedEndpoints,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub icon: Option<Symbol>,
}

/// Plage des identifiants des appareils pontés : identifiants de nœud que Matter réserve
/// (groupes, CASE temporaires…) et que l'appairage ne tire donc jamais.
///
/// Le haut de la plage (`CAMERA_ID_MIN` et au-delà, voir `protocol::wire`) est réservé aux
/// caméras (#4) : aucun nouvel identifiant d'appareil ponté n'y est tiré. Les identifiants déjà
/// attribués plus haut restent valables, ce sont des appareils et non des caméras.
pub const BRIDGED_ID_MIN: u64 = 0xFFFF_FFF0_0000_0000;
pub const BRIDGED_ID_MAX: u64 = protocol::wire::CAMERA_ID_MIN - 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredNode {
    pub node_id: u64,
    pub name: String,
    /// Dernière adresse connue, `[ipv6]:port`.
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub device_types: Vec<String>,
    #[serde(flatten)]
    pub endpoints: TrackedEndpoints,
    /// La structure (endpoints, clusters) a été lue au moins une fois.
    #[serde(default)]
    pub interviewed: bool,
    /// Nom choisi par l'utilisateur (« Porte d'entrée ») ; prioritaire sur `name`,
    /// qui reste le nom annoncé par l'appareil et peut être relu.
    #[serde(default)]
    pub custom_name: Option<String>,
    /// Icône choisie par l'utilisateur (ex. une prise qui alimente une lampe).
    #[serde(default)]
    pub icon: Option<Symbol>,
    /// Version de la lecture de structure ; une version plus ancienne que
    /// `INTERVIEW_VERSION` déclenche une nouvelle lecture (nouveaux clusters suivis).
    #[serde(default)]
    pub interview_version: u32,
    /// Appareils exposés si ce nœud est un pont (vide sinon).
    #[serde(default)]
    pub bridged: Vec<StoredBridgedDevice>,
}

impl StoredNode {
    pub fn bridged(&self, device_id: u64) -> Option<&StoredBridgedDevice> {
        self.bridged.iter().find(|b| b.device_id == device_id)
    }

    pub fn bridged_mut(&mut self, device_id: u64) -> Option<&mut StoredBridgedDevice> {
        self.bridged.iter_mut().find(|b| b.device_id == device_id)
    }
}

/// Version courante de la lecture de structure des nœuds.
/// 3 : volets (Window Covering) et appareils derrière un pont.
pub const INTERVIEW_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredState {
    pub version: u32,
    pub identity: ControllerIdentity,
    #[serde(default)]
    pub nodes: Vec<StoredNode>,
}

impl StoredState {
    /// Tous les identifiants connus : nœuds et appareils pontés.
    pub fn device_ids(&self) -> Vec<u64> {
        self.nodes
            .iter()
            .flat_map(|n| std::iter::once(n.node_id).chain(n.bridged.iter().map(|b| b.device_id)))
            .collect()
    }
}

fn project_dir(organization: &str, application: &str) -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", organization, application)
        .map(|d| d.data_dir().to_path_buf())
}

/// Dossier de données imposé par `--data-dir` (essais, tests) ; fixé une seule fois, au
/// lancement. Sans lui, le dossier standard de l'application est utilisé.
static DATA_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Impose le dossier de données pour tout le processus. `false` s'il était déjà fixé.
pub fn set_data_dir(dir: PathBuf) -> bool {
    DATA_DIR_OVERRIDE.set(dir).is_ok()
}

/// Rend `text` (valeur de `--data-dir`) absolu, sans exiger que le dossier existe. Un chemin
/// relatif (`donnees`, `donnees/`) n'aurait pas de dossier parent exploitable : l'import renomme
/// le dossier de données et synchronise son parent, ce qui échouait sur un chemin vide.
pub fn absolute_data_dir(text: &str) -> Result<PathBuf> {
    if text.is_empty() {
        bail!("--data-dir attend un chemin non vide");
    }
    std::path::absolute(text).with_context(|| format!("chemin de données invalide : {text}"))
}

/// Crée `dir` (et ses parents) s'il manque, puis le met en 0700 : il contient les clés privées.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .with_context(|| format!("droits de {}", dir.display()))?;
    Ok(())
}

/// Dossier voisin « <dir>.avant-import-<date> » (laissé par un import) qui porte une identité
/// Matter, le plus récent d'abord.
pub fn previous_identity_neighbour(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let prefix = format!("{name}.avant-import-");
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .filter(|p| p.join(FILE_NAME).is_file())
        .collect();
    found.sort();
    found.pop()
}

/// Nom du dossier de données d'Ostal (`dev.ostal.OstalServer`). Volontairement différent de
/// l'identifiant de l'application (`dev.ostal.server`) : un framework de macOS qui créerait un
/// dossier vide à ce nom ne doit pas passer pour des données.
pub const DATA_DIR: (&str, &str) = ("ostal", "OstalServer");

fn current_dir() -> PathBuf {
    if let Some(dir) = DATA_DIR_OVERRIDE.get() {
        return dir.clone();
    }
    project_dir(DATA_DIR.0, DATA_DIR.1).unwrap_or_else(|| PathBuf::from("./ostal-data"))
}

/// Dossier de données standard (ou imposé par `--data-dir`), qu'il existe ou non : celui où
/// un import interrompu doit être repris, avant toute migration d'un ancien dossier.
pub fn current_data_dir() -> PathBuf {
    current_dir()
}

/// Dossiers des anciens noms, du plus récent au plus ancien : « Matter Home », puis
/// « matter-demo ». Aucun avec un dossier imposé (`--data-dir`, tests) : rien n'est alors lu
/// ni copié hors de ce dossier.
pub fn legacy_dirs() -> Vec<PathBuf> {
    if DATA_DIR_OVERRIDE.get().is_some() {
        return Vec::new();
    }
    [crate::legacy::DATA_DIR, crate::legacy::DEMO_DATA_DIR]
        .into_iter()
        .filter_map(|(org, app)| project_dir(org, app))
        .collect()
}

/// Dossier de données de l'application
/// (macOS : `~/Library/Application Support/dev.ostal.OstalServer`). Toujours celui d'Ostal,
/// même avant la copie d'un ancien dossier : on n'y lit jamais les données d'un ancien nom
/// (la copie se fait au démarrage, voir [`migrate_legacy_data_dir`]).
pub fn data_dir() -> PathBuf {
    current_dir()
}

/// Copie vers `new` le premier dossier de `legacy` qui porte une identité, si `new` n'en a
/// pas (voir [`crate::legacy::migrate_dir`]). Un ancien dossier déjà copié (identité
/// neutralisée) arrête la recherche : un nom plus ancien n'est jamais copié à sa place.
fn migrate_from(legacy: &[PathBuf], new: &Path) -> Result<Option<String>> {
    for old in legacy {
        if crate::legacy::identity_copied(old) && !old.join(FILE_NAME).exists() {
            return Ok(None);
        }
        if crate::backup::recovery_needed(old) || old.join(FILE_NAME).is_file() {
            return crate::legacy::migrate_dir(old, new);
        }
    }
    Ok(None)
}

/// À appeler au démarrage, verrous pris ([`acquire_data_lock`]) : copie le dossier de données
/// de l'ancien nom (« Matter Home », ou « matter-demo ») vers celui d'Ostal, s'il n'y est pas
/// encore. L'ancien reste intact. Rend un message pour le journal si la copie a eu lieu ; une
/// erreur si elle était nécessaire et n'a pas pu se faire (rien n'est alors démarré).
pub fn migrate_legacy_data_dir() -> Result<Option<String>> {
    migrate_from(&legacy_dirs(), &current_dir())
}

/// Un ancien dossier porte une identité que `dir` n'a pas, active ou déjà copiée vers Ostal
/// (neutralisée) : y créer une nouvelle fabric orphelinerait les appareils. Seulement pour le
/// dossier standard. Rend l'ancien dossier et le fichier d'identité qu'il contient.
fn legacy_identity_for(
    dir: &Path,
    current: &Path,
    legacy: &[PathBuf],
) -> Option<(PathBuf, &'static str)> {
    if dir != current || dir.join(FILE_NAME).exists() {
        return None;
    }
    legacy.iter().find_map(|old| {
        if old.join(FILE_NAME).is_file() {
            Some((old.clone(), FILE_NAME))
        } else if crate::legacy::identity_copied(old) {
            Some((old.clone(), crate::legacy::NEUTRALIZED_IDENTITY))
        } else {
            None
        }
    })
}

/// Refuse de créer une nouvelle identité (fabric) quand `controller.json` manque alors que
/// d'autres données existent dans `dir` : plan (`maison.json`), règles (`regles.json`), caméras (`cameras.json`) ou
/// historique (`history/` avec au moins un `*.jsonl`). Une nouvelle fabric ne reconnaîtrait plus aucun
/// appareil appairé, et le nettoyage de l'historique supprimerait celui des appareils
/// devenus inconnus. Les fichiers du mode démo (`*-demo.json`) ne comptent pas.
pub fn check_fresh_start(dir: &Path) -> Result<()> {
    match legacy_identity_for(dir, &current_dir(), &legacy_dirs()) {
        Some((old, FILE_NAME)) => bail!(
            "{FILE_NAME} est introuvable dans {} alors que l'ancien dossier de Matter Home {} en \
             contient un : ses données n'ont pas encore été copiées. Créer une nouvelle identité \
             ici orphelinerait les appareils appairés. Quittez l'ancienne application si elle \
             tourne, puis relancez Ostal Server : la copie se fait au démarrage.",
            dir.display(),
            old.display()
        ),
        Some((old, name)) => bail!(
            "{FILE_NAME} est introuvable dans {} alors que les données de Matter Home y ont déjà \
             été copiées depuis {} (son identité y est gardée sous le nom {name}). Créer une \
             nouvelle identité orphelinerait les appareils appairés. Pour recopier l'ancien \
             dossier : quittez Ostal Server, déplacez {} ailleurs, renommez {name} en \
             {FILE_NAME} dans {}, puis rouvrez Ostal Server.",
            dir.display(),
            old.display(),
            dir.display(),
            old.display()
        ),
        None => {}
    }
    // Un import a mis de côté un dossier qui portait une identité (« …avant-import-… »), et le
    // dossier de données n'en a plus : démarrer ici créerait une nouvelle fabric alors que
    // l'identité existe quelque part. Refus, même si `dir` est vide.
    if let Some(previous) = previous_identity_neighbour(dir) {
        bail!(
            "{FILE_NAME} est introuvable dans {} alors que le dossier voisin {} en contient un \
             (laissé par un import). Démarrer ainsi créerait une nouvelle fabric : les appareils \
             appairés ne seraient plus reconnus. Arrêtez le service, puis remettez la bonne \
             identité en place : déplacez {} de côté et renommez {} en {}. Pour repartir de zéro \
             malgré tout, déplacez ce dossier voisin ailleurs.",
            dir.display(),
            previous.display(),
            dir.display(),
            previous.display(),
            dir.display()
        );
    }
    let mut found = Vec::new();
    for name in [
        "maison.json",
        "regles.json",
        crate::cameras::FILE_NAME,
        crate::home_mode::FILE_NAME,
    ] {
        if dir.join(name).exists() {
            found.push(name.to_string());
        }
    }
    let history = dir.join("history");
    // Un dossier illisible est compté comme non vide : dans le doute, on ne purge rien.
    // Seuls les `*.jsonl` comptent (pas un `.DS_Store`).
    let history_used = history.is_dir()
        && std::fs::read_dir(&history)
            .map(|mut entries| {
                entries.any(|e| {
                    e.map(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                        .unwrap_or(true)
                })
            })
            .unwrap_or(true);
    if history_used {
        found.push("history/".to_string());
    }
    if found.is_empty() {
        return Ok(());
    }
    bail!(
        "{FILE_NAME} est introuvable dans {} alors que des données existent déjà ({}). \
         Démarrer ainsi créerait une nouvelle fabric : les appareils appairés ne seraient plus \
         reconnus et l'historique serait supprimé. Remettez {FILE_NAME} en place (sauvegarde \
         ou import d'un fichier .ostal), ou déplacez ces données si vous voulez repartir \
         de zéro.",
        dir.display(),
        found.join(", ")
    )
}

/// Fichier de verrou d'instance : à côté du dossier de données et non dedans, car l'import
/// de sauvegarde renomme ce dossier.
pub fn instance_lock_path() -> PathBuf {
    let dir = current_dir();
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ostal-data".into());
    dir.with_file_name(format!("{name}.lock"))
}

/// Verrous des anciens dossiers de données : ceux de « Matter Home » qui existent (dossier ou
/// fichier de verrou). Aucun avec un dossier imposé.
pub fn legacy_lock_paths() -> Vec<PathBuf> {
    legacy_lock_paths_for(&legacy_dirs())
}

fn legacy_lock_paths_for(legacy: &[PathBuf]) -> Vec<PathBuf> {
    legacy
        .iter()
        .filter_map(|dir| {
            let name = dir.file_name()?.to_string_lossy().into_owned();
            let lock = dir.with_file_name(format!("{name}.lock"));
            (dir.exists() || lock.exists()).then_some(lock)
        })
        .collect()
}

/// Les verrous d'instance tenus par ce processus : celui d'Ostal et ceux des anciens noms.
/// Tant qu'il vit, ni un autre serveur Ostal ni l'ancienne application « Matter Home Server »
/// (qui prend son propre verrou) ne peuvent démarrer sur les mêmes données ou sur leur copie.
#[derive(Debug)]
pub struct DataLock {
    _files: Vec<std::fs::File>,
}

/// Prend le verrou d'Ostal ([`instance_lock_path`]) puis ceux des anciens noms. Si l'un est
/// tenu ailleurs, l'erreur est un [`LockHeld`] qui nomme ce verrou-là, et aucun n'est gardé.
pub fn acquire_data_lock() -> Result<DataLock> {
    acquire_data_lock_at(&instance_lock_path(), &legacy_lock_paths())
}

pub fn acquire_data_lock_at(main: &Path, legacy: &[PathBuf]) -> Result<DataLock> {
    let mut files = vec![acquire_instance_lock(main)?];
    for path in legacy {
        files.push(acquire_instance_lock(path)?);
    }
    Ok(DataLock { _files: files })
}

/// Prend le verrou exclusif de `path` et le garde tant que le fichier retourné est ouvert.
/// Deux serveurs avec la même identité Matter se chasseraient (abonnements, règles
/// exécutées deux fois, `controller.json` écrasé) : le second doit refuser de démarrer.
pub fn acquire_instance_lock(path: &Path) -> Result<std::fs::File> {
    use std::fs::{File, TryLockError};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("ouverture de {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            // Le numéro du processus, pour que celui qui trouve le verrou pris sache qui le tient.
            // Un échec d'écriture n'empêche rien : c'est une information, pas le verrou.
            use std::io::Write;
            let _ = file.set_len(0);
            let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => Err(LockHeld {
            path: path.to_path_buf(),
        }
        .into()),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("verrou de {}", path.display()))
        }
    }
}

/// Le verrou d'instance est tenu par un autre processus.
#[derive(Debug, Clone)]
pub struct LockHeld {
    pub path: PathBuf,
}

impl std::fmt::Display for LockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "un autre serveur (Ostal, ou l'ancienne application Matter Home) utilise déjà ces \
             données (verrou {}). Arrêtez-le avant de lancer celui-ci. (L'application de bureau v0.2 ne prend pas ce verrou : \
             ne la lancez pas en même temps.)",
            self.path.display()
        )
    }
}

impl std::error::Error for LockHeld {}

/// Numéro du processus qui tient le verrou `path`, s'il l'a écrit (le serveur et l'application
/// de la barre de menus le font ; une version plus ancienne non).
pub fn lock_holder_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn state_path() -> PathBuf {
    data_dir().join(FILE_NAME)
}

pub fn load(path: &Path) -> Result<Option<StoredState>> {
    load_json(path)
}

/// Écriture atomique (fichier temporaire puis renommage), droits 0600 sous Unix.
pub fn save(path: &Path, state: &StoredState) -> Result<()> {
    save_json(path, state)
}

/// Lit un fichier JSON ; `None` s'il n'existe pas encore.
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    let value = serde_json::from_str(&text)
        .with_context(|| format!("fichier {} illisible", path.display()))?;
    Ok(Some(value))
}

/// Écrit un fichier JSON de façon atomique (fichier temporaire puis renommage), droits 0600.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value)?;
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("écriture de {}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("écriture de {}", tmp.display()))?;
        // Sur disque avant le renommage : après une coupure, jamais un fichier vide.
        file.sync_all()
            .with_context(|| format!("écriture de {}", tmp.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renommage vers {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredState {
        StoredState {
            version: 1,
            identity: ControllerIdentity {
                fabric_id: 42,
                controller_node_id: 112233,
                ca_private_key: "aa".into(),
                ca_cert_der: "bb".into(),
                controller_private_key: "cc".into(),
                controller_cert_der: "dd".into(),
            },
            nodes: vec![
                StoredNode {
                    node_id: 7,
                    name: "Capteur".into(),
                    endpoints: TrackedEndpoints {
                        temperature_endpoint: Some(1),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                StoredNode {
                    node_id: 8,
                    name: "TaHoma".into(),
                    bridged: vec![StoredBridgedDevice {
                        device_id: BRIDGED_ID_MIN + 5,
                        endpoint: 3,
                        name: "Volet salon".into(),
                        endpoints: TrackedEndpoints {
                            cover_endpoint: Some(3),
                            cover_position: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("ostal-test-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        assert!(load(&path).unwrap().is_none());
        save(&path, &sample()).unwrap();
        let back = load(&path).unwrap().unwrap();
        assert_eq!(back.identity.fabric_id, 42);
        assert_eq!(back.nodes, sample().nodes);
        assert_eq!(back.device_ids(), vec![7, 8, BRIDGED_ID_MIN + 5]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Les fichiers écrits avant les volets et les ponts (endpoints à plat) restent lisibles.
    #[test]
    fn reads_flat_legacy_node() {
        let text = r#"{"node_id": 9, "name": "Prise", "address": "[fd00::1]:5540",
            "power_endpoint": 2, "on_off_endpoint": 2, "interviewed": true, "interview_version": 2}"#;
        let n: StoredNode = serde_json::from_str(text).unwrap();
        assert_eq!(n.endpoints.power_endpoint, Some(2));
        assert_eq!(n.endpoints.on_off_endpoint, Some(2));
        assert_eq!(n.endpoints.cover_endpoint, None);
        assert!(n.bridged.is_empty());
        // Et le format reste à plat à l'écriture.
        let out = serde_json::to_string(&n).unwrap();
        assert!(out.contains("\"power_endpoint\":2"), "{out}");
        assert!(!out.contains("\"endpoints\""), "{out}");
    }

    /// Les identifiants d'appareils pontés dépassent 2^53 : un client JavaScript les arrondirait.
    #[test]
    fn bridged_ids_exceed_js_safe_integers() {
        const { assert!(BRIDGED_ID_MIN > 1 << 53) };
        // Les caméras ont leur plage, juste au-dessus : aucun recoupement avec les appareils pontés.
        const { assert!(BRIDGED_ID_MAX < protocol::wire::CAMERA_ID_MIN) };
        const { assert!(BRIDGED_ID_MAX > BRIDGED_ID_MIN) };
        // Deux identifiants voisins ne seraient plus distincts une fois passés par un f64.
        assert_eq!((BRIDGED_ID_MIN + 1) as f64, (BRIDGED_ID_MIN + 2) as f64);
    }

    #[test]
    fn refuses_fresh_identity_when_data_exists() {
        let base = std::env::temp_dir().join(format!("mhd-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // Dossier absent ou vide : première installation, on peut créer l'identité.
        assert!(check_fresh_start(&base).is_ok());
        std::fs::create_dir_all(&base).unwrap();
        assert!(check_fresh_start(&base).is_ok());
        // Les fichiers de démonstration et un historique vide ne comptent pas.
        std::fs::write(base.join("maison-demo.json"), "{}").unwrap();
        std::fs::write(base.join("regles-demo.json"), "{}").unwrap();
        std::fs::create_dir_all(base.join("history")).unwrap();
        assert!(check_fresh_start(&base).is_ok());

        // Le plan, les règles ou un historique non vide interdisent de repartir de zéro.
        std::fs::write(base.join("maison.json"), "{}").unwrap();
        let err = check_fresh_start(&base).unwrap_err().to_string();
        assert!(
            err.contains(FILE_NAME) && err.contains("maison.json"),
            "{err}"
        );
        std::fs::remove_file(base.join("maison.json")).unwrap();
        std::fs::write(base.join("regles.json"), "{}").unwrap();
        assert!(check_fresh_start(&base).is_err());
        std::fs::remove_file(base.join("regles.json")).unwrap();
        // Les caméras (adresses et mots de passe, qu'aucune autre source ne redonne) aussi.
        std::fs::write(base.join("cameras.json"), "{}").unwrap();
        let err = check_fresh_start(&base).unwrap_err().to_string();
        assert!(err.contains("cameras.json"), "{err}");
        std::fs::remove_file(base.join("cameras.json")).unwrap();
        // Un fichier parasite (.DS_Store) dans history/ ne bloque pas un démarrage neuf.
        std::fs::write(base.join("history").join(".DS_Store"), "").unwrap();
        assert!(check_fresh_start(&base).is_ok());
        std::fs::write(base.join("history").join("1.jsonl"), "").unwrap();
        let err = check_fresh_start(&base).unwrap_err().to_string();
        assert!(err.contains("history/"), "{err}");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Un dossier « …avant-import-… » qui porte une identité interdit de repartir de zéro,
    /// même si le dossier de données est vide ou absent.
    #[test]
    fn refuses_fresh_identity_when_a_previous_import_folder_has_one() {
        let base = std::env::temp_dir().join(format!("mhd-neighbour-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let data = base.join("donnees");
        std::fs::create_dir_all(&data).unwrap();
        // Un voisin sans identité, ou d'un autre nom, ne bloque rien.
        std::fs::create_dir_all(base.join("donnees.avant-import-20261101-100000")).unwrap();
        std::fs::create_dir_all(base.join("autre.avant-import-20261101-100000")).unwrap();
        std::fs::write(
            base.join("autre.avant-import-20261101-100000")
                .join(FILE_NAME),
            "{}",
        )
        .unwrap();
        assert!(check_fresh_start(&data).is_ok());
        // Un voisin avec identité bloque, dossier vide ou absent.
        let previous = base.join("donnees.avant-import-20261102-100000");
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join(FILE_NAME), "{}").unwrap();
        let refused = |data: &Path| {
            let err = check_fresh_start(data).unwrap_err().to_string();
            assert!(
                err.contains("avant-import-20261102-100000") && err.contains("nouvelle fabric"),
                "{err}"
            );
        };
        refused(&data);
        std::fs::remove_dir_all(&data).unwrap();
        refused(&data);
        // Avec une identité dans le dossier de données, la question ne se pose plus.
        assert_eq!(
            previous_identity_neighbour(&data).as_deref(),
            Some(previous.as_path())
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn data_dir_option_is_made_absolute() {
        for text in ["donnees", "donnees/", "./donnees", "a/b/donnees/"] {
            let dir = absolute_data_dir(text).unwrap();
            assert!(dir.is_absolute(), "{text} -> {}", dir.display());
            assert!(dir.parent().is_some_and(|p| !p.as_os_str().is_empty()));
            assert_eq!(dir.file_name().unwrap(), "donnees", "{text}");
        }
        assert_eq!(
            absolute_data_dir("/tmp/x/donnees").unwrap(),
            PathBuf::from("/tmp/x/donnees")
        );
        assert!(absolute_data_dir("").is_err());
    }

    #[test]
    fn instance_lock_is_exclusive() {
        let base = std::env::temp_dir().join(format!("mhd-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("donnees.lock");
        let first = acquire_instance_lock(&path).unwrap();
        let err = acquire_instance_lock(&path).unwrap_err().to_string();
        assert!(err.contains("utilise déjà"), "{err}");
        let held = acquire_instance_lock(&path).unwrap_err();
        assert!(held.downcast_ref::<LockHeld>().is_some());
        assert_eq!(lock_holder_pid(&path), Some(std::process::id()));
        // Le verrou tombe avec le fichier (arrêt ou plantage du premier serveur).
        drop(first);
        assert!(acquire_instance_lock(&path).is_ok());
        // Il est à côté du dossier de données, pas dedans.
        assert_ne!(instance_lock_path().parent(), Some(current_dir().as_path()));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn legacy_dirs_are_resolved_copied_and_locked() {
        let base = std::env::temp_dir().join(format!("ostal-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let matter_home = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        let demo = base.join("dev.matter-demo.MatterDemo");
        let new = base.join("dev.ostal.OstalServer");
        let legacy = [matter_home.clone(), demo.clone()];
        std::fs::create_dir_all(demo.join("history")).unwrap();
        std::fs::write(demo.join(FILE_NAME), "demo").unwrap();
        // Le dossier de données est toujours celui d'Ostal, même avant la copie.
        assert_eq!(data_dir(), current_data_dir());

        // Aucun verrou pour un ancien nom absent ; un verrou pour celui qui existe.
        assert_eq!(
            legacy_lock_paths_for(&legacy),
            vec![base.join("dev.matter-demo.MatterDemo.lock")]
        );
        std::fs::create_dir_all(matter_home.join("history")).unwrap();
        std::fs::write(matter_home.join(FILE_NAME), "matter home").unwrap();
        // Le plus récent des anciens noms gagne.
        assert!(migrate_from(&legacy, &new).unwrap().is_some());
        assert_eq!(std::fs::read(new.join(FILE_NAME)).unwrap(), b"matter home");
        // Son identité est neutralisée ; celle de « matter-demo » n'est pas touchée.
        assert!(crate::legacy::identity_copied(&matter_home));
        assert!(!matter_home.join(FILE_NAME).exists() && demo.join(FILE_NAME).is_file());
        assert_eq!(migrate_from(&legacy, &new).unwrap(), None);
        // Le nouveau dossier perd son identité : rien n'est recopié, pas même depuis
        // « matter-demo », et aucune nouvelle fabric n'est créée sans un geste explicite.
        std::fs::rename(new.join(FILE_NAME), base.join("perdu")).unwrap();
        assert_eq!(migrate_from(&legacy, &new).unwrap(), None);
        assert!(!new.join(FILE_NAME).exists());
        assert_eq!(
            legacy_identity_for(&new, &new, &legacy),
            Some((matter_home.clone(), crate::legacy::NEUTRALIZED_IDENTITY))
        );
        std::fs::rename(base.join("perdu"), new.join(FILE_NAME)).unwrap();

        // Pas de nouvelle fabric dans le dossier standard tant qu'un ancien a une identité.
        let empty = base.join("vide");
        assert_eq!(
            legacy_identity_for(&empty, &empty, &[demo.clone()]),
            Some((demo.clone(), FILE_NAME))
        );
        assert_eq!(legacy_identity_for(&new, &new, &legacy), None);
        assert_eq!(legacy_identity_for(&empty, &new, &legacy), None);

        // Deux verrous : l'ancienne application (qui prend l'ancien) est tenue à l'écart, et un
        // ancien verrou tenu empêche de démarrer.
        let main_lock = base.join("dev.ostal.OstalServer.lock");
        let old_locks = legacy_lock_paths_for(&legacy);
        assert_eq!(old_locks.len(), 2);
        let held = acquire_data_lock_at(&main_lock, &old_locks).unwrap();
        let err = acquire_instance_lock(&old_locks[0]).unwrap_err();
        assert_eq!(err.downcast_ref::<LockHeld>().unwrap().path, old_locks[0]);
        drop(held);
        let old_app = acquire_instance_lock(&old_locks[0]).unwrap();
        let err = acquire_data_lock_at(&main_lock, &old_locks).unwrap_err();
        assert_eq!(err.downcast_ref::<LockHeld>().unwrap().path, old_locks[0]);
        drop(old_app);
        // Rien n'est resté tenu après l'échec.
        assert!(acquire_data_lock_at(&main_lock, &old_locks).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }
}
