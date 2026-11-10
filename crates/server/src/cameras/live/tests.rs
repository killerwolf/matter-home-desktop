//! Tests du relais : une vraie session `retina` contre la fausse caméra locale (127.0.0.1, jamais
//! de trafic réel), et des sessions sans caméra pour le comportement des spectateurs.

use std::sync::Arc;
use std::time::Duration;

use protocol::wire::{
    CameraProblem, CameraStatus, StreamKind, StreamPacket, StreamStatus, CODEC_H264,
};

use super::*;
use crate::cameras::CameraDraft;
use crate::hub::Hub;

#[path = "../../../tests/support/fake_camera.rs"]
mod fake_camera;
use fake_camera::{Options, CLIP_HEIGHT, CLIP_WIDTH};

const GRACE: Duration = Duration::from_millis(400);

fn fast() -> LiveConfig {
    LiveConfig {
        grace: GRACE,
        connect_timeout: Duration::from_secs(3),
        silence_timeout: Duration::from_secs(2),
        retry_first: Duration::from_millis(100),
        retry_max: Duration::from_millis(400),
        target_check: Duration::from_millis(100),
        ..LiveConfig::default()
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mh-live-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn draft(port: u16, path: &str) -> CameraDraft {
    CameraDraft {
        name: "Locale".into(),
        url: format!("rtsp://127.0.0.1:{port}/{path}"),
        ..CameraDraft::default()
    }
}

fn hub_with(tag: &str, draft: &CameraDraft) -> (Arc<Hub>, u64) {
    let dir = temp_dir(tag);
    let (hub, _rx) = crate::hub::tests::hub_with(&dir, 16);
    let hub = Arc::new(hub);
    let id = hub.add_camera(draft).unwrap();
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

/// Ce qu'un spectateur a reçu jusqu'ici.
#[derive(Default)]
struct Seen {
    statuses: Vec<StreamStatus>,
    config: Option<StreamPacket>,
    frames: Vec<(bool, Vec<u8>)>,
    ended: Option<EndReason>,
}

/// Lit des événements jusqu'à ce que `done` soit vrai (5 s au plus).
async fn read_until(sub: &mut Subscription, seen: &mut Seen, mut done: impl FnMut(&Seen) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !done(seen) {
        let event = tokio::time::timeout_at(deadline, sub.next())
            .await
            .expect("événement attendu");
        match event {
            Event::Status(s) => seen.statuses.push(s),
            Event::End(reason) => {
                seen.ended = Some(reason);
                return;
            }
            Event::Video(bytes) => match StreamPacket::decode(&bytes).unwrap() {
                config @ StreamPacket::Config { .. } => seen.config = Some(config),
                StreamPacket::Frame { key, data, .. } => seen.frames.push((key, data)),
            },
        }
    }
}

/// Les types des NAL d'une image AVCC (longueur sur 4 octets) ; `None` si le découpage est faux.
fn avcc_nal_types(data: &[u8]) -> Option<Vec<u8>> {
    let mut types = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let nal = rest.get(4..4 + len)?;
        types.push(*nal.first()? & 0x1f);
        rest = &rest[4 + len..];
    }
    Some(types)
}

// ---------------------------------------------------------------------------
// Relais contre une fausse caméra
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_relay_starts_with_the_first_viewer_and_stops_with_the_last_lease() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("relay", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    // Sans spectateur : aucune connexion vers la caméra, ni témoin.
    assert_eq!(camera.open(), 0);
    assert_eq!(hub.camera_viewers(id), 0);
    assert_eq!(hub.cameras()[0].status, CameraStatus::Unknown);

    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    assert_eq!(hub.camera_viewers(id), 1, "la session prend un témoin");
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| s.frames.len() >= 20).await;

    // Configuration, puis images : les NAL sont précédées de leur longueur, la première image est
    // une clé qui porte SPS, PPS et image IDR.
    let Some(StreamPacket::Config {
        codec,
        width,
        height,
        avcc,
        ..
    }) = seen.config
    else {
        panic!("pas de configuration");
    };
    assert_eq!(
        (codec, width, height),
        (CODEC_H264, CLIP_WIDTH, CLIP_HEIGHT)
    );
    assert_eq!(avcc[0], 1, "enregistrement AVCDecoderConfigurationRecord");
    assert_eq!(seen.statuses.first(), Some(&StreamStatus::Connecting));
    assert!(seen.statuses.contains(&StreamStatus::Live));
    let (key, first) = &seen.frames[0];
    assert!(*key);
    let types = avcc_nal_types(first).expect("AVCC valide");
    assert!(
        types.contains(&7) && types.contains(&8) && types.contains(&5),
        "{types:?}"
    );
    for (key, data) in &seen.frames {
        let types = avcc_nal_types(data).expect("AVCC valide");
        assert_eq!(*key, types.contains(&5), "{types:?}");
    }
    // L'état de la caméra suit la session saine, sans que la surveillance ait rien fait.
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
    assert_eq!(camera.playing(), 1);

    // Le départ du dernier spectateur ne coupe pas tout de suite (grâce)…
    drop(sub);
    assert_eq!(camera.playing(), 1);
    assert_eq!(hub.camera_viewers(id), 1);
    // … puis la connexion à la caméra est fermée, et le témoin rendu.
    until("la connexion est fermée", || camera.open() == 0).await;
    until("le témoin est rendu", || hub.camera_viewers(id) == 0).await;
    assert_eq!(camera.playing(), 0);
    assert_eq!(live.sessions(), 0);
    assert_eq!(camera.plays(), 1);
}

#[tokio::test]
async fn viewers_of_the_same_camera_and_stream_share_one_session() {
    let camera = fake_camera::start(Options::default()).await;
    let mut d = draft(camera.port, "live/ch00_0");
    d.sub_url = format!("rtsp://127.0.0.1:{}/live/ch00_1", camera.port);
    let (hub, id) = hub_with("share", &d);
    let live = LiveRegistry::new(fast());

    let mut a = live.subscribe(&hub, id, StreamKind::Sub).unwrap();
    let mut b = live.subscribe(&hub, id, StreamKind::Sub).unwrap();
    let (mut seen_a, mut seen_b) = (Seen::default(), Seen::default());
    read_until(&mut a, &mut seen_a, |s| s.frames.len() >= 5).await;
    read_until(&mut b, &mut seen_b, |s| s.frames.len() >= 5).await;
    assert_eq!(live.sessions(), 1);
    assert_eq!(
        camera.plays(),
        1,
        "une seule connexion à la caméra pour deux spectateurs"
    );
    assert_eq!(hub.camera_viewers(id), 1);

    // Un autre flux de la même caméra est une autre session (une autre connexion).
    let mut main = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen_main = Seen::default();
    read_until(&mut main, &mut seen_main, |s| !s.frames.is_empty()).await;
    assert_eq!(live.sessions(), 2);
    assert_eq!(camera.plays(), 2);
    drop(main);

    // Le premier qui part ne coupe rien.
    drop(a);
    tokio::time::sleep(GRACE * 2).await;
    assert_eq!(camera.playing(), 1);
    let before = seen_b.frames.len();
    read_until(&mut b, &mut seen_b, |s| s.frames.len() > before + 3).await;
    drop(b);
    until("tout est fermé", || camera.open() == 0).await;
    assert_eq!(hub.camera_viewers(id), 0);
    assert_eq!(live.sessions(), 0);
}

#[tokio::test]
async fn a_viewer_who_comes_back_during_the_grace_period_keeps_the_session() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("grace", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    let mut first = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut first, &mut seen, |s| s.frames.len() >= 3).await;
    drop(first);
    tokio::time::sleep(GRACE / 4).await;

    let mut again = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    // Il reçoit la configuration et la suite d'images gardée : une image clé d'abord.
    read_until(&mut again, &mut seen, |s| !s.frames.is_empty()).await;
    assert!(seen.config.is_some());
    assert!(
        seen.frames[0].0,
        "il repart d'une image clé, sans attendre la suivante"
    );
    tokio::time::sleep(GRACE * 2).await;
    assert_eq!(
        camera.playing(),
        1,
        "la grâce ne s'applique plus : quelqu'un regarde"
    );
    assert_eq!(camera.plays(), 1);
    drop(again);
    until("fermée", || camera.open() == 0).await;
}

