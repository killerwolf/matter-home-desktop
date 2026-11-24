//! Les commandes d'un agent d'IA (étape E2) : `ostal_set_on_off`, pour **les prises et les lampes
//! seulement**. Pas de volet, pas de mode, pas de `Toggle` (non idempotent : l'agent vise l'état
//! final, `On` ou `Off`), et **aucune confirmation** (elles arrivent à l'étape E3) : tout ce qui en
//! demanderait une est donc refusé.
//!
//! Ordre des contrôles, chacun ne révélant que ce qu'il doit :
//!
//! 1. arguments (`id`, `command` : `On` ou `Off`) ;
//! 2. l'appareil est dans la liste des appareils permis du jeton (`devices` dans `agents.json`,
//!    vide par défaut, 20 au plus) **avant** de chercher s'il existe : un agent ne sonde pas la
//!    maison ;
//! 3. l'appareil n'est pas dans la liste « jamais par un agent » (`never`). Les deux refus disent
//!    la même phrase ;
//! 4. l'appareil existe et est une prise ou une lampe ;
//! 5. aucune automatisation active déclenchée par cette commande (directement ou de proche en
//!    proche) ne change le mode, n'appelle une adresse web, ne commande un volet ou un appareil de
//!    la liste « jamais » : ce serait faire faire à l'agent ce qu'il n'a pas le droit de faire ;
//! 6. `Hub::set_on_off`, puis une ligne au journal.
//!
//! Le débit (20 commandes par minute et par jeton) est compté avant, dans `mod.rs`.

use std::collections::HashSet;

use protocol::automation::{Action as RuleAction, OnOffCommand, RuleSet, Trigger};
use protocol::wire::{parse_id, ErrorCode};
use serde_json::{json, Map, Value};

use super::tools::{name_of, only_keys, Ctx, ToolError};
use crate::audit::{Action, Entry, Origin};
use crate::security::agent_tokens::AgentStore;

pub(super) const SET_ON_OFF: &str = "ostal_set_on_off";

pub(super) fn set_on_off_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "Identifiant de la prise ou de la lampe, en texte décimal (ostal_list_devices)."},
            "command": {"type": "string", "enum": ["On", "Off"], "description": "L'état voulu. Pas d'« inverser » : dites On ou Off."}
        },
        "required": ["id", "command"],
        "additionalProperties": false
    })
}

const NOT_PERMITTED: &str = "Cet agent n'a pas le droit de commander cet appareil.";
const RULE_REFUSED: &str = "Cette commande ferait partir une automatisation que seule une personne peut déclencher (changer le mode, appeler une adresse web ou commander un volet). Faites-la depuis l'app.";

/// Refuse un appel : une ligne `agent_denied` (au plus une par minute, par agent et par motif) et
/// l'erreur rendue à l'agent. `reason` est un mot fermé.
fn refuse(ctx: &Ctx<'_>, reason: &'static str, message: &str) -> ToolError {
    let entry = crate::agent_audit::denied_entry(ctx.who, &ctx.agent.label, reason);
    ctx.hub.audit().record_throttled(
        &format!("agent-denied:{}:{reason}", ctx.agent.id),
        60.0,
        entry,
    );
    ToolError::new("forbidden", message)
}

pub(super) fn set_on_off(
    ctx: &Ctx<'_>,
    arguments: &Map<String, Value>,
) -> Result<Value, ToolError> {
    only_keys(arguments, &["id", "command"])?;
    let id = match arguments.get("id") {
        Some(Value::String(text)) => parse_id(text).map_err(|_| {
            ToolError::invalid("id : identifiant invalide (texte décimal, sans zéro initial).")
        })?,
        _ => {
            return Err(ToolError::invalid(
                "id est obligatoire : l'identifiant de l'appareil, en texte décimal.",
            ))
        }
    };
    let command = match arguments.get("command").and_then(Value::as_str) {
        Some("On") => OnOffCommand::On,
        Some("Off") => OnOffCommand::Off,
        _ => {
            return Err(ToolError::invalid(
                "command doit valoir On ou Off (pas d'inversion : visez l'état final).",
            ))
        }
    };
    // 2 et 3 : la même phrase, pour ne rien apprendre sur la maison.
    if !ctx.agent.may_command(id) || ctx.agents.never_commanded(id) {
        return Err(refuse(ctx, "device", NOT_PERMITTED));
    }
    // 4.
    let snapshot = ctx.hub.current_snapshot();
    let Some(view) = snapshot.devices.iter().find(|v| v.device.node_id == id) else {
        return Err(ToolError::new("not_found", "Appareil introuvable."));
    };
    if !is_plug_or_lamp(&view.device.caps) {
        return Err(ToolError::invalid(
            "Cet outil ne commande que les prises et les lampes.",
        ));
    }
    // 5.
    if triggers_risky_rule(&ctx.hub.rules(), id, command, ctx.agents) {
        return Err(refuse(ctx, "rule", RULE_REFUSED));
    }
    // 6.
    let result = ctx.hub.set_on_off(id, command);
    let name = {
        let clean = ctx.hub.push_cleaner();
        name_of(&clean, &view.device.name)
    };
    ctx.hub.audit().record(
        Entry::new(Action::DeviceCommand, ctx.who)
            .target(&name)
            .detail(match command {
                OnOffCommand::On => "on",
                _ => "off",
            })
            .source(Origin::Agent)
            .outcome(&result),
    );
    match result {
        Ok(()) => Ok(json!({
            "id": id.to_string(),
            "name": name,
            "command": if command == OnOffCommand::On { "On" } else { "Off" },
            "sent": true,
            "note": "La commande est envoyée. Relisez l'appareil (ostal_get_device) pour vérifier son état."
        })),
        Err(e) => Err(match e.code {
            ErrorCode::NotFound => ToolError::new("not_found", "Appareil introuvable."),
            ErrorCode::BadRequest => {
                ToolError::invalid("Cet outil ne commande que les prises et les lampes.")
            }
            _ => ToolError::new("unavailable", "Commande impossible pour l'instant."),
        }),
    }
}

