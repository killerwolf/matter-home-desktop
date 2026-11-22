//! Ce que la page « Widgets et raccourcis » de l'app web installe sur l'iPhone, **déjà rempli** :
//!
//! - les quatre raccourcis de la personne (« Je pars », « Je rentre », « Mode nuit »,
//!   « Mode vacances »), avec l'adresse publique du serveur et **son** jeton de mode écrits dedans,
//!   puis signés par `/usr/bin/shortcuts sign` ;
//! - le script du widget Scriptable, avec l'adresse et **son** jeton de lecture écrits dedans.
//!
//! Rien à taper sur l'iPhone. Les noms des raccourcis sont **sans accent** : le nom importé vient du
//! nom du fichier, qu'iOS enregistre en Unicode décomposé (« e » + accent) ; le widget, qui les
//! ouvre par `shortcuts://run-shortcut?name=…` en Unicode composé, ne les trouvait pas.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use protocol::wire::{ApiError, ErrorCode};
use rand::RngCore;

/// Noms des raccourcis : ceux des fichiers téléchargés, et ceux que le widget appelle.
pub const LEAVING_NAME: &str = "Je pars";
pub const ARRIVING_NAME: &str = "Je rentre";
pub const NUIT_NAME: &str = "Mode nuit";
pub const VACANCES_NAME: &str = "Mode vacances";

/// Le script du widget, avec deux emplacements à remplir.
const WIDGET_TEMPLATE: &str = include_str!("../web/raccourcis/ostal.js");
const SERVER_SLOT: &str = "const SERVEUR = \"__SERVEUR__\";";
const TOKEN_SLOT: &str = "const TOKEN = \"__JETON_DE_LECTURE__\";";

/// Délai d'une signature : elle passe par les serveurs d'Apple (compte iCloud du Mac).
const SIGN_TIMEOUT: Duration = Duration::from_secs(25);
/// Délai de l'ensemble des signatures. Elles se font l'une après l'autre ; la requête de l'app web
/// n'a pas de délai à elle, mais Safari lâche une requête restée sans réponse environ une minute.
/// Quatre signatures de 25 s feraient 100 s : on s'arrête à 55 s, avec le même message d'erreur,
/// et rien ne change (le jeton n'est remplacé que si tout a réussi).
const SIGN_BUDGET: Duration = Duration::from_secs(55);
const SHORTCUTS_BIN: &str = "/usr/bin/shortcuts";

/// Un raccourci de la page « Widgets et raccourcis » : deux qui disent où est la personne (« Je
/// pars », « Je rentre ») et deux qui changent le mode de toute la maison, à la main (Nuit,
/// Vacances).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeShortcut {
    Leaving,
    Arriving,
    Nuit,
    Vacances,
}

/// Ce que fait un raccourci : une requête `PUT` avec un corps JSON d'un seul champ.
struct Request {
    path: &'static str,
    key: &'static str,
    value: &'static str,
}

impl HomeShortcut {
    pub const ALL: [HomeShortcut; 4] = [
        HomeShortcut::Leaving,
        HomeShortcut::Arriving,
        HomeShortcut::Nuit,
        HomeShortcut::Vacances,
    ];

    /// Nom du raccourci (et du fichier, sans `.shortcut`), en ASCII.
    pub fn name(self) -> &'static str {
        match self {
            HomeShortcut::Leaving => LEAVING_NAME,
            HomeShortcut::Arriving => ARRIVING_NAME,
            HomeShortcut::Nuit => NUIT_NAME,
            HomeShortcut::Vacances => VACANCES_NAME,
        }
    }

    /// La requête envoyée : `PUT /api/presence {"state": …}` pour dire où l'on est (rien d'autre ne
    /// change), `PUT /api/mode {"mode": …}` pour changer le mode de la maison.
    fn request(self) -> Request {
        match self {
            HomeShortcut::Leaving => Request {
                path: "/api/presence",
                key: "state",
                value: "away",
            },
            HomeShortcut::Arriving => Request {
                path: "/api/presence",
                key: "state",
                value: "home",
            },
            HomeShortcut::Nuit => Request {
                path: "/api/mode",
                key: "mode",
                value: "nuit",
            },
            HomeShortcut::Vacances => Request {
                path: "/api/mode",
                key: "mode",
                value: "vacances",
            },
        }
    }

    /// La clé de la réponse qui dit que le serveur a bien répondu. Pour « Je pars » et « Je rentre »,
    /// c'est `message` : la phrase du serveur dit ce que la maison est devenue (armée, ou laissée en
    /// Présent parce que quelqu'un d'autre est là), et la notification l'affiche telle quelle.
    fn reply_key(self) -> &'static str {
        match self {
            HomeShortcut::Leaving | HomeShortcut::Arriving => "message",
            HomeShortcut::Nuit | HomeShortcut::Vacances => self.request().key,
        }
    }

    /// Ce que dit la notification quand le serveur a répondu. `None` : la phrase vient du serveur
    /// (clé `message`).
    fn done_text(self) -> Option<&'static str> {
        match self {
            HomeShortcut::Leaving | HomeShortcut::Arriving => None,
            HomeShortcut::Nuit => Some("Maison en mode Nuit."),
            HomeShortcut::Vacances => Some("Maison en mode Vacances."),
        }
    }

    /// Ce que dit la notification quand le serveur a refusé.
    fn failed_text(self) -> &'static str {
        match self {
            HomeShortcut::Leaving | HomeShortcut::Arriving => {
                "Rien n'a changé. Réponse du serveur : "
            }
            HomeShortcut::Nuit | HomeShortcut::Vacances => {
                "Le mode n'a pas changé. Réponse du serveur : "
            }
        }
    }

    /// Couleur et pictogramme de l'icône (ceux des raccourcis d'avant : « Je pars » reprend celui
    /// d'Absent, « Je rentre » celui de Présent). Nuit et Vacances reprennent celui d'Absent : on
    /// n'invente ni couleur ni glyphe sans l'accord d'Hamdi.
    fn icon(self) -> (i64, i64) {
        match self {
            HomeShortcut::Leaving | HomeShortcut::Nuit | HomeShortcut::Vacances => {
                (4292093695, 59446)
            }
            HomeShortcut::Arriving => (431817727, 59511),
        }
    }

    pub fn file_name(self) -> String {
        format!("{}.shortcut", self.name())
    }
}

