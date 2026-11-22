//! Le journal d'audit : qui a fait quoi, depuis quel téléphone ou ordinateur, quand.
//!
//! Un fichier par mois, `serveur/audit/AAAA-MM.jsonl` (une ligne JSON par événement), en ajout
//! seul. Le dossier `serveur/` n'est ni exporté ni remplacé par un import : le journal reste donc
//! sur le Mac du serveur. Les fichiers de plus de 6 mois sont supprimés.
//!
//! Chaque écriture ouvre le fichier en ajout, écrit la ligne d'un seul coup et le referme : aucune
//! poignée gardée, aucun index en mémoire. Le serveur, la ligne de commande (`ostal-server user …`,
//! `import`, `audit`) et la fenêtre d'Ostal Server sont des processus ou des fils différents qui
//! écrivent et lisent le même dossier sans se coordonner.
//!
//! Ce qui n'y entre jamais : mot de passe, jeton, code d'appairage, mesure. Les appelants ne donnent
//! que des noms (personne, connexion, équipement, automatisation) et des mots d'un vocabulaire
//! fermé. Un identifiant saisi lors d'une connexion refusée n'y entre que s'il désigne un compte
//! existant (on tape parfois son mot de passe dans la case de l'identifiant). Une écriture qui
//! échoue est notée au journal technique et ne fait jamais échouer l'action de la personne.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Datelike, Local, Months, NaiveDate, TimeZone, Timelike};
use protocol::mode::{HomeMode, ModeSource};
use protocol::wire::{ConnectionKind, Via};
use serde::{Deserialize, Serialize};

/// Sous-dossier de `serveur/`.
pub const DIR: &str = "audit";
/// Durée de conservation, en mois.
pub const RETENTION_MONTHS: u32 = 6;
/// Longueur maximale d'un texte gardé (nom, cible).
const MAX_TEXT: usize = 120;
/// Les noms des « connexions » qui ne sont pas un téléphone ni un ordinateur de la maison.
pub const SERVER_APP: &str = "Ostal Server";
pub const COMMAND_LINE: &str = "Ligne de commande";

/// Ce qui s'est passé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Ouverture de session (réussie, ou refusée).
    Login,
    /// Première ouverture de session depuis un téléphone ou un ordinateur.
    NewConnection,
    /// Connexion coupée (ou déconnexion volontaire).
    ConnectionCut,
    PasswordChanged,
    UserCreated,
    UserDisabled,
    UserEnabled,
    ModeChanged,
    /// Commande d'un équipement (prise, volet…).
    DeviceCommand,
    AutomationChanged,
    AutomationTested,
    DevicePaired,
    DeviceRemoved,
    Export,
    Import,
    /// « Je pars » ou « Je rentre », ou un téléphone vu (ou plus vu) sur le Wi-Fi.
    Presence,
    /// Un téléphone relié à un compte (ou retiré). Cible : le compte ; jamais le nom ni l'adresse
    /// du téléphone.
    Phone,
    /// Un réglage de la box : reliée, déliée, mot de passe changé (jamais le mot de passe),
    /// mode automatique activé ou désactivé.
    BoxSetting,
    /// Une action d'une version plus récente.
    #[serde(other)]
    Other,
}

/// Comment ça s'est terminé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Refused,
    Error,
}

/// D'où vient un changement de mode, en quatre mots : une personne qui le choisit, un raccourci,
/// une automatisation, ou la maison toute seule (la règle de présence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Person,
    Shortcut,
    Automation,
    Automatic,
}

impl Origin {
    /// L'origine d'après la source annoncée sur le fil (le protocole ne change pas).
    pub fn of(source: &ModeSource) -> Origin {
        match source {
            ModeSource::Shortcut | ModeSource::Widget => Origin::Shortcut,
            ModeSource::Presence => Origin::Automatic,
            ModeSource::Rule { .. } => Origin::Automation,
            ModeSource::Web | ModeSource::Desktop | ModeSource::ServerApp | ModeSource::Other => {
                Origin::Person
            }
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Origin::Person => "personne",
            Origin::Shortcut => "raccourci",
            Origin::Automation => "automatisation",
            Origin::Automatic => "automatique",
        }
    }
}

/// Le téléphone ou l'ordinateur d'où vient l'action (copie : elle survit à la coupure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnRef {
    /// Identifiant de l'installation ; jamais pour une connexion inconnue (le sien dérive d'une
    /// session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    /// `web`, `mac`, `shortcut`, `server`, `cli` ou `other`.
    pub kind: String,
}

impl ConnRef {
    pub fn new(id: Option<&str>, name: &str, kind: &str) -> ConnRef {
        ConnRef {
            id: id.map(clean_text),
            name: clean_text(name),
            kind: clean_text(kind),
        }
    }

