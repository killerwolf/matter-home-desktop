//! Fausse caméra RTSP (aucun trafic réel : tout reste sur 127.0.0.1), qui joue en boucle un petit
//! clip H.264 gardé dans `tests/fixtures/`. Partagée par les tests du relais (`cameras/live.rs`,
//! `tests/camera_stream.rs`) et par l'exemple `camera_demo`, qui l'incluent avec `#[path]`.
//!
//! Elle parle assez de RTSP pour `retina` : `OPTIONS`, `DESCRIBE` (SDP du clip, avec ses SPS et
//! PPS), `SETUP` en TCP entrelacé (elle reprend les canaux demandés), `PLAY` (paquets RTP : une
//! image par paquet ou découpée en FU-A, marqueur sur le dernier paquet de l'image),
//! `GET_PARAMETER`/`SET_PARAMETER` (maintien de session) et `TEARDOWN`.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// Le clip : 45 images de 640 × 360 à 15 images par seconde, une image clé toutes les 15.
pub const CLIP: &[u8] = include_bytes!("../fixtures/clip-640x360.h264");
pub const CLIP_WIDTH: u16 = 640;
pub const CLIP_HEIGHT: u16 = 360;

#[derive(Clone)]
pub struct Options {
    pub fps: u32,
    /// Authentification Basic exigée (`utilisateur`, `mot de passe`).
    pub basic: Option<(&'static str, &'static str)>,
    /// Annonce H.265 dans le SDP (pour tester le refus d'un codec non relayé).
    pub h265: bool,
    /// Coupe la connexion après ce nombre d'images envoyées (la caméra « décroche »).
    pub cut_after_frames: Option<usize>,
    /// Première image du clip envoyée : au-delà de 0, la lecture démarre au milieu d'une suite
    /// d'images, sans image clé d'abord.
    pub start_at_frame: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fps: 15,
            basic: None,
            h265: false,
            cut_after_frames: None,
            start_at_frame: 0,
        }
    }
}

/// Ce que la fausse caméra a vu.
#[derive(Default)]
pub struct Stats {
    /// Connexions TCP ouvertes en ce moment.
    pub open: AtomicUsize,
    /// Connexions TCP reçues depuis le début.
    pub accepted: AtomicUsize,
    pub describes: AtomicUsize,
    /// Nombre de `PLAY` reçus depuis le début.
    pub plays: AtomicUsize,
    /// Sessions en lecture en ce moment.
    pub playing: AtomicUsize,
    /// Images envoyées au total.
    pub frames_sent: AtomicUsize,
    /// Refuse (ferme aussitôt) les nouvelles connexions : caméra éteinte.
    pub refuse: AtomicBool,
}

pub struct FakeCamera {
    pub port: u16,
    pub stats: Arc<Stats>,
    cut: watch::Sender<u64>,
}

impl FakeCamera {
    /// Ferme toutes les connexions en cours (la caméra redémarre).
    pub fn cut(&self) {
        self.cut.send_modify(|n| *n += 1);
    }

    pub fn open(&self) -> usize {
        self.stats.open.load(Ordering::SeqCst)
    }

    pub fn playing(&self) -> usize {
        self.stats.playing.load(Ordering::SeqCst)
    }

    pub fn plays(&self) -> usize {
        self.stats.plays.load(Ordering::SeqCst)
    }
}

pub async fn start(options: Options) -> FakeCamera {
    start_on("127.0.0.1:0", options).await
}

pub async fn start_on(addr: &str, options: Options) -> FakeCamera {
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stats = Arc::new(Stats::default());
    let (cut, cut_rx) = watch::channel(0u64);
    let accepted_stats = stats.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            accepted_stats.accepted.fetch_add(1, Ordering::SeqCst);
            if accepted_stats.refuse.load(Ordering::SeqCst) {
                continue;
            }
            let (options, stats, cut_rx) =
                (options.clone(), accepted_stats.clone(), cut_rx.clone());
            tokio::spawn(async move {
                stats.open.fetch_add(1, Ordering::SeqCst);
                serve(socket, port, options, stats.clone(), cut_rx).await;
                stats.open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    FakeCamera { port, stats, cut }
}

// ---------------------------------------------------------------------------
// Le clip, découpé en images
// ---------------------------------------------------------------------------

/// Les unités NAL (sans leur code de début) d'un flux Annex B.
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        // Les zéros qui précèdent le code suivant (code sur 4 octets) ne font pas partie de la NAL.
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        nals.push(&data[start..end]);
    }
    nals
}

