//! `ostal_set_mode` (étape E3) : changer le mode de la maison, avec le droit « mode » (qui suppose
//! « occupation » : pour changer le mode, il faut le connaître, et la réponse le dit).
//!
//! **Une confirmation est décidée par l'effet réel**, celui du tableau de **chaque caméra**
//! (`CameraView.modes`), pas par une liste de modes :
//!
//! 1. une caméra qui surveille cesserait de surveiller (elle passe en `Eteinte` ou `Diffusion`) :
//!    on se désarme ;
//! 2. une caméra **intérieure** se mettrait à surveiller ou à diffuser alors que quelqu'un est à la
//!    maison, **ou que la présence est inconnue** (box injoignable, personne n'a rien déclaré) ;
//! 3. le mode demandé est `vacances`, toujours ;
//! 4. le changement fait partir des règles « quand le mode devient… » dont une action appelle une
//!    adresse web, change le mode, est inconnue, ou commande un appareil sensible ou « jamais ».
//!
//! Avec les réglages d'origine, le passage Nuit → Présent en demande donc une aussi (les caméras
//! extérieures quittent la surveillance) : c'est le prix de la règle « par effet ».
//!
//! Le même mode que l'actuel : rien ne change, rien n'est demandé. Le changement lui-même se fait
//! **sous le verrou du mode** (`Hub::set_mode_if`) : si le mode a bougé depuis le calcul de l'effet,
//! on ne change rien (l'effet approuvé n'est plus celui qui se produirait).

use protocol::automation::RuleSet;
use protocol::mode::{HomeMode, ModeSource};
use protocol::wire::{CameraBehavior, CameraPlacement, CameraView};
use serde_json::{json, Map, Value};

use super::commands::{rule_names, walk, Start};
use super::confirm::{confirmation_arg, gate, id_property, Gate, Plan, Reason};
use super::occupation::mode_json;
use super::reads::behavior_word;
use super::tools::{name_of, only_keys, Ctx, ToolError};
use crate::audit::{ModeCause, Origin};
use crate::presence::Whereabouts;
use crate::rights::Capability;
use crate::wifi::Link;

pub(super) const SET_MODE: &str = "ostal_set_mode";

pub(super) fn set_mode_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mode": {"type": "string", "enum": ["present", "absent", "nuit", "vacances"], "description": "Le mode voulu."},
            "confirmation_id": id_property()
        },
        "required": ["mode"],
        "additionalProperties": false
    })
}

/// Ce que ce changement de mode ferait.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Effect {
    pub reasons: Vec<Reason>,
    /// Les règles qui partiraient (identifiants).
    pub fired: Vec<u64>,
}

/// L'effet du passage de `from` à `to`, d'après le tableau réel de chaque caméra.
/// `someone_home` : quelqu'un est (ou peut être) à la maison. `flagged` : un appareil sensible ou
/// « jamais » (les appareils hors de la liste « permis » du jeton n'y sont pas : c'est « sensible
/// ou jamais » que la spécification demande, pas « non permis »).
pub(super) fn effect(
    cameras: &[CameraView],
    from: HomeMode,
    to: HomeMode,
    someone_home: bool,
    rules: &RuleSet,
    flagged: &dyn Fn(u64) -> bool,
) -> Effect {
    let mut reasons = Vec::new();
    let mut disarm = false;
    let mut privacy = false;
    for camera in cameras {
        let (before, after) = (camera.modes.get(from), camera.modes.get(to));
        if before == CameraBehavior::Surveillance && after != CameraBehavior::Surveillance {
            disarm = true;
        }
        if camera.emplacement == CameraPlacement::Interieure
            && after != before
            && matches!(
                after,
                CameraBehavior::Surveillance | CameraBehavior::Diffusion
            )
            && someone_home
        {
            privacy = true;
        }
    }
    if disarm {
        reasons.push(Reason::Disarm);
    }
    if privacy {
        reasons.push(Reason::Privacy);
    }
    if to == HomeMode::Vacances {
        reasons.push(Reason::Vacances);
    }
    let walked = walk(rules, &[Start::Mode(to)], flagged);
    if !walked.risky.is_empty() {
        reasons.push(Reason::Rules);
    }
    Effect {
        reasons,
        fired: walked.fired,
    }
}

/// Quelqu'un est-il à la maison ? **Oui** dès qu'une personne active est déclarée là, et **oui
/// aussi** quand l'état d'une personne active est inconnu (rien de déclaré) ou peut être périmé :
/// une box Wi-Fi liée qui ne répond plus (injoignable ou refusée) fige les dernières déclarations
/// « absent », alors la personne a pu rentrer. Dans le doute, on suppose quelqu'un.
fn someone_home(ctx: &Ctx<'_>) -> bool {
    let active: Vec<String> = ctx
        .accounts
        .users()
        .iter()
        .filter(|u| u.active)
        .map(|u| u.id.clone())
        .collect();
    someone_home_in(
        box_is_silent(ctx.hub.wifi()),
        &ctx.hub.presence_of(ctx.accounts),
        &active,
    )
}

