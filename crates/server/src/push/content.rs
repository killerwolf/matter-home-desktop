//! Le texte des notifications : français, court, et **jamais** d'adresse de caméra, de mot de
//! passe, d'identifiant d'appareil brut ni d'adresse `rtsp://` (le texte d'une notification
//! s'affiche sur l'écran verrouillé de l'iPhone).
//!
//! Le nettoyage :
//! 1. les caractères de contrôle deviennent des espaces ;
//! 2. chaque secret connu (hôte, identifiant et mot de passe des caméras) est masqué tel quel ;
//! 3. toute adresse avec un schéma (`rtsp://…`, `https://…`) ou une adresse IPv4 devient
//!    « <adresse> » ; un mot qui ressemble à un identifiant de nœud (`0x…`, 16 chiffres
//!    hexadécimaux) devient « <appareil> » ;
//! 4. les chemins locaux sont masqués (`Scrubber`, comme pour les clients) ;
//! 5. titre et corps sont coupés (80 et 240 caractères).

use protocol::wire::{DetectionEventView, PushPayload};

pub const TITLE_MAX: usize = 80;
pub const BODY_MAX: usize = 240;

/// Masque les secrets et ce qui ressemble à une adresse ou un identifiant.
#[derive(Debug, Clone, Default)]
pub struct Sanitizer {
    /// Masqués tels quels, du plus long au plus court.
    secrets: Vec<String>,
}

impl Sanitizer {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Sanitizer {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .map(|s| s.trim().to_string())
            // Trop court, un « secret » masquerait des mots ordinaires (« 1 », « ab »).
            .filter(|s| s.chars().count() >= 3)
            .collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Sanitizer { secrets }
    }