#[tokio::test]
async fn the_session_reconnects_by_itself_when_the_camera_cuts() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("reconnect", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| s.frames.len() >= 3).await;
    camera.cut();
    let mut after = Seen::default();
    read_until(&mut sub, &mut after, |s| {
        s.statuses.contains(&StreamStatus::Live) && !s.frames.is_empty()
    })
    .await;
    assert!(
        after
            .statuses
            .iter()
            .any(|s| matches!(s, StreamStatus::Retrying { .. })),
        "{:?}",
        after.statuses
    );
    assert_eq!(camera.plays(), 2);
    assert_eq!(live.sessions(), 1);
    assert!(after.frames[0].0, "la reprise commence par une image clé");
}

#[tokio::test]
async fn an_unreachable_camera_is_retried_and_reported_with_the_monitors_rules() {
    let camera = fake_camera::start(Options::default()).await;
    camera
        .stats
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (hub, id) = hub_with("refuse", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| {
        s.statuses
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    StreamStatus::Retrying {
                        problem: CameraProblem::Unreachable
                    }
                )
            })
            .count()
            >= 2
    })
    .await;
    // Pas encore vue joignable : injoignable dès le premier échec, comme pour la surveillance.
    assert!(matches!(
        hub.cameras()[0].status,
        CameraStatus::Unreachable {
            problem: CameraProblem::Unreachable,
            ..
        }
    ));
    // La caméra revient : la session reprend et la caméra repasse en ligne.
    camera
        .stats
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    read_until(&mut sub, &mut seen, |s| !s.frames.is_empty()).await;
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
}

