use std::sync::Arc;

use tokio::sync::mpsc;

use protocol::wire::{CameraStatus, ServerMessage, StreamInfo, WireEvent};

use super::super::tests::{hub_with, temp};
use super::super::{Hub, HubConfig, Subscription};
use crate::cameras::{self, CameraDraft, ProbeError, StreamKind};

const CAM_SECRET: &str = "mdp-camera-tres-secret";
const CAM_HOST: &str = "192.0.2.55";

fn cam_draft(name: &str) -> CameraDraft {
    CameraDraft {
        name: name.into(),
        url: format!("rtsp://{CAM_HOST}:554/live/ch00_0"),
        sub_url: format!("rtsp://{CAM_HOST}:554/live/ch00_1"),
        username: "admin".into(),
        password: Some(CAM_SECRET.into()),
    }
}

thread_local! {
    /// Lignes de journal écrites par le fil de ce test (voir `captured_logs`).
    static CAPTURED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Journal capturé : ce que le serveur écrit, depuis le fil de ce test uniquement. Le journal
/// `log` est global au processus et les tests tournent en parallèle : sans ce découpage par fil,
/// les lignes des autres tests se mélangeraient à celles-ci (et le test ne verrait plus
/// forcément les siennes). Les lignes déjà reçues par ce fil sont effacées à l'appel.
fn captured_logs() -> CapturedLogs {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    struct Capture;
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            let _ = CAPTURED.try_with(|c| c.borrow_mut().push(format!("{}", record.args())));
        }
        fn flush(&self) {}
    }
    INSTALL.call_once(|| {
        let _ = log::set_logger(&Capture);
        log::set_max_level(log::LevelFilter::Trace);
    });
    CAPTURED.with(|c| c.borrow_mut().clear());
    CapturedLogs
}

struct CapturedLogs;

impl CapturedLogs {
    fn lines(&self) -> Vec<String> {
        CAPTURED.with(|c| c.borrow().clone())
    }
}

fn drain(sub: &mut Subscription) -> Vec<String> {
    std::iter::from_fn(|| sub.rx.try_recv().ok().map(|m| m.to_string())).collect()
}

