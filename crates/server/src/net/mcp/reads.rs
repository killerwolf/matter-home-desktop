//! Les autres outils de lecture (étape E1b) : pièces, historique, automatisations, exécutions de
//! règles, caméras. Même règle que `tools.rs` : chaque objet est construit **à la main**, champ par
//! champ ; le texte qui vient de la maison passe par `name_of` ; jamais le `Snapshot` ni une règle
//! sérialisés tels quels (une règle porte des adresses de webhook, le texte de ses notifications).
//!
//! Ce qui ne sort jamais : une adresse de webhook ou son corps, le titre et le message d'une
//! notification, le compte rendu d'une exécution (il peut citer l'hôte d'un webhook, l'erreur de
//! `curl`, ou le mode demandé : « mode : Nuit »), une adresse, un flux ou une image de caméra, des
//! coordonnées de plan.

use protocol::automation::{Action, Condition, Rule, Trigger};
use protocol::events::MeasureKind;
use protocol::mode::HomeMode;
use protocol::wire::{
    self, parse_id, CameraBehavior, CameraPlacement, CameraStatus, HistoryQuery, Metric, Snapshot,
};
use serde_json::{json, Map, Value};

use super::tools::{name_of, only_keys, ToolError};
use crate::hub::Hub;

/// Pièces au plus dans la réponse (tous étages).
const MAX_ROOMS: usize = 200;
/// Automatisations au plus dans la réponse.
const MAX_RULES: usize = 100;
/// Exécutions au plus dans la réponse, et par défaut.
const MAX_RUNS: usize = 50;
const DEFAULT_RUNS: usize = 20;
/// Points d'historique au plus par appel (au-delà : les plus récents, `truncated`).
pub const MAX_HISTORY_POINTS: usize = 500;
/// Tranches de temps au plus, et par défaut.
const MAX_BUCKETS: usize = 48;
const DEFAULT_BUCKETS: usize = 24;
/// Fenêtre par défaut de l'historique, en secondes.
const DEFAULT_WINDOW_S: f64 = 24.0 * 3600.0;

// ---------------------------------------------------------------------------
// Pièces
// ---------------------------------------------------------------------------

pub(super) fn list_rooms(hub: &Hub) -> Value {
    let snapshot = hub.current_snapshot();
    let plan = &snapshot.plan.0;
    let clean = hub.push_cleaner();
    // Nombre d'appareils par (étage, pièce) : la même règle que partout (`HomePlan::room_of`).
    let mut counts: std::collections::HashMap<(u64, u64), usize> = Default::default();
    let mut unplaced = 0;
    for view in &snapshot.devices {
        match plan.room_of(view.device.node_id) {
            Some((floor, room)) => *counts.entry((floor.id, room.id)).or_default() += 1,
            None => unplaced += 1,
        }
    }
    let mut budget = MAX_ROOMS;
    let mut truncated = false;
    let floors: Vec<Value> = plan
        .floors
        .iter()
        .map(|floor| {
            let rooms: Vec<Value> = floor
                .rooms
                .iter()
                .filter(|_| {
                    if budget == 0 {
                        truncated = true;
                        false
                    } else {
                        budget -= 1;
                        true
                    }
                })
                .map(|room| {
                    json!({
                        "id": room.id.to_string(),
                        "name": name_of(&clean, &room.name),
                        "device_count": counts.get(&(floor.id, room.id)).copied().unwrap_or(0),
                    })
                })
                .collect();
            json!({
                "id": floor.id.to_string(),
                "name": name_of(&clean, &floor.name),
                "rooms": rooms,
            })
        })
        .collect();
    json!({
        "floors": floors,
        "devices_without_room": unplaced,
        "truncated": truncated,
    })
}

// ---------------------------------------------------------------------------
// Historique
// ---------------------------------------------------------------------------

pub(super) fn history_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "Identifiant de l'appareil, en texte décimal."},
            "metrics": {
                "type": "array",
                "items": {"type": "string", "enum": Metric::ALL.map(Metric::key)},
                "maxItems": Metric::ALL.len(),
                "description": "Grandeurs voulues ; toutes par défaut."
            },
            "since": {"type": "number", "description": "Début, en secondes Unix. Par défaut : 24 h avant `until`."},
            "until": {"type": "number", "description": "Fin, en secondes Unix. Par défaut : maintenant."},
            "buckets": {"type": "integer", "minimum": 1, "maximum": MAX_BUCKETS, "description": "Nombre de tranches de temps."}
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

