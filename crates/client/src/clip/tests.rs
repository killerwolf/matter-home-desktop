use super::*;
use ostal_video::mp4::SampleRef;

/// 10 images à 100 ms, une clé toutes les 4 (0, 4, 8).
fn track() -> Track {
    Track {
        width: 640,
        height: 360,
        avcc: vec![1],
        samples: (0..10)
            .map(|i| SampleRef {
                offset: 0,
                size: 0,
                ts_ms: i * 100,
                key: i % 4 == 0,
            })
            .collect(),
        duration_ms: 1000,
    }
}

#[test]
fn the_clock_runs_pauses_and_stops_at_the_end() {
    let mut clock = Clock::default();
    assert!(!clock.playing());
    assert_eq!(clock.position(5000, 1000), 0);
    clock.play(1000, 1000);
    assert!(clock.playing());
    assert_eq!(clock.position(1250, 1000), 250);
    clock.pause(1400, 1000);
    // En pause, le temps ne compte plus.
    assert_eq!(clock.position(9000, 1000), 400);
    clock.play(9000, 1000);
    assert_eq!(clock.position(9100, 1000), 500);
    // Borné à la durée.
    assert_eq!(clock.position(20_000, 1000), 1000);
}

#[test]
fn a_seek_keeps_the_play_state() {
    let mut clock = Clock::default();
    clock.play(100, 1000);
    clock.seek(700, 300);
    assert!(clock.playing());
    assert_eq!(clock.position(400, 1000), 800);
    clock.pause(400, 1000);
    clock.seek(200, 500);
    assert!(!clock.playing());
    assert_eq!(clock.position(900, 1000), 200);
}

#[test]
fn frames_are_due_at_their_mp4_timestamp_and_late_ones_are_skipped() {
    let queue = [100u64, 200, 300, 400];
    assert_eq!(due_count(queue.iter().copied(), 0), 0);
    assert_eq!(due_count(queue.iter().copied(), 99), 0);
    assert_eq!(due_count(queue.iter().copied(), 100), 1);
    assert_eq!(due_count(queue.iter().copied(), 250), 2);
    // En retard de plusieurs images : toutes sont dues (la dernière est montrée).
    assert_eq!(due_count(queue.iter().copied(), 10_000), 4);
    assert_eq!(due_count(std::iter::empty(), 10), 0);
}

#[test]
fn a_seek_restarts_at_the_preceding_key_frame_and_shows_from_the_target() {
    let track = track();
    assert_eq!(
        plan_seek(&track, 0),
        SeekPlan {
            start: 0,
            target: 0
        }
    );
    // 650 ms : image 6, clé précédente 4.
    assert_eq!(
        plan_seek(&track, 650),
        SeekPlan {
            start: 4,
            target: 6
        }
    );
    // Exactement sur une clé.
    assert_eq!(
        plan_seek(&track, 800),
        SeekPlan {
            start: 8,
            target: 8
        }
    );
    // Au-delà de la fin : la dernière image.
    assert_eq!(
        plan_seek(&track, 5000),
        SeekPlan {
            start: 8,
            target: 9
        }
    );
}

#[test]
fn time_is_shown_as_minutes_and_seconds() {
    assert_eq!(format_time(0), "0:00");
    assert_eq!(format_time(7_999), "0:07");
    assert_eq!(format_time(65_000), "1:05");
    assert_eq!(format_time(600_000), "10:00");
}

#[test]
fn a_clip_that_cannot_be_fetched_shows_a_french_error_not_a_crash() {
    let player = ClipPlayer::open("pas un identifiant", || {});
    let start = Instant::now();
    while player.error().is_none() && start.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let error = player.error().expect("une erreur");
    assert!(error.starts_with("Clip indisponible"), "{error}");
    assert_eq!(player.duration_ms(), 0);
    assert!(!player.loading());
}

#[test]
fn a_corrupt_file_is_reported_in_french() {
    let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let Err(error) = start(vec![0xAB; 64], notify) else {
        panic!("un fichier corrompu doit être refusé");
    };
    assert!(error.starts_with("Clip illisible"), "{error}");
}