/// Une box Wi-Fi est liée mais ne répond plus : ses déclarations « absent » sont figées.
fn box_is_silent(wifi: &crate::wifi::Wifi) -> bool {
    let status = wifi.status();
    status.linked && matches!(status.link, Link::Unreachable | Link::Refused)
}

fn someone_home_in(
    box_silent: bool,
    declared: &[crate::presence::Declared],
    active: &[String],
) -> bool {
    box_silent
        || declared.iter().any(|d| d.state == Whereabouts::Home)
        || active
            .iter()
            .any(|id| !declared.iter().any(|d| &d.user_id == id))
}

pub(super) fn set_mode(ctx: &Ctx<'_>, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    only_keys(arguments, &["mode", "confirmation_id"])?;
    let Some(to) = arguments
        .get("mode")
        .and_then(Value::as_str)
        .and_then(HomeMode::from_key)
    else {
        return Err(ToolError::invalid(
            "mode doit valoir present, absent, nuit ou vacances.",
        ));
    };
    let confirmation = confirmation_arg(arguments)?;
    let from = ctx.hub.mode();
    let snapshot = ctx.hub.current_snapshot();
    let clean = ctx.hub.push_cleaner();
    if from == to {
        // Rien ne change : ni demande, ni ligne « mode changé ».
        if confirmation.is_some() {
            // Un identifiant donné est toujours vérifié ; ici l'empreinte ne peut plus convenir.
            let nothing = Plan {
                tool: "set_mode",
                capability: Capability::ChangeMode,
                text: String::new(),
                key: format!("set_mode|{}|{}", from.key(), to.key()),
                reasons: Vec::new(),
                rules: Vec::new(),
                tell_agent: true,
            };
            if let Gate::Wait(answer) = gate(ctx, &nothing, confirmation.as_deref())? {
                return Ok(answer);
            }
        }
        // La réponse dit le mode et ce que fait chaque caméra : c'est une lecture d'« occupation »,
        // écrite une par une comme les autres (ce n'est ni une lecture ordinaire ni un changement).
        ctx.hub.audit().record(crate::agent_audit::occupation_entry(
            ctx.who,
            &ctx.agent.label,
            SET_MODE,
        ));
        return Ok(outcome(&snapshot, to, false, &[]));
    }
    let rules = ctx.hub.rules();
    let fx = effect(
        &snapshot.cameras,
        from,
        to,
        someone_home(ctx),
        &rules,
        &|id| ctx.agents.never_commanded(id) || ctx.agents.is_sensitive(id),
    );
    let fired = rule_names(&rules, &fx.fired, &clean);
    let ids: Vec<String> = fx.fired.iter().map(u64::to_string).collect();
    let plan = Plan {
        tool: "set_mode",
        capability: Capability::ChangeMode,
        text: format!(
            "Mettre la maison en mode {} (elle est en {}).",
            to.label(),
            from.label()
        ),
        key: format!("set_mode|{}|{}|{}", from.key(), to.key(), ids.join(",")),
        reasons: fx.reasons,
        rules: fired.clone(),
        tell_agent: true,
    };
    if let Gate::Wait(answer) = gate(ctx, &plan, confirmation.as_deref())? {
        return Ok(answer);
    }
    // Sous le verrou du mode : seulement si c'est encore `from` qu'on quitte.
    let cause = ModeCause {
        who: ctx.who.clone(),
        origin: Origin::Agent,
        detail: Some(name_of(&clean, &ctx.agent.label)),
    };
    let changed = ctx
        .hub
        .set_mode_if(
            |current| (current == from).then_some(to),
            ModeSource::Agent,
            cause,
        )
        .map_err(|_| {
            ToolError::new(
                "unavailable",
                "Changement de mode impossible pour l'instant.",
            )
        })?;
    match changed {
        Some(_) => Ok(outcome(&ctx.hub.current_snapshot(), to, true, &fired)),
        None if ctx.hub.mode() == to => Ok(outcome(&snapshot, to, false, &[])),
        None => Err(ToolError::new(
            "confirmation_invalid",
            "Le mode a changé entre-temps : refaites la demande.",
        )),
    }
}

