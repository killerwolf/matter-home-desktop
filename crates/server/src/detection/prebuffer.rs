//! Le pré-enregistrement : les ~10 dernières secondes de H.264 **brut** d'une caméra surveillée,
//! en mémoire (ni décodage ni disque), à partir d'une image clé. Quand un passage commence, ce
//! tampon devient le début du clip ([`Recorder::start_clip`]), puis toutes les images suivantes s'y
//! ajoutent jusqu'à la fin du passage et de la suite ([`Recorder::finish_clip`]).
//!
//! Mémoire bornée : le tampon par durée **et** par taille ([`PrebufferConfig::max_bytes`]), le clip
//! par durée et par taille ([`PrebufferConfig::clip_max`], [`PrebufferConfig::clip_max_bytes`]) ;
//! au-delà, le clip s'arrête (il reste lisible).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use axum::body::Bytes;

/// Une image telle que la caméra l'envoie (AVCC, SPS et PPS en tête des images clés).
#[derive(Debug, Clone)]
pub struct Frame {
    pub at: Instant,
    /// Instant de la session de lecture, en millisecondes.
    pub ts_ms: u64,
    pub key: bool,
    pub data: Bytes,
}

#[derive(Debug, Clone, Copy)]
pub struct PrebufferConfig {
    /// Durée gardée avant un passage.
    pub keep: Duration,
    pub max_bytes: usize,
    pub clip_max: Duration,
    pub clip_max_bytes: usize,
}

impl Default for PrebufferConfig {
    fn default() -> Self {
        Self {
            keep: Duration::from_secs(10),
            // Un flux réduit à 1 Mbit/s fait ~1,3 Mo en 10 s : de la marge pour un débit plus haut.
            max_bytes: 8 * 1024 * 1024,
            clip_max: Duration::from_secs(120),
            clip_max_bytes: 48 * 1024 * 1024,
        }
    }
}

/// Un clip terminé : la configuration du flux et ses images, depuis une image clé.
#[derive(Debug, Clone)]
pub struct Clip {
    pub avcc: Vec<u8>,
    pub width: u16,
    pub height: u16,
    pub frames: Vec<Frame>,
    /// Le clip a été coupé (trop long ou trop gros).
    pub truncated: bool,
}

impl Clip {
    pub fn duration(&self) -> Duration {
        match (self.frames.first(), self.frames.last()) {
            (Some(a), Some(b)) => b.at.saturating_duration_since(a.at),
            _ => Duration::ZERO,
        }
    }
}

#[derive(Debug, Default)]
struct Active {
    frames: Vec<Frame>,
    bytes: usize,
    truncated: bool,
}

/// Le tampon et le clip en cours d'une caméra.
#[derive(Debug)]
pub struct Recorder {
    config: PrebufferConfig,
    avcc: Option<(Vec<u8>, u16, u16)>,
    ring: VecDeque<Frame>,
    ring_bytes: usize,
    clip: Option<Active>,
}

impl Recorder {
    pub fn new(config: PrebufferConfig) -> Self {
        Self {
            config,
            avcc: None,
            ring: VecDeque::new(),
            ring_bytes: 0,
            clip: None,
        }
    }

    /// Mémoire occupée par les images (tampon et clip).
    pub fn bytes(&self) -> usize {
        self.ring_bytes + self.clip.as_ref().map_or(0, |c| c.bytes)
    }

    /// Durée couverte par le tampon.
    pub fn buffered(&self) -> Duration {
        match (self.ring.front(), self.ring.back()) {
            (Some(a), Some(b)) => b.at.saturating_duration_since(a.at),
            _ => Duration::ZERO,
        }
    }

    /// Nouvelle configuration du flux (`avcC`). Une configuration différente vide le tampon : les
    /// images d'avant ne se décodent pas avec elle. Un clip en cours s'arrête là.
    pub fn set_config(&mut self, avcc: &[u8], width: u16, height: u16) {
        if self
            .avcc
            .as_ref()
            .is_some_and(|(a, w, h)| a == avcc && *w == width && *h == height)
        {
            return;
        }
        let first = self.avcc.is_none();
        self.avcc = Some((avcc.to_vec(), width, height));
        if !first {
            self.ring.clear();
            self.ring_bytes = 0;
            if let Some(clip) = &mut self.clip {
                clip.truncated = true;
            }
        }
    }