/// Un nombre fini de secondes Unix, ou une erreur d'argument.
fn seconds(arguments: &Map<String, Value>, key: &str) -> Result<Option<f64>, ToolError> {
    match arguments.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(Some)
            .ok_or_else(|| {
                ToolError::invalid(format!("{key} doit être un nombre de secondes Unix."))
            }),
    }
}

pub(super) fn get_history(hub: &Hub, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    only_keys(arguments, &["id", "metrics", "since", "until", "buckets"])?;
    let Some(Value::String(text)) = arguments.get("id") else {
        return Err(ToolError::invalid(
            "id est obligatoire : l'identifiant de l'appareil, en texte décimal.",
        ));
    };
    let id = parse_id(text).map_err(|_| {
        ToolError::invalid("id : identifiant invalide (texte décimal, sans zéro initial).")
    })?;
    let metrics = match arguments.get("metrics") {
        None => None,
        Some(Value::Array(items)) if items.len() <= Metric::ALL.len() => {
            let mut keys: Vec<&'static str> = Vec::new();
            for item in items {
                let metric = item
                    .as_str()
                    .and_then(Metric::from_key)
                    .ok_or_else(|| ToolError::invalid("metrics : grandeur inconnue."))?;
                if !keys.contains(&metric.key()) {
                    keys.push(metric.key());
                }
            }
            (!keys.is_empty()).then(|| keys.join(","))
        }
        Some(_) => {
            return Err(ToolError::invalid(
                "metrics doit être une liste de grandeurs connues.",
            ))
        }
    };
    let buckets = match arguments.get("buckets") {
        None => DEFAULT_BUCKETS,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_BUCKETS as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| {
                ToolError::invalid(format!("buckets doit être un entier de 1 à {MAX_BUCKETS}."))
            })?,
    };
    let until = seconds(arguments, "until")?.unwrap_or_else(crate::events::now_unix);
    let since = seconds(arguments, "since")?.unwrap_or(until - DEFAULT_WINDOW_S);
    if since > until {
        return Err(ToolError::invalid("since doit précéder until."));
    }
    let query = HistoryQuery {
        since: Some(since),
        until: Some(until),
        metrics,
        limit: Some(MAX_HISTORY_POINTS),
        buckets: Some(buckets),
    };
    let response = hub.history(id, &query).map_err(|e| match e.code {
        wire::ErrorCode::NotFound => ToolError::new("not_found", "Appareil introuvable."),
        wire::ErrorCode::BadRequest => ToolError::invalid("Fenêtre de temps invalide."),
        _ => ToolError::new("unavailable", "Historique indisponible pour l'instant."),
    })?;
    // `limit` s'applique à chaque grandeur : le total est borné ici (les plus récents restent).
    let mut points = response.points;
    let mut truncated = response.truncated;
    if points.len() > MAX_HISTORY_POINTS {
        truncated = true;
        points.drain(..points.len() - MAX_HISTORY_POINTS);
    }
    Ok(json!({
        "id": id.to_string(),
        "since": response.since,
        "until": response.until,
        "truncated": truncated,
        "points": points
            .iter()
            .map(|p| json!({"at": p.at, "metric": p.metric.key(), "value": p.value}))
            .collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Automatisations
// ---------------------------------------------------------------------------

/// Le nom d'un appareil pour une phrase : nettoyé, ou « un appareil » s'il n'existe plus.
fn device_label(snapshot: &Snapshot, clean: &dyn Fn(&str) -> String, node_id: u64) -> String {
    snapshot
        .devices
        .iter()
        .find(|v| v.device.node_id == node_id)
        .map_or_else(
            || "un appareil".to_string(),
            |v| name_of(clean, &v.device.name),
        )
}

fn measure_word(measure: MeasureKind) -> &'static str {
    match measure {
        MeasureKind::Temperature => "la température",
        MeasureKind::Humidity => "l'humidité",
        MeasureKind::Power => "la puissance",
        MeasureKind::Energy => "l'énergie",
        MeasureKind::Battery => "la pile",
        MeasureKind::Position => "l'ouverture",
    }
}

/// « Quand… » : une phrase fermée, sans texte libre de la règle. Les modes ne sont pas nommés : une
/// règle « quand le mode devient Vacances » existe, elle ne dit pas dans quel mode on est.
fn trigger_phrase(
    trigger: &Trigger,
    snapshot: &Snapshot,
    clean: &dyn Fn(&str) -> String,
) -> String {
    let device = |id| device_label(snapshot, clean, id);
    match trigger {
        Trigger::Contact { node_id, open } => format!(
            "{} {}",
            device(*node_id),
            if *open { "s'ouvre" } else { "se ferme" }
        ),
        Trigger::Threshold {
            node_id,
            measure,
            above,
            value,
            ..
        } => format!(
            "{} : {} passe {} {value}",
            device(*node_id),
            measure_word(*measure),
            if *above { "au-dessus de" } else { "sous" }
        ),
        Trigger::Switch { node_id, on } => format!(
            "{} {}",
            device(*node_id),
            if *on { "s'allume" } else { "s'éteint" }
        ),
        Trigger::Unreachable { node_id, for_s } => {
            format!("{} reste injoignable {for_s} s", device(*node_id))
        }
        Trigger::Time { hour, minute, .. } => format!("il est {hour:02}:{minute:02}"),
        Trigger::Mode { .. } => "le mode de la maison change".to_string(),
        Trigger::Detection { .. } => "une caméra détecte quelqu'un ou quelque chose".to_string(),
        Trigger::Unknown => "un déclencheur inconnu de ce serveur".to_string(),
    }
}

fn action_word(action: &Action) -> &'static str {
    match action {
        Action::Notify { .. } => "notifier",
        Action::SetOnOff { .. } => "commander une prise ou une lampe",
        Action::SetCover { .. } => "commander un volet",
        Action::Webhook { .. } => "appeler un webhook",
        Action::Delay { .. } => "attendre",
        Action::SetMode { .. } => "changer le mode de la maison",
        Action::Unknown => "une action inconnue de ce serveur",
    }
}

/// La règle touche-t-elle au mode ou à la détection (donc à l'occupation de la maison) ?
fn touches_occupation(rule: &Rule) -> bool {
    matches!(
        rule.trigger,
        Trigger::Mode { .. } | Trigger::Detection { .. }
    ) || rule
        .conditions
        .iter()
        .any(|c| matches!(c, Condition::Mode { .. }))
        || rule
            .actions
            .iter()
            .any(|a| matches!(a, Action::SetMode { .. }))
}

pub(super) fn list_automations(hub: &Hub) -> Value {
    let snapshot = hub.current_snapshot();
    let clean = hub.push_cleaner();
    let rules = hub.rules();
    let shown: Vec<Value> = rules
        .rules
        .iter()
        .take(MAX_RULES)
        .map(|rule| {
            let mut actions: Vec<&str> = Vec::new();
            for word in rule.actions.iter().map(action_word) {
                if !actions.contains(&word) {
                    actions.push(word);
                }
            }
            let then = if actions.is_empty() {
                "aucune action".to_string()
            } else {
                actions.join(", ")
            };
            let conditions = match rule.conditions.len() {
                0 => String::new(),
                1 => ", sous 1 condition".to_string(),
                n => format!(", sous {n} conditions"),
            };
            json!({
                "id": rule.id.to_string(),
                "name": name_of(&clean, &rule.name),
                "enabled": rule.enabled,
                "summary": format!(
                    "Quand {}{conditions} : {then}.",
                    trigger_phrase(&rule.trigger, &snapshot, &clean)
                ),
            })
        })
        .collect();
    json!({
        "automations": shown,
        "total": rules.rules.len(),
        "truncated": rules.rules.len() > MAX_RULES,
    })
}

pub(super) fn runs_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": MAX_RUNS}},
        "additionalProperties": false
    })
}