fn outcome(
    snapshot: &protocol::wire::Snapshot,
    mode: HomeMode,
    changed: bool,
    rules: &[String],
) -> Value {
    let cameras: Vec<Value> = snapshot
        .cameras
        .iter()
        .map(|c| json!({"id": c.id.to_string(), "behavior_now": behavior_word(c.modes.get(mode))}))
        .collect();
    let mut out = json!({"mode": mode_json(mode), "changed": changed, "cameras": cameras});
    if changed {
        out["rules_triggered"] = json!(rules);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::automation::{Action as RuleAction, OnOffCommand, Rule, Trigger};
    use protocol::wire::{CameraModes, CameraStatus};

    fn camera(id: u64, placement: CameraPlacement) -> CameraView {
        CameraView {
            id,
            name: format!("Caméra {id}"),
            status: CameraStatus::Online,
            has_sub_stream: false,
            stream: None,
            emplacement: placement,
            modes: CameraModes::for_placement(placement),
        }
    }

    fn nobody(_: u64) -> bool {
        false
    }

    fn fx(cameras: &[CameraView], from: HomeMode, to: HomeMode, home: bool) -> Vec<Reason> {
        effect(cameras, from, to, home, &RuleSet::default(), &nobody).reasons
    }

    use HomeMode::{Absent, Nuit, Present, Vacances};

    /// Une extérieure et une intérieure, réglages d'origine.
    fn house() -> Vec<CameraView> {
        vec![
            camera(1, CameraPlacement::Exterieure),
            camera(2, CameraPlacement::Interieure),
        ]
    }

    #[test]
    fn a_silent_box_means_someone_may_be_home_and_the_indoor_camera_asks() {
        use crate::presence::{Declared, Source};
        use crate::wifi::bbox::{BoxApi, BoxError, Host};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        struct Flaky(Arc<AtomicBool>);
        impl BoxApi for Flaky {
            fn hosts(&self, _url: &str, _password: &str) -> Result<Vec<Host>, BoxError> {
                if self.0.load(Ordering::SeqCst) {
                    Err(BoxError::Unreachable)
                } else {
                    Ok(vec![Host {
                        name: "iPhone-de-Sam".into(),
                        active: true,
                    }])
                }
            }
        }
        let down = Arc::new(AtomicBool::new(false));
        let wifi = crate::wifi::Wifi::in_memory(Box::new(Flaky(down.clone())));
        // Pas de box liée : rien n'est silencieux.
        assert!(!box_is_silent(&wifi));
        wifi.link("http://box.test", "mot-de-passe").unwrap();
        assert!(!box_is_silent(&wifi));
        // La box se tait : le dernier relevé est figé.
        down.store(true, Ordering::SeqCst);
        wifi.observe(1000.0, true);
        assert!(box_is_silent(&wifi));

        // Sam, seule personne, est « absent » d'après le Wi-Fi.
        let away = [Declared {
            user_id: "sam".into(),
            state: Whereabouts::Away,
            since: 1.0,
            source: Source::Wifi,
            said: 0.0,
        }];
        let active = ["sam".to_string()];
        assert!(!someone_home_in(false, &away, &active));
        assert!(someone_home_in(true, &away, &active));
        // Une personne sans déclaration, ou déclarée là : quelqu'un.
        assert!(someone_home_in(false, &[], &active));
        assert!(!someone_home_in(false, &[], &[]));

        // Passer d'« présent » à « absent » avec une caméra intérieure en surveillance : on demande.
        let mut indoor = camera(2, CameraPlacement::Interieure);
        indoor.modes.set(Absent, CameraBehavior::Surveillance);
        let home = someone_home_in(true, &away, &active);
        let reasons = effect(
            &[indoor.clone()],
            Present,
            Absent,
            home,
            &RuleSet::default(),
            &nobody,
        )
        .reasons;
        assert!(reasons.contains(&Reason::Privacy), "{reasons:?}");
        let blind = effect(
            &[indoor],
            Present,
            Absent,
            false,
            &RuleSet::default(),
            &nobody,
        )
        .reasons;
        assert!(!blind.contains(&Reason::Privacy));
    }

    #[test]
    fn the_default_cameras_decide_which_changes_ask_for_a_person() {
        let cameras = house();
        // On s'arme : pas de confirmation.
        assert!(fx(&cameras, Present, Nuit, true).is_empty());
        // Le matin : les caméras extérieures quittent la surveillance (Nuit → Présent).
        assert_eq!(fx(&cameras, Nuit, Present, true), vec![Reason::Disarm]);
        // Départ avec quelqu'un à la maison : la caméra intérieure se mettrait à surveiller.
        assert_eq!(fx(&cameras, Present, Absent, true), vec![Reason::Privacy]);
        // Départ, plus personne : on s'arme, rien à demander.
        assert!(fx(&cameras, Present, Absent, false).is_empty());
        // Absent ou Vacances → Nuit : l'intérieur s'éteint (on se désarme).
        assert_eq!(fx(&cameras, Absent, Nuit, false), vec![Reason::Disarm]);
        // Retour : désarmement de l'intérieur et de l'extérieur.
        assert_eq!(fx(&cameras, Absent, Present, true), vec![Reason::Disarm]);
        // Vacances : toujours.
        assert_eq!(
            fx(&cameras, Present, Vacances, false),
            vec![Reason::Vacances]
        );
        assert!(fx(&cameras, Absent, Vacances, false).contains(&Reason::Vacances));
        // Sans caméra, seul Vacances demande.
        assert!(fx(&[], Nuit, Present, true).is_empty());
        assert_eq!(fx(&[], Present, Vacances, true), vec![Reason::Vacances]);
    }

    #[test]
    fn a_hand_tuned_camera_table_is_what_counts() {
        // Une extérieure réglée pour ne jamais surveiller : plus de désarmement le matin.
        let mut quiet = camera(1, CameraPlacement::Exterieure);
        quiet.modes.set(Nuit, CameraBehavior::Diffusion);
        quiet.modes.set(Absent, CameraBehavior::Diffusion);
        assert!(fx(&[quiet], Nuit, Present, true).is_empty());
        // Une intérieure réglée pour surveiller la nuit : Présent → Nuit, quelqu'un là : vie privée.
        let mut hall = camera(2, CameraPlacement::Interieure);
        hall.modes.set(Nuit, CameraBehavior::Surveillance);
        assert_eq!(
            fx(&[hall.clone()], Present, Nuit, true),
            vec![Reason::Privacy]
        );
        // Une intérieure qui diffuse (flux visible) compte aussi.
        hall.modes.set(Nuit, CameraBehavior::Diffusion);
        assert_eq!(fx(&[hall], Present, Nuit, true), vec![Reason::Privacy]);
        // Une extérieure ne compte jamais pour la vie privée.
        let mut garden = camera(3, CameraPlacement::Exterieure);
        garden.modes.set(Absent, CameraBehavior::Surveillance);
        garden.modes.set(Present, CameraBehavior::Eteinte);
        assert!(fx(&[garden], Present, Absent, true).is_empty());
    }

    #[test]
    fn presence_matters_only_for_indoor_cameras() {
        let cameras = house();
        assert_eq!(fx(&cameras, Present, Absent, true), vec![Reason::Privacy]);
        assert!(fx(&cameras, Present, Absent, false).is_empty());
    }

    fn mode_rule(id: u64, to: HomeMode, actions: Vec<RuleAction>) -> Rule {
        Rule {
            id,
            name: format!("r{id}"),
            enabled: true,
            trigger: Trigger::Mode { to },
            conditions: vec![],
            actions,
            cooldown_s: 60,
        }
    }

    #[test]
    fn rules_that_wait_for_the_mode_ask_when_they_do_what_only_a_person_decides() {
        let cameras: Vec<CameraView> = vec![];
        let ask = |rules: Vec<Rule>, flagged: &dyn Fn(u64) -> bool| {
            effect(
                &cameras,
                Present,
                Nuit,
                true,
                &RuleSet { version: 1, rules },
                flagged,
            )
        };
        let hook = RuleAction::Webhook {
            url: "https://exemple.invalid/x".into(),
            body: String::new(),
        };
        let lamp = RuleAction::SetOnOff {
            node_id: 5,
            command: OnOffCommand::Off,
        };
        // Une lampe ordinaire : la règle part, personne n'est dérangé.
        let calm = ask(vec![mode_rule(1, Nuit, vec![lamp.clone()])], &nobody);
        assert!(calm.reasons.is_empty());
        assert_eq!(calm.fired, vec![1]);
        // Une adresse web, un changement de mode, une action inconnue : on demande.
        for action in [
            hook,
            RuleAction::SetMode { mode: Absent },
            RuleAction::Unknown,
        ] {
            let asked = ask(vec![mode_rule(1, Nuit, vec![action])], &nobody);
            assert_eq!(asked.reasons, vec![Reason::Rules]);
        }
        // Un appareil sensible ou « jamais ».
        let guarded = ask(vec![mode_rule(1, Nuit, vec![lamp])], &|id| id == 5);
        assert_eq!(guarded.reasons, vec![Reason::Rules]);
        // Une règle d'un autre mode, ou désactivée : rien.
        let mut off = mode_rule(2, Nuit, vec![RuleAction::Unknown]);
        off.enabled = false;
        let none = ask(
            vec![mode_rule(1, Absent, vec![RuleAction::Unknown]), off],
            &nobody,
        );
        assert_eq!(none, Effect::default());
    }
}