    /// La lecture repart de zéro (nouvelle session) : tout est oublié sauf un clip en cours, qui
    /// s'arrête.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.ring_bytes = 0;
        self.avcc = None;
        if let Some(clip) = &mut self.clip {
            clip.truncated = true;
        }
    }

    pub fn push(&mut self, frame: Frame) {
        if let Some(clip) = &mut self.clip {
            if !clip.truncated {
                let too_long = clip.frames.first().is_some_and(|f| {
                    frame.at.saturating_duration_since(f.at) > self.config.clip_max
                });
                if too_long || clip.bytes + frame.data.len() > self.config.clip_max_bytes {
                    clip.truncated = true;
                } else {
                    clip.bytes += frame.data.len();
                    clip.frames.push(frame.clone());
                }
            }
        }
        // Le tampon commence toujours par une image clé.
        if self.ring.is_empty() && !frame.key {
            return;
        }
        self.ring_bytes += frame.data.len();
        self.ring.push_back(frame);
        self.trim();
    }

    /// Retire du début tant qu'une image clé plus récente couvre encore `keep`, ou que la taille
    /// dépasse `max_bytes` (en gardant au moins la dernière suite d'images).
    fn trim(&mut self) {
        let Some(newest) = self.ring.back().map(|f| f.at) else {
            return;
        };
        loop {
            // La deuxième image clé du tampon.
            let Some(next_key) = self.ring.iter().skip(1).position(|f| f.key).map(|p| p + 1) else {
                return;
            };
            let next_at = self.ring[next_key].at;
            let old_enough = newest.saturating_duration_since(next_at) >= self.config.keep;
            if !old_enough && self.ring_bytes <= self.config.max_bytes {
                return;
            }
            for frame in self.ring.drain(..next_key) {
                self.ring_bytes -= frame.data.len();
            }
        }
    }

    /// Un passage commence : le clip démarre avec le tampon. Sans effet si un clip est en cours.
    pub fn start_clip(&mut self) {
        if self.clip.is_some() {
            return;
        }
        let frames: Vec<Frame> = self.ring.iter().cloned().collect();
        let bytes = frames.iter().map(|f| f.data.len()).sum();
        self.clip = Some(Active {
            frames,
            bytes,
            truncated: false,
        });
    }

    pub fn recording(&self) -> bool {
        self.clip.is_some()
    }

    /// Le passage est fini : le clip (s'il commence par une image clé et que la configuration est
    /// connue).
    pub fn finish_clip(&mut self) -> Option<Clip> {
        let mut active = self.clip.take()?;
        let (avcc, width, height) = self.avcc.clone()?;
        let start = active.frames.iter().position(|f| f.key)?;
        active.frames.drain(..start);
        Some(Clip {
            avcc,
            width,
            height,
            frames: active.frames,
            truncated: active.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 images/s, une image clé toutes les 2 s (20 images), 1 ko chacune.
    fn feed(recorder: &mut Recorder, base: Instant, from: u64, to: u64) {
        for i in from..to {
            recorder.push(Frame {
                at: base + Duration::from_millis(i * 100),
                ts_ms: i * 100,
                key: i % 20 == 0,
                data: Bytes::from(vec![0u8; 1000]),
            });
        }
    }

    #[test]
    fn keeps_about_ten_seconds_from_a_key_frame() {
        let mut r = Recorder::new(PrebufferConfig::default());
        r.set_config(&[1, 2, 3], 640, 360);
        let base = Instant::now();
        feed(&mut r, base, 0, 300); // 30 s
        let b = r.buffered();
        assert!(
            b >= Duration::from_secs(10) && b < Duration::from_secs(12),
            "{b:?}"
        );
        assert!(r.ring.front().unwrap().key);
        assert!(r.bytes() <= 120 * 1000);
    }

    #[test]
    fn starts_at_a_key_frame_and_bounds_memory() {
        let mut r = Recorder::new(PrebufferConfig {
            max_bytes: 30_000,
            ..PrebufferConfig::default()
        });
        let base = Instant::now();
        // Commence au milieu d'une suite : rien avant la clé.
        feed(&mut r, base, 5, 25);
        assert_eq!(r.ring.len(), 5);
        assert!(r.ring.front().unwrap().key);
        feed(&mut r, base, 25, 200);
        assert!(r.bytes() <= 30_000, "{}", r.bytes());
        assert!(r.ring.front().unwrap().key);
    }

    #[test]
    fn clip_is_prebuffer_plus_what_follows() {
        let mut r = Recorder::new(PrebufferConfig::default());
        r.set_config(&[1, 2, 3], 640, 360);
        let base = Instant::now();
        feed(&mut r, base, 0, 150);
        r.start_clip();
        feed(&mut r, base, 150, 230);
        let clip = r.finish_clip().unwrap();
        assert!(clip.frames[0].key);
        assert!(!clip.truncated);
        let d = clip.duration();
        assert!(
            d >= Duration::from_secs(17) && d <= Duration::from_secs(21),
            "{d:?}"
        );
        assert!(!r.recording());
    }

    #[test]
    fn long_clips_are_cut() {
        let mut r = Recorder::new(PrebufferConfig {
            clip_max: Duration::from_secs(5),
            ..PrebufferConfig::default()
        });
        r.set_config(&[1], 1, 1);
        let base = Instant::now();
        feed(&mut r, base, 0, 20);
        r.start_clip();
        feed(&mut r, base, 20, 200);
        let clip = r.finish_clip().unwrap();
        assert!(clip.truncated);
        assert!(clip.duration() <= Duration::from_secs(5));
    }

    #[test]
    fn a_new_configuration_empties_the_buffer() {
        let mut r = Recorder::new(PrebufferConfig::default());
        r.set_config(&[1], 1, 1);
        feed(&mut r, Instant::now(), 0, 30);
        r.set_config(&[1], 1, 1);
        assert!(!r.ring.is_empty(), "même configuration");
        r.set_config(&[2], 1, 1);
        assert!(r.ring.is_empty());
    }
}