    pub fn of_kind(kind: ConnectionKind) -> &'static str {
        match kind {
            ConnectionKind::Web => "web",
            ConnectionKind::Mac => "mac",
            ConnectionKind::Other => "other",
        }
    }

    fn is_server_side(&self) -> bool {
        matches!(self.kind.as_str(), "server" | "cli")
    }
}

/// Qui agit, et d'où. Tout est facultatif : une automatisation ou le serveur lui-même n'ont ni
/// personne, ni connexion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Who {
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub connection: Option<ConnRef>,
    pub ip: Option<String>,
    pub via: Option<Via>,
}

impl Who {
    pub fn user(id: &str, name: Option<String>) -> Who {
        Who {
            user_id: Some(clean_text(id)),
            user_name: name.map(|n| clean_text(&n)).filter(|n| !n.is_empty()),
            ..Who::default()
        }
    }

    /// La fenêtre d'Ostal Server.
    pub fn server_app() -> Who {
        Who {
            connection: Some(ConnRef::new(None, SERVER_APP, "server")),
            ..Who::default()
        }
    }

    /// `ostal-server` lancé dans un Terminal.
    pub fn command_line() -> Who {
        Who {
            connection: Some(ConnRef::new(None, COMMAND_LINE, "cli")),
            ..Who::default()
        }
    }
}

/// Qui change le mode, d'où, et de quelle façon : ce que le journal retient d'un changement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeCause {
    pub who: Who,
    pub origin: Origin,
    /// Le raccourci, la règle ou l'application, en un mot.
    pub detail: Option<String>,
}

impl ModeCause {
    /// Ce qu'on sait d'après la seule source annoncée : sans personne pour le web, le bureau et
    /// les raccourcis (la route la complète), avec la fenêtre du serveur pour `ServerApp`.
    pub fn from_source(source: &ModeSource) -> ModeCause {
        let (who, detail) = match source {
            ModeSource::ServerApp => (Who::server_app(), Some(SERVER_APP.to_string())),
            ModeSource::Web => (Who::default(), Some("l'app web".to_string())),
            ModeSource::Desktop => (Who::default(), Some("l'app Mac".to_string())),
            ModeSource::Rule { name } if !name.is_empty() => {
                (Who::default(), Some(clean_text(name)))
            }
            _ => (Who::default(), None),
        };
        ModeCause {
            who,
            origin: Origin::of(source),
            detail,
        }
    }
}

/// Une ligne du journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Secondes Unix.
    pub at: f64,
    /// Identifiant du compte (stable) et son nom affiché à ce moment-là.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    pub action: Action,
    /// Sur quoi : un équipement, une automatisation, un compte, une connexion, un mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub result: Outcome,
    /// Pour un changement de mode seulement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Origin>,
    /// Un mot du vocabulaire du code (commande d'un équipement) ou un nom (raccourci, règle) ;
    /// jamais un message d'erreur.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Entry {
    pub fn new(action: Action, who: &Who) -> Entry {
        Entry {
            at: crate::events::now_unix(),
            user: who.user_id.clone(),
            user_name: who.user_name.clone(),
            connection: who.connection.clone(),
            ip: who.ip.clone(),
            via: who.via,
            action,
            target: None,
            result: Outcome::Ok,
            source: None,
            detail: None,
        }
    }

    pub fn at(mut self, at: f64) -> Entry {
        self.at = at;
        self
    }

    pub fn target(mut self, target: impl AsRef<str>) -> Entry {
        let target = clean_text(target.as_ref());
        self.target = (!target.is_empty()).then_some(target);
        self
    }

    pub fn detail(mut self, detail: impl AsRef<str>) -> Entry {
        let detail = clean_text(detail.as_ref());
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }

    pub fn source(mut self, origin: Origin) -> Entry {
        self.source = Some(origin);
        self
    }

    pub fn result(mut self, result: Outcome) -> Entry {
        self.result = result;
        self
    }

    /// `Ok` ou `Error` selon le résultat de l'action.
    pub fn outcome<T, E>(self, result: &Result<T, E>) -> Entry {
        self.result(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        })
    }
}

/// Un texte propre et court : sans caractères de contrôle ni retour à la ligne.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TEXT)
        .collect()
}

/// Ce qu'on cherche dans le journal.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Identifiant de compte.
    pub user: Option<String>,
    /// Bornes, en secondes Unix.
    pub since: Option<f64>,
    pub until: Option<f64>,
    /// Nombre maximal de lignes rendues (les plus récentes).
    pub limit: Option<usize>,
}

pub struct AuditLog {
    /// `None` : journal éteint (dossier inutilisable) ; rien n'est écrit ni lu.
    dir: Option<PathBuf>,
    /// Mois du dernier nettoyage.
    purged: Mutex<Option<(i32, u32)>>,
    /// Dernière écriture d'une ligne « limitée » (clé → secondes Unix).
    throttle: Mutex<HashMap<String, f64>>,
}

