//! Un MP4 minimal, **sans réencodage** : les images H.264 de la caméra (format AVCC, longueur sur
//! 4 octets, celui du MP4) sont recopiées telles quelles dans `mdat`, l'`avcC` vient de la
//! configuration du flux.
//!
//! Une seule piste vidéo, un seul bloc (`chunk`), `moov` **avant** `mdat` (« faststart ») pour que
//! Safari sur iPhone lise le clip sans tout télécharger. Écrit à la main (une centaine de lignes)
//! plutôt qu'avec une caisse : il n'y a qu'un cas, et rien à maintenir de plus.

/// Une image du clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Instant de l'image, en millisecondes (croissant).
    pub ts_ms: u64,
    pub key: bool,
    /// Données AVCC **sans** SPS ni PPS (ils sont dans l'`avcC`).
    pub data: Vec<u8>,
}

/// Durée donnée à la dernière image (et à une image dont l'instant ne progresse pas).
const DEFAULT_DELTA_MS: u32 = 66;
const TIMESCALE: u32 = 1000;

fn bx(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 8);
    out.extend_from_slice(&((content.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(content);
    out
}

/// Une boîte « pleine » : version et drapeaux d'abord.
fn full(kind: &[u8; 4], version: u8, flags: u32, content: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(content.len() + 4);
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(content);
    bx(kind, &body)
}

struct W(Vec<u8>);

impl W {
    fn new() -> Self {
        W(Vec::new())
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.extend(std::iter::repeat_n(0, n));
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    fn matrix(&mut self) -> &mut Self {
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(v);
        }
        self
    }
}

/// Pourquoi un clip n'a pas pu être fait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxError {
    Empty,
    /// La première image n'est pas une image clé.
    NoKeyFrame,
    TooLarge,
}

/// Le fichier MP4 complet. Les images doivent commencer par une image clé.
pub fn mux(avcc: &[u8], width: u16, height: u16, samples: &[Sample]) -> Result<Vec<u8>, MuxError> {
    let first = samples.first().ok_or(MuxError::Empty)?;
    if !first.key {
        return Err(MuxError::NoKeyFrame);
    }
    let total: usize = samples.iter().map(|s| s.data.len()).sum();
    if total > u32::MAX as usize / 2 {
        return Err(MuxError::TooLarge);
    }
    // Durées : écart avec l'image suivante.
    let deltas: Vec<u32> = samples
        .windows(2)
        .map(|w| {
            let d = w[1].ts_ms.saturating_sub(w[0].ts_ms);
            if d == 0 || d > 10_000 {
                DEFAULT_DELTA_MS
            } else {
                d as u32
            }
        })
        .chain(std::iter::once(DEFAULT_DELTA_MS))
        .collect();
    let duration: u64 = deltas.iter().map(|d| u64::from(*d)).sum();
    let duration = u32::try_from(duration).unwrap_or(u32::MAX);

    let ftyp = bx(
        b"ftyp",
        &W::new()
            .bytes(b"isom")
            .u32(0x200)
            .bytes(b"isomiso2avc1mp41")
            .0,
    );
    let moov_with = |offset: u32| moov(avcc, width, height, samples, &deltas, duration, offset);
    let moov_len = moov_with(0).len();
    let data_offset = (ftyp.len() + moov_len + 8) as u32;
    let moov = moov_with(data_offset);

    let mut out = Vec::with_capacity(data_offset as usize + total);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&((total + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for s in samples {
        out.extend_from_slice(&s.data);
    }
    Ok(out)
}

fn moov(
    avcc: &[u8],
    width: u16,
    height: u16,
    samples: &[Sample],
    deltas: &[u32],
    duration: u32,
    data_offset: u32,
) -> Vec<u8> {
    let mvhd = full(
        b"mvhd",
        0,
        0,
        &W::new()
            .u32(0)
            .u32(0)
            .u32(TIMESCALE)
            .u32(duration)
            .u32(0x0001_0000)
            .u16(0x0100)
            .zeros(10)
            .matrix()
            .zeros(24)
            .u32(2)
            .0,
    );
    let tkhd = full(
        b"tkhd",
        0,
        3,
        &W::new()
            .u32(0)
            .u32(0)
            .u32(1)
            .u32(0)
            .u32(duration)
            .zeros(8)
            .u16(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .matrix()
            .u32(u32::from(width) << 16)
            .u32(u32::from(height) << 16)
            .0,
    );
    let mdhd = full(
        b"mdhd",
        0,
        0,
        &W::new()
            .u32(0)
            .u32(0)
            .u32(TIMESCALE)
            .u32(duration)
            .u16(0x55C4) // « und »
            .u16(0)
            .0,
    );
    let hdlr = full(
        b"hdlr",
        0,
        0,
        &W::new()
            .u32(0)
            .bytes(b"vide")
            .zeros(12)
            .bytes(b"VideoHandler\0")
            .0,
    );
    let vmhd = full(b"vmhd", 0, 1, &W::new().zeros(8).0);
    let dinf = bx(
        b"dinf",
        &full(
            b"dref",
            0,
            0,
            &W::new().u32(1).bytes(&full(b"url ", 0, 1, &[])).0,
        ),
    );
    let avc1 = bx(
        b"avc1",
        &W::new()
            .zeros(6)
            .u16(1)
            .zeros(16)
            .u16(width)
            .u16(height)
            .u32(0x0048_0000)
            .u32(0x0048_0000)
            .u32(0)
            .u16(1)
            .zeros(32)
            .u16(0x0018)
            .u16(0xFFFF)
            .bytes(&bx(b"avcC", avcc))
            .0,
    );
    let stsd = full(b"stsd", 0, 0, &W::new().u32(1).bytes(&avc1).0);
    // Durées regroupées par suites identiques.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for d in deltas {
        match runs.last_mut() {
            Some((n, v)) if v == d => *n += 1,
            _ => runs.push((1, *d)),
        }
    }
    let mut stts = W::new();
    stts.u32(runs.len() as u32);
    for (n, d) in &runs {
        stts.u32(*n).u32(*d);
    }
    let keys: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.key)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    let mut stss = W::new();
    stss.u32(keys.len() as u32);
    for k in &keys {
        stss.u32(*k);
    }
    let stsc = W::new()
        .u32(1)
        .u32(1)
        .u32(samples.len() as u32)
        .u32(1)
        .0
        .clone();
    let mut stsz = W::new();
    stsz.u32(0).u32(samples.len() as u32);
    for s in samples {
        stsz.u32(s.data.len() as u32);
    }
    let stco = W::new().u32(1).u32(data_offset).0.clone();
    let stbl = bx(
        b"stbl",
        &[
            stsd,
            full(b"stts", 0, 0, &stts.0),
            full(b"stss", 0, 0, &stss.0),
            full(b"stsc", 0, 0, &stsc),
            full(b"stsz", 0, 0, &stsz.0),
            full(b"stco", 0, 0, &stco),
        ]
        .concat(),
    );
    let minf = bx(b"minf", &[vmhd, dinf, stbl].concat());
    let mdia = bx(b"mdia", &[mdhd, hdlr, minf].concat());
    let trak = bx(b"trak", &[tkhd, mdia].concat());
    bx(b"moov", &[mvhd, trak].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les boîtes de premier niveau : (type, début, taille).
    fn top_level(file: &[u8]) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= file.len() {
            let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            out.push((
                String::from_utf8_lossy(&file[at + 4..at + 8]).into_owned(),
                at,
                size,
            ));
            assert!(size >= 8, "taille de boîte");
            at += size;
        }
        assert_eq!(at, file.len(), "les boîtes couvrent tout le fichier");
        out
    }

    fn find(file: &[u8], kind: &[u8; 4]) -> usize {
        file.windows(4)
            .position(|w| w == kind)
            .expect("boîte présente")
    }

    fn samples() -> Vec<Sample> {
        (0..5)
            .map(|i| Sample {
                ts_ms: 1000 + i * 100,
                key: i % 3 == 0,
                data: vec![0, 0, 0, 2, 0x65, i as u8],
            })
            .collect()
    }

    #[test]
    fn faststart_layout_and_offsets() {
        let file = mux(&[1, 0x64, 0, 0x1f, 0xff], 640, 360, &samples()).unwrap();
        let boxes = top_level(&file);
        let kinds: Vec<&str> = boxes.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
        // `stco` pointe sur le début des données de `mdat`.
        let stco = find(&file, b"stco");
        let offset = u32::from_be_bytes(file[stco + 12..stco + 16].try_into().unwrap()) as usize;
        assert_eq!(offset, boxes[2].1 + 8);
        assert_eq!(&file[offset..offset + 6], &[0, 0, 0, 2, 0x65, 0]);
        // Images clés : 1 et 4.
        let stss = find(&file, b"stss");
        assert_eq!(
            &file[stss + 8..stss + 20],
            &[0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 4]
        );
        // Durée : 4 × 100 + 66.
        let mvhd = find(&file, b"mvhd");
        let duration = u32::from_be_bytes(file[mvhd + 20..mvhd + 24].try_into().unwrap());
        assert_eq!(duration, 466);
        // L'avcC est recopié tel quel.
        let avcc = find(&file, b"avcC");
        assert_eq!(&file[avcc + 4..avcc + 9], &[1, 0x64, 0, 0x1f, 0xff]);
    }

    #[test]
    fn refuses_empty_or_keyless_clips() {
        assert_eq!(mux(&[1], 1, 1, &[]), Err(MuxError::Empty));
        let mut s = samples();
        s.remove(0);
        assert_eq!(mux(&[1], 1, 1, &s), Err(MuxError::NoKeyFrame));
    }
}
