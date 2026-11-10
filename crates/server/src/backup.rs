//! Export et import de toutes les données de l'application dans un seul fichier
//! (`.matterhome`), pour changer de Mac sans réappairer les appareils.
//!
//! Le fichier est un JSON qui embarque les fichiers du dossier de données :
//! `controller.json`, `maison.json`, `regles.json`, `cameras.json` (adresses et **mots de passe**
//! des caméras RTSP, en clair comme les clés du contrôleur) et `history/*.jsonl`.
//!
//! # Import
//!
//! L'import n'écrase rien tant que l'application tourne (elle réécrit ses fichiers) :
//! il valide à fond la sauvegarde (identité du contrôleur, plan, règles), la dépose dans le
//! dossier de données (`import-en-attente.matterhome`, créée en 0600, écriture atomique), et
//! c'est le lancement suivant qui l'applique ([`apply_pending`]), avant tout chargement.
//!
//! L'application est atomique et survit à une coupure à n'importe quel instant. Le nouveau
//! dossier est construit **à côté** du dossier de données `D` :
//!
//! 1. `D.import-partiel` : fichiers de la sauvegarde et copie de `serveur/`, chacun écrit en
//!    0600 et synchronisé sur disque, puis les dossiers ;
//! 2. renommage en `D.import-nouveau` : c'est le marqueur « complet » ;
//! 3. `D` est mis de côté sous `D.avant-import-<date>`, puis `D.import-nouveau` devient `D`.
//!
//! Au démarrage, [`recover`] reprend un échange interrompu entre les deux renommages de
//! l'étape 3 (`D` absent, `D.import-nouveau` présent : on le renomme en `D`) et nettoie les
//! restes des autres étapes (la sauvegarde en attente, restée dans `D`, est alors rejouée
//! depuis le début). Jamais une nouvelle identité n'est créée : un dossier vide n'existe à
//! aucun moment sans que le prochain lancement ne le répare.
//!
//! Le sous-dossier du serveur réseau (`serveur/` : compte, sessions, certificat TLS et sa
//! clé) n'est jamais exporté, jamais accepté dans une sauvegarde, et copié de l'ancien dossier
//! dans le nouveau : l'empreinte du certificat et le compte ne changent pas.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use protocol::automation::RuleSet;
use protocol::home::HomePlan;

use crate::matter::certs::JsonCertManager;
use crate::persistence::{StoredState, FILE_NAME, SERVER_DIR};
use crate::security::write_secret;

/// Extension proposée dans les boîtes de dialogue.
pub const EXTENSION: &str = "matterhome";

const FORMAT: &str = "matter-home-desktop-sauvegarde";
const VERSION: u32 = 1;
const PENDING: &str = "import-en-attente.matterhome";
const PLAN: &str = "maison.json";
const RULES: &str = "regles.json";
const CAMERAS: &str = crate::cameras::FILE_NAME;
/// Suffixes des dossiers voisins du dossier de données pendant un import.
const PARTIAL_SUFFIX: &str = ".import-partiel";
const NEW_SUFFIX: &str = ".import-nouveau";
/// Essais d'application d'un import avant de renoncer à démarrer.
const APPLY_ATTEMPTS: usize = 3;

#[derive(Debug, Serialize, Deserialize)]
struct Bundle {
    format: String,
    version: u32,
    /// Date de création, ISO 8601.
    created: String,
    /// Chemin relatif (séparateur `/`) → contenu texte du fichier.
    files: BTreeMap<String, String>,
}

/// Ce que contient une sauvegarde, pour l'afficher à l'utilisateur.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub created: String,
    pub devices: usize,
    pub rooms: usize,
    pub rules: usize,
    /// Caméras RTSP (leurs mots de passe sont dans la sauvegarde).
    pub cameras: usize,
    pub history_files: usize,
    pub has_plan: bool,
    pub has_rules: bool,
    /// Noms des règles désactivées à l'import parce que leur webhook était incomplet
    /// (brouillons enregistrés par la v0.2.1). Leur contenu est conservé.
    pub disabled_rules: Vec<String>,
}

impl Summary {
    pub fn describe(&self) -> String {
        let date = self.created.get(..10).unwrap_or(&self.created);
        let mut parts = vec![format!("{} appareil(s)", self.devices)];
        if self.has_plan {
            parts.push(format!("plan de la maison ({} pièce(s))", self.rooms));
        }
        if self.has_rules {
            parts.push(format!("{} automatisation(s)", self.rules));
        }
        if self.cameras > 0 {
            parts.push(format!("{} caméra(s)", self.cameras));
        }
        if self.history_files > 0 {
            parts.push(format!("historique de {} appareil(s)", self.history_files));
        }
        let mut text = format!("sauvegarde du {date} : {}", parts.join(", "));
        if !self.disabled_rules.is_empty() {
            text.push_str(&format!(". {}", describe_disabled(&self.disabled_rules)));
        }
        text
    }
}

/// « Règle désactivée… » : la phrase qui nomme les brouillons mis à l'arrêt par l'import.
pub fn describe_disabled(names: &[String]) -> String {
    let list = names
        .iter()
        .map(|n| format!("« {n} »"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {} désactivée(s) à l'import, adresse de webhook incomplète (contenu conservé, \
         à corriger puis réactiver) : {list}",
        names.len(),
        if names.len() > 1 { "règles" } else { "règle" }
    )
}

/// Une sauvegarde refusée : le texte dit pourquoi, sans chemin ni contenu.
#[derive(Debug)]
pub struct Invalid(pub String);

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

/// Pourquoi une sauvegarde n'a pas été mise de côté.
#[derive(Debug)]
pub enum StageError {
    /// Le fichier n'est pas une sauvegarde valable (rien n'a été touché).
    Invalid(String),
    /// Le disque a refusé l'écriture.
    Io(anyhow::Error),
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageError::Invalid(why) => f.write_str(why),
            StageError::Io(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for StageError {}

/// Seuls ces fichiers sont exportés et acceptés à l'import : jamais de chemin libre.
fn is_allowed(name: &str) -> bool {
    if matches!(name, FILE_NAME | PLAN | RULES | CAMERAS) {
        return true;
    }
    name.strip_prefix("history/")
        .and_then(|f| f.strip_suffix(".jsonl"))
        .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Vérifie à fond le contenu de la sauvegarde, comme le serveur le chargerait :
/// identité du contrôleur (clés), plan et règles (forme et bornes de `validate`).
fn analyze(bundle: &Bundle) -> Result<Summary, Invalid> {
    summarize(bundle, true)
}

/// Résumé d'une sauvegarde. `strict` : contrôle complet (import). Sans lui (export), seule la
/// lecture de `controller.json` est exigée : on ne refuse pas d'exporter des données existantes
/// parce qu'elles dépassent une borne du serveur, l'import dira alors précisément ce qui ne va pas.
fn summarize(bundle: &Bundle, strict: bool) -> Result<Summary, Invalid> {
    let controller = bundle.files.get(FILE_NAME).ok_or_else(|| {
        Invalid(format!(
            "{FILE_NAME} manque : ce n'est pas une sauvegarde complète"
        ))
    })?;
    let state: StoredState =
        serde_json::from_str(controller).map_err(|_| Invalid(format!("{FILE_NAME} illisible")))?;
    if strict {
        JsonCertManager::from_identity(&state.identity)
            .and_then(|cm| cm.check_certificates())
            .map_err(|e| {
                Invalid(format!(
                    "{FILE_NAME} : identité du contrôleur invalide ({e:#})"
                ))
            })?;
    }

    let plan = bundle
        .files
        .get(PLAN)
        .map(|text| -> Result<HomePlan, Invalid> {
            let plan: HomePlan =
                serde_json::from_str(text).map_err(|_| Invalid(format!("{PLAN} illisible")))?;
            if strict {
                crate::validate::plan(&plan).map_err(|e| {
                    Invalid(format!("{PLAN} refusé : {}", e.detail.unwrap_or_default()))
                })?;
            }
            Ok(plan)
        })
        .transpose()?;
    let rules = bundle
        .files
        .get(RULES)
        .map(|text| -> Result<RuleSet, Invalid> {
            let rules: RuleSet =
                serde_json::from_str(text).map_err(|_| Invalid(format!("{RULES} illisible")))?;
            if strict {
                crate::validate::rules(&rules).map_err(|e| {
                    Invalid(format!("{RULES} refusé : {}", e.detail.unwrap_or_default()))
                })?;
            }
            Ok(rules)
        })
        .transpose()?;

    let cameras = bundle
        .files
        .get(CAMERAS)
        .map(|text| -> Result<crate::cameras::CameraFile, Invalid> {
            let file: crate::cameras::CameraFile =
                serde_json::from_str(text).map_err(|_| Invalid(format!("{CAMERAS} illisible")))?;
            if strict {
                // Les messages de `validate` citent le nom de la caméra, jamais son adresse.
                file.validate()
                    .map_err(|e| Invalid(format!("{CAMERAS} refusé : {e}")))?;
            }
            Ok(file)
        })
        .transpose()?;

    Ok(Summary {
        created: bundle.created.clone(),
        devices: state.device_ids().len(),
        rooms: plan
            .as_ref()
            .map_or(0, |p| p.floors.iter().map(|f| f.rooms.len()).sum()),
        rules: rules.as_ref().map_or(0, |r| r.rules.len()),
        cameras: cameras.as_ref().map_or(0, |c| c.cameras.len()),
        history_files: bundle
            .files
            .keys()
            .filter(|k| k.starts_with("history/"))
            .count(),
        has_plan: plan.is_some(),
        has_rules: rules.is_some(),
        disabled_rules: Vec::new(),
    })
}

/// Une sauvegarde dont le plan n'a aucun étage (`floors: []`) reste importable : le plan est
/// remplacé par le plan par défaut, comme au chargement (`Hub::new`). Le serveur, lui, refuse
/// un tel plan envoyé par un client (`validate::plan`) ; une vraie sauvegarde ne doit pas être
/// refusée pour autant. Un plan illisible n'est pas touché : l'analyse le refusera.
fn repair_plan(bundle: &mut Bundle) {
    let Some(text) = bundle.files.get(PLAN) else {
        return;
    };
    let Ok(mut plan) = serde_json::from_str::<HomePlan>(text) else {
        return;
    };
    if plan.ensure_floor() {
        if let Ok(fixed) = serde_json::to_string(&plan) {
            log::warn!("{PLAN} sans étage dans la sauvegarde : plan par défaut utilisé");
            bundle.files.insert(PLAN.to_string(), fixed);
        }
    }
}

/// Désactive les règles **activées** dont un webhook n'a pas d'adresse valable (vide, sans
/// `http`, avec un espace…) et rend leurs noms. La v0.2.1 enregistrait ces brouillons ; le
/// serveur refuse de les activer (`validate::rules`), et une seule règle ainsi faisait refuser
/// toute la sauvegarde, sans même dire laquelle. Le contenu est gardé tel quel (seul `enabled`
/// change) : l'utilisateur corrige l'adresse puis réactive. Une règle désactivée ne s'exécute
/// jamais. Un fichier illisible n'est pas touché : l'analyse le refusera.
fn repair_rules(bundle: &mut Bundle) -> Vec<String> {
    let Some(text) = bundle.files.get(RULES) else {
        return Vec::new();
    };
    let Ok(mut set) = serde_json::from_str::<RuleSet>(text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for rule in set.rules.iter_mut().filter(|r| r.enabled) {
        let incomplete = rule.actions.iter().any(|a| {
            matches!(a, protocol::automation::Action::Webhook { url, .. }
                if !protocol::automation::is_webhook_url(url))
        });
        if incomplete {
            rule.enabled = false;
            names.push(if rule.name.trim().is_empty() {
                format!("(sans nom, règle {})", rule.id)
            } else {
                rule.name.clone()
            });
        }
    }
    if !names.is_empty() {
        if let Ok(fixed) = serde_json::to_string(&set) {
            log::warn!(
                "{RULES} : {} règle(s) à webhook incomplet désactivée(s) dans la sauvegarde",
                names.len()
            );
            bundle.files.insert(RULES.to_string(), fixed);
        } else {
            names.clear();
        }
    }
    names
}

fn parse(text: &str) -> Result<(Bundle, Summary), Invalid> {
    let mut bundle: Bundle = serde_json::from_str(text)
        .map_err(|_| Invalid("ce fichier n'est pas une sauvegarde Matter Home".into()))?;
    if bundle.format != FORMAT {
        return Err(Invalid(
            "ce fichier n'est pas une sauvegarde Matter Home".into(),
        ));
    }
    if bundle.version > VERSION {
        return Err(Invalid(format!(
            "sauvegarde créée par une version plus récente de l'application (format {})",
            bundle.version
        )));
    }
    if let Some(bad) = bundle.files.keys().find(|k| !is_allowed(k)) {
        return Err(Invalid(format!(
            "fichier inattendu dans la sauvegarde : {bad}"
        )));
    }
    repair_plan(&mut bundle);
    let disabled = repair_rules(&mut bundle);
    let mut summary = analyze(&bundle)?;
    summary.disabled_rules = disabled;
    Ok((bundle, summary))
}

fn read_bundle(path: &Path) -> Result<(Bundle, Summary)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    Ok(parse(&text)?)
}

/// Dossier parent de `dir`, jamais vide : « . » pour un chemin relatif d'un seul élément
/// (`Path::new("donnees").parent()` est `""`, que le système ne sait pas ouvrir).
fn parent_of(dir: &Path) -> &Path {
    match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Synchronise un dossier sur le disque (ses entrées : créations, renommages).
fn sync_dir(dir: &Path) -> Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("synchronisation de {}", dir.display()))
}

/// Crée `path` (échoue s'il existe) directement en 0600, y écrit `contents` et le synchronise.
fn write_new_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("création de {}", path.display()))?;
    file.write_all(contents)
        .and_then(|_| file.sync_all())
        .with_context(|| format!("écriture de {}", path.display()))
}

/// Écrit `contents` dans `path`, droits 0600 sous Unix dès la création : le fichier contient
/// des clés privées. (Fichier d'export choisi par l'utilisateur ; remplace l'existant.)
fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let _ = std::fs::remove_file(path);
    write_new_private(path, contents.as_bytes())
}