impl AuditLog {
    /// Le journal d'un dossier `serveur/` : crée `audit/` au premier ajout, et retire d'emblée les
    /// fichiers trop anciens.
    pub fn open(server_dir: &Path) -> AuditLog {
        let log = AuditLog {
            dir: Some(server_dir.join(DIR)),
            purged: Mutex::new(None),
            throttle: Mutex::new(HashMap::new()),
        };
        log.purge_if_new_month(crate::events::now_unix());
        log
    }

    /// Le journal d'un dossier `serveur/`, pour le lire seulement : rien n'est créé ni supprimé
    /// (la conservation se fait à l'ouverture du serveur, pas ici).
    pub fn reader(server_dir: &Path) -> AuditLog {
        AuditLog {
            dir: Some(server_dir.join(DIR)),
            purged: Mutex::new(None),
            throttle: Mutex::new(HashMap::new()),
        }
    }

    /// Un journal qui n'écrit nulle part.
    pub fn disabled() -> AuditLog {
        AuditLog {
            dir: None,
            purged: Mutex::new(None),
            throttle: Mutex::new(HashMap::new()),
        }
    }

    /// Le journal du dossier de données `data_dir` (`<données>/serveur/audit`).
    pub fn of_data_dir(data_dir: &Path) -> AuditLog {
        AuditLog::open(&data_dir.join(crate::persistence::SERVER_DIR))
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Ajoute une ligne. Ne rend rien : une panne du journal ne doit jamais bloquer l'action.
    pub fn record(&self, entry: Entry) {
        if let Err(e) = self.append(&entry) {
            log::error!("journal d'audit : écriture impossible : {e:#}");
        }
    }

    /// Comme [`AuditLog::record`], mais au plus une fois par `window_s` secondes pour cette `key`
    /// (les refus en rafale d'une attaque ne remplissent pas le disque).
    pub fn record_throttled(&self, key: &str, window_s: f64, entry: Entry) {
        {
            let mut seen = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
            if seen.get(key).is_some_and(|t| entry.at - *t < window_s) {
                return;
            }
            if seen.len() > 256 {
                seen.retain(|_, t| entry.at - *t < window_s);
            }
            seen.insert(key.to_string(), entry.at);
        }
        self.record(entry);
    }

    fn append(&self, entry: &Entry) -> anyhow::Result<()> {
        use anyhow::Context;
        let Some(dir) = &self.dir else { return Ok(()) };
        let name = file_name_of(entry.at).context("date hors limites")?;
        ensure_dir(dir)?;
        let mut line = serde_json::to_vec(entry).context("sérialisation")?;
        line.push(b'\n');
        let path = dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .with_context(|| format!("ouverture de {}", path.display()))?;
        // Une seule écriture : deux processus qui ajoutent en même temps ne mêlent pas leurs lignes.
        file.write_all(&line)
            .with_context(|| format!("écriture de {}", path.display()))?;
        self.purge_if_new_month(entry.at);
        Ok(())
    }

    /// Nettoie une fois par mois et par processus (au démarrage, puis au changement de mois).
    fn purge_if_new_month(&self, now: f64) {
        let Some(month) = month_of(now) else { return };
        {
            let mut purged = self.purged.lock().unwrap_or_else(|e| e.into_inner());
            if *purged == Some(month) {
                return;
            }
            *purged = Some(month);
        }
        let removed = self.purge(now);
        if removed > 0 {
            log::info!("journal d'audit : {removed} fichier(s) de plus de {RETENTION_MONTHS} mois supprimé(s)");
        }
    }

    /// Supprime les fichiers dont tous les jours ont plus de 6 mois à `now`. Ne touche qu'aux
    /// fichiers nommés exactement `AAAA-MM.jsonl`. Rend le nombre de fichiers supprimés.
    pub fn purge(&self, now: f64) -> usize {
        let (Some(dir), Some(cutoff)) = (&self.dir, cutoff_date(now)) else {
            return 0;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some((year, month)) = name.to_str().and_then(parse_file_name) else {
                continue;
            };
            let (ny, nm) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            // Le mois est fini depuis plus de 6 mois : aucune de ses lignes n'est à garder.
            let over = NaiveDate::from_ymd_opt(ny, nm, 1).is_some_and(|first| first <= cutoff);
            if over && entry.file_type().is_ok_and(|t| t.is_file()) {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => removed += 1,
                    Err(e) => log::warn!(
                        "journal d'audit : {} non supprimé : {e}",
                        name.to_string_lossy()
                    ),
                }
            }
        }
        removed
    }