#[tokio::test]
async fn a_codec_that_is_not_relayed_is_refused_without_playing_it() {
    let camera = fake_camera::start(Options {
        h265: true,
        ..Options::default()
    })
    .await;
    let (hub, id) = hub_with("h265", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| {
        s.statuses
            .iter()
            .any(|s| matches!(s, StreamStatus::Unsupported { .. }))
    })
    .await;
    assert!(seen.statuses.contains(&StreamStatus::Unsupported {
        codec: "h265".into()
    }));
    assert_eq!(
        camera.plays(),
        0,
        "aucun PLAY pour un codec qu'on ne relaie pas"
    );
    drop(sub);
    until("fermée", || hub.camera_viewers(id) == 0).await;
    assert_eq!(camera.open(), 0);
}

#[tokio::test]
async fn a_camera_that_is_changed_or_deleted_cuts_its_viewers() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("changed", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());

    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| s.frames.len() >= 2).await;
    // Un renommage ne change pas ce qu'on lit.
    let mut renamed = draft(camera.port, "live/ch00_0");
    renamed.name = "Autre nom".into();
    hub.update_camera(id, &renamed).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(camera.playing(), 1);
    // Une autre adresse, si.
    hub.update_camera(id, &draft(camera.port, "live/autre"))
        .unwrap();
    read_until(&mut sub, &mut seen, |_| false).await;
    assert_eq!(seen.ended, Some(EndReason::Changed));
    until("coupée", || camera.open() == 0).await;
    until("témoin rendu", || hub.camera_viewers(id) == 0).await;
    assert_eq!(live.sessions(), 0);
    drop(sub);

    // Supprimée : on ne peut plus la regarder.
    hub.delete_camera(id).unwrap();
    assert_eq!(
        live.subscribe(&hub, id, StreamKind::Main).err(),
        Some(SubscribeError::UnknownCamera)
    );
}

#[tokio::test]
async fn the_default_stream_must_exist() {
    let camera = fake_camera::start(Options::default()).await;
    let (hub, id) = hub_with("nosub", &draft(camera.port, "live/ch00_0"));
    let live = LiveRegistry::new(fast());
    assert_eq!(
        live.subscribe(&hub, id, StreamKind::Sub).err(),
        Some(SubscribeError::NoSuchStream)
    );
    assert_eq!(
        hub.camera_viewers(id),
        0,
        "un refus ne laisse pas de témoin"
    );
    assert_eq!(camera.open(), 0);
}

