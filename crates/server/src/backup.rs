//! Export et import de toutes les données de l'application dans un seul fichier
//! (`.matterhome`), pour changer de Mac sans réappairer les appareils.
//!
//! Le fichier est un JSON qui embarque les fichiers du dossier de données :
//! `controller.json`, `maison.json`, `regles.json` et `history/*.jsonl`.
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
    pub history_files: usize,
    pub has_plan: bool,
    pub has_rules: bool,
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
        if self.history_files > 0 {
            parts.push(format!("historique de {} appareil(s)", self.history_files));
        }
        format!("sauvegarde du {date} : {}", parts.join(", "))
    }
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
    if matches!(name, FILE_NAME | PLAN | RULES) {
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
        JsonCertManager::from_identity(&state.identity).map_err(|e| {
            Invalid(format!(
                "{FILE_NAME} : identité du contrôleur invalide ({e})"
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

    Ok(Summary {
        created: bundle.created.clone(),
        devices: state.device_ids().len(),
        rooms: plan
            .as_ref()
            .map_or(0, |p| p.floors.iter().map(|f| f.rooms.len()).sum()),
        rules: rules.as_ref().map_or(0, |r| r.rules.len()),
        history_files: bundle
            .files
            .keys()
            .filter(|k| k.starts_with("history/"))
            .count(),
        has_plan: plan.is_some(),
        has_rules: rules.is_some(),
    })
}

fn parse(text: &str) -> Result<(Bundle, Summary), Invalid> {
    let bundle: Bundle = serde_json::from_str(text)
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
    let summary = analyze(&bundle)?;
    Ok((bundle, summary))
}

fn read_bundle(path: &Path) -> Result<(Bundle, Summary)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    Ok(parse(&text)?)
}

/// Synchronise un dossier sur le disque (ses entrées : créations, renommages).
fn sync_dir(dir: &Path) -> Result<()> {
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
    for name in [FILE_NAME, PLAN, RULES] {
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
    if let Ok(meta) = std::fs::metadata(dir) {
        // Mêmes droits que le dossier de données actuel.
        std::fs::set_permissions(partial, meta.permissions())
            .map_err(io("droits du dossier partiel"))?;
    }
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
    sync_dir(dir.parent().unwrap_or(Path::new("."))).map_err(ApplyError::Transient)?;
    checkpoint(hook, "publie")
}

/// Applique la sauvegarde en attente de `dir`. Rend le résumé et le dossier des anciennes données.
fn apply_with(dir: &Path, hook: Hook<'_>) -> Result<(Summary, PathBuf), ApplyError> {
    let (bundle, summary) =
        read_bundle(&dir.join(PENDING)).map_err(|e| match e.downcast::<Invalid>() {
            Ok(invalid) => ApplyError::Invalid(invalid.0),
            Err(io) => ApplyError::Transient(io),
        })?;
    let parent = dir.parent().unwrap_or(Path::new("."));
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
    let parent = dir.parent().unwrap_or(Path::new("."));
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
    if remove_tree(&partial).is_err() {
        log::warn!("reste d'un import à nettoyer : {}", partial.display());
    }
    if dir.exists() && !dir.join(PENDING).exists() {
        let _ = remove_tree(&complete);
    }
    Ok(None)
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
    let mut last = None;
    for attempt in 1..=APPLY_ATTEMPTS {
        match apply_with(dir, hook) {
            Ok((summary, backup)) => {
                notes.push(format!(
                    "Import terminé ({}). Anciennes données conservées dans {}.",
                    summary.describe(),
                    backup.display()
                ));
                return Ok(Some(notes.join(" ")));
            }
            Err(ApplyError::Invalid(why)) => {
                // Une sauvegarde invalide ne doit pas être retentée à chaque lancement.
                let _ = std::fs::remove_file(dir.join(PENDING));
                notes.push(format!(
                    "Import impossible, données actuelles conservées : {why}"
                ));
                return Ok(Some(notes.join(" ")));
            }
            Err(ApplyError::Transient(e)) => {
                log::warn!("import non appliqué (essai {attempt}/{APPLY_ATTEMPTS}) : {e:#}");
                last = Some(e);
                if attempt < APPLY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
            Err(ApplyError::Fatal(e)) => return Err(e),
            Err(ApplyError::Crashed) => bail!("arrêt simulé"),
        }
    }
    // Les données actuelles sont intactes et la sauvegarde reste en attente (le serveur a
    // répondu 202 : on ne la supprime pas). Démarrer sans elle l'appliquerait peut-être des
    // jours plus tard, par surprise : on s'arrête, l'opérateur voit pourquoi.
    Err(last
        .unwrap_or_else(|| anyhow!("raison inconnue"))
        .context(format!(
            "l'import en attente n'a pas pu être appliqué après {APPLY_ATTEMPTS} essais ; les \
             données actuelles sont intactes et la sauvegarde reste en attente dans {}. \
             Corrigez la cause (espace disque, droits) et relancez le serveur",
            dir.join(PENDING).display()
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Clé P-256 valide (le scalaire 0x0101…01) : les sauvegardes d'essai doivent passer
    /// `JsonCertManager::from_identity` comme de vraies.
    const KEY: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    fn controller(name: &str) -> String {
        format!(
            r#"{{"version":1,"identity":{{"fabric_id":1,"controller_node_id":2,
            "ca_private_key":"{KEY}","ca_cert_der":"bb","controller_private_key":"{KEY}",
            "controller_cert_der":"dd"}},"nodes":[{{"node_id":7,"name":"{name}"}}]}}"#
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
        let evil_rules = r#"{"version":1,"rules":[{"id":1,"name":"r","enabled":true,
            "trigger":{"type":"Time","hour":7,"minute":0,"days":[true,true,true,true,true,true,true]},
            "conditions":[],"actions":[{"type":"Webhook","url":"file:///etc/passwd","body":""}],
            "cooldown_s":60}]}"#
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
}