/// Nom de fichier proposé à l'export.
pub fn default_file_name() -> String {
    format!(
        "matter-home-{}.{EXTENSION}",
        chrono::Local::now().format("%Y-%m-%d")
    )
}

/// Exporte le dossier de données `dir` vers le fichier `dest`.
pub fn export(dir: &Path, dest: &Path) -> Result<Summary> {
    let (text, summary) = export_to_string(dir)?;
    write_private(dest, &text)?;
    Ok(summary)
}

/// Sauvegarde de `dir` sous forme de texte (le contenu d'un fichier `.matterhome`).
/// Elle contient les clés privées en clair.
pub fn export_to_string(dir: &Path) -> Result<(String, Summary)> {
    let mut files = BTreeMap::new();
    for name in [FILE_NAME, PLAN, RULES, CAMERAS] {
        let path = dir.join(name);
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("lecture de {}", path.display()))?;
            files.insert(name.to_string(), text);
        }
    }
    if !files.contains_key(FILE_NAME) {
        bail!("aucun appareil appairé pour l'instant : il n'y a rien à exporter");
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("history")) {
        for entry in entries.flatten() {
            let name = format!("history/{}", entry.file_name().to_string_lossy());
            if is_allowed(&name) {
                // Un historique illisible ne doit pas empêcher d'exporter le reste.
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    files.insert(name, text);
                }
            }
        }
    }
    let bundle = Bundle {
        format: FORMAT.into(),
        version: VERSION,
        created: chrono::Local::now().to_rfc3339(),
        files,
    };
    let summary = summarize(&bundle, false).map_err(|e| anyhow!("{e}"))?;
    Ok((serde_json::to_string(&bundle)?, summary))
}

/// Lit et valide une sauvegarde sans rien modifier.
pub fn inspect(src: &Path) -> Result<Summary> {
    read_bundle(src).map(|(_, s)| s)
}

/// Valide une sauvegarde reçue en mémoire sans rien écrire (l'inspection d'un import).
pub fn inspect_text(text: &str) -> Result<Summary, Invalid> {
    parse(text).map(|(_, s)| s)
}

/// Valide `src` puis le met de côté dans `dir` : il sera appliqué au prochain lancement.
pub fn stage_import(dir: &Path, src: &Path) -> Result<Summary> {
    let text =
        std::fs::read_to_string(src).with_context(|| format!("lecture de {}", src.display()))?;
    Ok(stage_import_text(dir, &text)?)
}

/// Comme [`stage_import`], pour une sauvegarde reçue en mémoire (import par l'API : aucune
/// copie intermédiaire des clés sur le disque). Le fichier en attente est créé directement
/// en 0600, écrit puis renommé (jamais à moitié écrit), et le dossier est synchronisé.
pub fn stage_import_text(dir: &Path, text: &str) -> Result<Summary, StageError> {
    let (bundle, summary) = parse(text).map_err(|e| StageError::Invalid(e.0))?;
    let json = serde_json::to_vec(&bundle).map_err(|e| StageError::Io(e.into()))?;
    write_secret(&dir.join(PENDING), &json)
        .and_then(|()| sync_dir(dir))
        .map_err(StageError::Io)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Application de l'import, au démarrage
// ---------------------------------------------------------------------------

/// Panne injectée à un point de contrôle (tests seulement : en service, rien n'est injecté).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum Fault {
    None,
    /// Le processus s'arrête net : plus rien ne s'exécute, aucun nettoyage.
    Crash,
    /// Une opération échoue (disque plein, renommage refusé) : le code doit se rétablir.
    Fail,
}

type Hook<'a> = &'a mut dyn FnMut(&str) -> Fault;

enum ApplyError {
    /// La sauvegarde en attente n'est pas valable : inutile de réessayer.
    Invalid(String),
    /// Échec passager (disque, droits) : rien n'a changé, la sauvegarde reste en attente.
    Transient(anyhow::Error),
    /// Le retour en arrière a lui-même échoué : on ne démarre pas.
    Fatal(anyhow::Error),
    /// Coupure simulée par les tests.
    Crashed,
}

fn checkpoint(hook: Hook<'_>, step: &str) -> Result<(), ApplyError> {
    match hook(step) {
        Fault::None => Ok(()),
        Fault::Crash => Err(ApplyError::Crashed),
        Fault::Fail => Err(ApplyError::Transient(anyhow!("panne simulée ({step})"))),
    }
}

fn transient<E: Into<anyhow::Error>>(context: impl Into<String>) -> impl FnOnce(E) -> ApplyError {
    let context = context.into();
    move |e| ApplyError::Transient(e.into().context(context))
}

/// Dossier voisin de `dir`, nommé `<nom><suffixe>`.
fn sibling(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let name = dir.file_name().context("dossier de données sans nom")?;
    Ok(dir.with_file_name(format!("{}{suffix}", name.to_string_lossy())))
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Copie `from` dans `to` (qui ne doit pas exister) : dossiers et fichiers, mêmes droits,
/// chaque fichier et chaque dossier synchronisés. Les liens symboliques sont ignorés.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::metadata(from).with_context(|| format!("lecture de {}", from.display()))?;
    std::fs::create_dir(to).with_context(|| format!("création de {}", to.display()))?;
    for entry in
        std::fs::read_dir(from).with_context(|| format!("lecture de {}", from.display()))?
    {
        let entry = entry.with_context(|| format!("lecture de {}", from.display()))?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            let contents = std::fs::read(entry.path())
                .with_context(|| format!("lecture de {}", entry.path().display()))?;
            write_new_private(&target, &contents)?;
            std::fs::set_permissions(&target, entry.metadata()?.permissions())
                .with_context(|| format!("droits de {}", target.display()))?;
        }
    }
    std::fs::set_permissions(to, meta.permissions())
        .with_context(|| format!("droits de {}", to.display()))?;
    sync_dir(to)
}

