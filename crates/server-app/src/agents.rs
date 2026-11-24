//! Réglages › Agents IA : l'interrupteur général, la liste de tous les agents, « Tout couper » et
//! les listes « jamais par un agent » et « sensible ». Tout ce qui se décide sans interface, pour
//! que ce soit testable ; le dessin est dans `agents_ui.rs`.
//!
//! Tout vit dans `serveur/agents.json` (voir `ostal_server::security::agent_tokens`). La fenêtre
//! tourne **dans le même processus** que le serveur : elle écrit le fichier par les mêmes fonctions
//! que la ligne de commande et que l'app web, et le serveur le relit dès qu'il change (interrupteur
//! et révocations prennent effet à la requête suivante). Chaque geste écrit une ligne au journal
//! d'audit, au nom de la fenêtre (« Ostal Server »).
//!
//! **Ce qui ne se fait que d'ici** (ou en ligne de commande, c'est-à-dire devant le Mac du serveur) :
//! l'interrupteur général, « Tout couper » et les deux listes. L'app web montre l'interrupteur,
//! mais ne le change pas.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ostal_server::audit::{Action, AuditLog, Entry, Who};
use ostal_server::confirmations::Confirmations;
use ostal_server::hub::Hub;
use ostal_server::persistence::SERVER_DIR;
use ostal_server::security::agent_tokens::{self, AgentInfo, AgentStore, Scope};

/// Le dossier `serveur/` d'un dossier de données.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SERVER_DIR)
}

/// Ce que dit la page sur l'agent qui tourne sur ce Mac : la limite que Ostal ne peut pas lever.
pub const SAME_MAC_WARNING: &str = "Prévoyez l'agent sur un autre ordinateur. Un agent qui a un terminal sur ce Mac peut lancer « ostal-server », lire le dossier des données ou se créer un accès : Ostal ne peut pas l'en empêcher. Sinon, refusez-lui ces commandes et ce dossier dans ses propres règles.";

/// L'avertissement sur ce qui part chez le fournisseur de l'agent.
pub const PROVIDER_WARNING: &str = "Un agent d'IA envoie ce qu'il lit (appareils, pièces, historique, présence si vous l'avez permise) à son fournisseur, par exemple Anthropic pour Claude Code. N'autorisez que ce que vous acceptez de lui confier.";

/// L'interrupteur général : la phrase qui dit ce qu'il fait.
pub fn switch_text(enabled: bool) -> &'static str {
    if enabled {
        "Allumé : un agent qui a un jeton valide peut se connecter à ce serveur (local ou Tailscale, jamais en accès public)."
    } else {
        "Éteint : aucun agent ne peut se connecter, le serveur répond comme si cette fonction n'existait pas."
    }
}

// ---------------------------------------------------------------------------------------------
// Les propositions par défaut (décisions du 24/11/2026, point 7)
// ---------------------------------------------------------------------------------------------

/// Les noms sans majuscules ni accents, pour les comparer.
pub fn plain(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect()
}

/// Les mots d'un nom, sans accents ni majuscules.
fn words(name: &str) -> Vec<String> {
    plain(name).split_whitespace().map(str::to_string).collect()
}

/// « Jamais par un agent » : un congélateur, la box, la prise du Mac serveur.
pub fn suggests_never(name: &str) -> bool {
    let w = words(name);
    w.iter().any(|x| {
        x.starts_with("congel")
            || matches!(
                x.as_str(),
                "freezer" | "box" | "livebox" | "bbox" | "freebox" | "mac" | "serveur" | "ostal"
            )
    })
}

/// « Sensible » : la porte de garage, la baie vitrée.
pub fn suggests_sensitive(name: &str) -> bool {
    let w = words(name);
    w.iter().any(|x| matches!(x.as_str(), "garage" | "baie"))
}