/// Une prise ou une lampe : marche/arrêt, et pas un volet.
fn is_plug_or_lamp(caps: &protocol::events::Capabilities) -> bool {
    caps.on_off && !caps.cover
}

/// Cette commande fait-elle partir, directement ou de proche en proche, une automatisation active
/// dont une action change le mode, appelle une adresse web, commande un volet, est inconnue ou
/// commande un appareil de la liste « jamais » ?
///
/// - Pour l'appareil commandé, on tient compte du sens : `On` ne fait partir qu'un déclencheur
///   « s'allume », `Off` qu'un « s'éteint » ; un seuil (puissance…) part dans les deux sens.
/// - Pour les appareils que ces règles commandent à leur tour, on suppose les deux sens (on ne sait
///   pas ce qu'ils étaient avant).
/// - Les conditions sont ignorées : elles peuvent être vraies à ce moment-là.
/// - Les règles désactivées ne comptent pas : elles ne partiront pas. Le calcul est refait à chaque
///   commande.
fn triggers_risky_rule(
    rules: &RuleSet,
    node_id: u64,
    command: OnOffCommand,
    agents: &AgentStore,
) -> bool {
    risky_from(rules, node_id, command, &|id| agents.never_commanded(id))
}

fn risky_from(
    rules: &RuleSet,
    node_id: u64,
    command: OnOffCommand,
    never: &dyn Fn(u64) -> bool,
) -> bool {
    let mut seen: HashSet<u64> = HashSet::new();
    // (appareil, sens connu : `Some(true)` = s'allume, `Some(false)` = s'éteint, `None` = les deux)
    let mut queue: Vec<(u64, Option<bool>)> = vec![(
        node_id,
        match command {
            OnOffCommand::On => Some(true),
            OnOffCommand::Off => Some(false),
            OnOffCommand::Toggle => None,
        },
    )];
    seen.insert(node_id);
    while let Some((node, direction)) = queue.pop() {
        for rule in rules.rules.iter().filter(|r| r.enabled) {
            let fires = match &rule.trigger {
                Trigger::Switch { node_id, on } => {
                    *node_id == node && direction.is_none_or(|d| d == *on)
                }
                Trigger::Threshold { node_id, .. } => *node_id == node,
                _ => false,
            };
            if !fires {
                continue;
            }
            for action in &rule.actions {
                match action {
                    RuleAction::Notify { .. } | RuleAction::Delay { .. } => {}
                    RuleAction::SetOnOff { node_id, .. } => {
                        if never(*node_id) {
                            return true;
                        }
                        if seen.insert(*node_id) {
                            queue.push((*node_id, None));
                        }
                    }
                    // Le mode, une adresse web, un volet, une action inconnue.
                    _ => return true,
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::automation::{Condition, Rule};
    use protocol::mode::HomeMode;

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
    }

    #[test]
    fn mode_webhook_cover_and_unknown_actions_are_refused() {
        let cover = RuleAction::SetCover {
            node_id: 9,
            command: protocol::automation::CoverCommand::Open,
        };
        for action in [
            RuleAction::SetMode {
                mode: HomeMode::Present,
            },
            webhook(),
            cover,
            RuleAction::Unknown,
        ] {
            let rules = set(vec![rule(1, switch(1, true), vec![action])]);
            assert!(risky_from(&rules, 1, OnOffCommand::On, &nobody));
        }
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
        assert!(risky_from(&chain, 1, OnOffCommand::On, &nobody));
        // Une boucle sans effet dangereux se termine.
        let cycle = set(vec![
            rule(1, switch(1, true), vec![on_off(2)]),
            rule(2, switch(2, true), vec![on_off(1)]),
        ]);
        assert!(!risky_from(&cycle, 1, OnOffCommand::On, &nobody));
    }

    #[test]
    fn a_rule_that_commands_a_never_device_is_refused() {
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
}