#[tokio::test]
async fn the_cameras_credentials_are_used_and_never_leave_the_server() {
    let camera = fake_camera::start(Options {
        basic: Some(("admin", "mdp-de-la-camera")),
        ..Options::default()
    })
    .await;
    let mut d = draft(camera.port, "live/ch00_0");
    d.username = "admin".into();
    d.password = Some("mdp-de-la-camera".into());
    let (hub, id) = hub_with("creds", &d);
    let live = LiveRegistry::new(fast());
    let mut sub = live.subscribe(&hub, id, StreamKind::Main).unwrap();
    let mut seen = Seen::default();
    read_until(&mut sub, &mut seen, |s| !s.frames.is_empty()).await;
    for status in &seen.statuses {
        let json = serde_json::to_string(status).unwrap();
        assert!(
            !json.contains("127.0.0.1") && !json.contains("mdp"),
            "{json}"
        );
    }
    // Ce que reçoit un spectateur n'a aucun champ d'adresse : un message de statut, de la vidéo.
    assert!(matches!(seen.config, Some(StreamPacket::Config { .. })));
}

// ---------------------------------------------------------------------------
// Clients lents et spectateurs en retard, sans caméra
// ---------------------------------------------------------------------------

fn bare_session(backlog_frames: usize, gop_cache_bytes: usize) -> Arc<Session> {
    Session::new(
        1,
        StreamKind::Main,
        "Test".into(),
        LiveConfig {
            backlog_frames,
            gop_cache_bytes,
            ..fast()
        },
    )
}

fn config_packet() -> StreamPacket {
    StreamPacket::Config {
        codec: CODEC_H264,
        width: 640,
        height: 360,
        pixel_aspect: (1, 1),
        avcc: vec![1, 0x42, 0, 0x1e],
    }
}

/// Un spectateur qui ne lit plus ne retient pas les autres ; quand il reprend, il perd les images
/// jusqu'à la prochaine image clé et ne reçoit jamais une image qui en dépend sans sa clé.
#[tokio::test]
async fn a_slow_viewer_drops_frames_up_to_the_next_key_frame_and_stalls_nobody() {
    let session = bare_session(4, 1 << 20);
    session.publish_config(&config_packet());
    let mut slow = session.join();
    let mut fast = session.join();
    assert!(matches!(fast.next().await, Event::Status(_)));
    assert!(
        matches!(fast.next().await, Event::Video(_)),
        "configuration"
    );

    // Trois suites de six images, sans que `slow` lise quoi que ce soit.
    let mut sent = Vec::new();
    for gop in 0..3u32 {
        for i in 0..6u32 {
            let key = i == 0;
            let n = gop * 6 + i;
            session.push_frame(
                key,
                n,
                &[0, 0, 0, 2, if key { 0x65 } else { 0x41 }, n as u8],
            );
            sent.push(n);
            // Le rapide suit : il reçoit tout, dans l'ordre.
            let Event::Video(bytes) = fast.next().await else {
                panic!("image attendue")
            };
            let StreamPacket::Frame { timestamp_ms, .. } = StreamPacket::decode(&bytes).unwrap()
            else {
                panic!("image attendue")
            };
            assert_eq!(timestamp_ms, n);
        }
    }
    assert_eq!(fast.dropped(), 0);

    // Le lent reprend : état, la configuration reçue à son arrivée, puis (sa file a débordé) la
    // configuration de nouveau (elle a pu se perdre), puis rien de l'ancien.
    assert!(matches!(slow.next().await, Event::Status(_)));
    for _ in 0..2 {
        let Event::Video(bytes) = slow.next().await else {
            panic!("configuration attendue")
        };
        assert!(matches!(
            StreamPacket::decode(&bytes).unwrap(),
            StreamPacket::Config { .. }
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), slow.next())
            .await
            .is_err(),
        "les vieilles images ne sont pas rejouées : ce serait du retard"
    );
    assert!(slow.dropped() > 0);

    // Les images suivantes sans clé sont ignorées ; la clé suivante relance le flux.
    session.push_frame(false, 100, &[0, 0, 0, 2, 0x41, 1]);
    session.push_frame(false, 101, &[0, 0, 0, 2, 0x41, 2]);
    session.push_frame(true, 102, &[0, 0, 0, 2, 0x65, 3]);
    session.push_frame(false, 103, &[0, 0, 0, 2, 0x41, 4]);
    let mut got = Vec::new();
    while let Ok(Event::Video(bytes)) =
        tokio::time::timeout(Duration::from_millis(50), slow.next()).await
    {
        let StreamPacket::Frame {
            key, timestamp_ms, ..
        } = StreamPacket::decode(&bytes).unwrap()
        else {
            continue;
        };
        got.push((key, timestamp_ms));
    }
    assert_eq!(got, vec![(true, 102), (false, 103)]);
}

