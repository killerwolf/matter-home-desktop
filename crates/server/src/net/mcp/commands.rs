//! Les commandes d'un agent d'IA : `ostal_set_on_off` (prises et lampes, étape E2), puis
//! `ostal_set_cover` (un volet) et `ostal_set_room` (les appareils d'une pièce), étape E3. Pas de
//! `Toggle` (non idempotent : l'agent vise l'état final).
//!
//! Ordre des contrôles, chacun ne révélant que ce qu'il doit :
//!
//! 1. arguments (`id`, `command` ou `position`, `confirmation_id`) ;
//! 2. l'appareil est dans la liste des appareils permis du jeton (`devices` dans `agents.json`,
//!    vide par défaut, 20 au plus) **avant** de chercher s'il existe : un agent ne sonde pas la
//!    maison ;
//! 3. l'appareil n'est pas dans la liste « jamais par un agent » (`never`). Les deux refus disent
//!    la même phrase ;
//! 4. l'appareil existe et est du bon type (prise ou lampe, volet) ;
//! 5. **ce que ça déclenche** : l'appareil est-il « sensible » (liste `sensitive`) ? la commande
//!    fait-elle partir, directement ou de proche en proche, une automatisation active qui change le
//!    mode, appelle une adresse web, commande un appareil sensible ou « jamais » ou que ce jeton ne
//!    peut pas commander, ou qu'on ne connaît pas ? Alors **rien n'est envoyé** : une personne doit
//!    valider (module `confirm`) ;
//! 6. `Hub::set_on_off` ou `Hub::set_cover`, puis une ligne au journal par appareil.
//!
//! Le débit (20 commandes par minute et par jeton, une pièce comptant pour son nombre d'appareils)
//! est compté dans `mod.rs` et ici.
//!
//! Limite connue : une règle « quand la prise X reste injoignable » (`Unreachable`) n'est pas
//! suivie, ni un seuil posé sur **un autre appareil** dont la mesure dépend de la commande, ni le
//! capteur de contact d'une porte de garage (un autre appareil que son moteur).

use std::collections::HashSet;

use protocol::automation::{Action as RuleAction, CoverCommand, OnOffCommand, RuleSet, Trigger};
use protocol::mode::HomeMode;
use protocol::wire::{parse_id, ApiError, ErrorCode};
use serde_json::{json, Map, Value};

use super::confirm::{confirmation_arg, gate, id_property, Gate, Plan, Reason};
use super::tools::{name_of, only_keys, Ctx, ToolError};
use crate::audit::{Action, Entry, Origin};
use crate::rights::Capability;

pub(super) const SET_ON_OFF: &str = "ostal_set_on_off";
pub(super) const SET_COVER: &str = "ostal_set_cover";
pub(super) const SET_ROOM: &str = "ostal_set_room";

pub(super) fn set_on_off_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "Identifiant de la prise ou de la lampe, en texte décimal (ostal_list_devices)."},
            "command": {"type": "string", "enum": ["On", "Off"], "description": "L'état voulu. Pas d'« inverser » : dites On ou Off."},
            "confirmation_id": id_property()
        },
        "required": ["id", "command"],
        "additionalProperties": false
    })
}

pub(super) fn set_cover_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "Identifiant du volet, en texte décimal (ostal_list_devices)."},
            "command": {"type": "string", "enum": ["Open", "Close", "Stop"], "description": "Ouvrir, fermer ou arrêter. Ou donnez `position`, pas les deux."},
            "position": {"type": "integer", "minimum": 0, "maximum": 100, "description": "Ouverture visée en % (100 = tout ouvert). Ou donnez `command`, pas les deux."},
            "confirmation_id": id_property()
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

pub(super) fn set_room_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "room_id": {"type": "string", "description": "Identifiant de la pièce, en texte décimal (ostal_list_rooms). Jamais un nom : deux étages peuvent avoir une pièce du même nom."},
            "kind": {"type": "string", "enum": ["on_off", "cover"], "description": "on_off : les prises et lampes de la pièce ; cover : ses volets."},
            "command": {"type": "string", "enum": ["On", "Off", "Open", "Close", "Stop"], "description": "On ou Off pour on_off ; Open, Close ou Stop pour cover."},
            "position": {"type": "integer", "minimum": 0, "maximum": 100, "description": "Seulement pour cover : ouverture visée en %. Ou donnez `command`."},
            "confirmation_id": id_property()
        },
        "required": ["room_id", "kind"],
        "additionalProperties": false
    })
}

