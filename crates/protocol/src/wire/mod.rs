//! Protocole de fil entre le client et le serveur (étape 3 du découpage).
//!
//! Ce n'est ni `Command` ni `Event` : ces types internes (`crate::events`) ne sont pas
//! sérialisés tels quels. Ici, tout ce qui circule sur le réseau, avec des règles fixes :
//!
//! - la version du protocole ([`PROTOCOL_VERSION`]) est annoncée à la connexion ;
//! - les identifiants sont du **texte** (décimal) : un `u64` dépasse 2^53 et serait
//!   arrondi par JavaScript. Dans ce crate ils restent des `u64` (attribut
//!   [`text_id`]) ; seul le JSON les écrit en chaînes ;
//! - les erreurs sont des codes stables ([`ErrorCode`]), jamais des phrases ; un texte
//!   humain éventuel est un détail à part, sans chemin local ;
//! - les noms de champs et de variantes sont en `snake_case`. Les types partagés avec les
//!   fichiers du plan et des règles (`Symbol`, `CoverCommand`, `OnOffCommand`) gardent la
//!   casse de ces fichiers.
//!
//! Ce crate ne contient que des types, du serde et des fonctions pures : aucune
//! dépendance à tokio, axum ou TLS, pour que le client le réutilise tel quel.

mod api;
mod camera;
mod convert;
mod error;
mod message;
mod text_ids;

pub use api::*;
pub use camera::*;
pub use error::*;
pub use message::*;
pub use text_ids::{as_text_ids, parse_id, text_id, IdError, TextIds};

/// Version du protocole. Elle change quand un message existant change de sens ou de forme ;
/// ajouter un champ optionnel ou un message que l'on peut ignorer ne la change pas.
///
/// - 2 : caméras RTSP (#4) : `cameras` dans l'état complet et l'événement `cameras_changed`.
///   Un client de version 1 ne connaît pas cet événement (les énumérations serde refusent les
///   variantes inconnues) : client et serveur sortent ensemble.
pub const PROTOCOL_VERSION: u32 = 2;