/// Construit le nouveau dossier à côté de `dir`, le synchronise, puis le publie sous son nom
/// « complet » (`nouveau`). Rien de `dir` n'est modifié.
fn prepare(
    dir: &Path,
    partial: &Path,
    complete: &Path,
    bundle: &Bundle,
    hook: Hook<'_>,
) -> Result<(), ApplyError> {
    let io = |what: &'static str| transient(what);
    remove_tree(partial).map_err(io("nettoyage du dossier partiel"))?;
    remove_tree(complete).map_err(io("nettoyage du dossier d'un import précédent"))?;
    std::fs::create_dir(partial).map_err(io("création du dossier partiel"))?;
    // Le dossier de données contient les clés privées : 0700 dans tous les cas, quels que
    // soient les droits de l'ancien dossier (ou son absence).
    #[cfg(unix)]
    std::fs::set_permissions(partial, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .map_err(io("droits du dossier partiel"))?;
    for (i, (rel, text)) in bundle.files.iter().enumerate() {
        write_new_private(&partial.join(rel), text.as_bytes()).map_err(ApplyError::Transient)?;
        checkpoint(hook, &format!("fichier-{i}"))?;
    }
    if partial.join("history").is_dir() {
        sync_dir(&partial.join("history")).map_err(ApplyError::Transient)?;
    }
    let old_server = dir.join(SERVER_DIR);
    if old_server.is_dir() {
        copy_tree(&old_server, &partial.join(SERVER_DIR)).map_err(ApplyError::Transient)?;
    }
    checkpoint(hook, "serveur")?;
    sync_dir(partial).map_err(ApplyError::Transient)?;
    checkpoint(hook, "construit")?;
    // Le renommage est le marqueur « complet ».
    std::fs::rename(partial, complete).map_err(io("publication du nouveau dossier"))?;
    sync_dir(parent_of(dir)).map_err(ApplyError::Transient)?;
    checkpoint(hook, "publie")
}

/// Applique la sauvegarde en attente de `dir`. Rend le résumé et le dossier des anciennes données.
fn apply_with(dir: &Path, hook: Hook<'_>) -> Result<(Summary, PathBuf), ApplyError> {
    let (bundle, summary) =
        read_bundle(&dir.join(PENDING)).map_err(|e| match e.downcast::<Invalid>() {
            Ok(invalid) => ApplyError::Invalid(invalid.0),
            Err(io) => ApplyError::Transient(io),
        })?;
    let parent = parent_of(dir);
    let partial = sibling(dir, PARTIAL_SUFFIX).map_err(ApplyError::Transient)?;
    let complete = sibling(dir, NEW_SUFFIX).map_err(ApplyError::Transient)?;
    let backup = sibling(
        dir,
        &format!(
            ".avant-import-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ),
    )
    .map_err(ApplyError::Transient)?;
    if let Err(e) = prepare(dir, &partial, &complete, &bundle, hook) {
        if !matches!(e, ApplyError::Crashed) {
            let _ = remove_tree(&partial);
            let _ = remove_tree(&complete);
        }
        return Err(e);
    }

    // L'échange : deux renommages. Entre les deux, `dir` n'existe pas ; `recover` reprend.
    if backup.exists() {
        // Deux imports dans la même seconde : on n'écrase pas une sauvegarde existante.
        let _ = remove_tree(&complete);
        return Err(ApplyError::Transient(anyhow!(
            "{} existe déjà",
            backup.display()
        )));
    }
    if let Err(e) = std::fs::rename(dir, &backup) {
        let _ = remove_tree(&complete);
        return Err(ApplyError::Transient(
            anyhow::Error::from(e).context("mise de côté des données actuelles"),
        ));
    }
    let _ = sync_dir(parent);
    let swapped = checkpoint(hook, "ecarte").and_then(|()| {
        std::fs::rename(&complete, dir).map_err(transient("mise en place des nouvelles données"))
    });
    match swapped {
        Ok(()) => {}
        Err(ApplyError::Crashed) => return Err(ApplyError::Crashed),
        Err(failure) => {
            // Retour en arrière : les anciennes données reprennent leur place.
            return match std::fs::rename(&backup, dir) {
                Ok(()) => {
                    let _ = sync_dir(parent);
                    let _ = remove_tree(&complete);
                    Err(failure)
                }
                Err(e) => Err(ApplyError::Fatal(anyhow!(
                    "{e} : impossible de remettre {} en place, ni de terminer l'import \
                     (le nouveau dossier est {}). Renommez l'un des deux en {}.",
                    backup.display(),
                    complete.display(),
                    dir.display()
                ))),
            };
        }
    }
    let _ = sync_dir(parent);
    // La sauvegarde en attente est restée dans l'ancien dossier : elle n'est plus utile.
    let _ = std::fs::remove_file(backup.join(PENDING));
    match checkpoint(hook, "echange") {
        Err(ApplyError::Crashed) => Err(ApplyError::Crashed),
        _ => Ok((summary, backup)),
    }
}

/// À appeler au démarrage, avant tout (y compris la migration de l'ancien dossier) : reprend
/// ou nettoie un import interrompu. `Ok(Some(message))` si quelque chose s'est passé.
///
/// - `dir` absent et `dir.import-nouveau` présent : l'échange a été coupé entre ses deux
///   renommages ; le nouveau dossier, complet par construction, devient `dir`.
/// - Sinon, les restes d'une construction interrompue (`dir.import-partiel`, et le dossier
///   complet d'un import qui n'a pas eu lieu) sont supprimés ; la sauvegarde en attente, restée
///   dans `dir`, sera rejouée depuis le début par [`apply_pending`].
pub fn recover(dir: &Path) -> Result<Option<String>> {
    let partial = sibling(dir, PARTIAL_SUFFIX)?;
    let complete = sibling(dir, NEW_SUFFIX)?;
    let parent = parent_of(dir);
    if !dir.exists() && complete.exists() {
        std::fs::rename(&complete, dir).with_context(|| {
            format!(
                "import interrompu : impossible de mettre {} en place sous le nom {} \
                 (les anciennes données sont dans le dossier voisin « …avant-import-… »). \
                 Aucune nouvelle identité n'est créée ; corrigez le problème ou renommez \
                 le dossier à la main, puis relancez",
                complete.display(),
                dir.display()
            )
        })?;
        sync_dir(parent)?;
        let _ = remove_tree(&partial);
        return Ok(Some(format!(
            "Import interrompu repris : les nouvelles données sont en place dans {}.",
            dir.display()
        )));
    }
    // `dir` existe mais n'a aucune identité (pas de `controller.json`), aucun import en attente,
    // et un dossier complet attend : l'échange a été coupé, puis quelque chose a recréé `dir`
    // (typiquement `create-account` lancé avant la relance, qui ne met que `serveur/` dedans).
    // Supprimer le dossier complet perdrait l'identité et mènerait à une nouvelle fabric : on
    // termine l'échange. Ce `dir` presque vide est mis de côté, jamais supprimé : il peut porter
    // le compte qui vient d'être créé.
    if dir.exists()
        && !dir.join(FILE_NAME).exists()
        && !dir.join(PENDING).exists()
        && complete.join(FILE_NAME).exists()
    {
        let aside = sibling(
            dir,
            &format!(
                ".sans-identite-{}",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ),
        )?;
        std::fs::rename(dir, &aside).with_context(|| {
            format!(
                "import interrompu : impossible de mettre de côté {} (sans identité) pour y \
                 mettre {}. Aucune nouvelle identité n'est créée ; corrigez le problème ou \
                 renommez les dossiers à la main, puis relancez",
                dir.display(),
                complete.display()
            )
        })?;
        std::fs::rename(&complete, dir).with_context(|| {
            format!(
                "import interrompu : {} est mis de côté ({}) mais {} n'a pas pu prendre sa \
                 place. Renommez-le en {} à la main, puis relancez",
                dir.display(),
                aside.display(),
                complete.display(),
                dir.display()
            )
        })?;
        sync_dir(parent)?;
        let _ = remove_tree(&partial);
        return Ok(Some(format!(
            "Import interrompu repris : les nouvelles données sont en place dans {}. Le dossier \
             qui avait été recréé entre-temps, sans identité, est conservé dans {} (le compte \
             du serveur est celui de la sauvegarde ; recréez-le avec « create-account --replace » \
             si besoin).",
            dir.display(),
            aside.display()
        )));
    }
    if remove_tree(&partial).is_err() {
        log::warn!("reste d'un import à nettoyer : {}", partial.display());
    }
    // Ici, soit `dir` a son identité, soit le dossier complet n'en a pas (jamais construit
    // ainsi) : c'est un reste d'un import qui n'a pas eu lieu (typiquement : on y a renoncé en
    // supprimant la sauvegarde en attente). Il porte une copie des clés de la sauvegarde : on le
    // met de côté sous un nom parlant au lieu de le supprimer, c'est à l'utilisateur de le jeter.
    if dir.exists() && !dir.join(PENDING).exists() && complete.exists() {
        let aside = sibling(
            dir,
            &format!(
                ".import-abandonne-{}",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ),
        )?;
        return match std::fs::rename(&complete, &aside) {
            Ok(()) => {
                let _ = sync_dir(parent);
                Ok(Some(format!(
                    "Import abandonné : le dossier préparé pour lui est conservé dans {} \
                     (il contient une copie des clés de la sauvegarde ; supprimez-le quand \
                     vous n'en avez plus besoin).",
                    aside.display()
                )))
            }
            Err(e) => {
                log::warn!(
                    "reste d'un import abandonné non déplacé ({e}) : {}",
                    complete.display()
                );
                Ok(None)
            }
        };
    }
    Ok(None)
}

/// Un import interrompu attend d'être repris dans `dir` ([`recover`]) : le dossier complet
/// d'un import existe alors que `dir` est absent ou n'a pas d'identité.
pub fn recovery_needed(dir: &Path) -> bool {
    sibling(dir, NEW_SUFFIX).is_ok_and(|complete| {
        complete.join(FILE_NAME).exists()
            && !dir.join(FILE_NAME).exists()
            && !dir.join(PENDING).exists()
    })
}

/// Chemin de la sauvegarde mise en attente dans `dir`.
pub fn pending_path(dir: &Path) -> PathBuf {
    dir.join(PENDING)
}

/// Ce qui attend dans `dir`, comparé au fichier `src` qu'on veut importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    /// Rien n'est en attente.
    None,
    /// La sauvegarde en attente a exactement le contenu de `src` : un import de ce fichier a été
    /// interrompu (avant ou pendant l'échange), il peut être repris.
    SameFile,
    /// Une autre sauvegarde (import demandé par l'API, ou d'un autre fichier), ou illisible.
    Other,
}

/// Compare la sauvegarde en attente de `dir` (s'il y en a une) au fichier `src`.
pub fn pending_kind(dir: &Path, src: &Path) -> Result<PendingKind> {
    let pending = dir.join(PENDING);
    if !pending.exists() {
        return Ok(PendingKind::None);
    }
    let (wanted, _) = read_bundle(src)?;
    Ok(match read_bundle(&pending) {
        Ok((staged, _)) if staged.files == wanted.files => PendingKind::SameFile,
        _ => PendingKind::Other,
    })
}

/// Abandonne un import en attente : retire la sauvegarde mise de côté, puis met de côté le dossier
/// préparé « …import-nouveau » (reste d'un import interrompu) sous « …import-abandonne-<date> ».
/// L'appelant tient le verrou d'instance et a déjà repris un échange interrompu ([`recover`]).
/// Rend ce qui a été fait, en phrases.
///
/// Si l'import a été coupé entre ses deux renommages (`dir` absent, ou recréé sans identité,
/// alors que le dossier préparé est complet), il est réellement abandonné : les anciennes données
/// (« …avant-import-<date> » le plus récent, où l'import les avait mises de côté) reprennent la
/// place de `dir`, et le dossier préparé est mis de côté.
pub fn abandon(dir: &Path) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let complete = sibling(dir, NEW_SUFFIX)?;
    if !dir.join(FILE_NAME).exists()
        && !dir.join(PENDING).exists()
        && complete.join(FILE_NAME).exists()
    {
        let previous = newest_previous(dir)?.with_context(|| {
            format!(
                "l'import a été coupé en plein échange et aucun dossier « …avant-import-… » \
                 n'est là pour remettre les anciennes données en place : impossible de \
                 l'abandonner. Relancez « matter-home-server import FICHIER » pour le terminer \
                 (le dossier préparé est {})",
                complete.display()
            )
        })?;
        if dir.exists() {
            let aside = sibling(
                dir,
                &format!(
                    ".sans-identite-{}",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ),
            )?;
            std::fs::rename(dir, &aside)
                .with_context(|| format!("mise de côté de {}", dir.display()))?;
            done.push(format!(
                "Dossier recréé sans identité mis de côté : {}.",
                aside.display()
            ));
        }
        std::fs::rename(&previous, dir)
            .with_context(|| format!("remise en place de {}", previous.display()))?;
        sync_dir(parent_of(dir))?;
        done.push(format!(
            "Import coupé en plein échange : les anciennes données ({}) sont remises en place \
             dans {}.",
            previous.display(),
            dir.display()
        ));
    }
    let pending = dir.join(PENDING);
    if pending.exists() {
        std::fs::remove_file(&pending)
            .with_context(|| format!("suppression de {}", pending.display()))?;
        done.push(format!(
            "Sauvegarde en attente retirée ({}).",
            pending.display()
        ));
    }
    if let Some(note) = recover(dir)? {
        done.push(note);
    }
    Ok(done)
}