    /// Les lignes qui répondent au filtre, la plus récente d'abord. Les lignes illisibles
    /// (écriture interrompue) sont ignorées ; celles de plus de 6 mois ne sont jamais rendues, même
    /// si leur fichier est encore là.
    pub fn read(&self, filter: &Filter) -> Vec<Entry> {
        self.read_at(filter, crate::events::now_unix())
    }

    pub fn read_at(&self, filter: &Filter, now: f64) -> Vec<Entry> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let oldest = cutoff_ts(now).unwrap_or(0.0);
        let since = filter.since.map_or(oldest, |s| s.max(oldest));
        let until = filter.until.unwrap_or(now.max(since));
        let (Some(mut month), Some(last)) = (month_of(since), month_of(until)) else {
            return Vec::new();
        };
        let mut out: Vec<Entry> = Vec::new();
        while month <= last {
            let path = dir.join(format!("{:04}-{:02}.jsonl", month.0, month.1));
            if let Ok(bytes) = std::fs::read(&path) {
                let text = String::from_utf8_lossy(&bytes);
                for line in text.lines() {
                    let Ok(entry) = serde_json::from_str::<Entry>(line) else {
                        continue;
                    };
                    if entry.at < since || entry.at > until {
                        continue;
                    }
                    if filter
                        .user
                        .as_deref()
                        .is_some_and(|u| entry.user.as_deref() != Some(u))
                    {
                        continue;
                    }
                    out.push(entry);
                }
            }
            month = if month.1 == 12 {
                (month.0 + 1, 1)
            } else {
                (month.0, month.1 + 1)
            };
        }
        // Dans le fichier, l'ordre est celui des écritures : la plus récente d'abord, et à
        // l'égalité de la seconde, la dernière écrite.
        out.reverse();
        out.sort_by(|a, b| b.at.total_cmp(&a.at));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        out
    }
}

fn ensure_dir(dir: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    if dir.is_dir() {
        return Ok(());
    }
    // Le dossier du serveur (`serveur/`) peut manquer (premier import) : même droits que lui.
    for path in dir.parent().into_iter().chain([dir]) {
        if path.is_dir() {
            continue;
        }
        std::fs::create_dir(path).with_context(|| format!("création de {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("droits de {}", path.display()))?;
        }
    }
    Ok(())
}

fn local(at: f64) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_opt(at.floor() as i64, 0).single()
}

fn month_of(at: f64) -> Option<(i32, u32)> {
    local(at).map(|d| (d.year(), d.month()))
}

fn file_name_of(at: f64) -> Option<String> {
    month_of(at).map(|(y, m)| format!("{y:04}-{m:02}.jsonl"))
}

/// `2026-11.jsonl` → (2026, 11). Rien d'autre n'est reconnu.
fn parse_file_name(name: &str) -> Option<(i32, u32)> {
    let stem = name.strip_suffix(".jsonl")?;
    let (year, month) = stem.split_once('-')?;
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(year, 4) || !digits(month, 2) {
        return None;
    }
    let (year, month) = (year.parse().ok()?, month.parse().ok()?);
    (1..=12).contains(&month).then_some((year, month))
}

/// Le jour d'il y a 6 mois.
fn cutoff_date(now: f64) -> Option<NaiveDate> {
    local(now)?
        .checked_sub_months(Months::new(RETENTION_MONTHS))
        .map(|d| d.date_naive())
}

/// L'instant d'il y a 6 mois.
fn cutoff_ts(now: f64) -> Option<f64> {
    local(now)?
        .checked_sub_months(Months::new(RETENTION_MONTHS))
        .map(|d| d.timestamp() as f64)
}

// ---------------------------------------------------------------------------
// Les mots : ce que lisent la fenêtre d'Ostal Server et la ligne de commande
// ---------------------------------------------------------------------------

/// Le ton d'une ligne, pour la pastille : une information, un équipement, un point d'attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Device,
    Warn,
}

/// « Qui » : la personne, ou ce qui a agi à sa place.
pub fn who_label(entry: &Entry) -> String {
    if let Some(name) = entry.user_name.as_deref().filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if entry.source == Some(Origin::Automation) {
        return "Automatisation".to_string();
    }
    if entry
        .connection
        .as_ref()
        .is_some_and(ConnRef::is_server_side)
    {
        return "Serveur".to_string();
    }
    "Inconnu".to_string()
}

/// « Depuis » : le téléphone ou l'ordinateur, ou « — ».
pub fn from_label(entry: &Entry) -> String {
    entry
        .connection
        .as_ref()
        .map_or_else(|| "—".to_string(), |c| c.name.clone())
}

fn mode_label(key: Option<&str>) -> String {
    key.and_then(HomeMode::from_key)
        .map_or_else(|| "inconnu".to_string(), |m| m.label().to_string())
}

