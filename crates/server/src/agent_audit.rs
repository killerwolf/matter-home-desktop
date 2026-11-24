//! Le journal des lectures d'un agent d'IA (serveur MCP intégré, `docs/agents-ia.md`).
//!
//! Deux règles, celles des spécifications :
//!
//! - les lectures ordinaires sont **regroupées** : une ligne par agent et par tranche de 10
//!   minutes, écrite **à l'échéance** (cible : le libellé de l'agent, détail : `N:outil,outil`).
//!   `AuditLog::record_throttled` ne ferait pas l'affaire : elle garde la première ligne et jette
//!   les suivantes, sans rien compter ;
//! - les lectures du droit « occupation » (présence, mode) sont écrites **une par une**, jamais
//!   regroupées : voir [`occupation_entry`].
//!
//! [`ReadGroups`] ne fait que compter, en mémoire : c'est l'appelant qui écrit les lignes rendues
//! (un balayeur du réseau les rend à l'échéance même quand l'agent se tait, et à l'arrêt du
//! serveur). Jamais une valeur lue, jamais un argument, jamais un jeton : seulement le nom d'un
//! outil de la liste fermée du serveur.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use crate::audit::{Action, ConnRef, Entry, Origin, Who};
use crate::security::agent_tokens::AgentInfo;

/// Durée d'une tranche de regroupement, en secondes.
pub const GROUP_WINDOW_S: f64 = 600.0;
/// Préfixe commun des outils, retiré du détail (le journal est lu à l'œil, et le détail est coupé
/// à 120 caractères).
const TOOL_PREFIX: &str = "ostal_";
/// Agents suivis au plus à la fois (une tranche non écrite occupe une place).
const MAX_OPEN: usize = 256;

/// Qui agit : le compte propriétaire du jeton, et l'agent comme « connexion » de type `agent`
/// (identifiant du jeton, libellé). `display_name` : le nom affiché du compte ; `ip` et `via`
/// comme pour les autres routes (derrière `tailscale serve`, ils n'identifient rien).
pub fn who_of(
    agent: &AgentInfo,
    display_name: Option<String>,
    ip: Option<String>,
    via: Option<protocol::wire::Via>,
) -> Who {
    let mut who = Who::user(&agent.user_id, display_name);
    who.connection = Some(ConnRef::new(Some(&agent.id), &agent.label, "agent"));
    who.ip = ip;
    who.via = via;
    who
}

/// Le nom d'un outil tel qu'il s'écrit au journal : sans `ostal_`.
fn short(tool: &str) -> &str {
    tool.strip_prefix(TOOL_PREFIX).unwrap_or(tool)
}

/// La ligne d'une lecture du droit « occupation » : une par lecture.
pub fn occupation_entry(who: &Who, label: &str, tool: &str) -> Entry {
    Entry::new(Action::AgentOccupation, who)
        .target(label)
        .detail(short(tool))
        .source(Origin::Agent)
}

/// La ligne d'un appel refusé : `reason` est un mot fermé (`unknown`, `scope`, `rights`, `rate`).
/// Le nom d'un outil inconnu n'y entre jamais (c'est un texte de l'agent).
pub fn denied_entry(who: &Who, label: &str, reason: &str) -> Entry {
    Entry::new(Action::AgentDenied, who)
        .target(label)
        .detail(reason)
        .source(Origin::Agent)
        .result(crate::audit::Outcome::Refused)
}

struct Group {
    who: Who,
    label: String,
    /// Première lecture de la tranche (secondes Unix).
    since: f64,
    count: u32,
    tools: BTreeSet<String>,
}

impl Group {
    fn into_entry(self) -> Entry {
        let tools: Vec<&str> = self.tools.iter().map(String::as_str).collect();
        Entry::new(Action::AgentRead, &self.who)
            .at(self.since)
            .target(&self.label)
            .detail(format!("{}:{}", self.count.min(9999), tools.join(",")))
            .source(Origin::Agent)
    }
}

/// Les lectures en cours de regroupement, par agent (identifiant du jeton).
#[derive(Default)]
pub struct ReadGroups {
    open: Mutex<HashMap<String, Group>>,
}

impl ReadGroups {
    /// Note une lecture. Rend les lignes à écrire **maintenant** : la tranche de cet agent si elle
    /// est échue (la lecture ouvre alors la suivante), et toute autre tranche échue.
    pub fn note(&self, agent_id: &str, who: &Who, label: &str, tool: &str, now: f64) -> Vec<Entry> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let mut due = take_due(&mut open, now);
        if open.len() >= MAX_OPEN && !open.contains_key(agent_id) {
            // Trop de tranches ouvertes (jetons en rafale) : on ferme la plus ancienne plutôt que
            // de grossir sans fin.
            if let Some(oldest) = open
                .iter()
                .min_by(|a, b| a.1.since.total_cmp(&b.1.since))
                .map(|(k, _)| k.clone())
            {
                due.extend(open.remove(&oldest).map(Group::into_entry));
            }
        }
        let group = open.entry(agent_id.to_string()).or_insert_with(|| Group {
            who: who.clone(),
            label: label.to_string(),
            since: now,
            count: 0,
            tools: BTreeSet::new(),
        });
        group.count = group.count.saturating_add(1);
        group.tools.insert(short(tool).to_string());
        due
    }

    /// Les tranches échues à `now` (pour le balayeur).
    pub fn flush_due(&self, now: f64) -> Vec<Entry> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        take_due(&mut open, now)
    }

    /// Toutes les tranches, échues ou non (à l'arrêt du serveur).
    pub fn flush_all(&self) -> Vec<Entry> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.drain().map(|(_, g)| g.into_entry()).collect()
    }
}