/// L'adresse publique du serveur, telle que la page l'a ouverte (`location.origin`) : seulement
/// `https://<nom>.ts.net`, sans port. L'adresse directe (`:8443`, certificat auto-signé) est
/// refusée par Raccourcis et Scriptable : on ne l'écrit jamais dans un fichier.
pub fn public_address(origin: &str) -> Result<String, ApiError> {
    let refuse = || {
        ApiError::with_detail(
            ErrorCode::BadRequest,
            "Ouvre l'app par son adresse en .ts.net (Tailscale) : l'iPhone ne peut pas utiliser cette adresse-ci.",
        )
    };
    let host = origin
        .trim()
        .strip_prefix("https://")
        .ok_or_else(refuse)?
        .trim_end_matches('/')
        .to_ascii_lowercase();
    let well_formed = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && host
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'));
    let name = host.strip_suffix(".ts.net").unwrap_or("");
    if !well_formed || name.is_empty() {
        return Err(refuse());
    }
    Ok(format!("https://{host}"))
}

// ---------------------------------------------------------------------------
// Le raccourci (plist XML), porté de `build_shortcuts.py` (`mode_shortcut`)
// ---------------------------------------------------------------------------

/// Une valeur de liste de propriétés Apple.
enum Plist {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

fn s(text: &str) -> Plist {
    Plist::Str(text.to_string())
}

fn dict(pairs: Vec<(&'static str, Plist)>) -> Plist {
    Plist::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

impl Plist {
    fn write(&self, out: &mut String) {
        match self {
            Plist::Str(text) => {
                out.push_str("<string>");
                escape(text, out);
                out.push_str("</string>");
            }
            Plist::Int(n) => out.push_str(&format!("<integer>{n}</integer>")),
            Plist::Bool(true) => out.push_str("<true/>"),
            Plist::Bool(false) => out.push_str("<false/>"),
            Plist::Array(items) => {
                out.push_str("<array>");
                for item in items {
                    item.write(out);
                }
                out.push_str("</array>");
            }
            Plist::Dict(pairs) => {
                out.push_str("<dict>");
                for (key, value) in pairs {
                    out.push_str("<key>");
                    escape(key, out);
                    out.push_str("</key>");
                    value.write(out);
                }
                out.push_str("</dict>");
            }
        }
    }
}

/// Le caractère qui marque une variable dans un texte de Raccourcis.
const OBJ: char = '\u{FFFC}';

fn uuid() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode_upper(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// La sortie d'une action, utilisable comme variable.
fn output(uuid: &str, name: &str) -> Plist {
    dict(vec![
        ("OutputUUID", s(uuid)),
        ("Type", s("ActionOutput")),
        ("OutputName", s(name)),
    ])
}

/// Un texte avec des variables : chaque `OBJ` de `text` est remplacé par la variable suivante.
/// Les positions de `attachmentsByRange` comptent en UTF-16 (`NSString`), pas en octets.
fn tok(text: &str, attachments: Vec<Plist>) -> Plist {
    let mut attachments = attachments.into_iter();
    let mut by_range = Vec::new();
    let mut position = 0usize;
    for c in text.chars() {
        if c == OBJ {
            let value = attachments.next().expect("une variable par marque");
            by_range.push((format!("{{{position}, 1}}"), value));
        }
        position += c.len_utf16();
    }
    assert!(
        attachments.next().is_none(),
        "autant de variables que de marques"
    );
    dict(vec![
        ("WFSerializationType", s("WFTextTokenString")),
        (
            "Value",
            dict(vec![
                ("string", s(text)),
                ("attachmentsByRange", Plist::Dict(by_range)),
            ]),
        ),
    ])
}

fn attachment(value: Plist) -> Plist {
    dict(vec![
        ("WFSerializationType", s("WFTextTokenAttachment")),
        ("Value", value),
    ])
}

/// Un champ « dictionnaire » de Raccourcis (en-têtes, corps JSON).
fn dict_field(pairs: &[(&str, &str)]) -> Plist {
    let items = pairs
        .iter()
        .map(|(key, value)| {
            dict(vec![
                ("WFItemType", Plist::Int(0)),
                ("WFKey", tok(key, vec![])),
                ("WFValue", tok(value, vec![])),
            ])
        })
        .collect();
    dict(vec![
        ("WFSerializationType", s("WFDictionaryFieldValue")),
        (
            "Value",
            dict(vec![("WFDictionaryFieldValueItems", Plist::Array(items))]),
        ),
    ])
}

fn action(identifier: &str, parameters: Vec<(&'static str, Plist)>) -> Plist {
    dict(vec![
        ("WFWorkflowActionIdentifier", s(identifier)),
        ("WFWorkflowActionParameters", dict(parameters)),
    ])
}

fn notify(body: Plist) -> Plist {
    action(
        "is.workflow.actions.notification",
        vec![
            ("WFNotificationActionTitle", s("Ostal")),
            ("WFNotificationActionBody", body),
            ("WFNotificationActionSound", Plist::Bool(false)),
        ],
    )
}

fn conditional(group: &str, mode: i64, input: Option<Plist>) -> Plist {
    let mut parameters = vec![
        ("GroupingIdentifier", s(group)),
        ("WFControlFlowMode", Plist::Int(mode)),
    ];
    if let Some(input) = input {
        parameters.push((
            "WFInput",
            dict(vec![
                ("Type", s("Variable")),
                ("Variable", attachment(input)),
            ]),
        ));
        // 100 : « a une valeur ».
        parameters.push(("WFCondition", Plist::Int(100)));
    }
    action("is.workflow.actions.conditional", parameters)
}

/// Un raccourci, en plist XML (non signé). `address` vient de `public_address`, `token` est le jeton
/// de mode **de la personne** : tous deux écrits en dur, aucune question à l'import.
///
/// `PUT {address}/api/presence` ou `/api/mode` avec `Authorization: Bearer <jeton>` et un corps
/// JSON d'un champ ; notification de réussite seulement si la réponse a la clé attendue (`message` pour
/// « Je pars » et « Je rentre », qu'elle affiche ; `mode` pour Nuit et Vacances), sinon la réponse du serveur.
pub fn shortcut_plist(which: HomeShortcut, address: &str, token: &str) -> String {
    let (request, value, group) = (uuid(), uuid(), uuid());
    let what = which.request();
    let reply = || output(&request, "Contenu de l'URL");
    let actions = vec![
        action(
            "is.workflow.actions.downloadurl",
            vec![
                ("UUID", s(&request)),
                ("WFURL", tok(&format!("{address}{}", what.path), vec![])),
                ("WFHTTPMethod", s("PUT")),
                ("ShowHeaders", Plist::Bool(true)),
                (
                    "WFHTTPHeaders",
                    dict_field(&[("Authorization", &format!("Bearer {token}"))]),
                ),
                ("WFHTTPBodyType", s("JSON")),
                ("WFJSONValues", dict_field(&[(what.key, what.value)])),
            ],
        ),
        action(
            "is.workflow.actions.getvalueforkey",
            vec![
                ("UUID", s(&value)),
                ("WFInput", attachment(reply())),
                ("WFDictionaryKey", s(which.reply_key())),
                ("WFGetDictionaryValueType", s("Value")),
            ],
        ),
        conditional(&group, 0, Some(output(&value, "Valeur du dictionnaire"))),
        notify(match which.done_text() {
            Some(text) => s(text),
            None => tok(
                &OBJ.to_string(),
                vec![output(&value, "Valeur du dictionnaire")],
            ),
        }),
        conditional(&group, 1, None),
        notify(tok(&format!("{}{OBJ}", which.failed_text()), vec![reply()])),
        conditional(&group, 2, None),
    ];
    let (color, glyph) = which.icon();
    let workflow = dict(vec![
        ("WFWorkflowActions", Plist::Array(actions)),
        ("WFWorkflowClientVersion", s("2302.0.4")),
        ("WFWorkflowMinimumClientVersion", Plist::Int(900)),
        ("WFWorkflowMinimumClientVersionString", s("900")),
        ("WFWorkflowHasShortcutInputVariables", Plist::Bool(false)),
        (
            "WFWorkflowIcon",
            dict(vec![
                ("WFWorkflowIconStartColor", Plist::Int(color)),
                ("WFWorkflowIconGlyphNumber", Plist::Int(glyph)),
            ]),
        ),
        ("WFWorkflowImportQuestions", Plist::Array(vec![])),
        ("WFWorkflowInputContentItemClasses", Plist::Array(vec![])),
        ("WFWorkflowOutputContentItemClasses", Plist::Array(vec![])),
        ("WFWorkflowTypes", Plist::Array(vec![])),
        ("WFQuickActionSurfaces", Plist::Array(vec![])),
    ]);
    let mut body = String::new();
    workflow.write(&mut body);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">{body}</plist>\n"
    )
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// Qui signe les raccourcis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Signer {
    /// `/usr/bin/shortcuts sign --mode anyone` (le vrai, sur le Mac du serveur).
    #[default]
    Apple,
    /// Pour les essais seulement : le fichier rendu est le plist tel quel.
    Unsigned,
}

fn sign_error(detail: &str) -> ApiError {
    ApiError::with_detail(
        ErrorCode::Unavailable,
        format!(
            "La signature des raccourcis a échoué sur le Mac ({detail}). Il faut une session ouverte sur le Mac et un compte iCloud connecté. Rien n'a changé : les raccourcis déjà installés marchent toujours."
        ),
    )
}

/// Un dossier temporaire privé (0700), supprimé avec tout son contenu à la sortie, quoi qu'il
/// arrive : le plist d'entrée porte le jeton en clair.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> std::io::Result<Self> {
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        let path = std::env::temp_dir().join(format!("ostal-raccourci-{}", hex::encode(random)));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

impl Signer {
    /// Signe un raccourci (plist XML) et rend le fichier `.shortcut`. Bloquant (quelques secondes) :
    /// à appeler hors des threads du réseau. Aucun contenu (ni jeton) dans les erreurs ni le journal.
    pub fn sign(self, plist: &str) -> Result<Vec<u8>, ApiError> {
        self.sign_within(plist, SIGN_TIMEOUT)
    }

    fn sign_within(self, plist: &str, timeout: Duration) -> Result<Vec<u8>, ApiError> {
        match self {
            Signer::Unsigned => Ok(plist.as_bytes().to_vec()),
            Signer::Apple => sign_with(Path::new(SHORTCUTS_BIN), plist, timeout),
        }
    }

    /// Les raccourcis de `HomeShortcut::ALL`, remplis puis signés l'un après l'autre : `(nom de
    /// fichier, contenu)`. Un échec, ou le délai total ([`SIGN_BUDGET`]), annule tout.
    pub fn sign_shortcuts(
        self,
        address: &str,
        token: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
        sign_all(SIGN_BUDGET, |which, timeout| {
            self.sign_within(&shortcut_plist(which, address, token), timeout)
        })
    }
}

/// Signe tous les raccourcis avec `sign(raccourci, délai restant)`, sans dépasser `budget` en tout.
fn sign_all(
    budget: Duration,
    mut sign: impl FnMut(HomeShortcut, Duration) -> Result<Vec<u8>, ApiError>,
) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
    let deadline = Instant::now() + budget;
    let mut files = Vec::with_capacity(HomeShortcut::ALL.len());
    for which in HomeShortcut::ALL {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            log::error!(
                "signature des raccourcis : délai total de {}s dépassé",
                budget.as_secs()
            );
            return Err(sign_error("délai dépassé"));
        }
        files.push((which.file_name(), sign(which, left.min(SIGN_TIMEOUT))?));
    }
    Ok(files)
}

fn sign_with(program: &Path, plist: &str, timeout: Duration) -> Result<Vec<u8>, ApiError> {
    let dir = PrivateDir::create().map_err(|e| {
        log::error!("signature des raccourcis : dossier temporaire impossible : {e}");
        sign_error("dossier temporaire")
    })?;
    let input = dir.0.join("entree.shortcut");
    let output = dir.0.join("signe.shortcut");
    write_private(&input, plist.as_bytes()).map_err(|e| {
        log::error!("signature des raccourcis : écriture impossible : {e}");
        sign_error("écriture")
    })?;
    let mut child = Command::new(program)
        .args(["sign", "--mode", "anyone", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            log::error!(
                "signature des raccourcis : {} introuvable : {e}",
                program.display()
            );
            sign_error("outil shortcuts introuvable")
        })?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                log::error!(
                    "signature des raccourcis : délai de {}s dépassé",
                    timeout.as_secs()
                );
                return Err(sign_error("délai dépassé"));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                log::error!("signature des raccourcis : {e}");
                return Err(sign_error("outil interrompu"));
            }
        }
    };
    if !status.success() {
        log::error!("signature des raccourcis : l'outil a échoué ({status})");
        return Err(sign_error("refusée"));
    }
    let signed = std::fs::read(&output).map_err(|e| {
        log::error!("signature des raccourcis : fichier signé illisible : {e}");
        sign_error("fichier signé absent")
    })?;
    // Un raccourci signé est un conteneur Apple Encrypted Archive.
    if !signed.starts_with(b"AEA1") {
        log::error!("signature des raccourcis : le fichier rendu n'est pas signé");
        return Err(sign_error("fichier non signé"));
    }
    Ok(signed)
}

