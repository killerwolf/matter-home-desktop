//! Les demandes d'un agent d'IA qui attendent l'accord d'une personne (serveur MCP intégré,
//! étape E3, `docs/agents-ia.md`). Une action sensible n'est jamais exécutée sur la seule parole de
//! l'agent : le serveur la met ici, prévient la personne à qui appartient l'agent, et ne l'exécute
//! que si **cette personne** l'autorise dans l'app web (une session ; un jeton d'agent n'en est pas
//! une), puis que l'agent rappelle le **même** outil avec le **même** `confirmation_id`.
//!
//! - **En mémoire seulement** : un redémarrage annule toutes les demandes, c'est voulu.
//! - Valable **5 minutes**, **une seule fois**, pour **ce jeton** et **cette empreinte** (l'outil,
//!   ses arguments et ce que le serveur en a déduit : appareils visés, mode de départ, règles qui
//!   partiraient). Si la situation a changé entre la demande et l'exécution, l'empreinte diffère et
//!   la demande ne vaut plus rien.
//! - Bornes : 3 demandes vivantes par jeton, 10 nouvelles par heure et par jeton, 128 en tout.
//! - Seule la personne propriétaire de l'agent voit et valide la demande ; pour tout autre
//!   (ou un identifiant inconnu), elle n'existe pas.
//!
//! Le texte montré à la personne est **fabriqué par Ostal** (verbes d'une liste fermée, noms
//! nettoyés), jamais une chaîne de l'agent.
//!
//! Tout se calcule à partir d'une heure donnée (`*_at`) : les délais se testent sans attendre.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use rand::RngCore;
use serde::Serialize;

use crate::audit::{Action, Entry, Origin, Who};
use crate::rights::Capability;

/// Durée de vie d'une demande, en secondes.
pub const TTL_S: f64 = 300.0;
/// Demandes vivantes (en attente ou autorisées pas encore utilisées) au plus par jeton.
pub const MAX_LIVE_PER_AGENT: usize = 3;
/// Nouvelles demandes au plus par heure et par jeton.
pub const MAX_NEW_PER_HOUR: usize = 10;
/// Demandes gardées au plus (vivantes ou terminées depuis peu).
const MAX_TABLE: usize = 128;
/// Une demande terminée reste lisible (statut de l'agent) ce temps après son échéance.
const KEEP_AFTER_EXPIRY_S: f64 = 900.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Approved,
    Refused,
    Expired,
    Used,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Approved => "approved",
            State::Refused => "refused",
            State::Expired => "expired",
            State::Used => "used",
        }
    }

    fn live(self) -> bool {
        matches!(self, State::Pending | State::Approved)
    }
}

struct Item {
    id: String,
    agent_id: String,
    user_id: String,
    label: String,
    tool: &'static str,
    fingerprint: String,
    capability: Capability,
    text: String,
    details: Vec<String>,
    rules: Vec<String>,
    created: f64,
    expires: f64,
    state: State,
    /// L'agent qui a demandé, pour la ligne « expirée » du journal.
    who: Who,
    expiry_logged: bool,
}

/// Ce que l'outil demande de mettre en attente.
pub struct Draft {
    pub agent_id: String,
    pub user_id: String,
    pub label: String,
    pub who: Who,
    /// Le nom court de l'outil (`set_mode`, `set_cover`…), mot fermé.
    pub tool: &'static str,
    /// Ce que la personne devra pouvoir faire pour autoriser.
    pub capability: Capability,
    pub fingerprint: String,
    /// La phrase de la carte, fabriquée par Ostal.
    pub text: String,
    /// Pourquoi une personne doit valider, en phrases fermées.
    pub details: Vec<String>,
    /// Les automatisations qui partiraient (noms nettoyés).
    pub rules: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub id: String,
    pub state: State,
    pub expires: f64,
    /// `false` : la même demande attendait déjà (l'agent a rappelé sans identifiant).
    pub is_new: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    /// Déjà 3 demandes vivantes pour ce jeton.
    TooManyLive,
    /// Déjà 10 nouvelles demandes dans l'heure.
    TooManyNew,
    TableFull,
}