fn take_due(open: &mut HashMap<String, Group>, now: f64) -> Vec<Entry> {
    let due: Vec<String> = open
        .iter()
        .filter(|(_, g)| now - g.since >= GROUP_WINDOW_S)
        .map(|(k, _)| k.clone())
        .collect();
    let mut entries: Vec<Entry> = due
        .into_iter()
        .filter_map(|k| open.remove(&k))
        .map(Group::into_entry)
        .collect();
    entries.sort_by(|a, b| a.at.total_cmp(&b.at));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(label: &str) -> Who {
        let mut w = Who::user("u1", Some("Hamdi".into()));
        w.connection = Some(ConnRef::new(Some("a1"), label, "agent"));
        w
    }

    #[test]
    fn reads_are_grouped_per_agent_and_written_when_the_window_ends() {
        let groups = ReadGroups::default();
        let w = who("Claude Code");
        assert!(groups
            .note("a1", &w, "Claude Code", "ostal_list_devices", 1000.0)
            .is_empty());
        assert!(groups
            .note("a1", &w, "Claude Code", "ostal_get_device", 1100.0)
            .is_empty());
        assert!(groups
            .note("a1", &w, "Claude Code", "ostal_list_devices", 1500.0)
            .is_empty());
        // Un autre agent a sa propre tranche.
        assert!(groups
            .note("a2", &who("Cursor"), "Cursor", "ostal_list_rooms", 1550.0)
            .is_empty());
        // Rien à l'échéance du premier : 1000 + 600.
        assert!(groups.flush_due(1599.0).is_empty());
        let due = groups.flush_due(1600.0);
        assert_eq!(due.len(), 1);
        let line = &due[0];
        assert_eq!(line.action, Action::AgentRead);
        assert_eq!(line.at, 1000.0, "la ligne porte le début de la tranche");
        assert_eq!(line.target.as_deref(), Some("Claude Code"));
        assert_eq!(line.detail.as_deref(), Some("3:get_device,list_devices"));
        assert_eq!(line.source, Some(Origin::Agent));
        assert_eq!(line.connection.as_ref().unwrap().kind, "agent");
        // Le second est encore ouvert, puis écrit à l'arrêt.
        let rest = groups.flush_all();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].detail.as_deref(), Some("1:list_rooms"));
        assert!(groups.flush_all().is_empty());
    }

    #[test]
    fn a_read_after_the_window_closes_it_and_opens_the_next() {
        let groups = ReadGroups::default();
        let w = who("Claude Code");
        groups.note("a1", &w, "Claude Code", "ostal_home_summary", 0.0);
        let due = groups.note("a1", &w, "Claude Code", "ostal_list_rooms", 700.0);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].detail.as_deref(), Some("1:home_summary"));
        let next = groups.flush_all();
        assert_eq!(next[0].at, 700.0);
        assert_eq!(next[0].detail.as_deref(), Some("1:list_rooms"));
    }

    #[test]
    fn the_number_of_open_windows_is_bounded() {
        let groups = ReadGroups::default();
        let mut written = 0;
        for n in 0..(MAX_OPEN + 40) {
            let id = format!("a{n}");
            written += groups
                .note(&id, &who("X"), "X", "ostal_list_rooms", n as f64)
                .len();
        }
        assert_eq!(written, 40, "la plus ancienne est fermée à chaque excès");
        assert_eq!(groups.flush_all().len(), MAX_OPEN);
    }

    #[test]
    fn occupation_and_denied_lines_carry_a_closed_word_never_an_argument() {
        let w = who("Claude Code");
        let read = occupation_entry(&w, "Claude Code", "ostal_get_presence");
        assert_eq!(read.action, Action::AgentOccupation);
        assert_eq!(read.detail.as_deref(), Some("get_presence"));
        assert_eq!(read.source, Some(Origin::Agent));
        let denied = denied_entry(&w, "Claude Code", "scope");
        assert_eq!(denied.action, Action::AgentDenied);
        assert_eq!(denied.detail.as_deref(), Some("scope"));
        assert_eq!(denied.result, crate::audit::Outcome::Refused);
    }

    #[test]
    fn who_of_names_the_owner_and_the_agent_not_the_token() {
        let agent = AgentInfo {
            id: "id-du-jeton".into(),
            user_id: "u1".into(),
            label: "Claude Code".into(),
            scopes: vec![],
            created: 0.0,
            expires: f64::MAX,
        };
        let who = who_of(&agent, Some("Hamdi".into()), Some("127.0.0.1".into()), None);
        assert_eq!(who.user_name.as_deref(), Some("Hamdi"));
        let conn = who.connection.unwrap();
        assert_eq!(
            (conn.kind.as_str(), conn.name.as_str()),
            ("agent", "Claude Code")
        );
        assert_eq!(conn.id.as_deref(), Some("id-du-jeton"));
    }
}