#[test]
fn cameras_are_persisted_broadcast_and_reloaded() {
    let dir = temp("cam-flow");
    let (hub, _rx) = hub_with(&dir, 32);
    let mut sub = hub.subscribe();
    let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
    assert!(protocol::wire::is_camera_id(id));
    assert!(dir.join(cameras::FILE_NAME).exists());
    // Le client reçoit la liste complète, sans secret.
    let events = drain(&mut sub);
    assert_eq!(events.len(), 1);
    let ServerMessage::Event {
        event: WireEvent::CamerasChanged { cameras },
        ..
    } = serde_json::from_str(&events[0]).unwrap()
    else {
        panic!("{}", events[0])
    };
    assert_eq!(cameras.len(), 1);
    assert_eq!((cameras[0].id, cameras[0].name.as_str()), (id, "Entrée"));
    assert!(cameras[0].has_sub_stream);
    assert_eq!(cameras[0].status, CameraStatus::Unknown);
    // Un nouveau hub (redémarrage) retrouve la caméra, avec ses secrets côté serveur.
    let (again, _rx2) = hub_with(&dir, 32);
    assert_eq!(again.cameras(), cameras);
    let target = again.camera_target(id, StreamKind::Sub).unwrap();
    assert_eq!(target.url.path(), "/live/ch00_1");
    assert_eq!(target.credentials.unwrap().password, CAM_SECRET);
    assert!(again.camera_target(id, StreamKind::Main).is_some());
    // Modifier, puis supprimer : chaque changement est diffusé et enregistré.
    let mut edit = cam_draft("Entrée 2");
    edit.password = None;
    hub.update_camera(id, &edit).unwrap();
    assert_eq!(cameras::load(&dir)[0].password.as_deref(), Some(CAM_SECRET));
    hub.delete_camera(id).unwrap();
    assert!(cameras::load(&dir).is_empty());
    assert!(hub.cameras().is_empty());
    assert_eq!(drain(&mut sub).len(), 2);
    assert!(hub.delete_camera(id).is_err());
    assert!(hub.update_camera(id, &edit).is_err());
    // Aucune caméra n'a touché aux appareils ni au plan.
    assert!(hub.current_snapshot().devices.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_save_leaves_the_cameras_unchanged() {
    let dir = temp("cam-rollback");
    let (hub, _rx) = hub_with(&dir, 32);
    let id = hub.add_camera(&cam_draft("Garage")).unwrap();
    let mut sub = hub.subscribe();
    // Un dossier à la place du fichier temporaire : l'écriture échoue.
    std::fs::create_dir(dir.join("cameras.tmp")).unwrap();
    assert!(hub.add_camera(&cam_draft("Jardin")).is_err());
    assert!(hub.update_camera(id, &cam_draft("Renommée")).is_err());
    assert!(hub.delete_camera(id).is_err());
    assert_eq!(hub.cameras().len(), 1);
    assert_eq!(hub.cameras()[0].name, "Garage");
    assert!(
        drain(&mut sub).is_empty(),
        "rien n'est diffusé si rien n'a changé"
    );
    let message = hub.add_camera(&cam_draft("Jardin")).unwrap_err().0;
    assert!(!message.contains(CAM_SECRET) && !message.contains(CAM_HOST));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn probe_results_update_the_state_and_stale_ones_are_dropped() {
    let dir = temp("cam-probe");
    let (hub, _rx) = hub_with(&dir, 32);
    let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
    let mut sub = hub.subscribe();
    let (kind, target) = hub.camera_probe_target(id).unwrap();
    assert_eq!(
        kind,
        StreamKind::Sub,
        "on surveille le flux réduit s'il existe"
    );
    let info = StreamInfo {
        codec: "h264".into(),
        width: Some(1280),
        height: Some(720),
    };
    hub.record_camera_probe(id, kind, &target, Ok(info.clone()));
    let view = hub.cameras().remove(0);
    assert_eq!(
        (view.status, view.stream),
        (CameraStatus::Online, Some(info.clone()))
    );
    assert_eq!(drain(&mut sub).len(), 1);
    // Même résultat : rien à diffuser.
    hub.record_camera_probe(id, kind, &target, Ok(info));
    assert!(drain(&mut sub).is_empty());
    // Échec : injoignable, avec la phrase de l'utilisateur.
    let down = ProbeError {
        problem: protocol::wire::CameraProblem::Unauthorized,
        message: "La caméra refuse l'identifiant ou le mot de passe.".into(),
    };
    // Hystérésis : le premier échec d'une caméra joignable ne change rien, mais demande de
    // revérifier tout de suite ; le deuxième d'affilée la déclare injoignable.
    assert!(hub.record_camera_probe(id, kind, &target, Err(down.clone())));
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
    assert!(drain(&mut sub).is_empty());
    assert!(!hub.record_camera_probe(id, kind, &target, Err(down.clone())));
    assert!(matches!(
        hub.cameras()[0].status,
        CameraStatus::Unreachable {
            problem: protocol::wire::CameraProblem::Unauthorized,
            ..
        }
    ));
    assert_eq!(drain(&mut sub).len(), 1);
    // Une autre phrase pour le même motif n'est pas un changement : rien n'est rediffusé.
    let reworded = ProbeError {
        message: "Autre formulation.".into(),
        ..down.clone()
    };
    hub.record_camera_probe(id, kind, &target, Err(reworded));
    assert!(drain(&mut sub).is_empty());
    // Un succès (le flux réduit a changé de résolution, par exemple) repasse en ligne.
    let back = StreamInfo {
        codec: "h264".into(),
        width: Some(640),
        height: Some(360),
    };
    hub.record_camera_probe(id, kind, &target, Ok(back));
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
    drain(&mut sub);
    // La caméra est modifiée pendant une vérification : le résultat obsolète est écarté.
    let mut edit = cam_draft("Entrée");
    edit.sub_url = format!("rtsp://{CAM_HOST}:554/live/autre");
    edit.password = None;
    hub.update_camera(id, &edit).unwrap();
    assert_eq!(hub.cameras()[0].status, CameraStatus::Unknown);
    drain(&mut sub);
    hub.record_camera_probe(
        id,
        kind,
        &target,
        Ok(StreamInfo {
            codec: "h265".into(),
            width: None,
            height: None,
        }),
    );
    assert_eq!(hub.cameras()[0].status, CameraStatus::Unknown);
    assert!(drain(&mut sub).is_empty());
    // Caméra supprimée pendant la vérification : rien non plus.
    hub.delete_camera(id).unwrap();
    hub.record_camera_probe(
        id,
        kind,
        &target,
        Ok(StreamInfo {
            codec: "h264".into(),
            width: None,
            height: None,
        }),
    );
    assert!(hub.cameras().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Critère d'acceptation : ni l'adresse ni le mot de passe dans les réponses de l'API (l'état
/// complet), les événements, ni les journaux.
#[test]
fn neither_the_address_nor_the_password_leave_the_server() {
    let logs = captured_logs();
    let dir = temp("cam-secrets");
    let (hub, _rx) = hub_with(&dir, 32);
    let mut sub = hub.subscribe();
    let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
    let mut edit = cam_draft("Entrée");
    edit.url = format!("rtsp://admin:{CAM_SECRET}@{CAM_HOST}:554/live/ch00_9");
    edit.password = Some(String::new());
    hub.update_camera(id, &edit).unwrap();
    // Un refus ne répète pas non plus ce qui a été saisi.
    let mut bad = cam_draft("");
    bad.url = format!("http://admin:{CAM_SECRET}@{CAM_HOST}/x");
    let refused = hub.add_camera(&bad).unwrap_err();
    let (kind, target) = hub.camera_probe_target(id).unwrap();
    let down = ProbeError {
        problem: protocol::wire::CameraProblem::Unreachable,
        message: "La caméra ne répond pas.".into(),
    };
    hub.record_camera_probe(id, kind, &target, Err(down.clone()));
    hub.record_camera_probe(id, kind, &target, Err(down));
    // Test du formulaire (mode réel, mais la saisie est refusée avant tout trafic).
    let _ = hub.start_camera_test(&bad, None);
    hub.delete_camera(id).unwrap();

    let mut seen = drain(&mut sub);
    seen.push(refused.0);
    seen.push(serde_json::to_string(&hub.current_snapshot()).unwrap());
    seen.push(serde_json::to_string(&hub.cameras()).unwrap());
    seen.push(sub.snapshot.to_string());
    // Pendant que la caméra existe : l'état complet la montre aussi.
    let id2 = hub.add_camera(&cam_draft("Autre")).unwrap();
    seen.push(serde_json::to_string(&hub.current_snapshot()).unwrap());
    seen.push(hub.subscribe().snapshot.to_string());
    assert!(hub.current_snapshot().cameras.iter().any(|c| c.id == id2));
    let captured: Vec<String> = logs.lines();
    // Le test ne doit pas réussir à vide : le journal a bien reçu les lignes des caméras (la
    // vérification qu'elles ne contiennent rien de secret porte alors sur du texte réel).
    assert!(
        captured.iter().any(|l| l.contains("caméra ajoutée")),
        "journal capturé : {captured:?}"
    );
    assert!(captured.iter().any(|l| l.contains("caméra supprimée")));
    assert!(captured.iter().any(|l| l.contains("injoignable")));
    seen.extend(captured);
    for text in &seen {
        for needle in [CAM_SECRET, CAM_HOST, "rtsp://", "ch00_"] {
            assert!(!text.contains(needle), "{needle} dans : {text}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_demo_hub_has_fake_cameras_that_are_never_written() {
    let dir = temp("cam-demo");
    let (tx, _rx) = mpsc::unbounded_channel();
    let hub = Hub::new(HubConfig {
        data_dir: dir.clone(),
        demo: true,
        history_dir: dir.join("history-demo"),
        demo_history: None,
        broadcast_capacity: 8,
        commands: tx,
    });
    assert_eq!(hub.cameras().len(), 3);
    assert_eq!(hub.current_snapshot().cameras.len(), 3);
    let id = hub.add_camera(&cam_draft("Nouvelle")).unwrap();
    hub.delete_camera(id).unwrap();
    assert!(!dir.join(cameras::FILE_NAME).exists());
    // Le test du formulaire ne fait aucun trafic en démo.
    let results = hub
        .start_camera_test(&cam_draft("x"), None)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.outcome.is_ok()));
    // Il refuse quand même une saisie invalide.
    assert!(hub.start_camera_test(&cam_draft(""), None).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deleting_a_camera_also_removes_it_from_the_plan_in_one_transaction() {
    use protocol::home::{Item, ItemKind};
    let dir = temp("cam-plan");
    let (hub, _rx) = hub_with(&dir, 32);
    let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
    let other = hub.add_camera(&cam_draft("Jardin")).unwrap();
    let mut plan = hub.plan();
    for (n, camera_id) in [(50, id), (51, other)] {
        plan.floors[0].items.push(Item {
            id: n,
            kind: ItemKind::camera(camera_id),
            x: 1.0,
            y: 1.0,
            rotation: 90.0,
        });
    }
    hub.set_plan(plan).unwrap();
    let mut sub = hub.subscribe();
    // Pas de blocage (un seul passage par le verrou d'écriture), et le plan suit.
    hub.delete_camera(id).unwrap();
    let kinds: Vec<String> = drain(&mut sub)
        .iter()
        .map(
            |m| match serde_json::from_str::<ServerMessage>(m).unwrap() {
                ServerMessage::Event {
                    event: WireEvent::CamerasChanged { .. },
                    ..
                } => "cameras",
                ServerMessage::Event {
                    event: WireEvent::PlanChanged { .. },
                    ..
                } => "plan",
                _ => "autre",
            },
        )
        .map(String::from)
        .collect();
    assert_eq!(kinds, ["cameras", "plan"]);
    assert_eq!(
        hub.plan().placed_cameras(),
        std::collections::HashSet::from([other])
    );
    let on_disk: protocol::home::HomePlan = crate::persistence::load_json(&dir.join("maison.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        on_disk.placed_cameras(),
        std::collections::HashSet::from([other])
    );
    // Une caméra qui n'était pas sur le plan ne diffuse que la liste.
    let spare = hub.add_camera(&cam_draft("Garage")).unwrap();
    drain(&mut sub);
    hub.delete_camera(spare).unwrap();
    assert_eq!(drain(&mut sub).len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn watched_cameras_refuse_the_test_and_viewers_are_counted() {
    let dir = temp("cam-viewers");
    let (hub, _rx) = hub_with(&dir, 32);
    let hub = Arc::new(hub);
    let id = hub.add_camera(&cam_draft("Entrée")).unwrap();
    assert_eq!(hub.camera_viewers(id), 0);
    assert!(
        hub.acquire_camera_viewer(id + 1).is_none(),
        "caméra inconnue"
    );
    let first = hub.acquire_camera_viewer(id).unwrap();
    let second = hub.acquire_camera_viewer(id).unwrap();
    assert_eq!((hub.camera_viewers(id), first.camera_id()), (2, id));
    let refused = hub
        .start_camera_test(&cam_draft("Entrée"), Some(id))
        .err()
        .unwrap();
    assert!(refused.0.contains("en cours de lecture"), "{refused}");
    // Un nouveau formulaire (sans caméra existante) n'est pas concerné par ce refus.
    assert!(hub.start_camera_test(&cam_draft("Autre"), None).is_ok());
    drop(first);
    assert_eq!(hub.camera_viewers(id), 1);
    drop(second);
    assert_eq!(hub.camera_viewers(id), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_new_camera_never_takes_a_known_device_id() {
    let dir = temp("cam-ids");
    let (hub, _rx) = hub_with(&dir, 32);
    // Un appareil ponté déjà attribué dans la plage des caméras.
    let device = protocol::wire::CAMERA_ID_MIN + 7;
    hub.apply(&crate::events::Event::Node(crate::events::NodeInfo {
        node_id: device,
        name: "Volet".into(),
        ..Default::default()
    }));
    for i in 0..20 {
        let id = hub.add_camera(&cam_draft(&format!("c{i}"))).unwrap();
        assert_ne!(id, device);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_plan_may_reference_a_camera_the_server_does_not_know() {
    // Décision du ticket n° 6 : pas de refus à l'enregistrement du plan (un client peut envoyer
    // son plan juste avant une suppression) ; le client grise l'objet orphelin.
    use protocol::home::{Item, ItemKind};
    let dir = temp("cam-orphan");
    let (hub, _rx) = hub_with(&dir, 32);
    let mut plan = hub.plan();
    plan.floors[0].items.push(Item {
        id: 60,
        kind: ItemKind::camera(protocol::wire::CAMERA_ID_MIN + 99),
        x: 1.0,
        y: 1.0,
        rotation: 10.0,
    });
    hub.set_plan(plan.clone()).unwrap();
    assert_eq!(hub.plan(), plan);
    std::fs::remove_dir_all(dir).unwrap();
}