fn place(via: Option<Via>) -> &'static str {
    match via {
        Some(Via::Tailscale) => "à distance",
        _ => "sur place",
    }
}

/// « Quoi » : une phrase française, la même dans la fenêtre et dans le Terminal.
pub fn describe(entry: &Entry) -> (Tone, String) {
    let target = entry.target.as_deref().unwrap_or("");
    let detail = entry.detail.as_deref().unwrap_or("");
    let failed = entry.result != Outcome::Ok;
    let name = |entry: &Entry| {
        entry
            .connection
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    };
    let mut tone = Tone::Info;
    let text = match entry.action {
        Action::Login if entry.result == Outcome::Refused => {
            tone = Tone::Warn;
            if detail == "lockout" {
                format!("Connexion bloquée, trop d'essais, {}", place(entry.via))
            } else {
                format!("Connexion refusée, {}", place(entry.via))
            }
        }
        Action::Login => match entry.connection.as_ref() {
            None => {
                tone = Tone::Warn;
                format!(
                    "Connexion inconnue, {} : l'app n'est pas encore à jour",
                    place(entry.via)
                )
            }
            Some(c) => {
                let app = match c.kind.as_str() {
                    "mac" => "à l'app Mac",
                    "web" => "à l'app web",
                    _ => "à Ostal",
                };
                format!("Connexion {app}, {}", place(entry.via))
            }
        },
        Action::NewConnection => format!("Nouvelle connexion : {}", name(entry)),
        Action::ConnectionCut if detail == "logout" => "Déconnexion".to_string(),
        Action::ConnectionCut => {
            let from = if entry
                .connection
                .as_ref()
                .is_some_and(ConnRef::is_server_side)
            {
                format!(" depuis {}", name(entry))
            } else {
                String::new()
            };
            format!("Connexion « {target} » coupée{from}")
        }
        Action::PasswordChanged => format!("Mot de passe de {target} changé"),
        Action::UserCreated => format!("Compte de {target} créé"),
        Action::UserDisabled => format!("Compte de {target} désactivé"),
        Action::UserEnabled => format!("Compte de {target} réactivé"),
        Action::ModeChanged => {
            let mode = mode_label(entry.target.as_deref());
            match entry.source {
                Some(Origin::Automatic) if !detail.is_empty() => {
                    format!("La maison passe en {mode} (automatique, {detail})")
                }
                Some(Origin::Automatic) => format!("La maison passe en {mode} (automatique)"),
                Some(Origin::Automation) if !detail.is_empty() => {
                    format!("Mode {mode}, par l'automatisation « {detail} »")
                }
                Some(Origin::Automation) => format!("Mode {mode}, par une automatisation"),
                Some(Origin::Shortcut) if !detail.is_empty() => {
                    format!("Mode {mode}, par le raccourci « {detail} »")
                }
                Some(Origin::Shortcut) => format!("Mode {mode}, par un raccourci"),
                _ if !detail.is_empty() => format!("Mode {mode}, choisi dans {detail}"),
                _ => format!("Mode {mode}"),
            }
        }
        Action::DeviceCommand => {
            tone = Tone::Device;
            let what = match detail {
                "on" => "allumé".to_string(),
                "off" => "éteint".to_string(),
                "toggle" => "basculé".to_string(),
                "open" => "ouverture".to_string(),
                "close" => "fermeture".to_string(),
                "stop" => "arrêt".to_string(),
                d => d
                    .strip_prefix("position:")
                    .map_or_else(String::new, |p| format!("à {p} %")),
            };
            if what.is_empty() {
                target.to_string()
            } else {
                format!("{target} : {what}")
            }
        }
        Action::AutomationChanged => "Automatisations modifiées".to_string(),
        Action::AutomationTested => format!("Automatisation « {target} » testée"),
        Action::DevicePaired => "Appairage d'un équipement demandé".to_string(),
        Action::DeviceRemoved => format!("Équipement « {target} » supprimé"),
        Action::Export => "Sauvegarde exportée".to_string(),
        Action::Import => "Sauvegarde importée, le serveur redémarre".to_string(),
        Action::Presence if detail == "wifi" => {
            let seen = if target == "away" {
                "n'est plus vu sur le Wi-Fi"
            } else {
                "est vu sur le Wi-Fi"
            };
            format!("Téléphone de {} {seen}", who_label(entry))
        }
        Action::Presence => {
            let said = if target == "away" {
                "Je pars"
            } else {
                "Je rentre"
            };
            format!("{} : « {said} »", who_label(entry))
        }
        Action::Phone if detail == "cleared" => format!("Téléphone de {target} retiré"),
        Action::Phone => format!("Téléphone de {target} relié"),
        Action::BoxSetting => match detail {
            "linked" => "Box de la maison reliée".to_string(),
            "unlinked" => "Box de la maison déliée".to_string(),
            "password" => "Mot de passe de la box changé".to_string(),
            "auto_on" => "Mode automatique selon la présence activé".to_string(),
            "auto_off" => "Mode automatique selon la présence désactivé".to_string(),
            _ => "Réglage de la box".to_string(),
        },
        Action::Other => "Action inconnue".to_string(),
    };
    if failed && entry.action != Action::Login {
        let how = if entry.result == Outcome::Refused {
            "refusé"
        } else {
            "échec"
        };
        return (Tone::Warn, format!("{text} ({how})"));
    }
    (tone, text)
}

