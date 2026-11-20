//! Ce que la page « Widgets et raccourcis » de l'app web installe sur l'iPhone, **déjà rempli** :
//!
//! - les deux raccourcis de mode (« Mode absent », « Mode present »), avec l'adresse publique du
//!   serveur et le jeton de mode écrits dedans, puis signés par `/usr/bin/shortcuts sign` ;
//! - le script du widget Scriptable, avec l'adresse et le jeton de lecture écrits dedans.
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
pub const ABSENT_NAME: &str = "Mode absent";
pub const PRESENT_NAME: &str = "Mode present";

/// Le script du widget, avec deux emplacements à remplir.
const WIDGET_TEMPLATE: &str = include_str!("../web/raccourcis/matter-home.js");
const SERVER_SLOT: &str = "const SERVEUR = \"__SERVEUR__\";";
const TOKEN_SLOT: &str = "const TOKEN = \"__JETON_DE_LECTURE__\";";

/// Délai d'une signature : elle passe par les serveurs d'Apple (compte iCloud du Mac).
const SIGN_TIMEOUT: Duration = Duration::from_secs(25);
const SHORTCUTS_BIN: &str = "/usr/bin/shortcuts";

/// Le raccourci d'un mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeShortcut {
    Absent,
    Present,
}

impl ModeShortcut {
    pub const ALL: [ModeShortcut; 2] = [ModeShortcut::Absent, ModeShortcut::Present];

