//! Tests de la veille : la fausse caméra locale (127.0.0.1, jamais de trafic réel), un détecteur
//! factice et un décodeur synthétique (aucun modèle, aucune dépendance au GPU).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use protocol::mode::HomeMode;
use protocol::wire::{
    CameraBehavior, CameraModes, DetectedClass, StreamKind, StreamPacket, WireEvent,
};

use super::*;
use crate::cameras::CameraDraft;
use crate::detection::detector::{Detection, MockDetector};
use crate::detection::DetectionConfig;
use crate::hub::Hub;

// Même fausse caméra que les tests du relais : le fichier est inclus plusieurs fois.
#[allow(clippy::duplicate_mod)]
#[path = "../../../tests/support/fake_camera.rs"]
mod fake_camera;
use fake_camera::Options;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mh-detect-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Une caméra en Surveillance quand on est absent, en Diffusion sinon.
fn hub_with(tag: &str, port: u16) -> (Arc<Hub>, u64, std::path::PathBuf) {
    let dir = temp_dir(tag);
    let (hub, rx) = crate::hub::tests::hub_with(&dir, 64);
    // Les commandes vers le moteur (Command::Detection) : lues par le test au besoin.
    std::mem::forget(rx);
    let hub = Arc::new(hub);
    let mut modes = CameraModes::default();
    for mode in HomeMode::ALL {
        modes.set(mode, CameraBehavior::Diffusion);
    }
    modes.set(HomeMode::Absent, CameraBehavior::Surveillance);
    modes.set(HomeMode::Nuit, CameraBehavior::Eteinte);
    let id = hub
        .add_camera(&CameraDraft {
            name: "Allée".into(),
            url: format!("rtsp://127.0.0.1:{port}/live/ch00_0"),
            modes,
            ..CameraDraft::default()
        })
        .unwrap();
    hub.detection()
        .set_config(DetectionConfig {
            fps: 10.0,
            close_after_s: 2.0,
            ..DetectionConfig::default()
        })
        .unwrap();
    (hub, id, dir)
}

/// Des images synthétiques : un carré qui se déplace à chaque image demandée (du mouvement).
struct Synthetic {
    n: u32,
}

impl FrameDecoder for Synthetic {
    fn push(&mut self, packet: &StreamPacket, want: bool) -> Result<Option<Image>, String> {
        if !want || !matches!(packet, StreamPacket::Frame { .. }) {
            return Ok(None);
        }
        self.n += 1;
        let (w, h) = (160u32, 90u32);
        let mut rgb = vec![30u8; (w * h * 3) as usize];
        let x0 = (self.n * 17) % (w - 20);
        for y in 30..60 {
            for x in x0..x0 + 20 {
                let at = ((y * w + x) * 3) as usize;
                rgb[at..at + 3].copy_from_slice(&[240, 240, 240]);
            }
        }
        Ok(Some(Image {
            width: w,
            height: h,
            rgb,
        }))
    }
}

fn synthetic() -> DecoderFactory {
    Arc::new(|| Ok(Box::new(Synthetic { n: 0 }) as Box<dyn FrameDecoder>))
}

/// Une personne pendant `hits` images analysées, puis plus rien.
fn person_for(hits: usize) -> (SharedDetector, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let person = Detection {
        class: DetectedClass::Personne,
        score: 0.82,
        bbox: [0.4, 0.3, 0.6, 0.9],
    };
    let detector = MockDetector {
        script: VecDeque::from(vec![vec![person]; hits]),
        calls: calls.clone(),
    };
    (Arc::new(Mutex::new(Box::new(detector))), calls)
}