/// Une image : ses NAL (jeux de paramètres et SEI qui la précèdent compris) et si c'est une clé.
pub struct AccessUnit<'a> {
    pub nals: Vec<&'a [u8]>,
    pub key: bool,
}

pub fn access_units(data: &[u8]) -> Vec<AccessUnit<'_>> {
    let mut units = Vec::new();
    let mut pending: Vec<&[u8]> = Vec::new();
    for nal in split_annex_b(data) {
        pending.push(nal);
        let kind = nal[0] & 0x1f;
        if kind == 1 || kind == 5 {
            units.push(AccessUnit {
                nals: std::mem::take(&mut pending),
                key: kind == 5,
            });
        }
    }
    units
}

/// Le SPS et le PPS du clip.
pub fn parameter_sets() -> (Vec<u8>, Vec<u8>) {
    let nals = split_annex_b(CLIP);
    let find = |kind: u8| {
        nals.iter()
            .find(|n| n[0] & 0x1f == kind)
            .expect("jeu de paramètres dans le clip")
            .to_vec()
    };
    (find(7), find(8))
}

fn sdp(port: u16, h265: bool) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let (sps, pps) = parameter_sets();
    let video = if h265 {
        "m=video 0 RTP/AVP 97\r\na=rtpmap:97 H265/90000\r\na=control:trackID=1\r\n".to_string()
    } else {
        format!(
            "m=video 0 RTP/AVP 97\r\na=rtpmap:97 H264/90000\r\n\
             a=fmtp:97 packetization-mode=1;profile-level-id={:02X}{:02X}{:02X};sprop-parameter-sets={},{}\r\n\
             a=control:trackID=1\r\n",
            sps[1],
            sps[2],
            sps[3],
            b64.encode(&sps),
            b64.encode(&pps)
        )
    };
    let _ = port;
    format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=Fausse camera\r\nt=0 0\r\na=control:*\r\n{video}")
}

// ---------------------------------------------------------------------------
// RTSP
// ---------------------------------------------------------------------------

const MTU: usize = 1400;

struct Rtp {
    seq: u16,
    timestamp: u32,
}

/// Les paquets RTP (H.264, mode 1) d'une image.
fn packetize(unit: &AccessUnit<'_>, rtp: &mut Rtp) -> Vec<Vec<u8>> {
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for nal in &unit.nals {
        if nal.len() <= MTU {
            payloads.push(nal.to_vec());
        } else {
            let (indicator, kind) = ((nal[0] & 0xe0) | 28, nal[0] & 0x1f);
            let body = &nal[1..];
            let chunks: Vec<&[u8]> = body.chunks(MTU - 2).collect();
            for (i, chunk) in chunks.iter().enumerate() {
                let mut header = kind;
                if i == 0 {
                    header |= 0x80;
                }
                if i + 1 == chunks.len() {
                    header |= 0x40;
                }
                let mut p = vec![indicator, header];
                p.extend_from_slice(chunk);
                payloads.push(p);
            }
        }
    }
    let last = payloads.len() - 1;
    payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let mut packet = Vec::with_capacity(12 + payload.len());
            packet.push(0x80);
            packet.push(97 | if i == last { 0x80 } else { 0 });
            packet.extend_from_slice(&rtp.seq.to_be_bytes());
            packet.extend_from_slice(&rtp.timestamp.to_be_bytes());
            packet.extend_from_slice(&0x1234_5678u32.to_be_bytes());
            packet.extend_from_slice(&payload);
            rtp.seq = rtp.seq.wrapping_add(1);
            packet
        })
        .collect()
}