const NOT_PERMITTED: &str = "Cet agent n'a pas le droit de commander cet appareil.";

/// Refuse un appel : une ligne `agent_denied` (au plus une par minute, par agent et par motif) et
/// l'erreur rendue à l'agent. `reason` est un mot fermé.
fn refuse(ctx: &Ctx<'_>, reason: &'static str, message: &str) -> ToolError {
    note_denied(ctx, reason);
    ToolError::new("forbidden", message)
}

fn note_denied(ctx: &Ctx<'_>, reason: &'static str) {
    let entry = crate::agent_audit::denied_entry(ctx.who, &ctx.agent.label, reason);
    ctx.hub.audit().record_throttled(
        &format!("agent-denied:{}:{reason}", ctx.agent.id),
        60.0,
        entry,
    );
}

fn device_id(arguments: &Map<String, Value>, key: &str, what: &str) -> Result<u64, ToolError> {
    match arguments.get(key) {
        Some(Value::String(text)) => parse_id(text).map_err(|_| {
            ToolError::invalid(format!(
                "{key} : identifiant invalide (texte décimal, sans zéro initial)."
            ))
        }),
        _ => Err(ToolError::invalid(format!(
            "{key} est obligatoire : l'identifiant {what}, en texte décimal."
        ))),
    }
}

fn parse_on_off(arguments: &Map<String, Value>) -> Result<OnOffCommand, ToolError> {
    match arguments.get("command").and_then(Value::as_str) {
        Some("On") => Ok(OnOffCommand::On),
        Some("Off") => Ok(OnOffCommand::Off),
        _ => Err(ToolError::invalid(
            "command doit valoir On ou Off (pas d'inversion : visez l'état final).",
        )),
    }
}

/// `command` (Open, Close, Stop) ou `position` (0 à 100), l'un **ou** l'autre.
fn parse_cover(arguments: &Map<String, Value>) -> Result<CoverCommand, ToolError> {
    let bad = || {
        ToolError::invalid(
            "Donnez command (Open, Close ou Stop) ou position (0 à 100), pas les deux.",
        )
    };
    match (arguments.get("command"), arguments.get("position")) {
        (Some(Value::String(word)), None) => match word.as_str() {
            "Open" => Ok(CoverCommand::Open),
            "Close" => Ok(CoverCommand::Close),
            "Stop" => Ok(CoverCommand::Stop),
            _ => Err(bad()),
        },
        (None, Some(position)) => match position.as_u64() {
            Some(p) if p <= 100 => Ok(CoverCommand::Position(p as u8)),
            _ => Err(ToolError::invalid(
                "position : un entier de 0 à 100 (100 = tout ouvert).",
            )),
        },
        _ => Err(bad()),
    }
}

fn on_off_word(command: OnOffCommand) -> &'static str {
    match command {
        OnOffCommand::On => "on",
        OnOffCommand::Off => "off",
        OnOffCommand::Toggle => "toggle",
    }
}

fn cover_word(command: CoverCommand) -> String {
    match command {
        CoverCommand::Open => "open".into(),
        CoverCommand::Close => "close".into(),
        CoverCommand::Stop => "stop".into(),
        CoverCommand::Position(p) => format!("position:{p}"),
    }
}

fn cover_command_text(command: CoverCommand) -> String {
    match command {
        CoverCommand::Open => "Open".into(),
        CoverCommand::Close => "Close".into(),
        CoverCommand::Stop => "Stop".into(),
        CoverCommand::Position(p) => format!("position {p}"),
    }
}

/// Une prise ou une lampe : marche/arrêt, et pas un volet.
fn is_plug_or_lamp(caps: &protocol::events::Capabilities) -> bool {
    caps.on_off && !caps.cover
}

fn command_error(e: ApiError, cover: bool) -> ToolError {
    match e.code {
        ErrorCode::NotFound => ToolError::new("not_found", "Appareil introuvable."),
        ErrorCode::BadRequest if cover => {
            ToolError::invalid("Cet outil ne commande que les volets.")
        }
        ErrorCode::BadRequest => {
            ToolError::invalid("Cet outil ne commande que les prises et les lampes.")
        }
        _ => ToolError::new("unavailable", "Commande impossible pour l'instant."),
    }
}

