//! Tests des images fixes : une vraie session `retina` contre la fausse caméra locale (127.0.0.1,
//! jamais de trafic réel) pour la capture et le décodage VideoToolbox.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::cameras::live::{LiveConfig, LiveRegistry};
use crate::cameras::{CameraDraft, StreamKind};
use crate::hub::Hub;

// Même fausse caméra que les tests du relais (`live/tests.rs`) : le fichier est inclus deux fois.
#[allow(clippy::duplicate_mod)]
#[path = "../../../tests/support/fake_camera.rs"]
mod fake_camera;
use fake_camera::{Options, CLIP_HEIGHT, CLIP_WIDTH};

const GRACE: Duration = Duration::from_millis(300);

fn live_config() -> LiveConfig {
    LiveConfig {
        grace: GRACE,
        connect_timeout: Duration::from_secs(2),
        silence_timeout: Duration::from_secs(2),
        retry_first: Duration::from_millis(100),
        retry_max: Duration::from_millis(300),
        target_check: Duration::from_millis(100),
        ..LiveConfig::default()
    }
}

fn config() -> SnapshotConfig {
    SnapshotConfig {
        max_age: Duration::from_secs(10),
        timeout: Duration::from_secs(8),
        failure_ttl: Duration::from_secs(5),
        ..SnapshotConfig::default()
    }
}

fn hub_with(tag: &str, port: u16) -> (Arc<Hub>, u64) {
    let dir = std::env::temp_dir().join(format!("mh-snap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (hub, _rx) = crate::hub::tests::hub_with(&dir, 16);
    let hub = Arc::new(hub);
    let id = hub
        .add_camera(&CameraDraft {
            name: "Locale".into(),
            url: format!("rtsp://127.0.0.1:{port}/live/ch00_0"),
            ..CameraDraft::default()
        })
        .unwrap();
    (hub, id)
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("délai dépassé : {what}");
}

/// Largeur et hauteur lues dans l'en-tête d'un JPEG (marqueur SOF), ou `None` si ce n'en est pas un.
fn jpeg_size(bytes: &[u8]) -> Option<(u16, u16)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) || !bytes.ends_with(&[0xFF, 0xD9]) {
        return None;
    }
    let mut at = 2;
    while at + 9 < bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        let len = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if matches!(marker, 0xC0..=0xC2) {
            let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]);
            let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]);
            return Some((w, h));
        }
        at += 2 + len;
    }
    None
}