async fn until(what: &str, secs: u64, mut condition: impl FnMut() -> bool) {
    for _ in 0..secs * 20 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("délai dépassé : {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_surveillance_cameras_are_watched_and_a_passage_makes_one_event() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id, dir) = hub_with("event", camera.port);
    let mut events = hub.subscribe();
    let (detector, calls) = person_for(6);
    let (stop_tx, stop) = watch::channel(false);
    tokio::spawn(run(hub.clone(), detector, synthetic(), stop));

    // Présent : Diffusion, aucune connexion.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(camera.open(), 0, "aucune session hors Surveillance");
    assert!(hub.detection().status().watched.is_empty());

    // Absent : Surveillance, une session.
    hub.set_mode(HomeMode::Absent).unwrap();
    until("la caméra est surveillée", 5, || camera.playing() == 1).await;
    assert_eq!(hub.detection().status().watched, vec![id]);

    // Un spectateur du direct rejoint la même session : jamais une connexion de plus.
    let viewer = hub.live().subscribe(&hub, id, StreamKind::Main).unwrap();
    assert_eq!(hub.live().sessions(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(camera.playing(), 1);
    assert_eq!(camera.plays(), 1);

    // Le passage : 6 images avec une personne, puis 2 s sans rien → un seul événement.
    let store = hub.detection().store();
    until("l'événement est enregistré", 15, || {
        !store.all().is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let all = store.all();
    assert_eq!(all.len(), 1, "un passage, un événement");
    let event = &all[0];
    assert!(calls.load(Ordering::SeqCst) >= 6);
    assert_eq!(event.camera_id, id);
    assert_eq!(event.camera_name, "Allée");
    assert_eq!(event.classes, [DetectedClass::Personne]);
    assert!((event.score - 0.82).abs() < 1e-6);
    assert!(event.end >= event.start);
    assert!(event.has_clip, "un clip");
    let clip = std::fs::read(store.part(&event.id, crate::detection::Part::Clip).unwrap()).unwrap();
    assert_eq!(&clip[4..8], b"ftyp");
    // Pour l'essayer dans un lecteur : `MH_KEEP_CLIP=/chemin/clip.mp4 cargo test …`.
    if let Some(path) = std::env::var_os("MH_KEEP_CLIP") {
        std::fs::write(path, &clip).unwrap();
    }
    assert!(store
        .part(&event.id, crate::detection::Part::Snapshot)
        .is_some());

    // Diffusé aux clients.
    let mut announced = false;
    while let Ok(text) = events.rx.try_recv() {
        if text.contains("\"detection\"") {
            let message: protocol::wire::ServerMessage = serde_json::from_str(&text).unwrap();
            if let protocol::wire::ServerMessage::Event {
                event: WireEvent::Detection { event: e },
                ..
            } = message
            {
                assert_eq!(e.id, event.id);
                announced = true;
            }
        }
    }
    assert!(announced, "WireEvent::Detection diffusé");

    // Retour en Présent : la veille lâche la session, le spectateur la garde.
    hub.set_mode(HomeMode::Present).unwrap();
    until("plus de veille", 5, || {
        hub.detection().status().watched.is_empty()
    })
    .await;
    assert_eq!(camera.playing(), 1, "le direct continue");
    drop(viewer);

    // Nuit : éteinte, la session se ferme et la veille n'en rouvre pas.
    hub.set_mode(HomeMode::Nuit).unwrap();
    until("plus aucune connexion", 15, || camera.open() == 0).await;
    let _ = stop_tx.send(true);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_clip_becomes_an_mp4_without_parameter_sets_in_the_samples() {
    use crate::detection::prebuffer::{Frame, PrebufferConfig, Recorder};
    let sps = [0x67, 0x64, 0x00, 0x1f, 0xac];
    let pps = [0x68, 0xee, 0x3c, 0x80];
    let avcc = nal::build_avcc(&sps, &pps);
    let avcc_frame = |nals: &[&[u8]]| {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&(n.len() as u32).to_be_bytes());
            out.extend_from_slice(n);
        }
        Bytes::from(out)
    };
    let mut r = Recorder::new(PrebufferConfig::default());
    r.set_config(&avcc, 320, 180);
    let base = std::time::Instant::now();
    for i in 0..10u64 {
        let key = i % 5 == 0;
        let data = if key {
            avcc_frame(&[&sps, &pps, &[0x65, 0x88, i as u8]])
        } else {
            avcc_frame(&[&[0x41, 0x9a, i as u8]])
        };
        if i == 2 {
            r.start_clip();
        }
        r.push(Frame {
            at: base + Duration::from_millis(i * 100),
            ts_ms: i * 100,
            key,
            data,
        });
    }
    let clip = r.finish_clip().unwrap();
    let (mp4, seconds) = clip_to_mp4(&clip).unwrap();
    assert!((seconds - 0.9).abs() < 1e-6);
    // Les échantillons ne portent plus SPS ni PPS : la première image clé fait 4 + 3 octets.
    let stsz = mp4.windows(4).position(|w| w == b"stsz").unwrap();
    let count = u32::from_be_bytes(mp4[stsz + 12..stsz + 16].try_into().unwrap());
    assert_eq!(count, 10);
    let first = u32::from_be_bytes(mp4[stsz + 16..stsz + 20].try_into().unwrap());
    assert_eq!(first, 7);
}