/// Peut-on commander cet appareil : dans la liste du jeton, hors de la liste « jamais » ?
fn may(ctx: &Ctx<'_>, id: u64) -> bool {
    ctx.agent.may_command(id) && !ctx.agents.never_commanded(id)
}

/// Un appareil que cette commande ne doit pas toucher sans personne : « jamais », sensible, ou hors
/// des appareils permis du jeton.
fn flagged(ctx: &Ctx<'_>, id: u64) -> bool {
    ctx.agents.never_commanded(id) || ctx.agents.is_sensitive(id) || !ctx.agent.may_command(id)
}

// ---------------------------------------------------------------------------
// Un appareil : prise, lampe ou volet
// ---------------------------------------------------------------------------

enum One {
    OnOff(OnOffCommand),
    Cover(CoverCommand),
}

pub(super) fn set_on_off(
    ctx: &Ctx<'_>,
    arguments: &Map<String, Value>,
) -> Result<Value, ToolError> {
    single(ctx, arguments, false)
}

pub(super) fn set_cover(ctx: &Ctx<'_>, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    single(ctx, arguments, true)
}

fn single(ctx: &Ctx<'_>, arguments: &Map<String, Value>, cover: bool) -> Result<Value, ToolError> {
    only_keys(
        arguments,
        if cover {
            &["id", "command", "position", "confirmation_id"]
        } else {
            &["id", "command", "confirmation_id"]
        },
    )?;
    let id = device_id(arguments, "id", "de l'appareil")?;
    let what = if cover {
        One::Cover(parse_cover(arguments)?)
    } else {
        One::OnOff(parse_on_off(arguments)?)
    };
    let confirmation = confirmation_arg(arguments)?;
    // 2 et 3 : la même phrase, pour ne rien apprendre sur la maison.
    if !may(ctx, id) {
        return Err(refuse(ctx, "device", NOT_PERMITTED));
    }
    // 4.
    let snapshot = ctx.hub.current_snapshot();
    let Some(view) = snapshot.devices.iter().find(|v| v.device.node_id == id) else {
        return Err(ToolError::new("not_found", "Appareil introuvable."));
    };
    let right_kind = if cover {
        view.device.caps.cover
    } else {
        is_plug_or_lamp(&view.device.caps)
    };
    if !right_kind {
        return Err(ToolError::invalid(if cover {
            "Cet outil ne commande que les volets."
        } else {
            "Cet outil ne commande que les prises et les lampes."
        }));
    }
    let name = {
        let clean = ctx.hub.push_cleaner();
        name_of(&clean, &view.device.name)
    };
    // 5.
    let (tool, direction, text, key_word) = match &what {
        One::OnOff(c) => (
            "set_on_off",
            Some(*c == OnOffCommand::On),
            match c {
                OnOffCommand::On => format!("Allumer « {name} »."),
                _ => format!("Éteindre « {name} »."),
            },
            on_off_word(*c).to_string(),
        ),
        One::Cover(c) => (
            "set_cover",
            None,
            match c {
                CoverCommand::Open => format!("Ouvrir le volet « {name} »."),
                CoverCommand::Close => format!("Fermer le volet « {name} »."),
                CoverCommand::Stop => format!("Arrêter le volet « {name} »."),
                CoverCommand::Position(p) => format!("Mettre le volet « {name} » à {p} %."),
            },
            cover_word(*c),
        ),
    };
    let plan = device_plan(
        ctx,
        tool,
        format!("{tool}|{id}|{key_word}"),
        text,
        &[(id, direction)],
    );
    if let Gate::Wait(answer) = gate(ctx, &plan, confirmation.as_deref())? {
        return Ok(answer);
    }
    // 6.
    let result = match &what {
        One::OnOff(c) => ctx.hub.set_on_off(id, *c),
        One::Cover(c) => ctx.hub.set_cover(id, *c),
    };
    let detail = match &what {
        One::OnOff(c) => on_off_word(*c).to_string(),
        One::Cover(c) => cover_word(*c),
    };
    ctx.hub.audit().record(
        Entry::new(Action::DeviceCommand, ctx.who)
            .target(&name)
            .detail(detail)
            .source(Origin::Agent)
            .outcome(&result),
    );
    match result {
        Ok(()) => Ok(json!({
            "id": id.to_string(),
            "name": name,
            "command": match &what {
                One::OnOff(c) => if *c == OnOffCommand::On { "On".to_string() } else { "Off".to_string() },
                One::Cover(c) => cover_command_text(*c),
            },
            "sent": true,
            "note": "La commande est envoyée. Relisez l'appareil (ostal_get_device) pour vérifier son état."
        })),
        Err(e) => Err(command_error(e, cover)),
    }
}