// ---------------------------------------------------------------------------
// Les liens des raccourcis préparés
// ---------------------------------------------------------------------------

/// Durée de vie des liens de téléchargement.
pub const LINK_TTL: Duration = Duration::from_secs(5 * 60);

/// Les raccourcis préparés, en mémoire seulement, chacun au nom de la personne qui les a demandés
/// (ils portent son jeton de mode). Préparer les siens ne touche jamais aux liens des autres.
#[derive(Default)]
pub struct Downloads {
    files: std::sync::Mutex<std::collections::HashMap<String, Prepared>>,
}

struct Prepared {
    owner: String,
    file_name: String,
    body: Vec<u8>,
    expires: Instant,
}

impl Downloads {
    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Prepared>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remplace les liens d'avant **de cette personne** par ceux-ci ; rend leurs identifiants
    /// (128 bits), dans l'ordre.
    pub fn replace(&self, owner: &str, files: Vec<(String, Vec<u8>)>) -> Vec<String> {
        let mut map = self.lock();
        map.retain(|_, p| p.owner != owner);
        let expires = Instant::now() + LINK_TTL;
        files
            .into_iter()
            .map(|(file_name, body)| {
                let mut raw = [0u8; 16];
                rand::thread_rng().fill_bytes(&mut raw);
                let id = hex::encode(raw);
                map.insert(
                    id.clone(),
                    Prepared {
                        owner: owner.to_string(),
                        file_name,
                        body,
                        expires,
                    },
                );
                id
            })
            .collect()
    }