/// L'heure locale `HH:MM` d'une ligne.
pub fn clock_label(at: f64) -> String {
    local(at).map_or_else(
        || "--:--".to_string(),
        |d| format!("{:02}:{:02}", d.hour(), d.minute()),
    )
}

/// La date locale `AAAA-MM-JJ HH:MM`, pour la ligne de commande.
pub fn stamp_label(at: f64) -> String {
    local(at).map_or_else(
        || "?".to_string(),
        |d| d.format("%Y-%m-%d %H:%M").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ostal-audit-{tag}-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Midi, heure locale, le `day` du `month` : loin d'un changement de jour, quel que soit le fuseau.
    fn ts(year: i32, month: u32, day: u32) -> f64 {
        Local
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp() as f64
    }

    fn who() -> Who {
        let mut w = Who::user("u1", Some("Hamdi".into()));
        w.connection = Some(ConnRef::new(
            Some("iphone-hamdi-01"),
            "iPhone de Hamdi",
            "web",
        ));
        w.ip = Some("192.168.1.20".into());
        w.via = Some(Via::Local);
        w
    }

    #[test]
    fn a_line_is_appended_to_the_month_file_with_private_permissions() {
        let base = temp("append");
        let log = AuditLog::open(&base);
        let at = ts(2026, 11, 22);
        log.record(Entry::new(Action::Login, &who()).at(at));
        log.record(
            Entry::new(Action::ModeChanged, &who())
                .at(at + 60.0)
                .target("nuit")
                .source(Origin::Shortcut)
                .detail("Mode nuit"),
        );
        let file = base.join("audit/2026-11.jsonl");
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 2);
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        // Tous les champs du plan : date, utilisateur, connexion, IP, action, cible, résultat.
        assert_eq!(first["user"], "u1");
        assert_eq!(first["connection"]["name"], "iPhone de Hamdi");
        assert_eq!(first["ip"], "192.168.1.20");
        assert_eq!(first["action"], "login");
        assert_eq!(first["result"], "ok");
        assert!(first["at"].as_f64().unwrap() > 0.0);
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(base.join("audit"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // Une deuxième ouverture (autre processus) retrouve tout, et ajoute à la suite.
        let again = AuditLog::reader(&base);
        again.record(Entry::new(Action::Export, &Who::command_line()).at(at + 120.0));
        let all = again.read_at(&Filter::default(), at + 3600.0);
        assert_eq!(
            all.iter().map(|e| e.action).collect::<Vec<_>>(),
            vec![Action::Export, Action::ModeChanged, Action::Login],
            "la plus récente d'abord"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn filters_by_person_and_period_and_skips_broken_lines() {
        let base = temp("filter");
        let log = AuditLog::open(&base);
        let day = ts(2026, 11, 20);
        let mut sam = Who::user("u2", Some("Sam".into()));
        sam.via = Some(Via::Tailscale);
        log.record(Entry::new(Action::Login, &who()).at(day));
        log.record(Entry::new(Action::Login, &sam).at(day + 100.0));
        log.record(Entry::new(Action::Login, &who()).at(day + 86_400.0 * 3.0));
        // Une ligne tronquée (arrêt brutal) au milieu du fichier, une ligne vide.
        let file = base.join("audit/2026-11.jsonl");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str("{\"at\": 17\n\n");
        std::fs::write(&file, text).unwrap();
        log.record(Entry::new(Action::Login, &sam).at(day + 86_400.0 * 4.0));

        let now = day + 86_400.0 * 5.0;
        assert_eq!(log.read_at(&Filter::default(), now).len(), 4);
        let sams = log.read_at(
            &Filter {
                user: Some("u2".into()),
                ..Filter::default()
            },
            now,
        );
        assert_eq!(sams.len(), 2);
        assert!(sams.iter().all(|e| e.user.as_deref() == Some("u2")));
        let recent = log.read_at(
            &Filter {
                since: Some(day + 86_400.0 * 2.0),
                limit: Some(1),
                ..Filter::default()
            },
            now,
        );
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].user.as_deref(), Some("u2"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_period_over_two_months_reads_both_files() {
        let base = temp("months");
        let log = AuditLog::open(&base);
        log.record(Entry::new(Action::Export, &who()).at(ts(2026, 10, 30)));
        log.record(Entry::new(Action::Import, &who()).at(ts(2026, 11, 2)));
        let now = ts(2026, 11, 3);
        let both = log.read_at(
            &Filter {
                since: Some(ts(2026, 10, 29)),
                ..Filter::default()
            },
            now,
        );
        assert_eq!(both.len(), 2);
        assert!(base.join("audit/2026-10.jsonl").is_file());
        assert!(base.join("audit/2026-11.jsonl").is_file());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn files_older_than_six_months_are_deleted_and_nothing_else() {
        let base = temp("purge");
        let dir = base.join("audit");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "2025-10.jsonl",
            "2025-12.jsonl",
            "2026-04.jsonl",
            "2026-05.jsonl",
            "2026-11.jsonl",
        ] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        // Pas des fichiers du journal : jamais touchés.
        for name in [
            "notes.txt",
            "2026-4.jsonl",
            "2020-01.jsonl.bak",
            "2020-13.jsonl",
        ] {
            std::fs::write(dir.join(name), "garde").unwrap();
        }
        let log = AuditLog {
            dir: Some(dir.clone()),
            purged: Mutex::new(None),
            throttle: Mutex::new(HashMap::new()),
        };
        // Le 22/11/2026 : il y a 6 mois, le 22/05/2026. Avril est fini depuis plus de 6 mois ;
        // mai contient encore des jours à garder.
        assert_eq!(log.purge(ts(2026, 11, 22)), 3);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "2020-01.jsonl.bak",
                "2020-13.jsonl",
                "2026-05.jsonl",
                "2026-11.jsonl",
                "2026-4.jsonl",
                "notes.txt"
            ]
        );
        // Le 15/07/2026, à cheval sur deux années : il y a 6 mois, le 15/01/2026. Décembre 2025 est
        // fini depuis plus de 6 mois, janvier 2026 a encore des jours à garder.
        let log2 = AuditLog {
            dir: Some(dir.clone()),
            purged: Mutex::new(None),
            throttle: Mutex::new(HashMap::new()),
        };
        std::fs::write(dir.join("2025-12.jsonl"), "").unwrap();
        std::fs::write(dir.join("2026-01.jsonl"), "").unwrap();
        assert_eq!(
            log2.purge(ts(2026, 7, 15)),
            1,
            "2025-12 part, 2026-01 reste"
        );
        assert!(dir.join("2026-01.jsonl").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn opening_the_log_purges_and_old_lines_of_a_kept_file_are_not_read() {
        let base = temp("open-purge");
        let dir = base.join("audit");
        std::fs::create_dir_all(&dir).unwrap();
        // Il y a 8 mois : supprimé à l'ouverture.
        let old = Local::now()
            .checked_sub_months(Months::new(8))
            .unwrap()
            .timestamp() as f64;
        let old_name = file_name_of(old).unwrap();
        std::fs::write(dir.join(&old_name), "{}\n").unwrap();
        let _opened = AuditLog::open(&base);
        assert!(!dir.join(&old_name).exists());

        // Dans le mois « à cheval » : la ligne d'il y a 6 mois et 10 jours est ignorée.
        let now = ts(2026, 11, 22);
        let log = AuditLog {
            dir: Some(dir.clone()),
            purged: Mutex::new(Some(month_of(now).unwrap())),
            throttle: Mutex::new(HashMap::new()),
        };
        log.record(Entry::new(Action::Login, &who()).at(ts(2026, 5, 12)));
        log.record(Entry::new(Action::Login, &who()).at(ts(2026, 5, 28)));
        let read = log.read_at(
            &Filter {
                since: Some(ts(2026, 5, 1)),
                ..Filter::default()
            },
            now,
        );
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].at, ts(2026, 5, 28));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn throttled_lines_are_written_once_per_window() {
        let base = temp("throttle");
        let log = AuditLog::open(&base);
        let at = ts(2026, 11, 22);
        for i in 0..5 {
            log.record_throttled(
                "ip:1",
                60.0,
                Entry::new(Action::Login, &Who::default())
                    .at(at + f64::from(i))
                    .result(Outcome::Refused),
            );
        }
        log.record_throttled(
            "ip:2",
            60.0,
            Entry::new(Action::Login, &Who::default())
                .at(at)
                .result(Outcome::Refused),
        );
        log.record_throttled(
            "ip:1",
            60.0,
            Entry::new(Action::Login, &Who::default())
                .at(at + 61.0)
                .result(Outcome::Refused),
        );
        assert_eq!(log.read_at(&Filter::default(), at + 100.0).len(), 3);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn texts_are_cleaned_and_a_broken_directory_never_fails_the_action() {
        let entry = Entry::new(Action::DeviceCommand, &Who::default())
            .target("Lampe\ncanapé\u{7}")
            .detail("x".repeat(500));
        assert_eq!(entry.target.as_deref(), Some("Lampecanapé"));
        assert_eq!(
            entry.detail.as_ref().map(|d| d.chars().count()),
            Some(MAX_TEXT)
        );
        // `audit` est un fichier : le dossier ne peut pas être créé. Rien ne panique.
        let base = temp("broken");
        std::fs::write(base.join(DIR), "pas un dossier").unwrap();
        let log = AuditLog::open(&base);
        log.record(entry);
        assert!(log.read(&Filter::default()).is_empty());
        AuditLog::disabled().record(Entry::new(Action::Export, &Who::default()));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn unknown_actions_and_missing_fields_still_read() {
        let base = temp("future");
        let dir = base.join("audit");
        std::fs::create_dir_all(&dir).unwrap();
        let at = ts(2026, 11, 22);
        std::fs::write(
            dir.join("2026-11.jsonl"),
            format!("{{\"at\":{at},\"action\":\"quelque_chose_de_neuf\",\"result\":\"ok\"}}\n"),
        )
        .unwrap();
        let read = AuditLog::reader(&base).read_at(&Filter::default(), at + 10.0);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].action, Action::Other);
        assert_eq!(describe(&read[0]).1, "Action inconnue");
        assert_eq!(who_label(&read[0]), "Inconnu");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn lines_read_like_the_mockup() {
        let at = 1_763_800_000.0;
        let web = who();
        assert_eq!(
            describe(&Entry::new(Action::Login, &web).at(at)).1,
            "Connexion à l'app web, sur place"
        );
        let mut mac = who();
        mac.connection = Some(ConnRef::new(Some("mac-salon-0001"), "Mac du salon", "mac"));
        mac.via = Some(Via::Tailscale);
        assert_eq!(
            describe(&Entry::new(Action::Login, &mac)).1,
            "Connexion à l'app Mac, à distance"
        );
        let mut unknown = who();
        unknown.connection = None;
        let (tone, text) = describe(&Entry::new(Action::Login, &unknown));
        assert_eq!(tone, Tone::Warn);
        assert_eq!(
            text,
            "Connexion inconnue, sur place : l'app n'est pas encore à jour"
        );
        assert_eq!(from_label(&Entry::new(Action::Login, &unknown)), "—");
        let refused = Entry::new(Action::Login, &Who::default()).result(Outcome::Refused);
        assert_eq!(
            describe(&refused),
            (Tone::Warn, "Connexion refusée, sur place".into())
        );
        assert_eq!(who_label(&refused), "Inconnu");
        let lamp = Entry::new(Action::DeviceCommand, &web)
            .target("Lampe canapé")
            .detail("on");
        assert_eq!(
            describe(&lamp),
            (Tone::Device, "Lampe canapé : allumé".into())
        );
        let cover = Entry::new(Action::DeviceCommand, &web)
            .target("Volet salon")
            .detail("position:40")
            .result(Outcome::Error);
        assert_eq!(describe(&cover).1, "Volet salon : à 40 % (échec)");
        let night = Entry::new(Action::ModeChanged, &web)
            .target("nuit")
            .source(Origin::Shortcut)
            .detail("Mode nuit");
        assert_eq!(
            describe(&night).1,
            "Mode Nuit, par le raccourci « Mode nuit »"
        );
        let chosen = Entry::new(Action::ModeChanged, &web)
            .target("present")
            .source(Origin::Person)
            .detail("l'app web");
        assert_eq!(describe(&chosen).1, "Mode Présent, choisi dans l'app web");
        let auto = Entry::new(Action::ModeChanged, &web)
            .target("present")
            .source(Origin::Automatic);
        assert_eq!(
            describe(&auto).1,
            "La maison passe en Présent (automatique)"
        );
        let rule = Entry::new(Action::ModeChanged, &Who::default())
            .target("absent")
            .source(Origin::Automation)
            .detail("Départ");
        assert_eq!(who_label(&rule), "Automatisation");
        assert_eq!(
            describe(&rule).1,
            "Mode Absent, par l'automatisation « Départ »"
        );
        let cut = Entry::new(Action::ConnectionCut, &Who::server_app()).target("iPhone de Sam");
        assert_eq!(
            describe(&cut).1,
            "Connexion « iPhone de Sam » coupée depuis Ostal Server"
        );
        assert_eq!(who_label(&cut), "Serveur");
        let away = Entry::new(Action::Presence, &web).target("away");
        assert_eq!(describe(&away).1, "Hamdi : « Je pars »");
        assert_eq!(clock_label(at).len(), 5);
    }
}