/// Ce que ces commandes déclencheraient, et si une personne doit valider.
fn device_plan(
    ctx: &Ctx<'_>,
    tool: &'static str,
    key: String,
    text: String,
    targets: &[(u64, Option<bool>)],
) -> Plan {
    let rules = ctx.hub.rules();
    let starts: Vec<Start> = targets.iter().map(|(n, d)| Start::Node(*n, *d)).collect();
    let walked = walk(&rules, &starts, &|id| flagged(ctx, id));
    let mut reasons = Vec::new();
    if targets.iter().any(|(n, _)| ctx.agents.is_sensitive(*n)) {
        reasons.push(Reason::Sensitive);
    }
    if !walked.risky.is_empty() {
        reasons.push(Reason::Rules);
    }
    let ids: Vec<String> = walked.risky.iter().map(u64::to_string).collect();
    let clean = ctx.hub.push_cleaner();
    Plan {
        tool,
        capability: Capability::ControlDevices,
        text,
        key: format!("{key}|rules:{}", ids.join(",")),
        reasons,
        rules: rule_names(&rules, &walked.risky, &clean),
        tell_agent: false,
    }
}

/// Les noms (nettoyés) de ces règles, dans l'ordre des règles.
pub(super) fn rule_names(
    rules: &RuleSet,
    ids: &[u64],
    clean: &dyn Fn(&str) -> String,
) -> Vec<String> {
    rules
        .rules
        .iter()
        .filter(|r| ids.contains(&r.id))
        .map(|r| name_of(clean, &r.name))
        .collect()
}

// ---------------------------------------------------------------------------
// Une pièce
// ---------------------------------------------------------------------------

/// Appareils au plus dans un lot : la liste d'un jeton n'en permet pas davantage.
const MAX_BATCH: usize = crate::security::agent_tokens::MAX_DEVICES_PER_AGENT;