#[derive(Debug, PartialEq)]
pub enum ConsumeError {
    /// Inconnue, ou à un autre agent : la même réponse.
    Unknown,
    /// La personne n'a pas encore répondu.
    Pending {
        expires: f64,
    },
    Refused,
    Expired,
    Used,
    /// Autorisée, mais pour autre chose : la situation a changé depuis la demande.
    Changed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecideError {
    /// Inconnue, à une autre personne, ou de l'agent qu'on a révoqué : la même réponse.
    Unknown,
    /// Déjà répondue, utilisée ou expirée.
    NotPending,
    /// Le compte de la personne n'a plus le droit que l'action demande.
    NotAllowed,
}

/// Ce que la personne a décidé, pour la ligne du journal.
#[derive(Debug, PartialEq, Eq)]
pub struct Decided {
    pub label: String,
    pub tool: &'static str,
}

/// La carte « Demandes à valider » : rien que du texte fabriqué par Ostal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardView {
    pub id: String,
    /// Le libellé de l'agent (choisi par la personne).
    pub agent: String,
    pub text: String,
    pub details: Vec<String>,
    pub rules: Vec<String>,
    pub created: f64,
    pub expires: f64,
}

#[derive(Default)]
struct Table {
    items: Vec<Item>,
    /// Les nouvelles demandes de la dernière heure, par jeton.
    recent: HashMap<String, VecDeque<f64>>,
}

#[derive(Default)]
pub struct Confirmations {
    table: Mutex<Table>,
}

fn new_id() -> String {
    let mut raw = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut raw);
    hex::encode(raw)
}

/// Un identifiant de demande bien formé : 32 chiffres hexadécimaux minuscules.
pub fn is_valid_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Passe en `Expired` ce qui a dépassé son échéance, sans rien effacer.
fn expire(items: &mut [Item], now: f64) {
    for item in items.iter_mut() {
        if item.state.live() && now >= item.expires {
            item.state = State::Expired;
        }
    }
}