#[tokio::test]
async fn a_late_viewer_starts_from_the_last_key_frame_when_the_session_has_one() {
    let session = bare_session(16, 1 << 20);
    // Pas encore de clé : un spectateur attend la première, et ignore les images sans clé.
    session.push_frame(false, 1, &[0, 0, 0, 2, 0x41, 1]);
    let mut early = session.join();
    session.push_frame(false, 2, &[0, 0, 0, 2, 0x41, 2]);
    session.publish_config(&config_packet());
    session.push_frame(true, 3, &[0, 0, 0, 2, 0x65, 3]);
    session.push_frame(false, 4, &[0, 0, 0, 2, 0x41, 4]);
    session.push_frame(false, 5, &[0, 0, 0, 2, 0x41, 5]);
    // Un autre arrive en cours de suite : configuration, clé et images suivantes, dans l'ordre.
    let mut late = session.join();
    session.push_frame(false, 6, &[0, 0, 0, 2, 0x41, 6]);

    async fn frames(sub: &mut Subscription) -> Vec<(bool, u32)> {
        let mut out = Vec::new();
        let mut config_first = None;
        while let Ok(event) = tokio::time::timeout(Duration::from_millis(50), sub.next()).await {
            let Event::Video(bytes) = event else { continue };
            match StreamPacket::decode(&bytes).unwrap() {
                StreamPacket::Config { .. } => {
                    config_first.get_or_insert(out.len());
                }
                StreamPacket::Frame {
                    key, timestamp_ms, ..
                } => out.push((key, timestamp_ms)),
            }
        }
        assert_eq!(
            config_first,
            Some(0),
            "la configuration passe avant les images"
        );
        out
    }
    assert_eq!(
        frames(&mut late).await,
        vec![(true, 3), (false, 4), (false, 5), (false, 6)]
    );
    // Le premier n'a eu que ce qui suit la clé (les images 1 et 2 dépendaient d'une clé absente).
    assert_eq!(
        frames(&mut early).await,
        vec![(true, 3), (false, 4), (false, 5), (false, 6)]
    );
}

#[tokio::test]
async fn an_oversized_group_of_pictures_is_not_kept_for_late_viewers() {
    let session = bare_session(16, 40);
    session.publish_config(&config_packet());
    session.push_frame(true, 1, &[0; 20]);
    session.push_frame(false, 2, &[0; 20]);
    session.push_frame(false, 3, &[0; 20]);
    let mut late = session.join();
    assert!(matches!(late.next().await, Event::Status(_)));
    assert!(
        matches!(late.next().await, Event::Video(_)),
        "configuration"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), late.next())
            .await
            .is_err(),
        "pas de suite gardée : il attend la prochaine clé"
    );
    session.push_frame(false, 4, &[0; 4]);
    session.push_frame(true, 5, &[0; 4]);
    let Event::Video(bytes) = late.next().await else {
        panic!("image attendue")
    };
    assert!(matches!(
        StreamPacket::decode(&bytes).unwrap(),
        StreamPacket::Frame {
            key: true,
            timestamp_ms: 5,
            ..
        }
    ));
}