/// Fenêtre maximale d'une lecture d'historique (la rétention du serveur : 7 jours).
pub const HISTORY_MAX_WINDOW_S: u64 = 7 * 24 * 3600;
/// Nombre de mesures renvoyées par défaut, et maximum accepté, pour une lecture d'historique.
pub const HISTORY_DEFAULT_LIMIT: usize = 5_000;
pub const HISTORY_MAX_LIMIT: usize = 20_000;
/// Nombre maximal de tranches de temps (`buckets`) pour une lecture d'historique réduite.
pub const HISTORY_MAX_BUCKETS: usize = 2_000;
/// Nombre d'exécutions de règles gardées par le serveur (et envoyées avec l'état complet).
pub const RULE_RUNS_KEPT: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::{Action, Condition, CoverCommand, Rule, RuleSet, Trigger};
    use crate::events::{BridgeLink, Event, MeasureKind, NodeInfo, NodeStatus, StateKind};
    use crate::home::{HomePlan, Item, ItemKind};
    use serde_json::Value;

    /// Identifiant d'appareil ponté (plage réservée), bien au-delà de 2^53.
    const BRIDGED: u64 = 0xFFFF_FFF0_0000_0000 + 0xD3_0001;

    /// Aucun nombre entier supérieur à 2^53 ne doit rester dans le JSON.
    fn assert_no_big_numbers(value: &Value) {
        match value {
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    assert!(u <= 1 << 53, "entier trop grand dans le JSON : {u}");
                }
            }
            Value::Array(a) => a.iter().for_each(assert_no_big_numbers),
            Value::Object(o) => o.values().for_each(assert_no_big_numbers),
            _ => {}
        }
    }

    fn sample_rules() -> RuleSet {
        let mut rule = Rule::new(u64::MAX - 1);
        rule.trigger = Trigger::Contact {
            node_id: BRIDGED,
            open: true,
        };
        rule.conditions = vec![Condition::Switch {
            node_id: BRIDGED + 1,
            on: true,
        }];
        rule.actions = vec![Action::SetCover {
            node_id: BRIDGED + 2,
            command: CoverCommand::Position(40),
        }];
        RuleSet {
            version: 1,
            rules: vec![rule],
        }
    }

    fn sample_plan() -> HomePlan {
        let mut plan = HomePlan::default();
        plan.floors[0].items.push(Item {
            id: 3,
            kind: ItemKind::Device {
                node_id: BRIDGED,
                symbol: None,
            },
            x: 1.0,
            y: 2.0,
        });
        plan
    }

    #[test]
    fn ids_are_text_in_plan_and_rules() {
        let rules = serde_json::to_value(TextIds(sample_rules())).unwrap();
        assert_no_big_numbers(&rules);
        let rule = &rules["rules"][0];
        assert_eq!(rule["id"], Value::String((u64::MAX - 1).to_string()));
        assert_eq!(
            rule["trigger"]["node_id"],
            Value::String(BRIDGED.to_string())
        );
        assert_eq!(
            rule["actions"][0]["node_id"],
            Value::String((BRIDGED + 2).to_string())
        );
        let plan = serde_json::to_value(TextIds(sample_plan())).unwrap();
        assert_no_big_numbers(&plan);
        assert_eq!(
            plan["floors"][0]["items"][0]["kind"]["node_id"],
            Value::String(BRIDGED.to_string())
        );
        // Aller-retour sans perte.
        let back: TextIds<RuleSet> = serde_json::from_value(rules).unwrap();
        assert_eq!(back.0, sample_rules());
        let back: TextIds<HomePlan> = serde_json::from_value(plan).unwrap();
        assert_eq!(back.0, sample_plan());
        // Le format des fichiers, lui, ne change pas : identifiants numériques.
        let on_disk = serde_json::to_value(sample_rules()).unwrap();
        assert!(on_disk["rules"][0]["id"].is_u64());
    }

    #[test]
    fn numeric_or_malformed_ids_are_refused() {
        let mut v = serde_json::to_value(sample_rules()).unwrap();
        assert!(serde_json::from_value::<TextIds<RuleSet>>(v.clone()).is_err());
        v = serde_json::to_value(TextIds(sample_rules())).unwrap();
        v["rules"][0]["id"] = Value::String("007".into());
        assert!(serde_json::from_value::<TextIds<RuleSet>>(v.clone()).is_err());
        v["rules"][0]["id"] = Value::String("-1".into());
        assert!(serde_json::from_value::<TextIds<RuleSet>>(v.clone()).is_err());
        v["rules"][0]["id"] = Value::String("18446744073709551616".into());
        assert!(serde_json::from_value::<TextIds<RuleSet>>(v).is_err());
        assert_eq!(parse_id("0"), Ok(0));
        assert_eq!(parse_id("18446744073709551615"), Ok(u64::MAX));
        assert!(parse_id("").is_err());
        assert!(parse_id("+5").is_err());
    }

    fn info(id: u64) -> NodeInfo {
        NodeInfo {
            node_id: id,
            name: "Volet salon".into(),
            device_name: "Volet".into(),
            bridge: Some(BridgeLink {
                node_id: 0xD3_0005,
                endpoint: 3,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn events_round_trip_with_text_ids() {
        let clean = |s: &str| s.to_string();
        let events = vec![
            Event::Node(info(BRIDGED)),
            Event::NodeRemoved(BRIDGED),
            Event::Status {
                node_id: BRIDGED,
                status: NodeStatus::Unreachable {
                    error: "pas de réponse".into(),
                    retry_in_s: 20,
                },
            },
            Event::Measurement {
                node_id: BRIDGED,
                kind: MeasureKind::Position,
                value: 35.0,
                at: 10.5,
            },
            Event::State {
                node_id: BRIDGED,
                kind: StateKind::Open,
                value: true,
                at: 11.0,
            },
            Event::RuleRun {
                rule_id: u64::MAX,
                name: "Règle".into(),
                at: 1.0,
                ok: true,
                detail: "ok".into(),
            },
            Event::PairingDone {
                node_id: BRIDGED,
                name: "Capteur".into(),
            },
            Event::Notification {
                title: "t".into(),
                message: "m".into(),
                at: 2.0,
            },
        ];
        for event in events {
            let wire = WireEvent::from_event(&event, &clean).unwrap();
            let json = serde_json::to_value(&wire).unwrap();
            assert_no_big_numbers(&json);
            let back: WireEvent = serde_json::from_value(json).unwrap();
            assert_eq!(back, wire);
            assert_eq!(
                format!("{:?}", back.into_event().unwrap()),
                format!("{event:?}")
            );
        }
        assert!(WireEvent::from_event(&Event::Fatal("x".into()), &clean).is_none());
    }

    #[test]
    fn snapshot_and_messages_have_text_ids_only() {
        let snapshot = Snapshot {
            seq: 7,
            server_time: 1.0,
            devices: vec![DeviceView {
                device: (&info(BRIDGED)).into(),
                status: WireStatus::Online,
                last_seen: Some(1.0),
                mode: Some(WireMode::Polling { every_s: 30 }),
                latest: vec![LatestReading {
                    metric: Metric::Position,
                    value: 50.0,
                    at: 1.0,
                }],
                open_since: None,
                cover_motion: None,
            }],
            plan: TextIds(sample_plan()),
            rules: TextIds(sample_rules()),
            rule_runs: vec![RuleRunView {
                rule_id: u64::MAX,
                name: "r".into(),
                at: 1.0,
                ok: false,
                detail: "d".into(),
            }],
            pairing_busy: false,
            cameras: vec![CameraView {
                id: CAMERA_ID_MIN + 1,
                name: "Entrée".into(),
                status: CameraStatus::Online,
                has_sub_stream: true,
                stream: Some(StreamInfo {
                    codec: "h264".into(),
                    width: Some(1280),
                    height: Some(720),
                }),
            }],
        };
        let message = ServerMessage::Snapshot(Box::new(snapshot));
        let json = serde_json::to_value(&message).unwrap();
        assert_no_big_numbers(&json);
        assert_eq!(json["type"], "snapshot");
        assert_eq!(
            json["cameras"][0]["id"],
            Value::String((CAMERA_ID_MIN + 1).to_string())
        );
        assert_eq!(
            json["devices"][0]["device"]["node_id"],
            Value::String(BRIDGED.to_string())
        );
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, message);
        let hello = serde_json::to_value(ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            server_version: "0.1.0".into(),
        })
        .unwrap();
        assert_eq!(hello["type"], "hello");
    }

    /// Les codes d'erreur sont un contrat : un renommage doit casser ce test.
    #[test]
    fn error_codes_are_stable() {
        let codes = [
            (ErrorCode::Unauthorized, "unauthorized", 401),
            (ErrorCode::InvalidCredentials, "invalid_credentials", 401),
            (ErrorCode::TooManyAttempts, "too_many_attempts", 429),
            (ErrorCode::VersionIncompatible, "version_incompatible", 426),
            (ErrorCode::BadRequest, "bad_request", 400),
            (ErrorCode::PlanWithoutFloor, "plan_without_floor", 400),
            (ErrorCode::NotFound, "not_found", 404),
            (ErrorCode::PayloadTooLarge, "payload_too_large", 413),
            (ErrorCode::PairingBusy, "pairing_busy", 409),
            (ErrorCode::PairingFailed, "pairing_failed", 502),
            (ErrorCode::InvalidImport, "invalid_import", 400),
            (ErrorCode::NoData, "no_data", 404),
            (ErrorCode::DeviceUnreachable, "device_unreachable", 502),
            (ErrorCode::Unavailable, "unavailable", 503),
            (ErrorCode::Internal, "internal", 500),
        ];
        for (code, text, status) in codes {
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                Value::String(text.into())
            );
            assert_eq!(code.http_status(), status);
        }
        let body = serde_json::to_value(ErrorBody {
            error: ApiError::new(ErrorCode::NotFound),
        })
        .unwrap();
        assert_eq!(body, serde_json::json!({"error": {"code": "not_found"}}));
    }

    #[test]
    fn metric_keys_match_history_keys() {
        for m in Metric::ALL {
            assert_eq!(Metric::from_key(m.key()), Some(m));
            assert_eq!(
                serde_json::to_value(m).unwrap(),
                Value::String(m.key().into())
            );
        }
        assert_eq!(Metric::from_key("inconnu"), None);
    }

    #[test]
    fn history_query_buckets_round_trip_and_stay_compatible() {
        let q = HistoryQuery {
            since: Some(10.0),
            until: Some(20.0),
            metrics: Some("temperature".into()),
            limit: Some(5),
            buckets: Some(800),
        };
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(serde_json::from_str::<HistoryQuery>(&json).unwrap(), q);
        // Une requête d'un client rc.4 (sans `buckets`) reste valide : pas de réduction.
        let old: HistoryQuery =
            serde_json::from_str(r#"{"since":1.0,"until":2.0,"metrics":"temperature","limit":3}"#)
                .unwrap();
        assert_eq!(old.buckets, None);
        // Un serveur rc.4 (sans `buckets`) reçoit une requête qu'il comprend : la réponse garde
        // la même forme, avec ou sans réduction.
        let response = HistoryResponse {
            node_id: 7,
            since: 1.0,
            until: 2.0,
            truncated: false,
            points: vec![HistoryPoint {
                at: 1.5,
                metric: Metric::Temperature,
                value: 20.5,
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["node_id"], "7");
        assert_eq!(
            serde_json::from_value::<HistoryResponse>(json).unwrap(),
            response
        );
        const { assert!(HISTORY_MAX_BUCKETS >= 800 && HISTORY_MAX_BUCKETS * 2 + 2 <= HISTORY_MAX_LIMIT) };
    }

    #[test]
    fn secrets_never_appear_in_debug() {
        let login = LoginRequest {
            username: "hamdi".into(),
            password: "tres-secret".into(),
            protocol_version: 1,
        };
        assert!(!format!("{login:?}").contains("tres-secret"));
        let pair = PairRequest {
            code: "12345678901".into(),
        };
        assert!(!format!("{pair:?}").contains("12345678901"));
    }

    fn camera(id: u64, status: CameraStatus) -> CameraView {
        CameraView {
            id,
            name: "Salon".into(),
            status,
            has_sub_stream: false,
            stream: None,
        }
    }

    #[test]
    fn camera_ids_are_reserved_text_ids() {
        assert!(is_camera_id(CAMERA_ID_MIN) && is_camera_id(CAMERA_ID_MAX));
        assert!(!is_camera_id(CAMERA_ID_MIN - 1) && !is_camera_id(0) && !is_camera_id(42));
        const { assert!(CAMERA_ID_MIN > 1 << 53) };
        let view = camera(CAMERA_ID_MAX, CameraStatus::Unknown);
        let json = serde_json::to_value(&view).unwrap();
        assert_no_big_numbers(&json);
        assert_eq!(json["id"], Value::String(u64::MAX.to_string()));
        assert_eq!(serde_json::from_value::<CameraView>(json).unwrap(), view);
        // Un identifiant numérique est refusé, comme partout.
        let bad = serde_json::json!({"id": 5, "name": "x", "status": {"state": "unknown"}, "has_sub_stream": false});
        assert!(serde_json::from_value::<CameraView>(bad).is_err());
    }

    #[test]
    fn camera_events_and_statuses_round_trip() {
        let event = WireEvent::CamerasChanged {
            cameras: vec![
                camera(CAMERA_ID_MIN + 1, CameraStatus::Online),
                camera(
                    CAMERA_ID_MIN + 2,
                    CameraStatus::Unreachable {
                        problem: CameraProblem::Unauthorized,
                        detail: Some("identifiant refusé".into()),
                    },
                ),
            ],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_no_big_numbers(&json);
        assert_eq!(json["type"], "cameras_changed");
        assert_eq!(json["cameras"][1]["status"]["problem"], "unauthorized");
        assert_eq!(serde_json::from_value::<WireEvent>(json).unwrap(), event);
        // Ce n'est pas un événement du moteur : le client le traite à part.
        assert!(event.into_event().is_none());
    }

    /// Un client ne doit jamais pouvoir recevoir une adresse ou un secret : aucun champ des
    /// types de caméra n'a de quoi les porter. Ce test fige la liste des clés possibles ; en
    /// ajouter une oblige à relire cette règle.
    #[test]
    fn camera_view_has_no_field_for_an_address_or_a_secret() {
        let view = CameraView {
            id: CAMERA_ID_MIN,
            name: "n".into(),
            status: CameraStatus::Unreachable {
                problem: CameraProblem::Failed,
                detail: Some("d".into()),
            },
            has_sub_stream: true,
            stream: Some(StreamInfo {
                codec: "h264".into(),
                width: Some(1),
                height: Some(2),
            }),
        };
        let json = serde_json::to_value(&view).unwrap();
        let mut keys: Vec<String> = Vec::new();
        fn walk(v: &Value, keys: &mut Vec<String>) {
            match v {
                Value::Object(o) => {
                    for (k, v) in o {
                        keys.push(k.clone());
                        walk(v, keys);
                    }
                }
                Value::Array(a) => a.iter().for_each(|v| walk(v, keys)),
                _ => {}
            }
        }
        walk(&json, &mut keys);
        keys.sort();
        keys.dedup();
        assert_eq!(
            keys,
            [
                "codec",
                "detail",
                "has_sub_stream",
                "height",
                "id",
                "name",
                "problem",
                "state",
                "status",
                "stream",
                "width"
            ]
        );
    }

    /// Compatibilité : l'état complet d'un serveur de version 1 (sans `cameras`) se lit, et
    /// un champ inconnu (ajouté plus tard) est ignoré.
    #[test]
    fn snapshots_without_cameras_and_with_unknown_fields_still_parse() {
        let snapshot = Snapshot {
            seq: 1,
            server_time: 1.0,
            devices: vec![],
            plan: TextIds(sample_plan()),
            rules: TextIds(sample_rules()),
            rule_runs: vec![],
            pairing_busy: false,
            cameras: vec![camera(CAMERA_ID_MIN, CameraStatus::Online)],
        };
        let mut json = serde_json::to_value(&snapshot).unwrap();
        json.as_object_mut().unwrap().remove("cameras");
        let old: Snapshot = serde_json::from_value(json.clone()).unwrap();
        assert!(old.cameras.is_empty());
        // Champ inconnu au niveau de l'état complet, d'une caméra et d'un état de caméra.
        let mut json = serde_json::to_value(&snapshot).unwrap();
        json["futur"] = Value::Bool(true);
        json["cameras"][0]["orientation"] = Value::from(90);
        json["cameras"][0]["status"]["since"] = Value::from(1.5);
        let new: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(new, snapshot);
    }
}