#[tokio::test]
async fn a_snapshot_is_a_jpeg_of_the_camera_image_and_is_shared_through_the_cache() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("jpeg", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(config());

    assert_eq!(camera.plays(), 0, "rien n'est ouvert sans demande");
    let first = service.get(&live, &hub, id).await.unwrap();
    assert_eq!(
        jpeg_size(&first.jpeg),
        Some((CLIP_WIDTH, CLIP_HEIGHT)),
        "un JPEG de la taille du clip"
    );
    assert_eq!(camera.plays(), 1);

    // Deuxième demande : le cache, aucune connexion de plus, la même image.
    let second = service.get(&live, &hub, id).await.unwrap();
    assert_eq!(first.jpeg, second.jpeg);
    assert_eq!(camera.plays(), 1);
    assert_eq!(
        camera
            .stats
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // La session se ferme après sa grâce : plus de spectateur, plus de connexion.
    until("session fermée", || {
        hub.camera_viewers(id) == 0 && camera.open() == 0
    })
    .await;
    assert_eq!(live.sessions(), 0);
}

#[tokio::test]
async fn simultaneous_requests_make_one_capture() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("single", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(config());

    let results =
        futures_util::future::join_all((0..6).map(|_| service.get(&live, &hub, id))).await;
    let images: Vec<_> = results.into_iter().map(|r| r.unwrap().jpeg).collect();
    assert!(images.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(camera.plays(), 1, "une seule capture pour six demandes");
    assert!(camera.open() <= 1);
}

#[tokio::test]
async fn an_expired_image_is_captured_again() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("expiry", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(SnapshotConfig {
        max_age: Duration::from_millis(200),
        ..config()
    });

    service.get(&live, &hub, id).await.unwrap();
    assert_eq!(camera.plays(), 1);
    // La session se ferme, l'image vieillit : la demande suivante refait une capture.
    until("session fermée", || live.sessions() == 0).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    service.get(&live, &hub, id).await.unwrap();
    assert_eq!(camera.plays(), 2, "une nouvelle capture, pas le cache");
}

#[tokio::test]
async fn an_open_live_session_is_joined_without_another_connection() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("joined", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(config());

    // Quelqu'un regarde le flux principal (caméra sans flux réduit).
    let watcher = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    until("session ouverte", || camera.plays() == 1).await;

    let snapshot = service.get(&live, &hub, id).await.unwrap();
    assert!(jpeg_size(&snapshot.jpeg).is_some());
    assert_eq!(camera.plays(), 1, "la vignette se joint au direct");
    assert_eq!(live.sessions(), 1);
    drop(watcher);
}

// Le verrou std est tenu pendant l'appel : c'est ce qu'on veut montrer (la capture ne l'attend pas).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_test_or_check_in_progress_keeps_the_camera_to_itself() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("lock", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(config());

    let lock = hub.camera_probe_lock(id);
    let held = lock.lock().unwrap();
    let busy = service.get(&live, &hub, id).await;
    assert_eq!(busy.unwrap_err(), SnapshotError::Busy);
    assert_eq!(
        camera
            .stats
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(hub.camera_viewers(id), 0, "pas de spectateur pris");

    // Le verrou rendu, la capture se fait (un refus passager ne se garde pas).
    drop(held);
    assert!(service.get(&live, &hub, id).await.is_ok());
}

#[tokio::test]
async fn a_silent_camera_ends_at_the_deadline_and_the_failure_is_remembered() {
    let camera = fake_camera::start(Options::default()).await;
    camera
        .stats
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (hub, id) = hub_with("silent", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(SnapshotConfig {
        timeout: Duration::from_millis(1800),
        ..config()
    });

    let started = Instant::now();
    let error = service.get(&live, &hub, id).await.unwrap_err();
    assert_eq!(error, SnapshotError::NoImage);
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );

    // La session se ferme après sa grâce (plus de spectateur), puis l'échec gardé répond sans
    // rouvrir de connexion.
    until("session fermée", || live.sessions() == 0).await;
    let accepted = camera
        .stats
        .accepted
        .load(std::sync::atomic::Ordering::SeqCst);
    let started = Instant::now();
    assert_eq!(
        service.get(&live, &hub, id).await.unwrap_err(),
        SnapshotError::NoImage
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(
        camera
            .stats
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        accepted
    );

    assert_eq!(hub.camera_viewers(id), 0);
}

#[tokio::test]
async fn an_unknown_camera_has_no_image() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("unknown", camera.port);
    let live = LiveRegistry::new(live_config());
    let service = SnapshotService::new(config());
    assert_eq!(
        service.get(&live, &hub, id ^ 0x55).await.unwrap_err(),
        SnapshotError::UnknownCamera
    );
}

#[test]
fn big_images_are_reduced_before_encoding() {
    let image = Image {
        width: 1920,
        height: 1080,
        rgb: vec![128; 1920 * 1080 * 3],
    };
    let jpeg = encode_jpeg(&image, &SnapshotConfig::default()).unwrap();
    assert_eq!(jpeg_size(&jpeg), Some((640, 360)));
    let small = Image {
        width: 320,
        height: 180,
        rgb: vec![10; 320 * 180 * 3],
    };
    let jpeg = encode_jpeg(&small, &SnapshotConfig::default()).unwrap();
    assert_eq!(jpeg_size(&jpeg), Some((320, 180)));
    // Des pixels qui ne correspondent pas à la taille annoncée : refusés, pas de panique.
    let broken = Image {
        width: 10,
        height: 10,
        rgb: vec![0; 5],
    };
    assert!(encode_jpeg(&broken, &SnapshotConfig::default()).is_none());
}