/// Les listes proposées pour ces appareils (identifiant, nom), d'après leurs **noms** : des
/// propositions à cocher, jamais écrites toutes seules. « Jamais » l'emporte sur « sensible ».
/// Sans appareil du bon nom, les listes restent vides.
pub fn suggestion(devices: &[(u64, String)]) -> (Vec<u64>, Vec<u64>) {
    let never: Vec<u64> = devices
        .iter()
        .filter(|(_, n)| suggests_never(n))
        .map(|(id, _)| *id)
        .collect();
    let sensitive = devices
        .iter()
        .filter(|(id, n)| suggests_sensitive(n) && !never.contains(id))
        .map(|(id, _)| *id)
        .collect();
    (never, sensitive)
}

/// Les appareils qu'un agent pourrait commander (prises, lampes, volets) : (identifiant, nom),
/// par nom.
pub fn controllable(hub: &Hub) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = hub
        .current_snapshot()
        .devices
        .iter()
        .filter(|v| v.device.caps.on_off || v.device.caps.cover)
        .map(|v| (v.device.node_id, v.device.name.clone()))
        .collect();
    out.sort_by(|a, b| plain(&a.1).cmp(&plain(&b.1)).then(a.0.cmp(&b.0)));
    out
}

// ---------------------------------------------------------------------------------------------
// La liste des agents
// ---------------------------------------------------------------------------------------------

/// Une ligne de la liste : tout est déjà en phrases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub label: String,
    /// La personne à qui l'agent appartient.
    pub owner: String,
    /// « Lecture, Commandes ».
    pub rights: String,
    /// Les appareils permis, par nom (« 2 appareils : Frigo, Volet du salon ») ; vide sans droit de
    /// commander.
    pub devices: String,
    pub last_use: String,
    pub expires: String,
    pub expired: bool,
}