pub(super) fn list_runs(
    hub: &Hub,
    arguments: &Map<String, Value>,
    occupation: bool,
) -> Result<Value, ToolError> {
    only_keys(arguments, &["limit"])?;
    let limit = match arguments.get("limit") {
        None => DEFAULT_RUNS,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_RUNS as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| {
                ToolError::invalid(format!("limit doit être un entier de 1 à {MAX_RUNS}."))
            })?,
    };
    let rules = hub.rules();
    let clean = hub.push_cleaner();
    let mut runs = hub.rule_runs();
    runs.sort_by(|a, b| b.at.total_cmp(&a.at));
    // Sans le droit « occupation », une exécution d'une règle de mode ou de détection (ou d'une
    // règle qui n'existe plus : on ne sait pas la classer) n'est pas montrée : sa date dit que le
    // mode vient de changer.
    let shown: Vec<Value> = runs
        .iter()
        .filter(|run| {
            occupation
                || rules
                    .rules
                    .iter()
                    .find(|r| r.id == run.rule_id)
                    .is_some_and(|r| !touches_occupation(r))
        })
        .take(limit)
        .map(|run| {
            json!({
                "rule_id": run.rule_id.to_string(),
                "name": name_of(&clean, &run.name),
                "at": run.at,
                "ok": run.ok,
            })
        })
        .collect();
    Ok(json!({"runs": shown}))
}