fn reply(cseq: &str, status: &str, extra: &str, body: &str) -> String {
    format!(
        "RTSP/1.0 {status}\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(
    mut socket: TcpStream,
    port: u16,
    options: Options,
    stats: Arc<Stats>,
    mut cut: watch::Receiver<u64>,
) {
    let units_all = access_units(CLIP);
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut channel: Option<u8> = None;
    let mut playing = false;
    let mut ticker =
        tokio::time::interval(Duration::from_millis(1000 / u64::from(options.fps.max(1))));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut next_unit = options.start_at_frame % units_all.len();
    let mut rtp = Rtp {
        seq: 0,
        timestamp: 0,
    };
    let mut sent = 0usize;
    let _ = cut.borrow_and_update();
    let result: std::io::Result<()> = async {
        loop {
            tokio::select! {
                read = socket.read(&mut chunk) => {
                    let n = read?;
                    if n == 0 {
                        return Ok(());
                    }
                    pending.extend_from_slice(&chunk[..n]);
                    while let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&pending[..end]).into_owned();
                        pending.drain(..end + 4);
                        let cseq = head
                            .lines()
                            .find_map(|l| l.strip_prefix("CSeq: "))
                            .unwrap_or("1")
                            .trim()
                            .to_string();
                        let method = head.split_whitespace().next().unwrap_or("");
                        if let Some((user, password)) = options.basic {
                            let expected = format!(
                                "Authorization: Basic {}",
                                base64::engine::general_purpose::STANDARD
                                    .encode(format!("{user}:{password}"))
                            );
                            if !head.lines().any(|l| l == expected) {
                                let r = reply(
                                    &cseq,
                                    "401 Unauthorized",
                                    "WWW-Authenticate: Basic realm=\"camera\"\r\n",
                                    "",
                                );
                                socket.write_all(r.as_bytes()).await?;
                                continue;
                            }
                        }
                        let response = match method {
                            "OPTIONS" => reply(
                                &cseq,
                                "200 OK",
                                "Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER, SET_PARAMETER\r\n",
                                "",
                            ),
                            "DESCRIBE" => {
                                stats.describes.fetch_add(1, Ordering::SeqCst);
                                reply(
                                    &cseq,
                                    "200 OK",
                                    &format!(
                                        "Content-Type: application/sdp\r\nContent-Base: rtsp://127.0.0.1:{port}/live/\r\n"
                                    ),
                                    &sdp(port, options.h265),
                                )
                            }
                            "SETUP" => {
                                let requested = head
                                    .lines()
                                    .find_map(|l| l.strip_prefix("Transport: "))
                                    .and_then(|t| t.split(';').find_map(|p| p.strip_prefix("interleaved=")))
                                    .unwrap_or("0-1")
                                    .to_string();
                                channel = requested.split('-').next().and_then(|c| c.parse().ok());
                                reply(
                                    &cseq,
                                    "200 OK",
                                    &format!(
                                        "Transport: RTP/AVP/TCP;unicast;interleaved={requested};ssrc=12345678\r\nSession: 4242;timeout=60\r\n"
                                    ),
                                    "",
                                )
                            }
                            "PLAY" => {
                                playing = true;
                                stats.plays.fetch_add(1, Ordering::SeqCst);
                                stats.playing.fetch_add(1, Ordering::SeqCst);
                                reply(
                                    &cseq,
                                    "200 OK",
                                    &format!(
                                        "Session: 4242\r\nRTP-Info: url=rtsp://127.0.0.1:{port}/live/trackID=1;seq=0;rtptime=0\r\n"
                                    ),
                                    "",
                                )
                            }
                            "TEARDOWN" => {
                                let r = reply(&cseq, "200 OK", "Session: 4242\r\n", "");
                                socket.write_all(r.as_bytes()).await?;
                                return Ok(());
                            }
                            // GET_PARAMETER, SET_PARAMETER : maintien de session.
                            _ => reply(&cseq, "200 OK", "Session: 4242\r\n", ""),
                        };
                        socket.write_all(response.as_bytes()).await?;
                    }
                }
                _ = ticker.tick(), if playing => {
                    let unit = &units_all[next_unit];
                    next_unit = (next_unit + 1) % units_all.len();
                    let ch = channel.unwrap_or(0);
                    for packet in packetize(unit, &mut rtp) {
                        let mut framed = Vec::with_capacity(4 + packet.len());
                        framed.push(b'$');
                        framed.push(ch);
                        framed.extend_from_slice(&(packet.len() as u16).to_be_bytes());
                        framed.extend_from_slice(&packet);
                        socket.write_all(&framed).await?;
                    }
                    rtp.timestamp = rtp.timestamp.wrapping_add(90_000 / options.fps.max(1));
                    sent += 1;
                    stats.frames_sent.fetch_add(1, Ordering::SeqCst);
                    if options.cut_after_frames.is_some_and(|n| sent >= n) {
                        return Ok(());
                    }
                }
                _ = cut.changed() => return Ok(()),
            }
        }
    }
    .await;
    let _ = result;
    if playing {
        stats.playing.fetch_sub(1, Ordering::SeqCst);
    }
}