    pub fn clean(&self, text: &str) -> String {
        let mut out: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        for secret in &self.secrets {
            out = replace_ci(&out, secret);
        }
        // Des mots séparés par toute espace (y compris insécable) ou par `=` (`motdepasse=…`).
        let mut masked = String::with_capacity(out.len());
        let mut word = String::new();
        for c in out.chars() {
            if c.is_whitespace() || c == '=' {
                masked.push_str(&mask_word(&word));
                word.clear();
                masked.push(if c == '=' { '=' } else { ' ' });
            } else {
                word.push(c);
            }
        }
        masked.push_str(&mask_word(&word));
        masked.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Remplace `needle` par « ••• » sans tenir compte de la casse (`Admin`, `ADMIN`…).
fn replace_ci(text: &str, needle: &str) -> String {
    let needle: Vec<char> = needle.chars().collect();
    let chars: Vec<char> = text.chars().collect();
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if i + needle.len() <= chars.len()
            && chars[i..i + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| same(*a, *b))
        {
            out.push_str("•••");
            i += needle.len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn mask_word(word: &str) -> String {
    let is_edge = |c: char| "()[]{}<>\"'«»`,;.!?:".contains(c);
    let core = word.trim_matches(is_edge);
    if core.is_empty() {
        return word.to_string();
    }
    let start = word.find(core).unwrap_or(0);
    let (before, after) = (&word[..start], &word[start + core.len()..]);
    let masked = if looks_like_url(core) || looks_like_ipv4(core) {
        "<adresse>"
    } else if looks_like_node_id(core) {
        "<appareil>"
    } else {
        return word.to_string();
    };
    format!("{before}{masked}{after}")
}

/// `schéma://…` (rtsp, http, https…), ou `user:mot@hôte`.
fn looks_like_url(word: &str) -> bool {
    if let Some((scheme, rest)) = word.split_once("://") {
        return !scheme.is_empty()
            && !rest.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    }
    // Identifiants dans une adresse sans schéma : `admin:secret@192.168.1.2`.
    word.contains('@') && word.contains(':')
}

/// `192.168.1.20` ou `192.168.1.20:554` (et le début d'un chemin : `10.0.0.2/live`).
fn looks_like_ipv4(word: &str) -> bool {
    let host = word.split(['/', ':']).next().unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `0x1A2B…` ou 16 chiffres hexadécimaux (l'écriture `{:016X}` d'un nœud sans nom).
fn looks_like_node_id(word: &str) -> bool {
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(rest) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return hex(rest);
    }
    word.len() >= 12 && hex(word) && word.bytes().any(|b| b.is_ascii_digit())
}

/// Coupe proprement (« … » si coupé).
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// La page de l'app web à ouvrir.
pub const HOME_URL: &str = "/app/";

/// La page d'un événement de détection (l'app web ouvre sa fiche).
pub fn event_url(id: &str) -> String {
    // L'identifiant est vérifié (`wire::is_event_id`) avant d'arriver ici ; on n'y laisse de toute
    // façon que des caractères sûrs dans une URL.
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("/app/#evenement={safe}")
}

/// Une notification prête à chiffrer : textes nettoyés et coupés.
pub fn payload(
    clean: &dyn Fn(&str) -> String,
    title: &str,
    body: &str,
    url: String,
    tag: Option<String>,
) -> PushPayload {
    let title = truncate(&clean(title), TITLE_MAX);
    PushPayload {
        title: if title.is_empty() {
            "Ostal".into()
        } else {
            title
        },
        body: truncate(&clean(body), BODY_MAX),
        url,
        tag,
    }
}

/// Un passage détecté : « Personne · Entrée », et la description si elle est venue.
pub fn detection(clean: &dyn Fn(&str) -> String, event: &DetectionEventView) -> PushPayload {
    let classes: Vec<String> = event
        .classes
        .iter()
        .map(|c| c.label().to_string())
        .collect();
    let what = if classes.is_empty() {
        "Mouvement".to_string()
    } else {
        classes.join(", ")
    };
    let title = format!("{what} · {}", event.camera_name);
    let at = chrono::DateTime::from_timestamp(event.start as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    let body = match (&event.description, event.niveau) {
        (Some(d), Some(n)) if n >= 1 => {
            format!("{d} (menace : {})", protocol::wire::niveau_label(n))
        }
        (Some(d), _) => d.clone(),
        (None, _) if at.is_empty() => "Passage détecté.".to_string(),
        (None, _) => format!("Passage détecté à {at}."),
    };
    payload(
        clean,
        &title,
        &body,
        event_url(&event.id),
        Some(format!(
            "evenement-{}",
            event_url(&event.id).trim_start_matches("/app/#evenement=")
        )),
    )
}

/// Pile faible d'un appareil (nom déjà connu ; « un appareil » sinon).
pub fn battery(
    clean: &dyn Fn(&str) -> String,
    name: Option<&str>,
    percent: Option<f64>,
) -> PushPayload {
    let name = display_name(name);
    let body = match percent {
        Some(p) => format!("{name} : pile à {p:.0} %. Pensez à la remplacer."),
        None => format!("{name} : pile faible. Pensez à la remplacer."),
    };
    payload(clean, "Pile faible", &body, HOME_URL.into(), None)
}

/// Un appareil injoignable depuis `minutes`.
pub fn unreachable(
    clean: &dyn Fn(&str) -> String,
    name: Option<&str>,
    minutes: u64,
) -> PushPayload {
    let name = display_name(name);
    payload(
        clean,
        "Appareil injoignable",
        &format!("{name} ne répond plus depuis {minutes} minutes."),
        HOME_URL.into(),
        None,
    )
}

/// La notification d'essai.
pub fn test_message() -> PushPayload {
    PushPayload {
        title: "Notifications activées".into(),
        body: "Cet appareil recevra les alertes de la maison.".into(),
        url: HOME_URL.into(),
        tag: Some("essai".into()),
    }
}

fn display_name(name: Option<&str>) -> String {
    match name.map(str::trim) {
        Some(n) if !n.is_empty() && !looks_like_node_id(n) => n.to_string(),
        _ => "Un appareil".to_string(),
    }
}