// ---------------------------------------------------------------------------
// Caméras
// ---------------------------------------------------------------------------

fn placement_word(placement: CameraPlacement) -> &'static str {
    match placement {
        CameraPlacement::Interieure => "indoor",
        CameraPlacement::Exterieure => "outdoor",
    }
}

/// Ce que fait une caméra, en un mot fermé.
pub(super) fn behavior_word(behavior: CameraBehavior) -> &'static str {
    match behavior {
        CameraBehavior::Eteinte => "off",
        CameraBehavior::Diffusion => "live",
        CameraBehavior::Surveillance => "surveillance",
    }
}

/// Une caméra est « joignable » sauf si son dernier état est « injoignable ». Éteinte par le mode
/// ou pas encore vérifiée, elle compte comme joignable : sinon l'agent verrait quelles caméras sont
/// éteintes, donc le mode, sans le droit « occupation ».
fn reachable(status: &CameraStatus) -> bool {
    !matches!(status, CameraStatus::Unreachable { .. })
}

pub(super) fn list_cameras(hub: &Hub, occupation: bool) -> Value {
    let snapshot = hub.current_snapshot();
    let clean = hub.push_cleaner();
    let mode: Option<HomeMode> = snapshot.mode;
    let cameras: Vec<Value> = snapshot
        .cameras
        .iter()
        .map(|c| {
            let mut out = json!({
                "id": c.id.to_string(),
                "name": name_of(&clean, &c.name),
                "placement": placement_word(c.emplacement),
                "reachable": reachable(&c.status),
            });
            if occupation {
                if let Some(mode) = mode {
                    out["behavior_now"] = json!(behavior_word(c.modes.get(mode)));
                }
            }
            out
        })
        .collect();
    json!({"cameras": cameras})
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::home::HomePlan;

    #[test]
    fn a_camera_switched_off_or_unchecked_reads_as_reachable_so_the_mode_stays_hidden() {
        assert!(reachable(&CameraStatus::Online));
        assert!(reachable(&CameraStatus::Off));
        assert!(reachable(&CameraStatus::Unknown));
        assert!(!reachable(&CameraStatus::Unreachable {
            problem: wire::CameraProblem::Unreachable,
            detail: None,
            since: None,
        }));
    }

    #[test]
    fn behaviors_are_closed_words() {
        assert_eq!(behavior_word(CameraBehavior::Eteinte), "off");
        assert_eq!(behavior_word(CameraBehavior::Diffusion), "live");
        assert_eq!(behavior_word(CameraBehavior::Surveillance), "surveillance");
        assert_eq!(placement_word(CameraPlacement::Interieure), "indoor");
    }

    fn rule(trigger: Trigger, conditions: Vec<Condition>, actions: Vec<Action>) -> Rule {
        Rule {
            id: 1,
            name: "r".into(),
            enabled: true,
            trigger,
            conditions,
            actions,
            cooldown_s: 60,
        }
    }

    #[test]
    fn rules_that_touch_the_mode_or_the_detection_are_occupation_rules() {
        let notify = || Action::Notify {
            title: "t".into(),
            message: "m".into(),
        };
        let time = || Trigger::Time {
            hour: 8,
            minute: 0,
            days: [true; 7],
        };
        assert!(!touches_occupation(&rule(time(), vec![], vec![notify()])));
        assert!(touches_occupation(&rule(
            Trigger::Mode {
                to: HomeMode::Vacances
            },
            vec![],
            vec![notify()]
        )));
        assert!(touches_occupation(&rule(
            time(),
            vec![Condition::Mode {
                is: vec![HomeMode::Absent]
            }],
            vec![notify()]
        )));
        assert!(touches_occupation(&rule(
            time(),
            vec![],
            vec![Action::SetMode {
                mode: HomeMode::Nuit
            }]
        )));
        assert!(touches_occupation(&rule(
            Trigger::Detection {
                camera_id: None,
                classes: vec![]
            },
            vec![],
            vec![notify()]
        )));
    }

    #[test]
    fn history_is_smoothed_and_never_more_than_500_points_whatever_the_metrics() {
        use crate::events::{Event, NodeInfo};
        use crate::history_store::{HistoryStore, Record};
        let dir = crate::hub::tests::temp("mcp-hist");
        let (hub, _rx) = crate::hub::tests::hub_with(&dir, 16);
        hub.apply(&Event::Node(NodeInfo {
            node_id: 7,
            name: "Capteur".into(),
            ..Default::default()
        }));
        let now = crate::events::now_unix();
        let mut store = HistoryStore::new(dir.join("history"));
        // Les neuf grandeurs, une mesure toutes les 30 s sur deux heures, en dents de scie : chaque
        // tranche garde son minimum et son maximum, soit près de 100 points par grandeur.
        for metric in Metric::ALL {
            for i in 0..240 {
                let record = Record {
                    t: now - 7_200.0 + f64::from(i) * 30.0,
                    k: metric.key().into(),
                    v: f64::from(i % 7),
                };
                store.append(7, &record).unwrap();
            }
        }
        let args = |v: Value| v.as_object().unwrap().clone();
        let since = now - 7_300.0;
        let all = get_history(
            &hub,
            &args(json!({"id": "7", "buckets": 48, "since": since})),
        )
        .unwrap();
        assert_eq!(
            all["truncated"], true,
            "neuf grandeurs dépassent 500 points"
        );
        let points = all["points"].as_array().unwrap();
        assert_eq!(points.len(), MAX_HISTORY_POINTS);
        assert!(points
            .iter()
            .all(|p| Metric::from_key(p["metric"].as_str().unwrap()).is_some()));
        // Une grandeur : peu de points, rien de tronqué.
        let one = get_history(
            &hub,
            &args(json!({"id": "7", "metrics": ["temperature"], "buckets": 12, "since": since})),
        )
        .unwrap();
        assert_eq!(one["truncated"], false);
        let n = one["points"].as_array().unwrap().len();
        assert!((2..=2 * 12 + 2).contains(&n), "{n}");
        assert!(one["points"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["metric"] == "temperature"));
        // Les erreurs d'argument et l'appareil inconnu.
        for (bad, code) in [
            (
                json!({"id": "7", "metrics": ["inconnue"]}),
                "invalid_argument",
            ),
            (
                json!({"id": "7", "metrics": "temperature"}),
                "invalid_argument",
            ),
            (json!({"id": "7", "buckets": 49}), "invalid_argument"),
            (json!({"id": "7", "buckets": 0}), "invalid_argument"),
            (
                json!({"id": "7", "since": 10.0, "until": 5.0}),
                "invalid_argument",
            ),
            (json!({"id": "7", "since": "hier"}), "invalid_argument"),
            (json!({"id": "7", "since": -1}), "invalid_argument"),
            (json!({"id": "7", "extra": 1}), "invalid_argument"),
            (json!({"id": 7}), "invalid_argument"),
            (json!({"id": "8"}), "not_found"),
        ] {
            let err = get_history(&hub, &args(bad.clone())).unwrap_err();
            assert_eq!(err.code, code, "{bad}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_trigger_phrase_never_names_a_mode_nor_carries_free_text() {
        let snapshot = Snapshot {
            seq: 0,
            server_time: 0.0,
            devices: vec![],
            plan: wire::TextIds(HomePlan::default()),
            rules: wire::TextIds(Default::default()),
            rule_runs: vec![],
            pairing_busy: false,
            cameras: vec![],
            mode: None,
        };
        let clean = |t: &str| t.to_string();
        let phrase = trigger_phrase(
            &Trigger::Mode {
                to: HomeMode::Vacances,
            },
            &snapshot,
            &clean,
        );
        assert_eq!(phrase, "le mode de la maison change");
        assert!(!phrase.to_lowercase().contains("vacances"));
        assert_eq!(
            trigger_phrase(
                &Trigger::Contact {
                    node_id: 7,
                    open: true
                },
                &snapshot,
                &clean
            ),
            "un appareil s'ouvre"
        );
    }
}