/// Un MP4 à la façon du muxer du serveur (`ftyp`, `moov` avec `stts`/`stss`/`stsc`/`stsz`/`stco`,
/// puis `mdat`), fait de la vraie vidéo H.264 de l'essai du serveur.
#[cfg(target_os = "macos")]
fn real_clip() -> Vec<u8> {
    use ostal_video::nal::{annex_b_to_avcc_frames, build_avcc, prepare_sample};
    fn bx(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut out = ((content.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(content);
        out
    }
    fn full(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
        bx(kind, &[&[0u8; 4][..], content].concat())
    }
    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }
    const H264: &[u8] = include_bytes!("../../../server/tests/fixtures/clip-640x360.h264");
    let mut avcc = vec![];
    let mut datas = vec![];
    let mut keys = vec![];
    for (n, frame) in annex_b_to_avcc_frames(H264).iter().enumerate() {
        let sample = prepare_sample(frame).unwrap();
        if let Some(p) = &sample.parameters {
            avcc = build_avcc(&p.sps, &p.pps);
        }
        if sample.idr {
            keys.push(n as u32 + 1);
        }
        datas.push(sample.data);
    }
    let n = datas.len() as u32;
    let mut avc1 = vec![0u8; 24];
    avc1.extend_from_slice(&640u16.to_be_bytes());
    avc1.extend_from_slice(&360u16.to_be_bytes());
    avc1.extend_from_slice(&[0u8; 50]);
    avc1.extend_from_slice(&bx(b"avcC", &avcc));
    let sizes: Vec<u32> = datas.iter().map(|d| d.len() as u32).collect();
    let stbl = |offset: u32| {
        bx(
            b"stbl",
            &[
                full(b"stsd", &[&u32s(&[1])[..], &bx(b"avc1", &avc1)].concat()),
                full(b"stts", &u32s(&[1, n, 66])),
                full(
                    b"stss",
                    &[&u32s(&[keys.len() as u32])[..], &u32s(&keys)].concat(),
                ),
                full(b"stsc", &u32s(&[1, 1, n, 1])),
                full(b"stsz", &[&u32s(&[0, n])[..], &u32s(&sizes)].concat()),
                full(b"stco", &u32s(&[1, offset])),
            ]
            .concat(),
        )
    };
    let moov = |offset: u32| {
        let mdhd = full(b"mdhd", &u32s(&[0, 0, 1000, n * 66, 0]));
        let mdia = bx(b"mdia", &[mdhd, bx(b"minf", &stbl(offset))].concat());
        bx(b"moov", &bx(b"trak", &mdia))
    };
    let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
    let offset = (ftyp.len() + moov(0).len() + 8) as u32;
    [ftyp, moov(offset), bx(b"mdat", &datas.concat())].concat()
}

/// Bout en bout avec le vrai décodeur : la lecture va au rythme du MP4 jusqu'au bout et s'arrête,
/// un déplacement redonne une image, la boucle recommence.
#[cfg(target_os = "macos")]
#[test]
fn a_real_clip_plays_to_the_end_seeks_and_loops() {
    let mut player = ClipPlayer::from_bytes("1-1", real_clip(), || {});
    let run = |player: &mut ClipPlayer, ms: u128, until: &dyn Fn(&ClipPlayer) -> bool| -> usize {
        let start = Instant::now();
        let mut images = 0;
        while !until(player) && start.elapsed().as_millis() < ms {
            if player.update().is_some() {
                images += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        images
    };
    // Lecture complète : 45 images × 66 ms ≈ 3 s.
    let began = Instant::now();
    // (La lecture démarre à la première image décodée : on ne regarde l'arrêt qu'après.)
    let images = run(&mut player, 20_000, &|p| {
        !p.waiting_for_start() && !p.playing()
    });
    assert!(player.error().is_none(), "{:?}", player.error());
    assert_eq!(player.duration_ms(), 45 * 66);
    assert!(images >= 20, "{images} images");
    assert!(began.elapsed().as_millis() >= 2800, "lecture trop rapide");
    assert!(!player.playing());
    // Déplacement au milieu, en pause : l'image de l'instant arrive, la lecture reste arrêtée.
    player.seek(1500);
    let images = run(&mut player, 5_000, &|_| false);
    assert_eq!(images, 1, "une seule image après un déplacement en pause");
    assert!(!player.playing());
    // Avec la boucle, la lecture continue au-delà de la durée du clip.
    player.looping = true;
    player.seek(0);
    player.toggle_play();
    let images = run(&mut player, 4_500, &|_| false);
    assert!(player.playing(), "la boucle continue");
    assert!(player.error().is_none());
    assert!(images >= 30, "{images} images");
}

/// Régression : après la fin, ▶ replace au début ; `update` ne doit pas conclure à la fin du clip
/// sur l'état d'avant le déplacement (clip « fini », file vide) et remettre aussitôt en pause.
#[cfg(target_os = "macos")]
#[test]
fn play_after_the_end_restarts_and_does_not_pause_again() {
    let mut player = ClipPlayer::from_bytes("1-1", real_clip(), || {});
    let start = Instant::now();
    while (player.waiting_for_start() || player.playing()) && start.elapsed().as_secs() < 20 {
        player.update();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!player.playing());
    // À l'arrêt, la position est la durée du clip.
    assert_eq!(player.position_ms(), 45 * 66);
    player.toggle_play();
    player.update();
    assert!(player.playing(), "remis en pause tout de suite");
    assert!(player.position_ms() < 1000);
}