    /// Oublie les liens de cette personne.
    pub fn clear(&self, owner: &str) {
        self.lock().retain(|_, p| p.owner != owner);
    }

    /// Un fichier encore valable, sous son identifiant **et** son nom.
    pub fn get(&self, id: &str, file_name: &str) -> Option<Vec<u8>> {
        let mut map = self.lock();
        let now = Instant::now();
        map.retain(|_, p| p.expires > now);
        map.get(id)
            .filter(|p| p.file_name == file_name)
            .map(|p| p.body.clone())
    }
}

/// Au démarrage : supprime les dossiers de signature restés d'un arrêt brutal (ils portent un
/// jeton en clair). Seulement ceux de plus de 10 minutes, pour ne jamais toucher à la signature en
/// cours d'un autre serveur du même compte.
pub fn remove_leftovers() {
    remove_leftovers_in(&std::env::temp_dir(), Duration::from_secs(10 * 60));
}

fn remove_leftovers_in(dir: &Path, older_than: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("ostal-raccourci-")
            && !name.starts_with(crate::legacy::SHORTCUT_TEMP_PREFIX)
        {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if old && std::fs::remove_dir_all(entry.path()).is_ok() {
            log::info!(
                "dossier de signature resté supprimé : {}",
                entry.path().display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Le widget Scriptable
// ---------------------------------------------------------------------------

/// Le script du widget, prêt à coller : adresse et jeton de lecture écrits dedans (chaînes JSON,
/// donc sans risque de casser le script).
pub fn widget_script(address: &str, read_token: &str) -> String {
    let json = |v: &str| serde_json::to_string(v).expect("texte");
    WIDGET_TEMPLATE
        .replacen(
            SERVER_SLOT,
            &format!("const SERVEUR = {};", json(address)),
            1,
        )
        .replacen(
            TOKEN_SLOT,
            &format!("const TOKEN = {};", json(read_token)),
            1,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn only_a_ts_net_https_address_without_port_is_accepted() {
        assert_eq!(
            public_address("https://Mac-Mini.tailnet-12ab.ts.net").unwrap(),
            "https://mac-mini.tailnet-12ab.ts.net"
        );
        assert_eq!(
            public_address("https://mac.tailnet-12ab.ts.net/").unwrap(),
            "https://mac.tailnet-12ab.ts.net"
        );
        for bad in [
            "https://192.168.1.20:8443",
            "https://mac.tailnet-12ab.ts.net:8443",
            "https://mac.tailnet-12ab.ts.net:443",
            "http://mac.tailnet-12ab.ts.net",
            "https://.ts.net",
            "https://ts.net",
            "https://exemple.invalid",
            "https://mac..ts.net",
            "https://-mac.ts.net",
            "https://mac.ts.net/app/",
            "https://user@mac.ts.net",
            "https://mac.ts.net\"<",
            "https://mac.ts.net\u{0}",
            "null",
            "",
        ] {
            let e = public_address(bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest, "{bad}");
            assert!(e.detail.as_deref().unwrap().contains(".ts.net"), "{bad}");
        }
    }

    #[test]
    fn the_shortcut_carries_address_token_and_no_import_question() {
        let xml = shortcut_plist(HomeShortcut::Nuit, "https://mac.tailnet-12ab.ts.net", TOKEN);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<string>https://mac.tailnet-12ab.ts.net/api/mode</string>"));
        assert!(xml.contains(&format!("<string>Bearer {TOKEN}</string>")));
        assert!(xml.contains("<key>WFHTTPMethod</key><string>PUT</string>"));
        assert!(xml.contains("<key>WFHTTPBodyType</key><string>JSON</string>"));
        assert!(xml.contains("<string>nuit</string>"));
        assert!(xml.contains("<string>Maison en mode Nuit.</string>"));
        assert!(xml.contains("<key>WFWorkflowImportQuestions</key><array></array>"));
        assert!(!xml.contains("is.workflow.actions.gettext"));
        // La notification d'échec cite la réponse : la marque est en position UTF-16 (« é » compte 1).
        let failure = "Le mode n'a pas changé. Réponse du serveur : ";
        let position = failure.encode_utf16().count();
        assert_eq!(position, failure.chars().count());
        assert!(position < failure.len(), "le texte a des accents");
        assert!(xml.contains(&format!(
            "<key>attachmentsByRange</key><dict><key>{{{position}, 1}}</key><dict><key>OutputUUID</key>"
        )));
        // Les textes sans variable ont un dictionnaire vide.
        assert!(xml.contains("<key>attachmentsByRange</key><dict></dict>"));
    }

    /// « Je pars » et « Je rentre » disent où l'on est : ils appellent `/api/presence`, jamais
    /// `/api/mode` (c'est le serveur qui, provisoirement, en tire le mode).
    #[test]
    fn leaving_and_arriving_only_declare_where_the_person_is() {
        let leaving = shortcut_plist(HomeShortcut::Leaving, "https://m.ts.net", TOKEN);
        assert!(leaving.contains("<string>https://m.ts.net/api/presence</string>"));
        assert!(!leaving.contains("/api/mode"));
        assert!(leaving.contains("<string>away</string>"));
        // La notification affiche la phrase du serveur (`message`), pas un texte fixe : « Je pars »
        // ne doit pas laisser croire que la maison est armée quand elle ne l'est pas.
        assert!(leaving.contains("<key>WFDictionaryKey</key><string>message</string>"));
        assert!(!leaving.contains("C'est noté"));
        assert!(leaving.contains(&format!("<string>Bearer {TOKEN}</string>")));
        let arriving = shortcut_plist(HomeShortcut::Arriving, "https://m.ts.net", TOKEN);
        assert!(arriving.contains("<string>home</string>"));
        assert!(arriving.contains("<key>WFDictionaryKey</key><string>message</string>"));
        assert!(!arriving.contains("C'est noté"));
        assert!(!arriving.contains("/api/mode"));
    }

    /// Nuit et Vacances envoient une clé de mode que le serveur connaît, et disent le bon mode.
    #[test]
    fn the_mode_shortcuts_send_a_known_mode_and_say_it() {
        use protocol::mode::HomeMode;
        for which in [HomeShortcut::Nuit, HomeShortcut::Vacances] {
            let request = which.request();
            assert_eq!((request.path, request.key), ("/api/mode", "mode"));
            let mode = HomeMode::from_key(request.value).expect("clé de mode connue");
            let xml = shortcut_plist(which, "https://m.ts.net", TOKEN);
            assert!(xml.contains(&format!("<string>{}</string>", mode.key())));
            assert!(xml.contains(&format!("Maison en mode {}.", mode.label())));
            assert_eq!(which.file_name(), format!("{}.shortcut", which.name()));
        }
        // Nuit et Vacances reprennent l'icône d'Absent, que « Je pars » reprend aussi.
        assert_eq!(HomeShortcut::Nuit.icon(), HomeShortcut::Leaving.icon());
        assert_eq!(HomeShortcut::Vacances.icon(), HomeShortcut::Leaving.icon());
        assert_ne!(HomeShortcut::Arriving.icon(), HomeShortcut::Leaving.icon());
    }

    #[test]
    fn four_shortcuts_are_prepared_and_signed_within_the_total_budget() {
        // Sans signature (essais) : quatre fichiers, dans l'ordre, chacun avec son mode.
        let files = Signer::Unsigned
            .sign_shortcuts("https://m.ts.net", TOKEN)
            .unwrap();
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "Je pars.shortcut",
                "Je rentre.shortcut",
                "Mode nuit.shortcut",
                "Mode vacances.shortcut"
            ]
        );
        assert!(String::from_utf8_lossy(&files[3].1).contains("<string>vacances</string>"));
        // Chaque signature reçoit au plus SIGN_TIMEOUT, et moins quand le budget fond.
        let mut asked = Vec::new();
        let files = sign_all(Duration::from_secs(55), |_, left| {
            asked.push(left);
            Ok(b"x".to_vec())
        })
        .unwrap();
        assert_eq!(files.len(), 4);
        assert!(asked.iter().all(|d| *d <= SIGN_TIMEOUT));
        assert!(asked
            .windows(2)
            .all(|w| w[1] <= w[0] + Duration::from_millis(1)));
        // Budget épuisé : erreur claire, avant toute signature.
        let mut called = false;
        let e = sign_all(Duration::ZERO, |_, _| {
            called = true;
            Ok(vec![])
        })
        .unwrap_err();
        assert!(!called);
        assert_eq!(e.code, ErrorCode::Unavailable);
        assert!(e.detail.as_deref().unwrap().contains("Rien n'a changé"));
        // Un échec en route annule tout.
        let mut n = 0;
        assert!(sign_all(Duration::from_secs(55), |_, _| {
            n += 1;
            if n == 3 {
                Err(sign_error("essai"))
            } else {
                Ok(vec![])
            }
        })
        .is_err());
        assert_eq!(n, 3);
        // Le budget total laisse la place de quatre signatures « normales » (quelques secondes).
        assert!(SIGN_BUDGET >= 4 * Duration::from_secs(10) && SIGN_BUDGET >= SIGN_TIMEOUT);
    }

    #[test]
    fn the_plist_is_well_formed_for_plutil() {
        // `plutil -lint` (macOS) relit le fichier comme le ferait `shortcuts sign`.
        let plutil = Path::new("/usr/bin/plutil");
        if !plutil.exists() {
            return;
        }
        let dir = PrivateDir::create().unwrap();
        for which in HomeShortcut::ALL {
            let file = dir.0.join(which.file_name());
            let xml = shortcut_plist(which, "https://m.ts.net", TOKEN);
            write_private(&file, xml.as_bytes()).unwrap();
            let out = Command::new(plutil)
                .arg("-lint")
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{which:?} : {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }

    #[test]
    fn names_are_ascii_and_match_the_web_page_and_the_widget() {
        for which in HomeShortcut::ALL {
            assert!(which.name().is_ascii());
        }
        let page = include_str!("../web/widgets.js");
        for which in HomeShortcut::ALL {
            assert!(page.contains(&format!("\"{}\"", which.name())), "{which:?}");
        }
        assert_eq!(
            HomeShortcut::ALL.map(HomeShortcut::name),
            ["Je pars", "Je rentre", "Mode nuit", "Mode vacances"]
        );
        assert!(WIDGET_TEMPLATE.contains(&format!("const RACCOURCI_PART = \"{LEAVING_NAME}\";")));
        assert!(WIDGET_TEMPLATE.contains(&format!("const RACCOURCI_RENTRE = \"{ARRIVING_NAME}\";")));
    }

    #[test]
    fn the_widget_script_is_filled_once_with_address_and_read_token() {
        assert_eq!(WIDGET_TEMPLATE.matches(SERVER_SLOT).count(), 1);
        assert_eq!(WIDGET_TEMPLATE.matches(TOKEN_SLOT).count(), 1);
        let script = widget_script("https://mac.tailnet-12ab.ts.net", TOKEN);
        assert!(script.contains("const SERVEUR = \"https://mac.tailnet-12ab.ts.net\";"));
        assert!(script.contains(&format!("const TOKEN = \"{TOKEN}\";")));
        assert!(!script.contains("__SERVEUR__") && !script.contains("__JETON_DE_LECTURE__"));
        assert!(script.starts_with("// Variables used by Scriptable."));
        // Plus de connexion par mot de passe ni de jeton de session gardé.
        for gone in ["/api/login", "Keychain", "addSecureTextField", "password"] {
            assert!(!script.contains(gone), "{gone}");
        }
        // L'en-tête est posé d'un bloc (Scriptable ignore une clé modifiée après coup).
        assert!(script.contains("req.headers = headers;"));
        assert!(!script.contains("req.headers."));
    }

    #[test]
    fn links_are_unguessable_replaced_checked_by_name_and_expire() {
        let downloads = Downloads::default();
        let first = downloads.replace("lea", vec![("Je pars.shortcut".into(), b"a".to_vec())]);
        assert_eq!(first[0].len(), 32);
        // Réutilisable pendant sa durée de vie, mais seulement sous son nom.
        assert_eq!(downloads.get(&first[0], "Je pars.shortcut").unwrap(), b"a");
        assert!(downloads.get(&first[0], "Je pars.shortcut").is_some());
        assert!(downloads.get(&first[0], "Je rentre.shortcut").is_none());
        let second = downloads.replace(
            "lea",
            vec![
                ("Je pars.shortcut".into(), b"b".to_vec()),
                ("Je rentre.shortcut".into(), b"c".to_vec()),
            ],
        );
        assert!(downloads.get(&first[0], "Je pars.shortcut").is_none());
        assert_eq!(
            downloads.get(&second[1], "Je rentre.shortcut").unwrap(),
            b"c"
        );
        // Préparer les siens ne touche pas à ceux des autres.
        let adam = downloads.replace("adam", vec![("Je pars.shortcut".into(), b"d".to_vec())]);
        assert!(downloads.get(&second[1], "Je rentre.shortcut").is_some());
        downloads.clear("lea");
        assert!(downloads.get(&second[1], "Je rentre.shortcut").is_none());
        assert!(downloads.get(&adam[0], "Je pars.shortcut").is_some());
        downloads.lock().get_mut(&adam[0]).unwrap().expires = Instant::now();
        assert!(downloads.get(&adam[0], "Je pars.shortcut").is_none());
        assert!(downloads.lock().is_empty());
    }

    #[test]
    fn old_signing_leftovers_are_removed_at_startup_and_recent_ones_kept() {
        let base = std::env::temp_dir().join(format!("mhd-restes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ostal-raccourci-vieux")).unwrap();
        std::fs::write(base.join("ostal-raccourci-vieux/entree.shortcut"), "x").unwrap();
        std::fs::create_dir_all(base.join("autre-chose")).unwrap();
        // Tout est « récent » : rien n'est touché.
        remove_leftovers_in(&base, Duration::from_secs(3600));
        assert!(base.join("ostal-raccourci-vieux").exists());
        // Assez vieux : supprimé, le reste non.
        remove_leftovers_in(&base, Duration::ZERO);
        assert!(!base.join("ostal-raccourci-vieux").exists());
        assert!(base.join("autre-chose").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Un faux `shortcuts` : vérifie ses arguments, note le chemin de son entrée, puis `body`.
    fn fake_tool(tag: &str, body: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("mhd-faux-{tag}-{}", std::process::id()));
        let record = base.with_extension("txt");
        let script = base.with_extension("sh");
        // Arguments : sign --mode anyone --input <entrée> --output <sortie>.
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1 $2 $3 $4 $6\" = \"sign --mode anyone --input --output\" ] || exit 9\necho \"$5\" > '{}'\n{body}\n",
                record.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        (script, record)
    }

    /// Le dossier privé de l'entrée a disparu.
    fn input_dir_is_gone(record: &Path) {
        let input = PathBuf::from(std::fs::read_to_string(record).unwrap().trim());
        let dir = input.parent().unwrap();
        assert!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ostal-raccourci-"));
        assert!(!dir.exists(), "dossier temporaire resté");
        let _ = std::fs::remove_file(record);
    }

    #[test]
    fn a_failing_or_slow_signer_gives_a_clear_error_and_leaves_no_file() {
        // Outil absent.
        let e = sign_with(
            Path::new("/nonexistent/shortcuts"),
            "<plist/>",
            SIGN_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unavailable);
        assert!(e.detail.as_deref().unwrap().contains("Rien n'a changé"));
        // Outil qui échoue, ou qui ne rend pas un fichier signé.
        for (tag, body) in [
            ("echec", "exit 1"),
            ("vide", "exit 0"),
            ("texte", "cp \"$5\" \"$7\""),
        ] {
            let (script, record) = fake_tool(tag, body);
            assert!(
                sign_with(&script, "<plist/>", SIGN_TIMEOUT).is_err(),
                "{tag}"
            );
            input_dir_is_gone(&record);
            let _ = std::fs::remove_file(&script);
        }
        // Outil trop lent : arrêté au délai.
        let (script, record) = fake_tool("lent", "exec /bin/sleep 30");
        let started = Instant::now();
        let e = sign_with(&script, "<plist/>", Duration::from_secs(2)).unwrap_err();
        assert!(e.detail.as_deref().unwrap().contains("délai"));
        assert!(started.elapsed() < Duration::from_secs(10));
        input_dir_is_gone(&record);
        let _ = std::fs::remove_file(&script);
        // Un outil qui signe : le fichier rendu, et plus rien sur le disque.
        let (script, record) = fake_tool("ok", "{ printf AEA1; cat \"$5\"; } > \"$7\"");
        assert_eq!(
            sign_with(&script, "<plist/>", SIGN_TIMEOUT).unwrap(),
            b"AEA1<plist/>"
        );
        input_dir_is_gone(&record);
        let _ = std::fs::remove_file(&script);
        // Sans signature (essais) : le plist tel quel.
        assert_eq!(Signer::Unsigned.sign("<plist/>").unwrap(), b"<plist/>");
    }

    /// La vraie signature, à lancer à la main sur un Mac connecté à iCloud (valeurs factices) :
    /// `cargo test -p ostal-server --lib -- --ignored real_apple_signature`.
    #[test]
    #[ignore = "passe par /usr/bin/shortcuts et les serveurs d'Apple"]
    fn real_apple_signature() {
        let mut fake = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut fake);
        let xml = shortcut_plist(
            HomeShortcut::Arriving,
            "https://exemple.invalid",
            &hex::encode(fake),
        );
        let signed = Signer::Apple.sign(&xml).unwrap();
        assert!(signed.starts_with(b"AEA1") && signed.len() > xml.len() / 4);
    }
}