/// Les droits d'un agent en quelques mots.
pub fn rights_text(scopes: &[Scope]) -> String {
    let word = |s: &Scope| match s {
        Scope::Read => Some("Lecture"),
        Scope::Occupation => Some("Présence et mode"),
        Scope::Control => Some("Commandes"),
        Scope::Mode => Some("Changer le mode"),
        Scope::Unknown => None,
    };
    scopes
        .iter()
        .filter_map(word)
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

/// « Expire dans 29 jours », « Expire aujourd'hui ou demain », « Expiré ».
pub fn expiry_text(expires: f64, now: f64) -> String {
    let left = expires - now;
    if left <= 0.0 {
        return "Expiré".into();
    }
    let days = (left / 86_400.0).ceil() as u64;
    if days <= 1 {
        "Expire aujourd'hui ou demain".into()
    } else {
        format!("Expire dans {days} jours")
    }
}

/// Le dernier appel et les appels du jour, **depuis le démarrage du serveur** : on le dit, pour ne
/// pas faire croire qu'un agent muet n'a jamais servi.
pub fn usage_text(usage: Option<agent_tokens::Usage>, now: f64) -> String {
    let Some(u) = usage else {
        return "Pas encore utilisé depuis le démarrage".into();
    };
    let ago = (now - u.last).max(0.0) as u64;
    let when = match ago {
        0..=89 => "à l'instant".to_string(),
        90..=3599 => format!("il y a {} min", (ago + 30) / 60),
        3600..=86_399 => format!("il y a {} h", (ago + 1800) / 3600),
        _ => format!("il y a {} j", (ago + 43_200) / 86_400),
    };
    format!(
        "Dernier appel {when}, {} aujourd'hui",
        plural(u.today as usize, "appel", "appels")
    )
}

/// Les lignes de tous les agents, le plus récent d'abord. `owners` : (identifiant de compte, nom
/// affiché) ; `names` : les appareils (identifiant, nom).
pub fn rows(
    store: &AgentStore,
    owners: &[(String, String)],
    names: &[(u64, String)],
    now: f64,
) -> Vec<Row> {
    store
        .all_agents()
        .iter()
        .map(|a| row(a, store, owners, names, now))
        .collect()
}

fn row(
    agent: &AgentInfo,
    store: &AgentStore,
    owners: &[(String, String)],
    names: &[(u64, String)],
    now: f64,
) -> Row {
    let owner = owners
        .iter()
        .find(|(id, _)| *id == agent.user_id)
        .map_or_else(|| "Compte supprimé".to_string(), |(_, n)| n.clone());
    let devices = if agent.has(Scope::Control) {
        let list: Vec<String> = agent
            .devices
            .iter()
            .map(|text| {
                text.parse::<u64>()
                    .ok()
                    .and_then(|id| names.iter().find(|(i, _)| *i == id))
                    .map_or_else(|| "appareil retiré".to_string(), |(_, n)| n.clone())
            })
            .collect();
        if list.is_empty() {
            "Aucun appareil permis".to_string()
        } else {
            format!(
                "{} : {}",
                plural(list.len(), "appareil", "appareils"),
                list.join(", ")
            )
        }
    } else {
        String::new()
    };
    Row {
        id: agent.id.clone(),
        label: agent.label.clone(),
        owner,
        rights: rights_text(&agent.scopes),
        devices,
        last_use: usage_text(store.usage(&agent.id), now),
        expires: expiry_text(agent.expires, now),
        expired: agent.is_expired(),
    }
}

// ---------------------------------------------------------------------------------------------
// Les gestes (chacun écrit une ligne au journal, au nom de la fenêtre)
// ---------------------------------------------------------------------------------------------

fn record(data_dir: &Path, entry: Entry) {
    AuditLog::open(&dir(data_dir)).record(entry);
}

fn failed(what: &str, e: &anyhow::Error) -> String {
    log::warn!("agents d'IA : {what} : {e:#}");
    format!("{what} : le fichier des agents n'a pas pu être écrit.")
}

/// Allume ou éteint l'interrupteur général. Rend `true` si l'état a changé.
pub fn set_switch(data_dir: &Path, enabled: bool) -> Result<bool, String> {
    let changed = agent_tokens::set_enabled(&dir(data_dir), enabled)
        .map_err(|e| failed("Interrupteur", &e))?;
    if changed {
        record(
            data_dir,
            Entry::new(Action::AgentSwitch, &Who::server_app()).detail(if enabled {
                "on"
            } else {
                "off"
            }),
        );
    }
    Ok(changed)
}

/// Révoque un agent (tous les agents sont montrés ici, quelle que soit la personne).
pub fn revoke(data_dir: &Path, agent_id: &str) -> Result<Option<AgentInfo>, String> {
    let gone =
        agent_tokens::revoke_id(&dir(data_dir), agent_id).map_err(|e| failed("Révocation", &e))?;
    if let Some(a) = &gone {
        record(
            data_dir,
            Entry::new(Action::AgentRevoked, &Who::server_app()).target(&a.label),
        );
    }
    Ok(gone)
}

/// « Tout couper » : l'interrupteur s'éteint, **tous** les agents sont révoqués (une seule
/// écriture) et les demandes en attente ou autorisées sans avoir servi sont oubliées. Les listes
/// restent. Rend le nombre d'agents révoqués.
pub fn cut_all(data_dir: &Path, confirmations: Option<&Confirmations>) -> Result<usize, String> {
    let was_on = agent_tokens::list(&dir(data_dir))
        .map(|(on, _)| on)
        .unwrap_or(false);
    let gone = agent_tokens::cut_all(&dir(data_dir)).map_err(|e| failed("Coupure", &e))?;
    if let Some(c) = confirmations {
        c.clear();
    }
    let who = Who::server_app();
    if was_on {
        record(
            data_dir,
            Entry::new(Action::AgentSwitch, &who).detail("off"),
        );
    }
    for agent in &gone {
        record(
            data_dir,
            Entry::new(Action::AgentRevoked, &who).target(&agent.label),
        );
    }
    Ok(gone.len())
}

/// Écrit les deux listes. « Jamais » l'emporte sur « sensible » (un appareil des deux n'est que
/// « jamais »). Rend `true` si le fichier a changé.
pub fn save_lists(data_dir: &Path, never: &[u64], sensitive: &[u64]) -> Result<bool, String> {
    let changed = agent_tokens::set_lists(&dir(data_dir), never, sensitive)
        .map_err(|e| failed("Listes", &e))?;
    if changed {
        let unique = |ids: &[u64]| ids.iter().collect::<HashSet<_>>().len();
        let n = unique(never);
        let m = sensitive
            .iter()
            .filter(|id| !never.contains(id))
            .collect::<HashSet<_>>()
            .len();
        record(
            data_dir,
            Entry::new(Action::AgentLists, &Who::server_app()).detail(format!("{n}:{m}")),
        );
    }
    Ok(changed)
}

/// La phrase d'un geste réussi.
pub fn cut_notice(count: usize) -> String {
    match count {
        0 => "Accès des agents coupé. Il n'y avait aucun agent à révoquer.".into(),
        1 => "Accès des agents coupé : 1 agent révoqué.".into(),
        n => format!("Accès des agents coupé : {n} agents révoqués."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ostal-agents-ui-{tag}-{}-{}",
            std::process::id(),
            ostal_server::events::now_unix() as u64
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(SERVER_DIR)).unwrap();
        dir
    }

    fn log_lines(data_dir: &Path) -> Vec<Entry> {
        AuditLog::reader(&dir(data_dir)).read(&ostal_server::audit::Filter::default())
    }

    #[test]
    fn names_are_compared_without_accents_or_capitals() {
        assert_eq!(plain("Congélateur ÉTAGE"), "congelateur etage");
        assert!(suggests_never("Congélateur du garage"));
        assert!(suggests_never("Prise du Mac serveur"));
        assert!(suggests_never("Box Internet"));
        assert!(suggests_never("FREEZER"));
        assert!(!suggests_never("Boxe du salon"));
        assert!(!suggests_never("Macaron"));
        assert!(!suggests_never("Lampe du salon"));
        assert!(suggests_sensitive("Porte de garage"));
        assert!(suggests_sensitive("Baie vitrée"));
        assert!(!suggests_sensitive("Lampe du salon"));
    }

    #[test]
    fn the_suggestion_comes_from_names_and_never_wins() {
        let devices = vec![
            (1, "Lampe du salon".to_string()),
            (2, "Congélateur du garage".to_string()),
            (3, "Porte de garage".to_string()),
            (4, "Baie vitrée".to_string()),
            (5, "Prise Box".to_string()),
        ];
        let (never, sensitive) = suggestion(&devices);
        assert_eq!(never, vec![2, 5]);
        // « Congélateur du garage » est « jamais », pas « sensible ».
        assert_eq!(sensitive, vec![3, 4]);
        // Aucun appareil du bon nom : des listes vides à remplir.
        assert_eq!(
            suggestion(&[(1, "Lampe".into()), (2, "Volet".into())]),
            (vec![], vec![])
        );
        assert_eq!(suggestion(&[]), (vec![], vec![]));
    }

    #[test]
    fn the_switch_writes_the_file_and_one_journal_line_per_change() {
        let data = temp("switch");
        assert_eq!(set_switch(&data, true), Ok(true));
        assert_eq!(set_switch(&data, true), Ok(false));
        assert!(AgentStore::open(&dir(&data)).enabled());
        assert_eq!(set_switch(&data, false), Ok(true));
        let lines = log_lines(&data);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| l.action == Action::AgentSwitch));
        assert!(lines
            .iter()
            .all(|l| l.connection.as_ref().is_some_and(|c| c.kind == "server")));
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn cut_all_turns_off_revokes_everything_forgets_requests_and_keeps_the_lists() {
        let data = temp("cut");
        let d = dir(&data);
        agent_tokens::add(&d, "u1", "Claude Code", 30).unwrap();
        agent_tokens::add(&d, "u2", "Cursor", 30).unwrap();
        agent_tokens::set_enabled(&d, true).unwrap();
        agent_tokens::set_lists(&d, &[4], &[9]).unwrap();
        let confirmations = Confirmations::default();
        assert_eq!(cut_all(&data, Some(&confirmations)), Ok(2));
        let store = AgentStore::open(&d);
        assert!(!store.enabled());
        assert!(store.all_agents().is_empty());
        assert_eq!(store.lists(), (vec![4], vec![9]));
        let lines = log_lines(&data);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.action == Action::AgentRevoked)
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.action == Action::AgentSwitch)
                .count(),
            1
        );
        // Une seconde fois : rien à révoquer, rien d'écrit de plus.
        assert_eq!(cut_all(&data, Some(&confirmations)), Ok(0));
        assert_eq!(log_lines(&data).len(), lines.len());
        assert_eq!(
            cut_notice(0),
            "Accès des agents coupé. Il n'y avait aucun agent à révoquer."
        );
        assert_eq!(cut_notice(1), "Accès des agents coupé : 1 agent révoqué.");
        assert_eq!(cut_notice(3), "Accès des agents coupé : 3 agents révoqués.");
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn revoking_one_agent_leaves_the_others_and_is_journaled_with_the_label() {
        let data = temp("revoke");
        let d = dir(&data);
        let (a, _) = agent_tokens::add(&d, "u1", "Claude Code", 30).unwrap();
        agent_tokens::add(&d, "u1", "Cursor", 30).unwrap();
        assert_eq!(revoke(&data, &a.id).unwrap().unwrap().label, "Claude Code");
        assert!(revoke(&data, &a.id).unwrap().is_none());
        let left = AgentStore::open(&d).all_agents();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].label, "Cursor");
        let lines = log_lines(&data);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].target.as_deref(), Some("Claude Code"));
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn the_lists_are_saved_canonical_and_journaled_as_counts_never_names() {
        let data = temp("lists");
        assert_eq!(save_lists(&data, &[3, 1, 1], &[1, 2]), Ok(true));
        assert_eq!(save_lists(&data, &[1, 3], &[2]), Ok(false));
        let store = AgentStore::open(&dir(&data));
        assert_eq!(store.lists(), (vec![1, 3], vec![2]));
        let lines = log_lines(&data);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].action, Action::AgentLists);
        assert_eq!(lines[0].detail.as_deref(), Some("2:1"));
        assert!(save_lists(&data, &(0..300).collect::<Vec<u64>>(), &[]).is_err());
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn rows_say_who_what_and_until_when_without_any_token() {
        let data = temp("rows");
        let d = dir(&data);
        let (agent, token) = agent_tokens::add_with(
            &d,
            "u1",
            "Claude Code",
            30,
            &[Scope::Control],
            &[10, 11, 12],
        )
        .unwrap();
        agent_tokens::add(&d, "gone", "Orphelin", 1).unwrap();
        let store = AgentStore::open(&d);
        store.check_read(&agent.id).unwrap();
        let owners = vec![("u1".to_string(), "Hamdi".to_string())];
        let names = vec![
            (10, "Frigo".to_string()),
            (11, "Volet du salon".to_string()),
        ];
        let now = ostal_server::events::now_unix();
        let all = rows(&store, &owners, &names, now);
        assert_eq!(all.len(), 2);
        let mine = all.iter().find(|r| r.label == "Claude Code").unwrap();
        assert_eq!(mine.owner, "Hamdi");
        assert_eq!(mine.rights, "Lecture, Commandes");
        assert_eq!(
            mine.devices,
            "3 appareils : Frigo, Volet du salon, appareil retiré"
        );
        assert_eq!(mine.expires, "Expire dans 30 jours");
        assert!(
            mine.last_use.contains("à l'instant") && mine.last_use.contains("1 appel aujourd'hui")
        );
        let orphan = all.iter().find(|r| r.label == "Orphelin").unwrap();
        assert_eq!(orphan.owner, "Compte supprimé");
        assert_eq!(orphan.devices, "");
        assert_eq!(orphan.last_use, "Pas encore utilisé depuis le démarrage");
        let text = format!("{all:?}");
        assert!(!text.contains(&token));
        assert_eq!(expiry_text(10.0, 10.0), "Expiré");
        assert_eq!(expiry_text(100.0, 10.0), "Expire aujourd'hui ou demain");
        assert_eq!(
            rights_text(&[Scope::Read, Scope::Unknown, Scope::Mode]),
            "Lecture, Changer le mode"
        );
        let _ = std::fs::remove_dir_all(data);
    }
}