    /// Nom du raccourci (et du fichier, sans `.shortcut`), en ASCII.
    pub fn name(self) -> &'static str {
        match self {
            ModeShortcut::Absent => ABSENT_NAME,
            ModeShortcut::Present => PRESENT_NAME,
        }
    }

    /// Valeur envoyée à `PUT /api/mode`.
    fn mode(self) -> &'static str {
        match self {
            ModeShortcut::Absent => "absent",
            ModeShortcut::Present => "present",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ModeShortcut::Absent => "Absent",
            ModeShortcut::Present => "Présent",
        }
    }

    /// Couleur et pictogramme de l'icône (ceux des raccourcis d'avant).
    fn icon(self) -> (i64, i64) {
        match self {
            ModeShortcut::Absent => (4292093695, 59446),
            ModeShortcut::Present => (431817727, 59511),
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
            ("WFNotificationActionTitle", s("Matter Home")),
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

/// Le raccourci d'un mode, en plist XML (non signé). `address` vient de `public_address`, `token`
/// est le jeton de mode : tous deux écrits en dur, aucune question à l'import.
///
/// `PUT {address}/api/mode` avec `Authorization: Bearer <jeton>` et `{"mode": …}` ; notification
/// « Maison en mode … » seulement si la réponse a une clé `mode`, sinon la réponse du serveur.
pub fn mode_shortcut_plist(which: ModeShortcut, address: &str, token: &str) -> String {
    let (request, value, group) = (uuid(), uuid(), uuid());
    let reply = || output(&request, "Contenu de l'URL");
    let actions = vec![
        action(
            "is.workflow.actions.downloadurl",
            vec![
                ("UUID", s(&request)),
                ("WFURL", tok(&format!("{address}/api/mode"), vec![])),
                ("WFHTTPMethod", s("PUT")),
                ("ShowHeaders", Plist::Bool(true)),
                (
                    "WFHTTPHeaders",
                    dict_field(&[("Authorization", &format!("Bearer {token}"))]),
                ),
                ("WFHTTPBodyType", s("JSON")),
                ("WFJSONValues", dict_field(&[("mode", which.mode())])),
            ],
        ),
        action(
            "is.workflow.actions.getvalueforkey",
            vec![
                ("UUID", s(&value)),
                ("WFInput", attachment(reply())),
                ("WFDictionaryKey", s("mode")),
                ("WFGetDictionaryValueType", s("Value")),
            ],
        ),
        conditional(&group, 0, Some(output(&value, "Valeur du dictionnaire"))),
        notify(s(&format!("Maison en mode {}.", which.label()))),
        conditional(&group, 1, None),
        notify(tok(
            &format!("Le mode n'a pas changé. Réponse du serveur : {OBJ}"),
            vec![reply()],
        )),
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
        let path =
            std::env::temp_dir().join(format!("matter-home-raccourci-{}", hex::encode(random)));
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
        match self {
            Signer::Unsigned => Ok(plist.as_bytes().to_vec()),
            Signer::Apple => sign_with(Path::new(SHORTCUTS_BIN), plist, SIGN_TIMEOUT),
        }
    }
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

/// Les raccourcis préparés, en mémoire seulement. Le `Hub` ne les remplace ou ne les vide que
/// sous le changement du jeton de mode : un lien ne porte jamais un jeton révoqué.
#[derive(Default)]
pub struct Downloads {
    files: std::sync::Mutex<std::collections::HashMap<String, Prepared>>,
}

struct Prepared {
    file_name: String,
    body: Vec<u8>,
    expires: Instant,
}

impl Downloads {
    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Prepared>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remplace les liens d'avant par ceux-ci ; rend leurs identifiants (128 bits), dans l'ordre.
    pub fn replace(&self, files: Vec<(String, Vec<u8>)>) -> Vec<String> {
        let mut map = self.lock();
        map.clear();
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
                        file_name,
                        body,
                        expires,
                    },
                );
                id
            })
            .collect()
    }

    /// Oublie tous les liens.
    pub fn clear(&self) {
        self.lock().clear();
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
        if !name.to_string_lossy().starts_with("matter-home-raccourci-") {
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
    fn the_mode_shortcut_carries_address_token_and_no_import_question() {
        let xml = mode_shortcut_plist(
            ModeShortcut::Present,
            "https://mac.tailnet-12ab.ts.net",
            TOKEN,
        );
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<string>https://mac.tailnet-12ab.ts.net/api/mode</string>"));
        assert!(xml.contains(&format!("<string>Bearer {TOKEN}</string>")));
        assert!(xml.contains("<key>WFHTTPMethod</key><string>PUT</string>"));
        assert!(xml.contains("<key>WFHTTPBodyType</key><string>JSON</string>"));
        assert!(xml.contains("<string>present</string>"));
        assert!(xml.contains("<string>Maison en mode Présent.</string>"));
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
        let absent = mode_shortcut_plist(ModeShortcut::Absent, "https://m.ts.net", TOKEN);
        assert!(
            absent.contains("<string>absent</string>") && absent.contains("Maison en mode Absent.")
        );
    }

    #[test]
    fn the_plist_is_well_formed_for_plutil() {
        // `plutil -lint` (macOS) relit le fichier comme le ferait `shortcuts sign`.
        let plutil = Path::new("/usr/bin/plutil");
        if !plutil.exists() {
            return;
        }
        let dir = PrivateDir::create().unwrap();
        let file = dir.0.join("essai.shortcut");
        let xml = mode_shortcut_plist(ModeShortcut::Absent, "https://m.ts.net", TOKEN);
        write_private(&file, xml.as_bytes()).unwrap();
        let out = Command::new(plutil)
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn names_are_ascii_and_match_the_web_page_and_the_widget() {
        for which in ModeShortcut::ALL {
            assert!(which.name().is_ascii());
        }
        let page = include_str!("../web/widgets.js");
        assert!(
            page.contains(&format!("\"{ABSENT_NAME}\""))
                && page.contains(&format!("\"{PRESENT_NAME}\""))
        );
        assert!(WIDGET_TEMPLATE.contains(&format!("const RACCOURCI_ABSENT = \"{ABSENT_NAME}\";")));
        assert!(WIDGET_TEMPLATE.contains(&format!("const RACCOURCI_PRESENT = \"{PRESENT_NAME}\";")));
        assert!(!WIDGET_TEMPLATE.contains("Mode présent"));
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
        let first = downloads.replace(vec![("Mode absent.shortcut".into(), b"a".to_vec())]);
        assert_eq!(first[0].len(), 32);
        // Réutilisable pendant sa durée de vie, mais seulement sous son nom.
        assert_eq!(
            downloads.get(&first[0], "Mode absent.shortcut").unwrap(),
            b"a"
        );
        assert!(downloads.get(&first[0], "Mode absent.shortcut").is_some());
        assert!(downloads.get(&first[0], "Mode present.shortcut").is_none());
        let second = downloads.replace(vec![
            ("Mode absent.shortcut".into(), b"b".to_vec()),
            ("Mode present.shortcut".into(), b"c".to_vec()),
        ]);
        assert!(downloads.get(&first[0], "Mode absent.shortcut").is_none());
        assert_eq!(
            downloads.get(&second[1], "Mode present.shortcut").unwrap(),
            b"c"
        );
        downloads.lock().get_mut(&second[1]).unwrap().expires = Instant::now();
        assert!(downloads.get(&second[1], "Mode present.shortcut").is_none());
        downloads.clear();
        assert!(downloads.lock().is_empty());
    }

    #[test]
    fn old_signing_leftovers_are_removed_at_startup_and_recent_ones_kept() {
        let base = std::env::temp_dir().join(format!("mhd-restes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("matter-home-raccourci-vieux")).unwrap();
        std::fs::write(
            base.join("matter-home-raccourci-vieux/entree.shortcut"),
            "x",
        )
        .unwrap();
        std::fs::create_dir_all(base.join("autre-chose")).unwrap();
        // Tout est « récent » : rien n'est touché.
        remove_leftovers_in(&base, Duration::from_secs(3600));
        assert!(base.join("matter-home-raccourci-vieux").exists());
        // Assez vieux : supprimé, le reste non.
        remove_leftovers_in(&base, Duration::ZERO);
        assert!(!base.join("matter-home-raccourci-vieux").exists());
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
            .starts_with("matter-home-raccourci-"));
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
    /// `cargo test -p matter-home-server --lib -- --ignored real_apple_signature`.
    #[test]
    #[ignore = "passe par /usr/bin/shortcuts et les serveurs d'Apple"]
    fn real_apple_signature() {
        let mut fake = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut fake);
        let xml = mode_shortcut_plist(
            ModeShortcut::Present,
            "https://exemple.invalid",
            &hex::encode(fake),
        );
        let signed = Signer::Apple.sign(&xml).unwrap();
        assert!(signed.starts_with(b"AEA1") && signed.len() > xml.len() / 4);
    }
}