impl Confirmations {
    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Met une demande en attente. Si la **même** (même jeton, même empreinte) attend déjà, ou est
    /// déjà autorisée sans avoir servi, c'est elle qui est rendue (`is_new: false`) : un agent qui
    /// insiste n'épuise pas ses 3 places.
    pub fn request_at(&self, draft: Draft, now: f64) -> Result<Ticket, RequestError> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        table
            .items
            .retain(|i| now < i.expires + KEEP_AFTER_EXPIRY_S);
        if let Some(item) = table.items.iter().find(|i| {
            i.agent_id == draft.agent_id && i.fingerprint == draft.fingerprint && i.state.live()
        }) {
            return Ok(Ticket {
                id: item.id.clone(),
                state: item.state,
                expires: item.expires,
                is_new: false,
            });
        }
        let live = table
            .items
            .iter()
            .filter(|i| i.agent_id == draft.agent_id && i.state.live())
            .count();
        if live >= MAX_LIVE_PER_AGENT {
            return Err(RequestError::TooManyLive);
        }
        let recent = table.recent.entry(draft.agent_id.clone()).or_default();
        while recent.front().is_some_and(|t| now - *t >= 3600.0) {
            recent.pop_front();
        }
        if recent.len() >= MAX_NEW_PER_HOUR {
            return Err(RequestError::TooManyNew);
        }
        if table.items.len() >= MAX_TABLE {
            return Err(RequestError::TableFull);
        }
        table
            .recent
            .entry(draft.agent_id.clone())
            .or_default()
            .push_back(now);
        table.recent.retain(|_, q| !q.is_empty());
        let item = Item {
            id: new_id(),
            agent_id: draft.agent_id,
            user_id: draft.user_id,
            label: draft.label,
            tool: draft.tool,
            fingerprint: draft.fingerprint,
            capability: draft.capability,
            text: draft.text,
            details: draft.details,
            rules: draft.rules,
            created: now,
            expires: now + TTL_S,
            state: State::Pending,
            who: draft.who,
            expiry_logged: false,
        };
        let ticket = Ticket {
            id: item.id.clone(),
            state: State::Pending,
            expires: item.expires,
            is_new: true,
        };
        table.items.push(item);
        Ok(ticket)
    }

    /// L'état d'une demande **de cet agent** (jamais celle d'un autre : « inconnue »).
    pub fn status_at(
        &self,
        agent_id: &str,
        id: &str,
        now: f64,
    ) -> Option<(State, f64, &'static str)> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        table
            .items
            .iter()
            .find(|i| i.id == id && i.agent_id == agent_id)
            .map(|i| (i.state, i.expires, i.tool))
    }

    /// L'agent rappelle son outil avec `id` : la demande doit être autorisée, être la sienne, et
    /// porter la même empreinte. **Elle passe à « utilisée » ici, avant l'exécution** : une
    /// exécution qui échoue ne rend pas l'autorisation.
    pub fn consume_at(
        &self,
        agent_id: &str,
        id: &str,
        fingerprint: &str,
        now: f64,
    ) -> Result<(), ConsumeError> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        let Some(item) = table
            .items
            .iter_mut()
            .find(|i| i.id == id && i.agent_id == agent_id)
        else {
            return Err(ConsumeError::Unknown);
        };
        match item.state {
            State::Pending => Err(ConsumeError::Pending {
                expires: item.expires,
            }),
            State::Refused => Err(ConsumeError::Refused),
            State::Expired => Err(ConsumeError::Expired),
            State::Used => Err(ConsumeError::Used),
            State::Approved if item.fingerprint != fingerprint => Err(ConsumeError::Changed),
            State::Approved => {
                item.state = State::Used;
                Ok(())
            }
        }
    }

    /// Les demandes à valider de cette personne, les plus anciennes d'abord. `live` dit si l'agent
    /// existe encore (révoqué ou expiré : sa demande ne se montre plus).
    pub fn list_for_at(
        &self,
        user_id: &str,
        now: f64,
        live: &dyn Fn(&str) -> bool,
    ) -> Vec<CardView> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        let mut cards: Vec<CardView> = table
            .items
            .iter()
            .filter(|i| i.user_id == user_id && i.state == State::Pending && live(&i.agent_id))
            .map(|i| CardView {
                id: i.id.clone(),
                agent: i.label.clone(),
                text: i.text.clone(),
                details: i.details.clone(),
                rules: i.rules.clone(),
                created: i.created,
                expires: i.expires,
            })
            .collect();
        cards.sort_by(|a, b| a.created.total_cmp(&b.created));
        cards
    }

    /// La personne autorise ou refuse. Seule la **propriétaire de l'agent** le peut ; pour toute
    /// autre, ou un identifiant inconnu, la demande n'existe pas.
    pub fn decide_at(
        &self,
        user_id: &str,
        id: &str,
        approve: bool,
        now: f64,
        live: &dyn Fn(&str) -> bool,
        can: &dyn Fn(Capability) -> bool,
    ) -> Result<Decided, DecideError> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        let Some(item) = table
            .items
            .iter_mut()
            .find(|i| i.id == id && i.user_id == user_id && live(&i.agent_id))
        else {
            return Err(DecideError::Unknown);
        };
        if item.state != State::Pending {
            return Err(DecideError::NotPending);
        }
        // Autoriser demande de pouvoir faire la chose soi-même ; refuser, jamais.
        if approve && !can(item.capability) {
            return Err(DecideError::NotAllowed);
        }
        item.state = if approve {
            State::Approved
        } else {
            State::Refused
        };
        Ok(Decided {
            label: item.label.clone(),
            tool: item.tool,
        })
    }

    /// Une fois par minute : écrit les demandes expirées (« personne n'a répondu ») et oublie les
    /// anciennes. Rend les lignes du journal.
    pub fn sweep_at(&self, now: f64) -> Vec<Entry> {
        let mut table = self.lock();
        expire(&mut table.items, now);
        let mut lines = Vec::new();
        for item in table.items.iter_mut() {
            if item.state == State::Expired && !item.expiry_logged {
                item.expiry_logged = true;
                lines.push(
                    Entry::new(Action::ConfirmationExpired, &item.who)
                        .target(&item.label)
                        .detail(item.tool)
                        .source(Origin::Agent),
                );
            }
        }
        table
            .items
            .retain(|i| now < i.expires + KEEP_AFTER_EXPIRY_S);
        lines
    }

    pub fn sweep(&self) -> Vec<Entry> {
        self.sweep_at(crate::events::now_unix())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(agent: &str, user: &str, fingerprint: &str) -> Draft {
        Draft {
            agent_id: agent.into(),
            user_id: user.into(),
            label: "Claude Code".into(),
            who: Who::default(),
            tool: "set_mode",
            capability: Capability::ChangeMode,
            fingerprint: fingerprint.into(),
            text: "Mettre la maison en mode Vacances.".into(),
            details: vec!["Vacances".into()],
            rules: vec![],
        }
    }

    fn alive(_: &str) -> bool {
        true
    }

    fn allowed(_: Capability) -> bool {
        true
    }

    #[test]
    fn a_request_lives_five_minutes_and_is_used_once() {
        let c = Confirmations::default();
        let t = c.request_at(draft("a1", "u1", "f"), 1000.0).unwrap();
        assert!(t.is_new && t.state == State::Pending);
        assert_eq!(t.expires, 1000.0 + TTL_S);
        assert!(is_valid_id(&t.id));
        // Pas encore autorisée.
        assert_eq!(
            c.consume_at("a1", &t.id, "f", 1001.0),
            Err(ConsumeError::Pending { expires: 1300.0 })
        );
        c.decide_at("u1", &t.id, true, 1002.0, &alive, &allowed)
            .unwrap();
        // Une seule fois.
        assert_eq!(c.consume_at("a1", &t.id, "f", 1003.0), Ok(()));
        assert_eq!(
            c.consume_at("a1", &t.id, "f", 1004.0),
            Err(ConsumeError::Used)
        );
        assert_eq!(c.status_at("a1", &t.id, 1005.0).unwrap().0, State::Used);
    }

    #[test]
    fn an_approval_is_for_this_token_and_these_arguments_only() {
        let c = Confirmations::default();
        let t = c.request_at(draft("a1", "u1", "f"), 0.0).unwrap();
        c.decide_at("u1", &t.id, true, 1.0, &alive, &allowed)
            .unwrap();
        // Un autre jeton : inconnue, et sans effet sur la demande.
        assert_eq!(
            c.consume_at("a2", &t.id, "f", 2.0),
            Err(ConsumeError::Unknown)
        );
        assert!(c.status_at("a2", &t.id, 2.0).is_none());
        // D'autres arguments : refusé, et l'autorisation n'est pas brûlée.
        assert_eq!(
            c.consume_at("a1", &t.id, "g", 3.0),
            Err(ConsumeError::Changed)
        );
        assert_eq!(c.consume_at("a1", &t.id, "f", 4.0), Ok(()));
    }

    #[test]
    fn it_expires_after_five_minutes_even_once_approved() {
        let c = Confirmations::default();
        let t = c.request_at(draft("a1", "u1", "f"), 0.0).unwrap();
        c.decide_at("u1", &t.id, true, 10.0, &alive, &allowed)
            .unwrap();
        assert_eq!(
            c.consume_at("a1", &t.id, "f", TTL_S),
            Err(ConsumeError::Expired)
        );
        // Une demande jamais répondue : expirée, une ligne au journal, une seule.
        let t2 = c.request_at(draft("a1", "u1", "g"), 0.0).unwrap();
        assert_eq!(
            c.decide_at("u1", &t2.id, true, TTL_S + 1.0, &alive, &allowed),
            Err(DecideError::NotPending)
        );
        let lines = c.sweep_at(TTL_S + 2.0);
        assert_eq!(lines.len(), 2);
        assert!(lines
            .iter()
            .all(|l| l.action == Action::ConfirmationExpired
                && l.detail.as_deref() == Some("set_mode")));
        assert!(c.sweep_at(TTL_S + 3.0).is_empty());
        // Oubliée un quart d'heure après son échéance.
        assert!(c.status_at("a1", &t.id, TTL_S + 10.0).is_some());
        c.sweep_at(TTL_S + KEEP_AFTER_EXPIRY_S + 1.0);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn only_the_owner_decides_and_a_refusal_is_final() {
        let c = Confirmations::default();
        let t = c.request_at(draft("a1", "u1", "f"), 0.0).unwrap();
        // Une autre personne : la demande n'existe pas pour elle, et ne figure pas dans sa liste.
        assert_eq!(
            c.decide_at("u2", &t.id, true, 1.0, &alive, &allowed),
            Err(DecideError::Unknown)
        );
        assert!(c.list_for_at("u2", 1.0, &alive).is_empty());
        assert_eq!(c.list_for_at("u1", 1.0, &alive).len(), 1);
        // L'agent révoqué : plus rien à valider.
        assert!(c.list_for_at("u1", 1.0, &|_| false).is_empty());
        assert_eq!(
            c.decide_at("u1", &t.id, true, 1.0, &|_| false, &allowed),
            Err(DecideError::Unknown)
        );
        // Un compte qui n'a plus le droit ne peut pas autoriser, mais peut refuser.
        assert_eq!(
            c.decide_at("u1", &t.id, true, 1.0, &alive, &|_| false),
            Err(DecideError::NotAllowed)
        );
        assert!(c
            .decide_at("u1", &t.id, false, 2.0, &alive, &|_| false)
            .is_ok());
        assert_eq!(
            c.consume_at("a1", &t.id, "f", 3.0),
            Err(ConsumeError::Refused)
        );
        assert_eq!(
            c.decide_at("u1", &t.id, true, 3.0, &alive, &allowed),
            Err(DecideError::NotPending)
        );
        assert!(c.list_for_at("u1", 3.0, &alive).is_empty());
    }

    #[test]
    fn three_live_requests_per_token_and_ten_new_ones_per_hour() {
        let c = Confirmations::default();
        for n in 0..MAX_LIVE_PER_AGENT {
            c.request_at(draft("a1", "u1", &format!("f{n}")), n as f64)
                .unwrap();
        }
        assert_eq!(
            c.request_at(draft("a1", "u1", "f9"), 5.0).err(),
            Some(RequestError::TooManyLive)
        );
        // Un autre jeton n'est pas touché.
        c.request_at(draft("a2", "u1", "f0"), 5.0).unwrap();
        // La même demande, rappelée : la même, pas une quatrième.
        let again = c.request_at(draft("a1", "u1", "f0"), 6.0).unwrap();
        assert!(!again.is_new);
        // Dix par heure : trois toutes les 5 minutes, jusqu'à la dixième.
        let c = Confirmations::default();
        let mut now = 0.0;
        let mut made = 0;
        loop {
            match c.request_at(draft("a1", "u1", &format!("g{made}")), now) {
                Ok(t) => {
                    made += 1;
                    c.decide_at("u1", &t.id, false, now, &alive, &allowed)
                        .unwrap();
                }
                Err(e) => {
                    assert_eq!(e, RequestError::TooManyNew);
                    break;
                }
            }
            now += 1.0;
            assert!(made <= MAX_NEW_PER_HOUR);
        }
        assert_eq!(made, MAX_NEW_PER_HOUR);
        // Une heure plus tard, de nouveau possible.
        assert!(c.request_at(draft("a1", "u1", "h"), 3601.0).is_ok());
    }

    #[test]
    fn a_refused_request_can_be_asked_again_and_the_table_is_bounded() {
        let c = Confirmations::default();
        let t = c.request_at(draft("a1", "u1", "f"), 0.0).unwrap();
        c.decide_at("u1", &t.id, false, 1.0, &alive, &allowed)
            .unwrap();
        let again = c.request_at(draft("a1", "u1", "f"), 2.0).unwrap();
        assert!(again.is_new && again.id != t.id);
        // Une table pleine refuse, sans rien effacer de vivant.
        let c = Confirmations::default();
        for n in 0..MAX_TABLE {
            // Un jeton par demande : ni les 3 vivantes ni les 10 par heure ne bornent ici.
            c.request_at(draft(&format!("a{n}"), "u1", "f"), 0.0)
                .unwrap();
        }
        assert_eq!(
            c.request_at(draft("zz", "u1", "f"), 1.0).err(),
            Some(RequestError::TableFull)
        );
        assert_eq!(c.len(), MAX_TABLE);
    }

    #[test]
    fn identifiers_are_32_lowercase_hex_digits() {
        assert!(is_valid_id(&new_id()));
        assert_ne!(new_id(), new_id());
        for bad in ["", "abc", &"A".repeat(32), &"g".repeat(32), &"a".repeat(33)] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }
}