pub(super) fn set_room(ctx: &Ctx<'_>, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    only_keys(
        arguments,
        &["room_id", "kind", "command", "position", "confirmation_id"],
    )?;
    let room_id = device_id(arguments, "room_id", "de la pièce")?;
    let cover = match arguments.get("kind").and_then(Value::as_str) {
        Some("on_off") => false,
        Some("cover") => true,
        _ => {
            return Err(ToolError::invalid(
                "kind doit valoir on_off (prises et lampes) ou cover (volets).",
            ))
        }
    };
    let what = if cover {
        One::Cover(parse_cover(arguments)?)
    } else {
        if arguments.contains_key("position") {
            return Err(ToolError::invalid(
                "position ne vaut que pour les volets (kind: cover).",
            ));
        }
        One::OnOff(parse_on_off(arguments)?)
    };
    let confirmation = confirmation_arg(arguments)?;

    let snapshot = ctx.hub.current_snapshot();
    let plan = &snapshot.plan.0;
    let Some(room) = plan
        .floors
        .iter()
        .flat_map(|f| f.rooms.iter())
        .find(|r| r.id == room_id)
    else {
        return Err(ToolError::new("not_found", "Pièce introuvable."));
    };
    let clean = ctx.hub.push_cleaner();
    let room_name = name_of(&clean, &room.name);
    // Les appareils de la pièce, du bon type, dans un ordre fixe.
    let mut devices: Vec<(u64, String)> = snapshot
        .devices
        .iter()
        .filter(|v| super::tools::room_id_of(plan, v.device.node_id) == Some(room_id))
        .filter(|v| {
            if cover {
                v.device.caps.cover
            } else {
                is_plug_or_lamp(&v.device.caps)
            }
        })
        .map(|v| (v.device.node_id, name_of(&clean, &v.device.name)))
        .collect();
    devices.sort_by_key(|(id, _)| *id);
    if devices.is_empty() {
        return Err(ToolError::invalid(
            "Aucun appareil de ce type dans cette pièce.",
        ));
    }
    // Les appareils jamais permis (hors de la liste du jeton, ou « jamais ») sont sautés, et dits :
    // un seul motif pour les deux, comme pour une commande seule.
    let (lot, skipped): (Vec<_>, Vec<_>) = devices.into_iter().partition(|(id, _)| may(ctx, *id));
    if lot.is_empty() {
        return Err(refuse(ctx, "device", NOT_PERMITTED));
    }
    if lot.len() > MAX_BATCH {
        return Err(ToolError::invalid(
            "Trop d'appareils dans cette pièce (20 au plus).",
        ));
    }
    if !skipped.is_empty() {
        note_denied(ctx, "device");
    }
    let direction = match &what {
        One::OnOff(c) => Some(*c == OnOffCommand::On),
        One::Cover(_) => None,
    };
    let word = match &what {
        One::OnOff(c) => on_off_word(*c).to_string(),
        One::Cover(c) => cover_word(*c),
    };
    let names: Vec<&str> = lot.iter().map(|(_, n)| n.as_str()).collect();
    let shown = if names.len() > 6 {
        format!("{}…", names[..6].join(", "))
    } else {
        names.join(", ")
    };
    let verb = match &what {
        One::OnOff(OnOffCommand::On) => "Allumer".to_string(),
        One::OnOff(_) => "Éteindre".to_string(),
        One::Cover(CoverCommand::Open) => "Ouvrir".to_string(),
        One::Cover(CoverCommand::Close) => "Fermer".to_string(),
        One::Cover(CoverCommand::Stop) => "Arrêter".to_string(),
        One::Cover(CoverCommand::Position(p)) => format!("Mettre à {p} %"),
    };
    let text = format!(
        "{verb} {} appareil{} de la pièce « {room_name} » : {shown}.",
        lot.len(),
        if lot.len() > 1 { "s" } else { "" }
    );
    let ids: Vec<String> = lot.iter().map(|(id, _)| id.to_string()).collect();
    let targets: Vec<(u64, Option<bool>)> = lot.iter().map(|(id, _)| (*id, direction)).collect();
    let tool_plan = device_plan(
        ctx,
        "set_room",
        format!(
            "set_room|{room_id}|{}|{word}|{}",
            if cover { "cover" } else { "on_off" },
            ids.join(",")
        ),
        text,
        &targets,
    );
    // Le débit : un appareil, une commande (la première est déjà comptée dans `mod.rs`). Il se compte
    // **avant** que l'autorisation soit consommée : une minute pleine ne doit jamais brûler l'accord
    // d'une personne sans que rien ne parte. Si finalement rien ne part, on rend ce qui a été compté.
    let extra = lot.len() - 1;
    let will_run = confirmation.is_some() || tool_plan.reasons.is_empty();
    if will_run {
        if let Err(wait) = ctx.agents.check_commands(&ctx.agent.id, extra) {
            return Err(ToolError::new(
                "rate_limited",
                format!("Trop de commandes (20 par minute) : réessayez dans {wait} s."),
            ));
        }
    }
    match gate(ctx, &tool_plan, confirmation.as_deref()) {
        Ok(Gate::Go) => {}
        Ok(Gate::Wait(answer)) => {
            if will_run {
                ctx.agents.refund_commands(&ctx.agent.id, extra);
            }
            return Ok(answer);
        }
        Err(e) => {
            if will_run {
                ctx.agents.refund_commands(&ctx.agent.id, extra);
            }
            return Err(e);
        }
    }
    let mut sent = Vec::new();
    let mut failed = Vec::new();
    for (id, name) in &lot {
        let result = match &what {
            One::OnOff(c) => ctx.hub.set_on_off(*id, *c),
            One::Cover(c) => ctx.hub.set_cover(*id, *c),
        };
        ctx.hub.audit().record(
            Entry::new(Action::DeviceCommand, ctx.who)
                .target(name)
                .detail(&word)
                .source(Origin::Agent)
                .outcome(&result),
        );
        match result {
            Ok(()) => sent.push(json!({"id": id.to_string(), "name": name})),
            Err(e) => {
                let error = command_error(e, cover);
                failed.push(json!({"id": id.to_string(), "name": name, "code": error.code}));
            }
        }
    }
    Ok(json!({
        "room": {"id": room_id.to_string(), "name": room_name},
        "kind": if cover { "cover" } else { "on_off" },
        "sent": sent,
        "failed": failed,
        "skipped": skipped.iter()
            .map(|(id, name)| json!({"id": id.to_string(), "name": name, "reason": "not_permitted"}))
            .collect::<Vec<_>>(),
        "note": "Les commandes sont envoyées, pas confirmées. Relisez les appareils (ostal_get_device) pour vérifier leur état."
    }))
}