/// Le plus récent des dossiers « <dir>.avant-import-<date> » qui porte une identité.
fn newest_previous(dir: &Path) -> Result<Option<PathBuf>> {
    let name = dir.file_name().context("dossier de données sans nom")?;
    let prefix = format!("{}.avant-import-", name.to_string_lossy());
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent_of(dir))
        .with_context(|| format!("lecture de {}", parent_of(dir).display()))?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .filter(|p| p.join(FILE_NAME).is_file())
        .collect();
    found.sort();
    Ok(found.pop())
}

/// L'identité Matter d'un dossier ou d'une sauvegarde, pour la comparer avant un import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityBrief {
    pub fabric_id: u64,
    /// Nombre d'appareils (nœuds et appareils pontés).
    pub devices: usize,
    /// Un libellé par appareil : « nom (0xidentifiant) ».
    pub nodes: Vec<String>,
}

impl IdentityBrief {
    fn of(state: &StoredState) -> Self {
        Self {
            fabric_id: state.identity.fabric_id,
            devices: state.device_ids().len(),
            nodes: state
                .nodes
                .iter()
                .map(|n| {
                    let name = n.custom_name.as_deref().unwrap_or(&n.name);
                    format!("{name} ({:#x})", n.node_id)
                })
                .collect(),
        }
    }

    /// « fabric 0x1234, 3 appareil(s) : a (0x1), b (0x2), c (0x3) ».
    pub fn describe(&self) -> String {
        let nodes = if self.nodes.is_empty() {
            "aucun nœud appairé".to_string()
        } else {
            format!("{} nœud(s) : {}", self.nodes.len(), self.nodes.join(", "))
        };
        format!("fabric {:#x}, {nodes}", self.fabric_id)
    }
}

/// Identité portée par le fichier `src` (une sauvegarde valide).
pub fn identity_in_file(src: &Path) -> Result<IdentityBrief> {
    let (bundle, _) = read_bundle(src)?;
    let text = bundle
        .files
        .get(FILE_NAME)
        .with_context(|| format!("{FILE_NAME} manque dans la sauvegarde"))?;
    let state: StoredState = serde_json::from_str(text).context("identité illisible")?;
    Ok(IdentityBrief::of(&state))
}

/// Identité portée par une sauvegarde reçue en mémoire (déjà validée ou non : refus si illisible).
pub fn identity_in_text(text: &str) -> Result<IdentityBrief, Invalid> {
    let (bundle, _) = parse(text)?;
    let controller = bundle
        .files
        .get(FILE_NAME)
        .ok_or_else(|| Invalid(format!("{FILE_NAME} manque")))?;
    let state: StoredState =
        serde_json::from_str(controller).map_err(|_| Invalid(format!("{FILE_NAME} illisible")))?;
    Ok(IdentityBrief::of(&state))
}

