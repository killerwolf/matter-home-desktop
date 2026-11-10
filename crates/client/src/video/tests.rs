use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use protocol::wire::{StreamPacket, CODEC_H264};

use super::nal::{build_avcc, ParameterSets};
use super::*;

const SPS: [u8; 6] = [0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x80];
const SPS_2: [u8; 6] = [0x67, 0x42, 0xc0, 0x28, 0xd9, 0x80];
const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];
const IDR: [u8; 4] = [0x65, 0x88, 0x84, 0x80];
const DELTA: [u8; 3] = [0x41, 0x9a, 0x01];

fn avcc_of(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn config(sps: &[u8]) -> StreamPacket {
    StreamPacket::Config {
        codec: CODEC_H264,
        width: 640,
        height: 360,
        pixel_aspect: (1, 1),
        avcc: build_avcc(sps, &PPS),
    }
}

fn key(sps: Option<&[u8]>, t: u32) -> StreamPacket {
    let data = match sps {
        Some(sps) => avcc_of(&[sps, &PPS, &IDR]),
        None => avcc_of(&[&IDR]),
    };
    StreamPacket::Frame {
        key: true,
        timestamp_ms: t,
        data,
    }
}

fn delta(t: u32) -> StreamPacket {
    StreamPacket::Frame {
        key: false,
        timestamp_ms: t,
        data: avcc_of(&[&DELTA]),
    }
}

#[derive(Default)]
struct Calls {
    starts: Vec<ParameterSets>,
    decoded: Vec<(Vec<u8>, u32)>,
}

/// Un décodeur de pacotille : enregistre ce qu'on lui demande, et échoue quand on le lui dit.
struct Mock {
    calls: std::rc::Rc<std::cell::RefCell<Calls>>,
    fail_next: Option<BackendError>,
    fail_start: Option<BackendError>,
}

impl H264Backend for Mock {
    fn start(&mut self, parameters: &ParameterSets) -> Result<(), BackendError> {
        if let Some(e) = self.fail_start.take() {
            return Err(e);
        }
        self.calls.borrow_mut().starts.push(parameters.clone());
        Ok(())
    }

    fn decode(&mut self, sample: &[u8], timestamp_ms: u32) -> Result<(), BackendError> {
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        self.calls
            .borrow_mut()
            .decoded
            .push((sample.to_vec(), timestamp_ms));
        Ok(())
    }
}

type Shared = std::rc::Rc<std::cell::RefCell<Calls>>;

fn decoder() -> (StreamDecoder<Mock>, Shared) {
    let calls = Shared::default();
    let mock = Mock {
        calls: calls.clone(),
        fail_next: None,
        fail_start: None,
    };
    (StreamDecoder::with_backend(mock), calls)
}

fn backend(d: &mut StreamDecoder<Mock>) -> &mut Mock {
    &mut d.backend
}

#[test]
fn nothing_is_decoded_before_the_first_key_frame() {
    let (mut d, calls) = decoder();
    d.push(&config(&SPS)).unwrap();
    d.push(&delta(1)).unwrap();
    d.push(&delta(2)).unwrap();
    assert!(calls.borrow().decoded.is_empty() && calls.borrow().starts.is_empty());
    d.push(&key(Some(&SPS), 3)).unwrap();
    d.push(&delta(4)).unwrap();
    let calls = calls.borrow();
    assert_eq!(calls.starts.len(), 1);
    // Les jeux de paramètres ne vont pas dans les données : seulement dans la description.
    assert_eq!(calls.decoded[0], (avcc_of(&[&IDR]), 3));
    assert_eq!(calls.decoded[1], (avcc_of(&[&DELTA]), 4));
    assert_eq!(d.stats().dropped, 2);
    assert_eq!(d.info().unwrap().width, 640);
}

#[test]
fn a_key_frame_without_in_band_parameters_uses_the_configuration() {
    let (mut d, calls) = decoder();
    d.push(&key(None, 1)).unwrap();
    assert!(
        calls.borrow().decoded.is_empty(),
        "ni configuration ni SPS : on attend"
    );
    d.push(&config(&SPS)).unwrap();
    d.push(&key(None, 2)).unwrap();
    assert_eq!(calls.borrow().starts[0].sps, SPS.to_vec());
    assert_eq!(calls.borrow().decoded.len(), 1);
}

#[test]
fn a_parameter_change_restarts_the_session_at_the_key_frame_and_only_then() {
    let (mut d, calls) = decoder();
    d.push(&config(&SPS)).unwrap();
    d.push(&key(Some(&SPS), 1)).unwrap();
    d.push(&delta(2)).unwrap();
    // Mêmes paramètres à la clé suivante : pas de nouvelle session.
    d.push(&key(Some(&SPS), 3)).unwrap();
    assert_eq!(calls.borrow().starts.len(), 1);
    // Une nouvelle configuration au milieu d'une suite d'images ne coupe rien.
    d.push(&config(&SPS_2)).unwrap();
    d.push(&delta(4)).unwrap();
    assert_eq!(calls.borrow().starts.len(), 1);
    assert_eq!(calls.borrow().decoded.len(), 4);
    // Le changement prend effet à la clé qui le porte.
    d.push(&key(Some(&SPS_2), 5)).unwrap();
    d.push(&delta(6)).unwrap();
    assert_eq!(calls.borrow().starts.len(), 2);
    assert_eq!(calls.borrow().starts[1].sps, SPS_2.to_vec());
    assert_eq!(calls.borrow().decoded.len(), 6);
    assert_eq!(d.stats().sessions, 2);
}

#[test]
fn a_decode_error_waits_for_the_next_key_frame() {
    let (mut d, calls) = decoder();
    d.push(&key(Some(&SPS), 1)).unwrap();
    backend(&mut d).fail_next = Some(BackendError::BadData);
    d.push(&delta(2)).unwrap();
    // Les images qui dépendent de la perdue ne sont pas montrées.
    d.push(&delta(3)).unwrap();
    d.push(&delta(4)).unwrap();
    assert_eq!(calls.borrow().decoded.len(), 1);
    d.push(&key(Some(&SPS), 5)).unwrap();
    d.push(&delta(6)).unwrap();
    assert_eq!(calls.borrow().decoded.len(), 3);
    assert_eq!(
        calls.borrow().starts.len(),
        1,
        "la session n'était pas perdue"
    );
    assert_eq!(d.stats().errors, 1);
}

#[test]
fn a_lost_session_is_rebuilt_at_the_next_key_frame() {
    let (mut d, calls) = decoder();
    d.push(&key(Some(&SPS), 1)).unwrap();
    backend(&mut d).fail_next = Some(BackendError::SessionLost);
    d.push(&delta(2)).unwrap();
    d.push(&delta(3)).unwrap();
    d.push(&key(Some(&SPS), 4)).unwrap();
    d.push(&delta(5)).unwrap();
    assert_eq!(calls.borrow().starts.len(), 2);
    assert_eq!(calls.borrow().decoded.len(), 3);
}

#[test]
fn a_damaged_frame_never_reaches_the_decoder() {
    let (mut d, calls) = decoder();
    d.push(&key(Some(&SPS), 1)).unwrap();
    let mut truncated = avcc_of(&[&DELTA]);
    truncated.pop();
    d.push(&StreamPacket::Frame {
        key: false,
        timestamp_ms: 2,
        data: truncated,
    })
    .unwrap();
    d.push(&StreamPacket::Frame {
        key: false,
        timestamp_ms: 3,
        data: vec![],
    })
    .unwrap();
    d.push(&delta(4)).unwrap();
    assert_eq!(calls.borrow().decoded.len(), 1);
    assert_eq!(d.stats().errors, 2);
    // Un SPS qui ne fait pas ce qu'il dit, dans une clé : l'image est écartée, rien ne casse.
    d.push(&key(Some(&SPS), 5)).unwrap();
    assert_eq!(calls.borrow().decoded.len(), 2);
}

#[test]
fn a_start_failure_is_absorbed_but_a_missing_decoder_is_final() {
    let (mut d, calls) = decoder();
    backend(&mut d).fail_start = Some(BackendError::BadData);
    d.push(&key(Some(&SPS), 1)).unwrap();
    d.push(&delta(2)).unwrap();
    assert!(calls.borrow().decoded.is_empty());
    d.push(&key(Some(&SPS), 3)).unwrap();
    assert_eq!(calls.borrow().decoded.len(), 1);

    let (mut d, _) = decoder();
    backend(&mut d).fail_start = Some(BackendError::Unavailable("pas de décodeur".into()));
    assert_eq!(
        d.push(&key(Some(&SPS), 1)),
        Err(VideoError::Unavailable("pas de décodeur".into()))
    );
}

#[test]
fn only_h264_is_decoded() {
    let (mut d, _) = decoder();
    assert_eq!(
        d.push(&StreamPacket::Config {
            codec: 2,
            width: 1,
            height: 1,
            pixel_aspect: (1, 1),
            avcc: vec![],
        }),
        Err(VideoError::UnsupportedCodec(2))
    );
}

#[test]
fn a_reset_forgets_the_configuration_and_waits_for_a_key_frame_again() {
    let (mut d, calls) = decoder();
    d.push(&config(&SPS)).unwrap();
    d.push(&key(None, 1)).unwrap();
    d.reset();
    d.push(&delta(2)).unwrap();
    d.push(&key(None, 3)).unwrap();
    assert_eq!(
        calls.borrow().decoded.len(),
        1,
        "ni clé ni configuration valables"
    );
}

#[test]
fn the_sink_keeps_only_the_latest_image_and_wakes_the_interface() {
    let woken = Arc::new(AtomicUsize::new(0));
    let sink = {
        let woken = woken.clone();
        FrameSink::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
    };
    let image = |n: u8| Image {
        width: 1,
        height: 1,
        pixels: vec![eframe::egui::Color32::from_rgb(n, 0, 0)],
    };
    sink.put(image(1));
    sink.put(image(2));
    assert_eq!(
        sink.take().unwrap().pixels[0].r(),
        2,
        "la dernière seulement"
    );
    assert!(sink.take().is_none());
    assert_eq!((sink.frames(), woken.load(Ordering::SeqCst)), (2, 2));
}

// ---------------------------------------------------------------------------
// Le vrai décodeur (VideoToolbox), sur le clip de test du serveur
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod videotoolbox {
    use super::*;
    use crate::video::nal::{annex_b_to_avcc_frames, prepare_sample};

    const CLIP: &[u8] = include_bytes!("../../../server/tests/fixtures/clip-640x360.h264");

    fn real_decoder() -> (Decoder, Arc<FrameSink>) {
        let sink = FrameSink::new(|| {});
        let decoder = Decoder::new(sink.clone()).expect("décodeur");
        (decoder, sink)
    }

    fn packets() -> Vec<StreamPacket> {
        annex_b_to_avcc_frames(CLIP)
            .into_iter()
            .enumerate()
            .map(|(n, data)| StreamPacket::Frame {
                key: prepare_sample(&data).unwrap().idr,
                timestamp_ms: n as u32 * 66,
                data,
            })
            .collect()
    }

    fn mean_luma(image: &Image) -> f64 {
        let sum: u64 = image
            .pixels
            .iter()
            .map(|p| (u64::from(p.r()) * 3 + u64::from(p.g()) * 6 + u64::from(p.b())) / 10)
            .sum();
        sum as f64 / (image.width * image.height) as f64
    }

    #[test]
    fn the_clip_decodes_to_real_images() {
        let (mut d, sink) = real_decoder();
        let packets = packets();
        assert_eq!(packets.len(), 45);
        let mut last = None;
        for packet in &packets {
            d.push(packet).unwrap();
            if let Some(image) = sink.take() {
                last = Some(image);
            }
        }
        // Rendu synchrone : une image par image du clip.
        assert_eq!(sink.frames(), 45);
        let image = last.unwrap();
        assert_eq!((image.width, image.height), (640, 360));
        assert_eq!(image.pixels.len(), 640 * 360);
        assert!(image.pixels.iter().all(|p| p.a() == 255));
        // Les mires de `testsrc` ne sont ni noires ni unies.
        assert!(mean_luma(&image) > 30.0, "image noire ?");
        let distinct: std::collections::HashSet<eframe::egui::Color32> =
            image.pixels.iter().step_by(97).copied().collect();
        assert!(distinct.len() > 8, "image unie ?");
        assert_eq!(d.stats().errors, 0);
        assert_eq!(d.stats().sessions, 1, "une seule session pour tout le clip");
    }

    #[test]
    fn the_decoder_survives_a_start_without_key_garbage_and_a_lost_picture() {
        let (mut d, sink) = real_decoder();
        let packets = packets();
        // Départ au milieu d'une suite : rien n'est décodé avant la clé suivante (image 15).
        for packet in &packets[3..15] {
            d.push(packet).unwrap();
        }
        assert_eq!(sink.frames(), 0);
        assert_eq!(d.stats().dropped, 12);
        // Des octets quelconques, bien formés comme AVCC mais pas du H.264.
        let garbage = StreamPacket::Frame {
            key: false,
            timestamp_ms: 0,
            data: [&[0, 0, 0, 40][..], &[0x41; 40]].concat(),
        };
        d.push(&garbage).unwrap();
        for packet in &packets[15..25] {
            d.push(packet).unwrap();
        }
        let after_first_gop = sink.frames();
        assert!(after_first_gop >= 9, "{after_first_gop}");
        // Une image perdue (la 20e) : on ne montre plus rien d'abîmé avant la clé suivante (30).
        for packet in packets[25..].iter().filter(|_| true) {
            d.push(packet).unwrap();
        }
        assert!(sink.frames() > after_first_gop + 10);
        // Une image corrompue au milieu : le décodeur se rattrape à la clé suivante.
        let (mut d, sink) = real_decoder();
        for (n, packet) in packets.iter().enumerate() {
            let packet = match packet {
                StreamPacket::Frame {
                    key,
                    timestamp_ms,
                    data,
                } if n == 5 => StreamPacket::Frame {
                    key: *key,
                    timestamp_ms: *timestamp_ms,
                    data: data.iter().map(|b| b ^ 0x5a).collect(),
                },
                other => other.clone(),
            };
            d.push(&packet).unwrap();
        }
        assert!(sink.frames() >= 30, "{}", sink.frames());
        assert!(sink.take().is_some());
    }
}