// ---------------------------------------------------------------------------
// Ce qu'une commande ou un mode fait partir
// ---------------------------------------------------------------------------

/// D'où part la marche : un appareil commandé, ou un mode que la maison prend.
pub(super) enum Start {
    /// (appareil, sens connu : `Some(true)` = s'allume, `Some(false)` = s'éteint, `None` = les deux)
    Node(u64, Option<bool>),
    Mode(HomeMode),
}

/// Les règles actives qui partiraient, et celles qui, ce faisant, feraient ce que seule une
/// personne peut décider.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Walk {
    /// Toutes celles qui partent, dans l'ordre des règles.
    pub fired: Vec<u64>,
    /// Celles dont une action change le mode, appelle une adresse web, est inconnue, ou commande un
    /// appareil `flagged`.
    pub risky: Vec<u64>,
}

/// Suit les automatisations de proche en proche.
///
/// - Pour l'appareil commandé, on tient compte du sens : `On` ne fait partir qu'un déclencheur
///   « s'allume », `Off` qu'un « s'éteint » ; un seuil (puissance…) part dans les deux sens.
/// - Pour les appareils que ces règles commandent à leur tour, on suppose les deux sens (on ne sait
///   pas ce qu'ils étaient avant).
/// - Un appareil atteint avec un sens (la commande de départ) est revu sans sens quand une règle
///   le commande à son tour : une minuterie « allumé → éteint » peut en réveiller une autre.
/// - Les conditions sont ignorées : elles peuvent être vraies à ce moment-là.
/// - Les règles désactivées ne comptent pas : elles ne partiront pas. Le calcul est refait à chaque
///   commande.
pub(super) fn walk(rules: &RuleSet, starts: &[Start], flagged: &dyn Fn(u64) -> bool) -> Walk {
    let mut seen: HashSet<(u64, Option<bool>)> = HashSet::new();
    let mut queue: Vec<(u64, Option<bool>)> = Vec::new();
    let mut fired_ids: HashSet<u64> = HashSet::new();
    let mut risky_ids: HashSet<u64> = HashSet::new();
    let mut handle = |rule: &protocol::automation::Rule,
                      queue: &mut Vec<(u64, Option<bool>)>,
                      seen: &mut HashSet<(u64, Option<bool>)>| {
        fired_ids.insert(rule.id);
        for action in &rule.actions {
            match action {
                RuleAction::Notify { .. } | RuleAction::Delay { .. } => {}
                RuleAction::SetOnOff { node_id, .. } | RuleAction::SetCover { node_id, .. } => {
                    if flagged(*node_id) {
                        risky_ids.insert(rule.id);
                    }
                    if seen.insert((*node_id, None)) {
                        queue.push((*node_id, None));
                    }
                }
                // Le mode, une adresse web, une action inconnue.
                _ => {
                    risky_ids.insert(rule.id);
                }
            }
        }
    };
    for start in starts {
        match start {
            Start::Node(node, direction) => {
                if seen.insert((*node, *direction)) {
                    queue.push((*node, *direction));
                }
            }
            Start::Mode(mode) => {
                for rule in rules.rules.iter().filter(|r| r.enabled) {
                    if matches!(&rule.trigger, Trigger::Mode { to } if to == mode) {
                        handle(rule, &mut queue, &mut seen);
                    }
                }
            }
        }
    }
    while let Some((node, direction)) = queue.pop() {
        for rule in rules.rules.iter().filter(|r| r.enabled) {
            let fires = match &rule.trigger {
                Trigger::Switch { node_id, on } => {
                    *node_id == node && direction.is_none_or(|d| d == *on)
                }
                Trigger::Threshold { node_id, .. } => *node_id == node,
                _ => false,
            };
            if fires {
                handle(rule, &mut queue, &mut seen);
            }
        }
    }
    let in_order = |ids: &HashSet<u64>| -> Vec<u64> {
        rules
            .rules
            .iter()
            .map(|r| r.id)
            .filter(|id| ids.contains(id))
            .collect()
    };
    Walk {
        fired: in_order(&fired_ids),
        risky: in_order(&risky_ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::automation::{Condition, Rule};

    fn rule(id: u64, trigger: Trigger, actions: Vec<RuleAction>) -> Rule {
        Rule {
            id,
            name: format!("r{id}"),
            enabled: true,
            trigger,
            conditions: vec![],
            actions,
            cooldown_s: 60,
        }
    }

    fn set(rules: Vec<Rule>) -> RuleSet {
        RuleSet { version: 1, rules }
    }

    fn switch(node_id: u64, on: bool) -> Trigger {
        Trigger::Switch { node_id, on }
    }

    fn webhook() -> RuleAction {
        RuleAction::Webhook {
            url: "https://exemple.invalid/x".into(),
            body: String::new(),
        }
    }

    fn nobody(_: u64) -> bool {
        false
    }

    /// Cette commande ferait-elle partir une règle qui demande une personne ?
    fn risky_from(
        rules: &RuleSet,
        node_id: u64,
        command: OnOffCommand,
        flagged: &dyn Fn(u64) -> bool,
    ) -> bool {
        let direction = match command {
            OnOffCommand::On => Some(true),
            OnOffCommand::Off => Some(false),
            OnOffCommand::Toggle => None,
        };
        !walk(rules, &[Start::Node(node_id, direction)], flagged)
            .risky
            .is_empty()
    }

    #[test]
    fn a_harmless_house_lets_the_command_through() {
        let rules = set(vec![
            rule(
                1,
                switch(1, true),
                vec![RuleAction::Notify {
                    title: "t".into(),
                    message: "m".into(),
                }],
            ),
            rule(
                2,
                Trigger::Time {
                    hour: 1,
                    minute: 0,
                    days: [true; 7],
                },
                vec![webhook()],
            ),
        ]);
        assert!(!risky_from(&rules, 1, OnOffCommand::On, &nobody));
        assert!(!risky_from(&set(vec![]), 1, OnOffCommand::Off, &nobody));
        // La règle de la notification part, mais n'a rien de risqué : elle est « déclenchée ».
        let walked = walk(&rules, &[Start::Node(1, Some(true))], &nobody);
        assert_eq!(walked.fired, vec![1]);
        assert!(walked.risky.is_empty());
    }

    #[test]
    fn a_rule_that_switches_the_commanded_device_back_is_followed() {
        let off = RuleAction::SetOnOff {
            node_id: 1,
            command: OnOffCommand::Off,
        };
        let rules = set(vec![
            rule(1, switch(1, true), vec![off]),
            rule(2, switch(1, false), vec![webhook()]),
        ]);
        assert!(risky_from(&rules, 1, OnOffCommand::On, &nobody));
        assert!(risky_from(&rules, 1, OnOffCommand::Off, &nobody));
        assert!(risky_from(&rules, 1, OnOffCommand::Toggle, &nobody));
        // Sans la règle 2, la boucle ne trouve rien (et se termine).
        let calm = set(vec![rules.rules[0].clone()]);
        assert!(!risky_from(&calm, 1, OnOffCommand::On, &nobody));
    }

    #[test]
    fn mode_webhook_and_unknown_actions_are_risky_and_a_cover_only_when_flagged() {
        for action in [
            RuleAction::SetMode {
                mode: HomeMode::Present,
            },
            webhook(),
            RuleAction::Unknown,
        ] {
            let rules = set(vec![rule(1, switch(1, true), vec![action])]);
            assert!(risky_from(&rules, 1, OnOffCommand::On, &nobody));
        }
        let cover = RuleAction::SetCover {
            node_id: 9,
            command: CoverCommand::Open,
        };
        let rules = set(vec![rule(1, switch(1, true), vec![cover])]);
        assert!(!risky_from(&rules, 1, OnOffCommand::On, &nobody));
        assert!(risky_from(&rules, 1, OnOffCommand::On, &|id| id == 9));
    }

    #[test]
    fn the_direction_counts_for_a_switch_but_not_for_a_threshold() {
        let rules = set(vec![rule(1, switch(1, true), vec![webhook()])]);
        assert!(risky_from(&rules, 1, OnOffCommand::On, &nobody));
        assert!(!risky_from(&rules, 1, OnOffCommand::Off, &nobody));
        let threshold = set(vec![rule(
            1,
            Trigger::Threshold {
                node_id: 1,
                measure: protocol::events::MeasureKind::Power,
                above: true,
                value: 50.0,
                for_s: 0,
            },
            vec![webhook()],
        )]);
        assert!(risky_from(&threshold, 1, OnOffCommand::Off, &nobody));
        assert!(risky_from(&threshold, 1, OnOffCommand::On, &nobody));
    }

    #[test]
    fn other_devices_and_disabled_rules_do_not_count_but_conditions_are_ignored() {
        let mut off = rule(1, switch(1, true), vec![webhook()]);
        off.enabled = false;
        assert!(!risky_from(&set(vec![off]), 1, OnOffCommand::On, &nobody));
        let elsewhere = set(vec![rule(1, switch(2, true), vec![webhook()])]);
        assert!(!risky_from(&elsewhere, 1, OnOffCommand::On, &nobody));
        let mut conditioned = rule(1, switch(1, true), vec![webhook()]);
        conditioned.conditions = vec![Condition::Mode {
            is: vec![HomeMode::Absent],
        }];
        assert!(risky_from(
            &set(vec![conditioned]),
            1,
            OnOffCommand::On,
            &nobody
        ));
    }

    #[test]
    fn a_chain_of_rules_is_followed_and_a_loop_ends() {
        let on_off = |node_id| RuleAction::SetOnOff {
            node_id,
            command: OnOffCommand::On,
        };
        // 1 allume 2, 2 allume 3, 3 appelle une adresse.
        let chain = set(vec![
            rule(1, switch(1, true), vec![on_off(2)]),
            rule(2, switch(2, true), vec![on_off(3)]),
            rule(3, switch(3, true), vec![webhook()]),
        ]);
        let walked = walk(&chain, &[Start::Node(1, Some(true))], &nobody);
        assert_eq!(walked.fired, vec![1, 2, 3]);
        assert_eq!(walked.risky, vec![3], "seule la dernière est en cause");
        // Une boucle sans effet dangereux se termine.
        let cycle = set(vec![
            rule(1, switch(1, true), vec![on_off(2)]),
            rule(2, switch(2, true), vec![on_off(1)]),
        ]);
        assert!(!risky_from(&cycle, 1, OnOffCommand::On, &nobody));
    }

    #[test]
    fn a_rule_that_commands_a_flagged_device_is_risky() {
        // « jamais », sensible, ou hors de la liste du jeton : l'appelant les réunit dans `flagged`.
        let rules = set(vec![rule(
            1,
            switch(1, false),
            vec![RuleAction::SetOnOff {
                node_id: 7,
                command: OnOffCommand::Off,
            }],
        )]);
        assert!(!risky_from(&rules, 1, OnOffCommand::Off, &nobody));
        assert!(risky_from(&rules, 1, OnOffCommand::Off, &|id| id == 7));
    }

    #[test]
    fn a_mode_start_follows_the_rules_that_wait_for_that_mode() {
        let rules = set(vec![
            rule(
                1,
                Trigger::Mode {
                    to: HomeMode::Vacances,
                },
                vec![webhook()],
            ),
            rule(
                2,
                Trigger::Mode {
                    to: HomeMode::Present,
                },
                vec![RuleAction::SetOnOff {
                    node_id: 4,
                    command: OnOffCommand::On,
                }],
            ),
            rule(3, switch(4, true), vec![RuleAction::Unknown]),
        ]);
        let vacances = walk(&rules, &[Start::Mode(HomeMode::Vacances)], &nobody);
        assert_eq!((vacances.fired, vacances.risky), (vec![1], vec![1]));
        // Présent allume 4, dont la règle 3 (inconnue) part à son tour.
        let present = walk(&rules, &[Start::Mode(HomeMode::Present)], &nobody);
        assert_eq!((present.fired, present.risky), (vec![2, 3], vec![3]));
        let nuit = walk(&rules, &[Start::Mode(HomeMode::Nuit)], &nobody);
        assert_eq!(nuit, Walk::default());
    }

    #[test]
    fn a_cover_command_is_parsed_strictly() {
        let args = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(
            parse_cover(&args(json!({"command": "Open"}))),
            Ok(CoverCommand::Open)
        );
        assert_eq!(
            parse_cover(&args(json!({"position": 30}))),
            Ok(CoverCommand::Position(30))
        );
        for bad in [
            json!({}),
            json!({"command": "Toggle"}),
            json!({"command": "open"}),
            json!({"command": "Open", "position": 10}),
            json!({"position": 101}),
            json!({"position": -1}),
            json!({"position": 10.5}),
            json!({"position": "10"}),
        ] {
            assert!(parse_cover(&args(bad.clone())).is_err(), "{bad}");
        }
    }
}