/// Identité du dossier de données `dir` : `None` s'il n'a pas de `controller.json`, une erreur
/// s'il est illisible (le doute profite à la prudence : l'appelant ne doit rien écraser).
pub fn identity_in_dir(dir: &Path) -> Result<Option<IdentityBrief>> {
    let path = dir.join(FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    let state = crate::persistence::load(&path)?
        .with_context(|| format!("{} a disparu", path.display()))?;
    Ok(Some(IdentityBrief::of(&state)))
}

/// Issue de [`apply_loop`].
enum Applied {
    Done(Summary, PathBuf),
    /// La sauvegarde en attente n'est pas valable : elle a été retirée, rien n'a changé.
    Rejected(String),
}

/// Pourquoi [`apply_loop`] n'a pas appliqué la sauvegarde en attente.
enum LoopError {
    /// Plusieurs essais ont échoué : les données actuelles sont intactes, la sauvegarde reste.
    Exhausted(anyhow::Error),
    /// État à réparer à la main (retour en arrière impossible) ou coupure simulée.
    Fatal(anyhow::Error),
}

/// Applique la sauvegarde en attente de `dir` (au plus [`APPLY_ATTEMPTS`] essais).
fn apply_loop(dir: &Path, hook: Hook<'_>) -> Result<Applied, LoopError> {
    let mut last = None;
    for attempt in 1..=APPLY_ATTEMPTS {
        match apply_with(dir, hook) {
            Ok((summary, backup)) => return Ok(Applied::Done(summary, backup)),
            Err(ApplyError::Invalid(why)) => {
                // Une sauvegarde invalide ne doit pas être retentée à chaque lancement.
                let _ = std::fs::remove_file(dir.join(PENDING));
                return Ok(Applied::Rejected(why));
            }
            Err(ApplyError::Transient(e)) => {
                log::warn!("import non appliqué (essai {attempt}/{APPLY_ATTEMPTS}) : {e:#}");
                last = Some(e);
                if attempt < APPLY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
            Err(ApplyError::Fatal(e)) => return Err(LoopError::Fatal(e)),
            Err(ApplyError::Crashed) => return Err(LoopError::Fatal(anyhow!("arrêt simulé"))),
        }
    }
    Err(LoopError::Exhausted(
        last.unwrap_or_else(|| anyhow!("raison inconnue")),
    ))
}

/// À appeler au démarrage, avant tout chargement : reprend un import interrompu puis applique
/// l'import en attente. `Ok(Some(message))` à journaliser si quelque chose s'est passé ;
/// `Err` si le serveur ne doit pas démarrer (import non appliqué malgré plusieurs essais, ou
/// état à réparer à la main) : il ne crée jamais de nouvelle identité à la place.
pub fn apply_pending(dir: &Path) -> Result<Option<String>> {
    apply_pending_with(dir, &mut |_| Fault::None)
}

fn apply_pending_with(dir: &Path, hook: Hook<'_>) -> Result<Option<String>> {
    let mut notes: Vec<String> = Vec::new();
    notes.extend(recover(dir)?);
    if !dir.join(PENDING).exists() {
        return Ok((!notes.is_empty()).then(|| notes.join(" ")));
    }
    match apply_loop(dir, hook) {
        Ok(Applied::Done(summary, backup)) => {
            notes.push(format!(
                "Import terminé ({}). Anciennes données conservées dans {}.",
                summary.describe(),
                backup.display()
            ));
            Ok(Some(notes.join(" ")))
        }
        Ok(Applied::Rejected(why)) => {
            notes.push(format!(
                "Import impossible, données actuelles conservées : {why}"
            ));
            Ok(Some(notes.join(" ")))
        }
        Err(LoopError::Fatal(e)) => Err(e),
        // Les données actuelles sont intactes et la sauvegarde reste en attente (le serveur a
        // répondu 202 : on ne la supprime pas). Démarrer sans elle l'appliquerait peut-être des
        // jours plus tard, par surprise : on s'arrête, l'opérateur voit pourquoi.
        Err(LoopError::Exhausted(e)) => {
            let pending = dir.join(PENDING);
            Err(e.context(format!(
                "l'import en attente n'a pas pu être appliqué après {APPLY_ATTEMPTS} essais ; \
                 les données actuelles sont intactes et la sauvegarde reste en attente dans {p}. \
                 Pour réessayer : corrigez la cause (espace disque, droits) et relancez le \
                 serveur. Pour y renoncer : arrêtez le service, lancez « matter-home-server \
                 import --abandon --data-dir {d} » (il retire ce fichier, {p}, et met de côté \
                 le dossier préparé « …{NEW_SUFFIX} » en « …import-abandonne-… »), puis \
                 relancez le service, qui démarrera avec les données actuelles",
                p = pending.display(),
                d = dir.display()
            )))
        }
    }
}

/// Résultat d'un import fait à la demande ([`import_file`]).
#[derive(Debug)]
pub struct Imported {
    pub summary: Summary,
    /// Où sont les anciennes données de `dir`.
    pub previous: PathBuf,
    /// Reprise d'un import interrompu faite avant celui-ci, s'il y en a eu une.
    pub resumed: Option<String>,
}

/// Importe le fichier `src` dans `dir` tout de suite (sous-commande `import` : le serveur ne
/// tourne pas). Même code atomique que l'import par l'API : validation complète, dossier
/// construit à côté, deux renommages, reprise possible à chaque instant. `dir` peut ne pas
/// exister (premier déploiement) ; il est alors créé en 0700.
///
/// L'appelant tient le verrou d'instance. Un échec laisse les données actuelles intactes et
/// ne laisse **aucune** sauvegarde en attente : un import demandé à la main n'est pas rejoué
/// plus tard par surprise.
pub fn import_file(dir: &Path, src: &Path) -> Result<Imported> {
    import_file_with(dir, src, &mut |_| Fault::None)
}

fn import_file_with(dir: &Path, src: &Path, hook: Hook<'_>) -> Result<Imported> {
    let text =
        std::fs::read_to_string(src).with_context(|| format!("lecture de {}", src.display()))?;
    // Validation complète avant la moindre écriture.
    let (bundle, staged) = parse(&text)?;
    let mut notes: Vec<String> = Vec::new();
    notes.extend(recover(dir)?);
    let pending = dir.join(PENDING);
    if pending.exists() {
        // Une sauvegarde identique à ce fichier : l'import précédent a été coupé après la mise de
        // côté. On le reprend là où il en était. Toute autre sauvegarde n'est pas à nous.
        let same = read_bundle(&pending).is_ok_and(|(b, _)| b.files == bundle.files);
        if !same {
            bail!(
                "une autre sauvegarde est déjà en attente dans {p} (import demandé par l'API, ou \
                 import d'un autre fichier interrompu). Lancez le serveur pour qu'il l'applique, \
                 ou renoncez-y avec « matter-home-server import --abandon --data-dir {d} »",
                p = pending.display(),
                d = dir.display()
            );
        }
        notes.push("Import interrompu de ce même fichier : il est repris.".into());
    } else {
        if !dir.exists() {
            crate::persistence::ensure_private_dir(dir)?;
        }
        stage_import_text(dir, &text).map_err(|e| anyhow!("{e}"))?;
    }
    let resumed = (!notes.is_empty()).then(|| notes.join(" "));
    match apply_loop(dir, hook) {
        Ok(Applied::Done(mut summary, previous)) => Ok(Imported {
            // La sauvegarde en attente est déjà réparée : les noms viennent de la validation.
            summary: {
                summary.disabled_rules = staged.disabled_rules;
                summary
            },
            previous,
            resumed,
        }),
        Ok(Applied::Rejected(why)) => {
            bail!("import impossible, données actuelles conservées : {why}")
        }
        Err(LoopError::Exhausted(e)) => {
            let _ = std::fs::remove_file(dir.join(PENDING));
            Err(e.context(format!(
                "l'import n'a pas pu être appliqué après {APPLY_ATTEMPTS} essais ; les données \
                 actuelles sont intactes et rien n'est resté en attente. Corrigez la cause \
                 (espace disque, droits) puis relancez la commande"
            )))
        }
        Err(LoopError::Fatal(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Clé P-256 valide (le scalaire 0x0101…01) : les sauvegardes d'essai doivent passer
    /// `JsonCertManager::from_identity` comme de vraies.
    const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    fn controller(name: &str) -> String {
        let id = crate::matter::certs::test_identity(KEY);
        format!(
            r#"{{"version":1,"identity":{{"fabric_id":1,"controller_node_id":2,
            "ca_private_key":"{KEY}","ca_cert_der":"{}","controller_private_key":"{KEY}",
            "controller_cert_der":"{}"}},"nodes":[{{"node_id":7,"name":"{name}"}}]}}"#,
            id.ca_cert_der, id.controller_cert_der
        )
    }

    fn temp(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("mhd-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn plan_json() -> String {
        serde_json::to_string(&HomePlan::default()).unwrap()
    }

    fn sample_dir(base: &Path) -> PathBuf {
        let dir = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(dir.join("history")).unwrap();
        std::fs::write(dir.join(FILE_NAME), controller("Capteur")).unwrap();
        std::fs::write(dir.join(PLAN), plan_json()).unwrap();
        std::fs::write(dir.join("history/B992B180DAC7AD3B.jsonl"), "{\"t\":1}\n").unwrap();
        std::fs::write(dir.join("history/notes.txt"), "ignoré").unwrap();
        dir
    }

    /// Dossier `serveur/` d'essai : compte, clé et certificat, droits 0700/0600.
    fn add_server_dir(dir: &Path) {
        let secret = dir.join(SERVER_DIR);
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (name, text) in [
            ("tls-key.pem", "CLE PRIVEE DU SERVEUR"),
            ("tls-cert.pem", "CERTIFICAT"),
            ("compte.json", "{\"username\":\"hamdi\"}"),
        ] {
            std::fs::write(secret.join(name), text).unwrap();
            std::fs::set_permissions(secret.join(name), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
    }

    /// Contenu et droits de tous les fichiers de `dir`.
    fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, u32)> {
        let mut out = BTreeMap::new();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        out.insert(".".into(), (Vec::new(), mode(dir)));
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            out.insert(
                name,
                (std::fs::read(entry.path()).unwrap(), mode(&entry.path())),
            );
        }
        out
    }

    /// Chemin relatif → contenu, pour tous les fichiers sous `dir` (sous-dossiers compris).
    fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn bundle_text(files: &[(&str, String)]) -> String {
        let bundle = Bundle {
            format: FORMAT.into(),
            version: VERSION,
            created: "2026-11-09T10:00:00+01:00".into(),
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        };
        serde_json::to_string(&bundle).unwrap()
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn backups_of(dir: &Path) -> Vec<PathBuf> {
        let prefix = format!(
            "{}.avant-import-",
            dir.file_name().unwrap().to_string_lossy()
        );
        std::fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
            .collect()
    }

    #[test]
    fn allowed_names() {
        assert!(is_allowed("controller.json") && is_allowed("history/3D76.jsonl"));
        for bad in [
            "../controller.json",
            "history/../x.jsonl",
            "history/x.jsonl",
            "history/.jsonl",
            "autre.json",
            "/etc/passwd",
        ] {
            assert!(!is_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn export_import_round_trip() {
        let base = temp("rt");
        let old = sample_dir(&base);
        let file = base.join("sauvegarde.matterhome");
        let summary = export(&old, &file).unwrap();
        assert_eq!(summary.devices, 1);
        assert_eq!(summary.history_files, 1);
        assert!(summary.has_plan && !summary.has_rules);
        assert_eq!(inspect(&file).unwrap(), summary);
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // « Nouveau Mac » : dossier avec une ancienne identité à remplacer.
        let new = base
            .join("neuf")
            .join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join(FILE_NAME), "ancien").unwrap();
        assert_eq!(stage_import(&new, &file).unwrap(), summary);
        // Rien n'est touché avant le lancement suivant.
        assert_eq!(
            std::fs::read_to_string(new.join(FILE_NAME)).unwrap(),
            "ancien"
        );

        let msg = apply_pending(&new).unwrap().unwrap();
        assert!(msg.contains("Import terminé"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(new.join(FILE_NAME)).unwrap(),
            controller("Capteur")
        );
        assert!(new.join("history/B992B180DAC7AD3B.jsonl").exists());
        assert!(!new.join("history/notes.txt").exists());
        assert!(!new.join(PENDING).exists());
        // Pas de reste de construction à côté.
        assert!(!sibling(&new, PARTIAL_SUFFIX).unwrap().exists());
        assert!(!sibling(&new, NEW_SUFFIX).unwrap().exists());
        // Les anciennes données sont gardées à côté.
        let kept = backups_of(&new);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read_to_string(kept[0].join(FILE_NAME)).unwrap(),
            "ancien"
        );
        // Deuxième lancement : rien à faire.
        assert!(apply_pending(&new).unwrap().is_none());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_bad_files() {
        let base = temp("bad");
        let file = base.join("x.matterhome");
        std::fs::write(&file, "pas du json").unwrap();
        assert!(inspect(&file).is_err());
        std::fs::write(
            &file,
            r#"{"format":"autre","version":1,"created":"","files":{}}"#,
        )
        .unwrap();
        assert!(inspect(&file).is_err());
        let evil = format!(
            r#"{{"format":"{FORMAT}","version":1,"created":"2026-01-01","files":{{"controller.json":{},"../../evil":"x"}}}}"#,
            serde_json::to_string(&controller("Capteur")).unwrap()
        );
        std::fs::write(&file, evil).unwrap();
        assert!(inspect(&file)
            .unwrap_err()
            .to_string()
            .contains("inattendu"));
        let future =
            format!(r#"{{"format":"{FORMAT}","version":99,"created":"2026-01-01","files":{{}}}}"#);
        std::fs::write(&file, future).unwrap();
        assert!(inspect(&file).is_err());
        // Pas de controller.json : refusé, et un export sans appareil aussi.
        let empty = base.join("vide");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(export(&empty, &base.join("o.matterhome")).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Une sauvegarde bien formée mais dont le contenu ne se chargerait pas est refusée avant
    /// tout dépôt : rien n'est écrit, ni fichier en attente ni reste.
    #[test]
    fn deep_validation_refuses_before_touching_anything() {
        let base = temp("deep");
        let dir = sample_dir(&base);
        add_server_dir(&dir);
        let before = listing(&dir);
        let siblings_before = listing(&base);

        let bad_key = controller("X").replace(KEY, "aa");
        let not_hex = controller("X").replace(KEY, &"zz".repeat(32));
        let zero_key = controller("X").replace(KEY, &"00".repeat(32));
        let no_identity = r#"{"version":1,"nodes":[]}"#.to_string();
        let long_name = {
            let mut plan = HomePlan::default();
            plan.floors[0].name = "x".repeat(10_000);
            serde_json::to_string(&plan).unwrap()
        };
        // Deux règles de même identifiant (un webhook incomplet, lui, n'est plus un refus : la
        // règle est désactivée, voir `draft_webhooks_are_disabled_with_their_content_not_refused`).
        let evil_rules = r#"{"version":1,"rules":[
            {"id":1,"name":"r","enabled":true,
            "trigger":{"type":"Time","hour":7,"minute":0,"days":[true,true,true,true,true,true,true]},
            "conditions":[],"actions":[],"cooldown_s":60},
            {"id":1,"name":"r2","enabled":true,
            "trigger":{"type":"Time","hour":99,"minute":0,"days":[true,true,true,true,true,true,true]},
            "conditions":[],"actions":[],"cooldown_s":60}]}"#
            .to_string();
        let cases: Vec<(&str, Vec<(&str, String)>)> = vec![
            ("clé trop courte", vec![(FILE_NAME, bad_key)]),
            ("clé non hexadécimale", vec![(FILE_NAME, not_hex)]),
            ("clé nulle", vec![(FILE_NAME, zero_key)]),
            ("sans identité", vec![(FILE_NAME, no_identity)]),
            (
                "plan illisible",
                vec![(FILE_NAME, controller("X")), (PLAN, "pas du json".into())],
            ),
            (
                "plan hors bornes",
                vec![(FILE_NAME, controller("X")), (PLAN, long_name)],
            ),
            (
                "règles illisibles",
                vec![(FILE_NAME, controller("X")), (RULES, "[1,2".into())],
            ),
            (
                "règles refusées par validate",
                vec![(FILE_NAME, controller("X")), (RULES, evil_rules)],
            ),
        ];
        for (what, files) in cases {
            match stage_import_text(&dir, &bundle_text(&files)) {
                Err(StageError::Invalid(why)) => {
                    assert!(!why.contains(KEY) && !why.contains(dir.to_str().unwrap()));
                }
                other => panic!("{what} : attendu un refus, reçu {other:?}"),
            }
            assert!(inspect_text(&bundle_text(&files)).is_err(), "{what}");
            assert_eq!(listing(&dir), before, "{what} : le dossier a changé");
            assert_eq!(listing(&base), siblings_before, "{what} : reste à côté");
        }
        // Et une bonne sauvegarde, elle, passe, avec son résumé.
        let good = bundle_text(&[(FILE_NAME, controller("X")), (PLAN, plan_json())]);
        assert!(inspect_text(&good).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Une vraie sauvegarde dont le plan n'a aucun étage n'est pas refusée : elle s'importe avec
    /// le plan par défaut (le serveur refuse pourtant un tel plan envoyé par un client).
    #[test]
    fn a_backup_with_an_empty_plan_is_imported_with_the_default_plan() {
        let base = temp("emptyplan");
        let dir = sample_dir(&base);
        let text = bundle_text(&[
            (FILE_NAME, controller("X")),
            (PLAN, r#"{"version":1,"floors":[]}"#.into()),
        ]);
        assert!(crate::validate::plan(
            &serde_json::from_str(r#"{"version":1,"floors":[]}"#).unwrap()
        )
        .is_err());
        let summary = inspect_text(&text).unwrap();
        assert!(summary.has_plan);
        stage_import_text(&dir, &text).unwrap();
        let pending: Bundle =
            serde_json::from_str(&std::fs::read_to_string(dir.join(PENDING)).unwrap()).unwrap();
        let staged: HomePlan = serde_json::from_str(&pending.files[PLAN]).unwrap();
        assert_eq!(staged, HomePlan::default());
        apply_pending(&dir).unwrap().unwrap();
        let applied: HomePlan =
            serde_json::from_str(&std::fs::read_to_string(dir.join(PLAN)).unwrap()).unwrap();
        assert_eq!(applied, HomePlan::default());
        std::fs::remove_dir_all(base).unwrap();
    }

    fn camera_file(names: &[&str]) -> String {
        let mut book = crate::cameras::CameraBook::default();
        for name in names {
            book.add(&crate::cameras::CameraDraft {
                name: (*name).into(),
                url: "rtsp://192.0.2.9:554/live/ch00_0".into(),
                sub_url: "rtsp://192.0.2.9:554/live/ch00_1".into(),
                username: "admin".into(),
                password: Some("mdp-de-la-camera".into()),
            })
            .unwrap();
        }
        serde_json::to_string(&crate::cameras::CameraFile {
            version: 1,
            cameras: book.cameras().to_vec(),
        })
        .unwrap()
    }

    #[test]
    fn cameras_are_part_of_the_backup_and_survive_export_and_import() {
        assert!(is_allowed("cameras.json"));
        let base = temp("cameras");
        let old = sample_dir(&base);
        let cameras = camera_file(&["Entrée", "Jardin"]);
        std::fs::write(old.join(crate::cameras::FILE_NAME), &cameras).unwrap();
        let file = base.join("sauvegarde.matterhome");
        let summary = export(&old, &file).unwrap();
        assert_eq!(summary.cameras, 2);
        assert!(
            summary.describe().contains("2 caméra(s)"),
            "{}",
            summary.describe()
        );
        assert_eq!(inspect(&file).unwrap().cameras, 2);
        // Les mots de passe voyagent dans le fichier (c'est dit à l'utilisateur) : il se garde
        // comme les clés du contrôleur.
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("mdp-de-la-camera"));

        let new = base
            .join("neuf")
            .join("dev.matter-home-desktop.MatterHomeDesktop");
        std::fs::create_dir_all(&new).unwrap();
        stage_import(&new, &file).unwrap();
        apply_pending(&new).unwrap().unwrap();
        let restored = new.join(crate::cameras::FILE_NAME);
        assert_eq!(std::fs::read_to_string(&restored).unwrap(), cameras);
        assert_eq!(
            std::fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(crate::cameras::load(&new).len(), 2);
        // Une sauvegarde sans caméras (ancienne version) reste valable : zéro caméra.
        let plain = bundle_text(&[(FILE_NAME, controller("Capteur"))]);
        assert_eq!(inspect_text(&plain).unwrap().cameras, 0);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn invalid_cameras_refuse_the_backup_without_repeating_the_secret() {
        let good = camera_file(&["Entrée"]);
        for (what, text) in [
            ("illisible", "pas du json".to_string()),
            ("version", good.replace("\"version\":1", "\"version\":9")),
            (
                "adresse avec identifiants",
                good.replace(
                    "rtsp://192.0.2.9:554/live/ch00_0",
                    "rtsp://admin:mdp-de-la-camera@192.0.2.9/x",
                ),
            ),
            (
                "http",
                good.replace("rtsp://192.0.2.9:554/live/ch00_0", "http://192.0.2.9/x"),
            ),
        ] {
            let bundle = bundle_text(&[
                (FILE_NAME, controller("X")),
                (crate::cameras::FILE_NAME, text),
            ]);
            let why = inspect_text(&bundle).unwrap_err().0;
            assert!(why.contains("cameras.json"), "{what} : {why}");
            assert!(
                !why.contains("mdp-de-la-camera") && !why.contains("192.0.2.9"),
                "{what} : {why}"
            );
        }
    }

    #[test]
    fn summary_counts_devices_rooms_rules_and_date() {
        let plan = r#"{"version":1,"floors":[
            {"id":1,"name":"RDC","rooms":[
                {"id":10,"name":"Salon","x":0.0,"y":0.0,"w":4.0,"h":3.0},
                {"id":11,"name":"Cuisine","x":4.0,"y":0.0,"w":3.0,"h":3.0}]},
            {"id":2,"name":"Étage","rooms":[
                {"id":12,"name":"Chambre","x":0.0,"y":0.0,"w":3.0,"h":3.0}]}]}"#;
        let rules = serde_json::to_string(&RuleSet {
            version: 1,
            rules: vec![
                protocol::automation::Rule::new(1),
                protocol::automation::Rule::new(2),
            ],
        })
        .unwrap();
        let text = bundle_text(&[
            (FILE_NAME, controller("Capteur")),
            (PLAN, plan.into()),
            (RULES, rules),
        ]);
        let summary = inspect_text(&text).unwrap();
        assert_eq!((summary.devices, summary.rooms, summary.rules), (1, 3, 2));
        assert_eq!(summary.created, "2026-11-09T10:00:00+01:00");
        assert!(summary.describe().contains("2026-11-09"));
    }

    #[test]
    fn invalid_pending_is_dropped_and_data_kept() {
        let base = temp("pend");
        let dir = sample_dir(&base);
        std::fs::write(dir.join(PENDING), "cassé").unwrap();
        let msg = apply_pending(&dir).unwrap().unwrap();
        assert!(msg.contains("Import impossible"), "{msg}");
        assert!(!dir.join(PENDING).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            controller("Capteur")
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Le fichier en attente naît en 0600, s'écrit par renommage (aucun fichier temporaire
    /// ne reste) et un second dépôt remplace le premier en entier.
    #[test]
    fn pending_file_is_private_and_written_atomically() {
        let base = temp("pending");
        let dir = sample_dir(&base);
        let first = bundle_text(&[(FILE_NAME, controller("Premier"))]);
        let second = bundle_text(&[(FILE_NAME, controller("Second"))]);
        stage_import_text(&dir, &first).unwrap();
        let mode = std::fs::metadata(dir.join(PENDING))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        stage_import_text(&dir, &second).unwrap();
        assert!(std::fs::read_to_string(dir.join(PENDING))
            .unwrap()
            .contains("Second"));
        assert!(listing(&dir).iter().all(|n| !n.ends_with(".tmp")));
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Le dossier du serveur réseau (compte, sessions, certificat) ne part pas dans un export,
    /// n'est pas accepté dans une sauvegarde, et passe tel quel (octets et droits) à l'import.
    #[test]
    fn server_dir_is_neither_exported_nor_replaced_by_import() {
        let base = temp("serveur");
        let dir = sample_dir(&base);
        add_server_dir(&dir);
        let secret_before = snapshot(&dir.join(SERVER_DIR));

        // Export : le dossier du serveur n'y figure pas.
        let (text, _) = export_to_string(&dir).unwrap();
        assert!(!text.contains("CLE PRIVEE DU SERVEUR") && !text.contains("compte.json"));
        assert!(!text.contains(SERVER_DIR));

        // Une sauvegarde qui prétend en fournir un est refusée.
        let evil = bundle_text(&[
            (FILE_NAME, controller("Capteur")),
            ("serveur/tls-key.pem", "x".into()),
        ]);
        assert!(stage_import_text(&dir, &evil).is_err());
        assert!(!dir.join(PENDING).exists());

        // Import : les données changent, le dossier du serveur reste identique.
        let other = text.replace("Capteur", "Capteur importé");
        stage_import_text(&dir, &other).unwrap();
        let msg = apply_pending(&dir).unwrap().unwrap();
        assert!(msg.contains("Import terminé"), "{msg}");
        assert!(std::fs::read_to_string(dir.join(FILE_NAME))
            .unwrap()
            .contains("Capteur importé"));
        assert_eq!(snapshot(&dir.join(SERVER_DIR)), secret_before);
        std::fs::remove_dir_all(base).unwrap();
    }

    fn marker_names(dir: &Path) -> (PathBuf, PathBuf) {
        (
            sibling(dir, PARTIAL_SUFFIX).unwrap(),
            sibling(dir, NEW_SUFFIX).unwrap(),
        )
    }

    /// Une coupure à n'importe quel point de contrôle de l'application : le dossier de données
    /// n'est jamais perdu, le compte et le certificat sont intacts, et le lancement suivant
    /// termine l'import. Jamais de dossier vide, donc jamais de nouvelle identité.
    #[test]
    fn an_interruption_at_any_step_never_loses_the_identity() {
        let imported = controller("Importé");
        let old = controller("Capteur");
        let mut k = 0usize;
        let mut steps: Vec<String> = Vec::new();
        loop {
            let base = temp(&format!("crash{k}"));
            let dir = sample_dir(&base);
            add_server_dir(&dir);
            let secret_before = snapshot(&dir.join(SERVER_DIR));
            let (partial, complete) = marker_names(&dir);
            stage_import_text(
                &dir,
                &bundle_text(&[
                    (FILE_NAME, imported.clone()),
                    ("history/AB12.jsonl", "{\"t\":2}\n".into()),
                ]),
            )
            .unwrap();

            let mut seen: Vec<String> = Vec::new();
            let result = apply_with(&dir, &mut |step| {
                seen.push(step.to_string());
                if seen.len() - 1 == k {
                    Fault::Crash
                } else {
                    Fault::None
                }
            });
            let crashed = matches!(result, Err(ApplyError::Crashed));
            if crashed {
                let at = seen.last().cloned().unwrap();
                // L'état laissé par la coupure : toujours l'un de ces trois, jamais un vide.
                let current = std::fs::read_to_string(dir.join(FILE_NAME)).ok();
                let mid_swap = !dir.exists() && complete.exists();
                assert!(
                    current.as_deref() == Some(old.as_str())
                        || current.as_deref() == Some(imported.as_str())
                        || mid_swap,
                    "coupure à « {at} » : état inattendu ({current:?})"
                );
                let live = if dir.exists() { &dir } else { &complete };
                assert_eq!(
                    snapshot(&live.join(SERVER_DIR)),
                    secret_before,
                    "coupure à « {at} » : le dossier du serveur a changé"
                );
                // Le lancement suivant (recover puis apply_pending).
                apply_pending(&dir).unwrap_or_else(|e| panic!("coupure à « {at} » : {e:#}"));
            } else {
                steps = seen;
                result.ok().expect("l'import doit réussir sans coupure");
            }
            // État final identique, quel que soit le point de coupure.
            assert_eq!(
                std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
                imported,
                "coupure n° {k}"
            );
            assert!(dir.join("history/AB12.jsonl").exists(), "coupure n° {k}");
            assert!(!dir.join("history/B992B180DAC7AD3B.jsonl").exists());
            assert_eq!(
                snapshot(&dir.join(SERVER_DIR)),
                secret_before,
                "coupure n° {k}"
            );
            assert!(!dir.join(PENDING).exists(), "coupure n° {k}");
            assert!(!partial.exists() && !complete.exists(), "coupure n° {k}");
            let kept = backups_of(&dir);
            assert_eq!(kept.len(), 1, "coupure n° {k} : {kept:?}");
            assert_eq!(
                std::fs::read_to_string(kept[0].join(FILE_NAME)).unwrap(),
                old
            );
            std::fs::remove_dir_all(&base).unwrap();
            if !crashed {
                break;
            }
            k += 1;
            assert!(k < 100);
        }
        // Les points de contrôle couvrent bien toute la séquence.
        for needed in [
            "fichier-0",
            "serveur",
            "construit",
            "publie",
            "ecarte",
            "echange",
        ] {
            assert!(steps.iter().any(|s| s == needed), "{needed} dans {steps:?}");
        }
    }

    /// Une panne (et non une coupure) à chaque point de contrôle : tout est remis en place,
    /// la sauvegarde reste en attente, et un nouvel essai réussit.
    #[test]
    fn a_failure_at_any_step_rolls_back_and_keeps_the_pending_import() {
        let imported = controller("Importé");
        let old = controller("Capteur");
        let mut k = 0usize;
        loop {
            let base = temp(&format!("fail{k}"));
            let dir = sample_dir(&base);
            add_server_dir(&dir);
            let secret_before = snapshot(&dir.join(SERVER_DIR));
            let (partial, complete) = marker_names(&dir);
            stage_import_text(&dir, &bundle_text(&[(FILE_NAME, imported.clone())])).unwrap();

            let mut count = 0usize;
            let result = apply_with(&dir, &mut |_| {
                count += 1;
                if count - 1 == k {
                    Fault::Fail
                } else {
                    Fault::None
                }
            });
            let failed = matches!(result, Err(ApplyError::Transient(_)));
            if failed {
                assert_eq!(
                    std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
                    old,
                    "panne n° {k}"
                );
                assert_eq!(
                    snapshot(&dir.join(SERVER_DIR)),
                    secret_before,
                    "panne n° {k}"
                );
                assert!(
                    dir.join(PENDING).exists(),
                    "panne n° {k} : sauvegarde perdue"
                );
                assert!(!partial.exists() && !complete.exists(), "panne n° {k}");
                assert!(backups_of(&dir).is_empty(), "panne n° {k}");
                // Nouvel essai, sans panne.
                let msg = apply_pending(&dir).unwrap().unwrap();
                assert!(msg.contains("Import terminé"), "{msg}");
            }
            assert_eq!(
                std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
                imported
            );
            std::fs::remove_dir_all(&base).unwrap();
            if !failed {
                break;
            }
            k += 1;
            assert!(k < 100);
        }
        assert!(k >= 5, "{k} points de contrôle seulement");
    }

    /// Un renommage impossible (dossier parent en lecture seule) est un échec passager :
    /// l'import annoncé n'est pas supprimé, les données actuelles sont intactes, le serveur
    /// ne démarre pas sans lui, et il s'applique dès que le problème disparaît.
    #[test]
    fn a_transient_failure_keeps_the_import_and_refuses_to_start() {
        let base = temp("transient");
        let dir = sample_dir(&base);
        add_server_dir(&dir);
        let secret_before = snapshot(&dir.join(SERVER_DIR));
        let imported = controller("Importé");
        stage_import_text(&dir, &bundle_text(&[(FILE_NAME, imported.clone())])).unwrap();

        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555)).unwrap();
        let outcome = apply_pending(&dir);
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = outcome.expect_err("le serveur ne doit pas démarrer sans l'import");
        assert!(
            format!("{error:#}").contains("reste en attente"),
            "{error:#}"
        );

        assert!(dir.join(PENDING).exists(), "l'import annoncé a disparu");
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            controller("Capteur")
        );
        assert_eq!(snapshot(&dir.join(SERVER_DIR)), secret_before);
        // Le problème disparaît : l'import s'applique au lancement suivant.
        let msg = apply_pending(&dir).unwrap().unwrap();
        assert!(msg.contains("Import terminé"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            imported
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Sans dossier de données ni reste d'import, `recover` ne crée rien (c'est le premier
    /// lancement) ; avec un reste partiel seul, il le nettoie sans rien inventer.
    #[test]
    fn recover_never_invents_a_data_dir() {
        let base = temp("recover");
        let dir = base.join("donnees");
        let (partial, _) = marker_names(&dir);
        assert!(recover(&dir).unwrap().is_none());
        assert!(!dir.exists());
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join(FILE_NAME), "à moitié").unwrap();
        assert!(recover(&dir).unwrap().is_none());
        assert!(!dir.exists() && !partial.exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    fn webhook_rule(id: u64, name: &str, enabled: bool, url: &str) -> protocol::automation::Rule {
        let mut rule = protocol::automation::Rule::new(id);
        rule.name = name.into();
        rule.enabled = enabled;
        rule.actions = vec![
            protocol::automation::Action::Notify {
                title: "t".into(),
                message: "m".into(),
            },
            protocol::automation::Action::Webhook {
                url: url.into(),
                body: "{\"x\":1}".into(),
            },
        ];
        rule
    }

    /// Les brouillons de la v0.2.1 (webhook vide, sans `http`, avec un espace) ne font plus
    /// refuser toute la sauvegarde : ces règles sont désactivées, leur contenu est gardé, elles
    /// sont nommées dans le résumé ; les autres règles ne bougent pas.
    #[test]
    fn draft_webhooks_are_disabled_with_their_content_not_refused() {
        let set = RuleSet {
            rules: vec![
                webhook_rule(1, "Bonne", true, "https://exemple.fr/ok"),
                webhook_rule(2, "Vide", true, ""),
                webhook_rule(3, "Sans http", true, "exemple.fr/hook"),
                webhook_rule(4, "Avec espace", true, "http://exemple.fr/a b"),
                webhook_rule(5, "Déjà éteinte", false, "http://"),
                webhook_rule(6, "   ", true, "ftp://x"),
            ],
            ..RuleSet::default()
        };
        let text = bundle_text(&[
            (FILE_NAME, controller("Capteur")),
            (RULES, serde_json::to_string(&set).unwrap()),
        ]);
        let summary = inspect_text(&text).expect("la sauvegarde doit être acceptée");
        assert_eq!(summary.rules, 6);
        assert_eq!(
            summary.disabled_rules,
            vec![
                "Vide".to_string(),
                "Sans http".into(),
                "Avec espace".into(),
                "(sans nom, règle 6)".into()
            ]
        );
        let described = summary.describe();
        assert!(described.contains("« Sans http »"), "{described}");

        // Import réel : le fichier écrit contient les mêmes règles, `enabled` seul a changé.
        let base = temp("drafts");
        let dir = base.join("donnees");
        std::fs::write(base.join("sauvegarde.matterhome"), &text).unwrap();
        let done = import_file(&dir, &base.join("sauvegarde.matterhome")).unwrap();
        assert_eq!(done.summary.disabled_rules.len(), 4);
        let stored: RuleSet =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RULES)).unwrap()).unwrap();
        assert_eq!(stored.rules.len(), 6);
        for (before, after) in set.rules.iter().zip(&stored.rules) {
            let mut expected = before.clone();
            expected.enabled = before.enabled && after.id == 1;
            assert_eq!(*after, expected, "règle {}", before.id);
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Les autres refus restent des refus (une règle désactivée garde ses bornes de taille).
    #[test]
    fn other_rule_problems_still_refuse_the_import() {
        let set = RuleSet {
            rules: vec![webhook_rule(1, "Trop longue", false, &"x".repeat(5_000))],
            ..RuleSet::default()
        };
        let text = bundle_text(&[
            (FILE_NAME, controller("Capteur")),
            (RULES, serde_json::to_string(&set).unwrap()),
        ]);
        let why = inspect_text(&text).unwrap_err().to_string();
        assert!(why.contains("regles.json"), "{why}");
    }

    /// Coupure entre les deux renommages, puis `create-account` avant la relance : `D` renaît
    /// avec le seul dossier `serveur/`. La reprise termine l'échange (même identité, mêmes
    /// fichiers du serveur) au lieu de supprimer le dossier complet.
    #[test]
    fn a_recreated_empty_data_dir_does_not_lose_the_interrupted_import() {
        let imported = controller("Importé");
        let base = temp("recreated");
        let dir = sample_dir(&base);
        add_server_dir(&dir);
        let secret_before = snapshot(&dir.join(SERVER_DIR));
        stage_import_text(&dir, &bundle_text(&[(FILE_NAME, imported.clone())])).unwrap();
        let result = apply_with(&dir, &mut |step| {
            if step == "ecarte" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(matches!(result, Err(ApplyError::Crashed)));
        let (_, complete) = marker_names(&dir);
        assert!(!dir.exists() && complete.exists());
        assert!(recovery_needed(&dir));

        // `create-account` d'une ancienne version : crée D avec le seul `serveur/`.
        let secret = dir.join(SERVER_DIR);
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("compte.json"), "{\"username\":\"autre\"}").unwrap();
        assert!(recovery_needed(&dir));

        let note = apply_pending(&dir)
            .unwrap()
            .expect("une reprise est annoncée");
        assert!(note.contains("sans-identite"), "{note}");
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            imported,
            "la reprise ne doit jamais créer une autre identité"
        );
        assert_eq!(snapshot(&dir.join(SERVER_DIR)), secret_before);
        assert!(!complete.exists() && !recovery_needed(&dir));
        // Le dossier recréé est conservé, pas supprimé.
        let aside: Vec<_> = listing(&base)
            .into_iter()
            .filter(|n| n.contains(".sans-identite-"))
            .collect();
        assert_eq!(aside.len(), 1, "{:?}", listing(&base));
        assert!(base
            .join(&aside[0])
            .join(SERVER_DIR)
            .join("compte.json")
            .exists());
        // Et le lancement suivant ne change plus rien.
        assert!(apply_pending(&dir).unwrap().is_none());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Un dossier qui a son identité garde la sienne : un dossier complet resté là (on a renoncé
    /// à l'import) est mis de côté sous « …import-abandonne-… », jamais supprimé.
    #[test]
    fn a_leftover_complete_dir_is_set_aside_not_deleted() {
        let base = temp("leftover");
        let dir = sample_dir(&base);
        let (_, complete) = marker_names(&dir);
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::write(complete.join(FILE_NAME), controller("Autre")).unwrap();
        assert!(!recovery_needed(&dir));
        let note = recover(&dir).unwrap().expect("l'abandon est signalé");
        assert!(note.contains("import-abandonne"), "{note}");
        assert!(!complete.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            controller("Capteur")
        );
        let aside: Vec<_> = std::fs::read_dir(&base)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".import-abandonne-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read_to_string(aside[0].path().join(FILE_NAME)).unwrap(),
            controller("Autre"),
            "le contenu est intact"
        );
        // Une deuxième reprise n'a plus rien à faire.
        assert!(recover(&dir).unwrap().is_none());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Premier déploiement : le dossier de données n'existe pas, ou ne contient que `serveur/`
    /// (compte créé avant). L'import construit tout, garde le compte et le certificat.
    #[test]
    fn import_file_works_on_a_missing_or_account_only_data_dir() {
        let imported = controller("Importé");
        let base = temp("fresh");
        let src = base.join("v021.matterhome");
        std::fs::write(
            &src,
            bundle_text(&[
                (FILE_NAME, imported.clone()),
                ("history/AB12.jsonl", "{\"t\":2}\n".into()),
            ]),
        )
        .unwrap();

        // 1. Dossier absent.
        let absent = base.join("absent");
        let done = import_file(&absent, &src).unwrap();
        assert_eq!(
            std::fs::read_to_string(absent.join(FILE_NAME)).unwrap(),
            imported
        );
        assert_eq!(done.summary.devices, 1);
        assert!(!absent.join(PENDING).exists());
        assert_eq!(
            std::fs::metadata(&absent).unwrap().permissions().mode() & 0o777,
            0o700
        );

        // 2. Seulement le compte.
        let account = base.join("compte");
        std::fs::create_dir_all(&account).unwrap();
        add_server_dir(&account);
        let secret_before = snapshot(&account.join(SERVER_DIR));
        import_file(&account, &src).unwrap();
        assert_eq!(snapshot(&account.join(SERVER_DIR)), secret_before);
        assert_eq!(
            std::fs::read_to_string(account.join(FILE_NAME)).unwrap(),
            imported
        );
        assert!(account.join("history/AB12.jsonl").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Un fichier refusé ne touche à rien et ne laisse rien en attente ; un import déjà en
    /// attente (API) n'est pas écrasé.
    #[test]
    fn import_file_refuses_cleanly() {
        let base = temp("refuse");
        let dir = sample_dir(&base);
        let bad = base.join("pas-une-sauvegarde.matterhome");
        std::fs::write(&bad, "{\"format\":\"autre\"}").unwrap();
        let before = tree(&dir);
        let before_names = listing(&base);
        let e = import_file(&dir, &bad).unwrap_err();
        assert!(format!("{e:#}").contains("pas une sauvegarde"), "{e:#}");
        assert_eq!(tree(&dir), before);
        assert_eq!(listing(&base), before_names);

        let good = base.join("bonne.matterhome");
        std::fs::write(&good, bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
        stage_import_text(&dir, &bundle_text(&[(FILE_NAME, controller("Via API"))])).unwrap();
        let e = import_file(&dir, &good).unwrap_err();
        assert!(format!("{e:#}").contains("déjà en attente"), "{e:#}");
        assert!(dir.join(PENDING).exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Une panne persistante pendant `import_file` : données intactes, rien n'est laissé en
    /// attente (la commande a échoué, elle ne doit pas être rejouée plus tard).
    #[test]
    fn a_failed_import_file_leaves_nothing_pending() {
        let base = temp("failfile");
        let dir = sample_dir(&base);
        let src = base.join("bonne.matterhome");
        std::fs::write(&src, bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
        let before = tree(&dir);
        let e = import_file_with(&dir, &src, &mut |step| {
            if step == "construit" {
                Fault::Fail
            } else {
                Fault::None
            }
        })
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("rien n'est resté en attente"),
            "{e:#}"
        );
        assert_eq!(tree(&dir), before);
        assert!(!dir.join(PENDING).exists());
        assert!(!recovery_needed(&dir));
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Après trois échecs, le message dit comment renoncer à l'import.
    #[test]
    fn the_giving_up_message_names_the_file_to_delete() {
        let base = temp("giveup");
        let dir = sample_dir(&base);
        stage_import_text(&dir, &bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
        let e = apply_pending_with(&dir, &mut |step| {
            if step == "construit" {
                Fault::Fail
            } else {
                Fault::None
            }
        })
        .unwrap_err();
        let text = format!("{e:#}");
        let pending = dir.join(PENDING);
        assert!(
            text.contains("Pour y renoncer")
                && text.contains("import --abandon")
                && text.contains(&dir.display().to_string())
                && text.contains(&pending.display().to_string())
                && text.contains("import-abandonne"),
            "{text}"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Renoncer : la sauvegarde en attente est retirée et le dossier complet déjà préparé est
    /// mis de côté ; les données actuelles ne bougent pas.
    #[test]
    fn abandoning_removes_the_pending_import_and_sets_the_prepared_dir_aside() {
        let base = temp("abandon");
        let dir = sample_dir(&base);
        // Coupure juste après la publication du dossier complet, avant l'échange.
        stage_import_text(&dir, &bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
        let before_pending = tree(&dir);
        let e = apply_pending_with(&dir, &mut |step| {
            if step == "publie" {
                Fault::Crash
            } else {
                Fault::None
            }
        });
        assert!(e.is_err());
        let (_, complete) = marker_names(&dir);
        assert!(complete.exists(), "dossier complet laissé par la coupure");
        let done = abandon(&dir).unwrap();
        assert!(done.iter().any(|l| l.contains("retirée")), "{done:?}");
        assert!(
            done.iter().any(|l| l.contains("import-abandonne")),
            "{done:?}"
        );
        assert!(!complete.exists() && !dir.join(PENDING).exists());
        // Les données actuelles sont celles d'avant (plus la sauvegarde retirée).
        let mut expected = before_pending;
        expected.remove(PENDING);
        assert_eq!(tree(&dir), expected);
        // Rien à abandonner ensuite.
        assert!(abandon(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Renoncer après une coupure entre les deux renommages : les anciennes données reprennent
    /// leur place (ce que dit le message), le dossier préparé est mis de côté, rien n'est perdu.
    #[test]
    fn abandoning_after_a_cut_between_the_renames_restores_the_old_data() {
        for recreated in [false, true] {
            let base = temp(if recreated {
                "abandon-swap-2"
            } else {
                "abandon-swap"
            });
            let dir = sample_dir(&base);
            stage_import_text(&dir, &bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
            let before = tree(&dir);
            assert!(apply_pending_with(&dir, &mut |step| {
                if step == "ecarte" {
                    Fault::Crash
                } else {
                    Fault::None
                }
            })
            .is_err());
            assert!(!dir.exists(), "coupé entre les deux renommages");
            if recreated {
                // `create-account` a recréé un dossier sans identité entre-temps.
                std::fs::create_dir_all(dir.join(SERVER_DIR)).unwrap();
            }
            let done = abandon(&dir).unwrap();
            let text = done.join(" | ");
            assert!(text.contains("remises en place"), "{text}");
            assert!(text.contains("import-abandonne"), "{text}");
            // Les données d'origine sont revenues, avec la sauvegarde retirée.
            let mut expected = before;
            expected.remove(PENDING);
            assert_eq!(tree(&dir), expected);
            let (_, complete) = marker_names(&dir);
            assert!(!complete.exists());
            assert!(abandon(&dir).unwrap().is_empty());
            std::fs::remove_dir_all(base).unwrap();
        }
        // Sans dossier « …avant-import-… » : on ne ment pas, on explique.
        let base = temp("abandon-swap-none");
        let dir = base.join("dev.matter-home-desktop.MatterHomeDesktop");
        let (_, complete) = marker_names(&dir);
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::write(complete.join(FILE_NAME), controller("Autre")).unwrap();
        let err = format!("{:#}", abandon(&dir).unwrap_err());
        assert!(err.contains("impossible de l'abandonner"), "{err}");
        assert!(complete.exists(), "rien n'a été touché");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// `import` relancé après une coupure qui a laissé la sauvegarde en attente : il reprend
    /// l'import de ce même fichier ; un autre fichier ou une sauvegarde de l'API est refusé.
    #[test]
    fn rerunning_import_resumes_the_interrupted_one() {
        let base = temp("resume");
        let dir = sample_dir(&base);
        let text = bundle_text(&[(FILE_NAME, controller("Importé"))]);
        let src = base.join("a.matterhome");
        std::fs::write(&src, &text).unwrap();
        for step in ["fichier-0", "construit", "publie", "ecarte"] {
            let _ = std::fs::remove_dir_all(&dir);
            let dir = sample_dir(&base);
            // Première tentative coupée net à ce point de contrôle.
            assert!(import_file_with(&dir, &src, &mut |s| {
                if s == step {
                    Fault::Crash
                } else {
                    Fault::None
                }
            })
            .is_err());
            let kind = pending_kind(&dir, &src).unwrap();
            // Après « ecarte », D est absent : `recover` (premier geste de toute commande) le
            // remet en place ; avant, la sauvegarde est restée en attente.
            if dir.exists() {
                assert_eq!(kind, PendingKind::SameFile, "{step}");
            }
            let done = import_file(&dir, &src).unwrap_or_else(|e| panic!("{step} : {e:#}"));
            assert_eq!(
                std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
                controller("Importé"),
                "{step}"
            );
            assert!(done.resumed.is_some(), "{step} : la reprise est signalée");
            assert!(!dir.join(PENDING).exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
        // Une autre sauvegarde en attente (API, autre fichier) n'est pas reprise.
        let dir = sample_dir(&base);
        let other = bundle_text(&[(FILE_NAME, controller("Autre"))]);
        stage_import_text(&dir, &other).unwrap();
        assert_eq!(pending_kind(&dir, &src).unwrap(), PendingKind::Other);
        let err = import_file(&dir, &src).unwrap_err().to_string();
        assert!(err.contains("import --abandon"), "{err}");
        assert!(dir.join(PENDING).exists(), "rien n'a été touché");
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            controller("Capteur")
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Le dossier de données issu d'un import est en 0700, quels que soient les droits de
    /// l'ancien, et un chemin relatif d'un seul élément fonctionne (parent « . »).
    #[test]
    fn the_new_data_dir_is_private_and_relative_paths_work() {
        let base = temp("mode");
        let dir = sample_dir(&base);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let src = base.join("a.matterhome");
        std::fs::write(&src, bundle_text(&[(FILE_NAME, controller("Importé"))])).unwrap();
        import_file(&dir, &src).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        // Dossier absent : créé en 0700 aussi.
        let fresh = base.join("neuf");
        import_file(&fresh, &src).unwrap();
        assert_eq!(mode(&fresh), 0o700);
        assert_eq!(parent_of(Path::new("donnees")), Path::new("."));
        assert_eq!(parent_of(Path::new("/donnees")), Path::new("/"));
        sync_dir(Path::new("")).unwrap();
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn identities_are_described_for_the_confirmation() {
        let base = temp("brief");
        let dir = sample_dir(&base);
        let src = base.join("a.matterhome");
        let mut other = controller("Salon").replace("\"fabric_id\":1", "\"fabric_id\":255");
        other = other.replace("\"node_id\":7", "\"node_id\":16");
        std::fs::write(&src, bundle_text(&[(FILE_NAME, other)])).unwrap();
        let here = identity_in_dir(&dir).unwrap().unwrap();
        let there = identity_in_file(&src).unwrap();
        assert_eq!(here.fabric_id, 1);
        assert_eq!(there.fabric_id, 255);
        assert_eq!(there.nodes, vec!["Salon (0x10)".to_string()]);
        assert!(
            there.describe().contains("fabric 0xff"),
            "{}",
            there.describe()
        );
        assert!(identity_in_dir(&base.join("absent")).unwrap().is_none());
        std::fs::write(dir.join(FILE_NAME), "pas du json").unwrap();
        assert!(identity_in_dir(&dir).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_backup_whose_certificates_are_not_der_is_refused() {
        let base = temp("der");
        let good = controller("X");
        let bad = good.replace(
            &crate::matter::certs::test_identity(KEY).ca_cert_der,
            "deadbeef",
        );
        assert_ne!(good, bad);
        let err = inspect_text(&bundle_text(&[(FILE_NAME, bad)])).unwrap_err();
        assert!(err.0.contains("certificat racine illisible"), "{err}");
        assert!(inspect_text(&bundle_text(&[(FILE_NAME, good)])).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }
}
