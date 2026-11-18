//! Démultiplexeur MP4 minimal, pour lire les clips des événements (ticket n° 22).
//!
//! Il lit ce qu'écrit le muxer du serveur (`crates/server/src/detection/mp4.rs`) : `ftyp`, `moov`
//! (avec `avcC`, `stts`, `stss`, `stsc`, `stsz`, `stco`) puis `mdat`, une seule piste H.264. Il reste
//! **tolérant** : `moov` avant ou après `mdat`, tailles sur 64 bits, `co64`, `stsz` à taille
//! constante, plusieurs blocs (`stsc`), absence de `stss` (toutes les images sont des clés), boîtes
//! inconnues ignorées. Les décalages de composition (`ctts`, images B) ne sont pas gérés : les
//! caméras n'en produisent pas.
//!
//! Rien n'est copié : la piste ne garde que, pour chaque image, sa place dans le fichier.

/// Où est une image dans le fichier, et quand elle est montrée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRef {
    pub offset: u64,
    pub size: u32,
    /// Instant de l'image depuis le début du clip, en millisecondes.
    pub ts_ms: u64,
    pub key: bool,
}

/// La piste vidéo d'un clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub width: u32,
    pub height: u32,
    /// `AVCDecoderConfigurationRecord` (SPS et PPS).
    pub avcc: Vec<u8>,
    pub samples: Vec<SampleRef>,
    /// Durée totale du clip, en millisecondes.
    pub duration_ms: u64,
}

/// Pourquoi un fichier n'est pas lisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Error {
    /// Pas de `moov`, ou boîte tronquée.
    Truncated,
    /// Pas de piste H.264 (`avc1` + `avcC`).
    NoVideoTrack,
    /// Tables d'images absentes, incohérentes ou vides.
    BadTables,
}

impl std::fmt::Display for Mp4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mp4Error::Truncated => "fichier vidéo tronqué ou illisible",
            Mp4Error::NoVideoTrack => "pas de piste vidéo H.264 dans le clip",
            Mp4Error::BadTables => "index des images du clip invalide",
        })
    }
}

impl std::error::Error for Mp4Error {}

/// Une boîte : son type et son contenu.
struct BoxRef<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// Les boîtes d'un niveau. Une boîte qui déborde arrête la lecture (un fichier coupé garde son
/// `moov` s'il est entier, mais pas un `mdat` incomplet).
fn boxes(data: &[u8]) -> Vec<BoxRef<'_>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size32 = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match size32 {
            0 => (8, (data.len() - at) as u64),
            1 => {
                let Some(big) = be64(data, at + 8) else { break };
                (16, big)
            }
            n => (8, u64::from(n)),
        };
        if size < header as u64 {
            break;
        }
        let end = at as u64 + size;
        if end > data.len() as u64 {
            break;
        }
        out.push(BoxRef {
            kind,
            body: &data[at + header..end as usize],
        });
        at = end as usize;
    }
    out
}

fn child<'a>(parent: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(parent)
        .into_iter()
        .find(|b| &b.kind == kind)
        .map(|b| b.body)
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Le corps d'une boîte « pleine » sans version ni drapeaux.
fn full_body(body: &[u8]) -> Option<&[u8]> {
    body.get(4..)
}

/// Lit le clip : la première piste qui porte du H.264.
pub fn parse(file: &[u8]) -> Result<Track, Mp4Error> {
    let top = boxes(file);
    let moov = top
        .iter()
        .find(|b| &b.kind == b"moov")
        .ok_or(Mp4Error::Truncated)?;
    for trak in boxes(moov.body).into_iter().filter(|b| &b.kind == b"trak") {
        if let Some(track) = parse_track(trak.body, file.len() as u64)? {
            return Ok(track);
        }
    }
    Err(Mp4Error::NoVideoTrack)
}

/// `Ok(None)` : une piste qui n'est pas du H.264 (son…).
fn parse_track(trak: &[u8], file_len: u64) -> Result<Option<Track>, Mp4Error> {
    let Some(mdia) = child(trak, b"mdia") else {
        return Ok(None);
    };
    let Some(stbl) = child(mdia, b"minf").and_then(|m| child(m, b"stbl")) else {
        return Ok(None);
    };
    let Some(stsd) = child(stbl, b"stsd").and_then(full_body) else {
        return Ok(None);
    };
    // stsd : nombre d'entrées sur 4 octets, puis les entrées (des boîtes).
    let Some(entries) = stsd.get(4..) else {
        return Ok(None);
    };
    let Some(avc1) = boxes(entries)
        .into_iter()
        .find(|b| &b.kind == b"avc1" || &b.kind == b"avc3")
    else {
        return Ok(None);
    };
    // Entrée visuelle : 78 octets (réservé, largeur, hauteur, résolution, profondeur…) avant les
    // boîtes filles.
    let body = avc1.body;
    if body.len() < 78 {
        return Err(Mp4Error::Truncated);
    }
    let width = u32::from(u16::from_be_bytes([body[24], body[25]]));
    let height = u32::from(u16::from_be_bytes([body[26], body[27]]));
    let avcc = child(&body[78..], b"avcC")
        .ok_or(Mp4Error::NoVideoTrack)?
        .to_vec();

    let timescale = child(mdia, b"mdhd")
        .and_then(|m| match m.first()? {
            1 => be32(m, 4 + 16),
            _ => be32(m, 4 + 8),
        })
        .filter(|t| *t > 0)
        .unwrap_or(1000);
    let to_ms = |ticks: u64| ticks.saturating_mul(1000) / u64::from(timescale);

    // Tailles.
    let stsz = child(stbl, b"stsz")
        .and_then(full_body)
        .ok_or(Mp4Error::BadTables)?;
    let constant = be32(stsz, 0).ok_or(Mp4Error::BadTables)?;
    let count = be32(stsz, 4).ok_or(Mp4Error::BadTables)? as usize;
    if count == 0 {
        return Err(Mp4Error::BadTables);
    }
    // Le nombre annoncé doit tenir dans la boîte (on n'alloue pas sur la foi d'un en-tête).
    if constant == 0 && 8 + 4 * count > stsz.len() {
        return Err(Mp4Error::BadTables);
    }
    let size_of = |i: usize| -> Option<u32> {
        if constant != 0 {
            Some(constant)
        } else {
            be32(stsz, 8 + 4 * i)
        }
    };

    // Durées : (nombre, delta) → instants de décodage.
    let stts = child(stbl, b"stts")
        .and_then(full_body)
        .ok_or(Mp4Error::BadTables)?;
    let runs = be32(stts, 0).ok_or(Mp4Error::BadTables)? as usize;
    let mut ticks: Vec<u64> = Vec::new();
    let mut now = 0u64;
    'runs: for r in 0..runs {
        let n = be32(stts, 4 + 8 * r).ok_or(Mp4Error::BadTables)?;
        let delta = be32(stts, 8 + 8 * r).ok_or(Mp4Error::BadTables)?;
        for _ in 0..n {
            if ticks.len() == count {
                break 'runs;
            }
            ticks.push(now);
            now += u64::from(delta);
        }
    }
    if ticks.len() != count {
        return Err(Mp4Error::BadTables);
    }
    let duration_ticks = now;

    // Images clés : absence de `stss` = toutes.
    let keys: Option<Vec<u32>> = child(stbl, b"stss").and_then(full_body).map(|s| {
        let n = be32(s, 0).unwrap_or(0) as usize;
        (0..n).map_while(|i| be32(s, 4 + 4 * i)).collect()
    });

    // Blocs : stsc (premier bloc, images par bloc), décalages stco ou co64.
    let stsc = child(stbl, b"stsc")
        .and_then(full_body)
        .ok_or(Mp4Error::BadTables)?;
    let stsc_n = be32(stsc, 0).ok_or(Mp4Error::BadTables)? as usize;
    let mut groups: Vec<(u32, u32)> = Vec::new();
    for g in 0..stsc_n {
        let first = be32(stsc, 4 + 12 * g).ok_or(Mp4Error::BadTables)?;
        let per = be32(stsc, 8 + 12 * g).ok_or(Mp4Error::BadTables)?;
        groups.push((first, per));
    }
    let chunk_offsets: Vec<u64> = if let Some(co) = child(stbl, b"stco").and_then(full_body) {
        let n = be32(co, 0).ok_or(Mp4Error::BadTables)? as usize;
        (0..n)
            .map(|i| {
                be32(co, 4 + 4 * i)
                    .map(u64::from)
                    .ok_or(Mp4Error::BadTables)
            })
            .collect::<Result<_, _>>()?
    } else if let Some(co) = child(stbl, b"co64").and_then(full_body) {
        let n = be32(co, 0).ok_or(Mp4Error::BadTables)? as usize;
        (0..n)
            .map(|i| be64(co, 4 + 8 * i).ok_or(Mp4Error::BadTables))
            .collect::<Result<_, _>>()?
    } else {
        return Err(Mp4Error::BadTables);
    };
    if groups.is_empty() || chunk_offsets.is_empty() {
        return Err(Mp4Error::BadTables);
    }

    let mut samples = Vec::with_capacity(count);
    let mut index = 0usize;
    'chunks: for (c, base) in chunk_offsets.iter().enumerate() {
        let chunk_no = c as u32 + 1;
        let per = groups
            .iter()
            .take_while(|(first, _)| *first <= chunk_no)
            .last()
            .map_or(0, |(_, per)| *per);
        let mut at = *base;
        for _ in 0..per {
            if index == count {
                break 'chunks;
            }
            let size = size_of(index).ok_or(Mp4Error::BadTables)?;
            let end = at.checked_add(u64::from(size)).ok_or(Mp4Error::BadTables)?;
            if end > file_len {
                // Fichier coupé : on garde les images entières qui précèdent.
                break 'chunks;
            }
            samples.push(SampleRef {
                offset: at,
                size,
                ts_ms: to_ms(ticks[index]),
                key: keys
                    .as_ref()
                    .is_none_or(|k| k.binary_search(&(index as u32 + 1)).is_ok()),
            });
            at = end;
            index += 1;
        }
    }
    // Une lecture ne peut commencer que sur une image clé.
    if samples.first().is_none_or(|s| !s.key) {
        return Err(Mp4Error::BadTables);
    }
    let duration_ms = if samples.len() == count {
        to_ms(duration_ticks)
    } else {
        // Clip coupé : jusqu'à la dernière image gardée, avec une image de plus.
        samples.last().map_or(0, |s| s.ts_ms) + 66
    };
    Ok(Some(Track {
        width,
        height,
        avcc,
        samples,
        duration_ms,
    }))
}

impl Track {
    /// Les octets de l'image `index` dans `file`, ou `None` si elle sort du fichier.
    pub fn sample_data<'a>(&self, file: &'a [u8], index: usize) -> Option<&'a [u8]> {
        let s = self.samples.get(index)?;
        let start = usize::try_from(s.offset).ok()?;
        file.get(start..start.checked_add(s.size as usize)?)
    }

    /// L'image montrée à l'instant `ms` : la dernière dont l'instant est `<= ms`.
    pub fn index_at(&self, ms: u64) -> usize {
        self.samples
            .partition_point(|s| s.ts_ms <= ms)
            .saturating_sub(1)
    }

    /// L'image clé qui précède (ou est) l'image `index` : là où le décodage doit repartir.
    pub fn keyframe_at_or_before(&self, index: usize) -> usize {
        let end = index.min(self.samples.len().saturating_sub(1));
        (0..=end).rev().find(|i| self.samples[*i].key).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    struct Layout {
        /// `(images par bloc)` : un bloc par entrée.
        chunks: Vec<usize>,
        moov_first: bool,
        /// Pas de `stss`.
        no_stss: bool,
        /// `stco` ou `co64`.
        wide: bool,
    }

    /// Un clip de `n` images (durée 100 ms, une clé toutes les 3) construit à la main, indépendamment
    /// du code du serveur (le test du serveur vérifie, lui, la compatibilité avec son muxer).
    fn clip(n: usize, layout: &Layout) -> (Vec<u8>, Vec<Vec<u8>>) {
        let datas: Vec<Vec<u8>> = (0..n).map(|i| vec![0, 0, 0, 2, 0x65, i as u8]).collect();
        let build = |offsets: &[u64]| -> Vec<u8> {
            let mut avc1 = vec![0u8; 24];
            avc1.extend_from_slice(&640u16.to_be_bytes());
            avc1.extend_from_slice(&360u16.to_be_bytes());
            avc1.extend_from_slice(&[0u8; 50]);
            avc1.extend_from_slice(&bx(b"avcC", &[1, 0x64, 0, 0x1f, 0xff]));
            let stsd = full(b"stsd", &[&u32s(&[1])[..], &bx(b"avc1", &avc1)].concat());
            let stts = full(b"stts", &u32s(&[1, n as u32, 100]));
            let keys: Vec<u32> = (0..n as u32)
                .filter(|i| i % 3 == 0)
                .map(|i| i + 1)
                .collect();
            let stss = full(
                b"stss",
                &[&u32s(&[keys.len() as u32])[..], &u32s(&keys)].concat(),
            );
            let mut stsc = vec![];
            let mut groups = 0;
            let mut last = 0;
            for (c, per) in layout.chunks.iter().enumerate() {
                if *per != last {
                    stsc.extend(u32s(&[c as u32 + 1, *per as u32, 1]));
                    groups += 1;
                    last = *per;
                }
            }
            let stsc = full(b"stsc", &[&u32s(&[groups])[..], &stsc].concat());
            let sizes: Vec<u32> = datas.iter().map(|d| d.len() as u32).collect();
            let stsz = full(
                b"stsz",
                &[&u32s(&[0, n as u32])[..], &u32s(&sizes)].concat(),
            );
            let chunk_table = if layout.wide {
                full(
                    b"co64",
                    &[
                        &u32s(&[offsets.len() as u32])[..],
                        &offsets
                            .iter()
                            .flat_map(|o| o.to_be_bytes())
                            .collect::<Vec<_>>(),
                    ]
                    .concat(),
                )
            } else {
                let o: Vec<u32> = offsets.iter().map(|o| *o as u32).collect();
                full(b"stco", &[&u32s(&[o.len() as u32])[..], &u32s(&o)].concat())
            };
            let mut parts = vec![stsd, stts];
            if !layout.no_stss {
                parts.push(stss);
            }
            parts.extend([stsc, stsz, chunk_table]);
            let stbl = bx(b"stbl", &parts.concat());
            let mdhd = full(b"mdhd", &u32s(&[0, 0, 1000, n as u32 * 100, 0]));
            let mdia = bx(b"mdia", &[mdhd, bx(b"minf", &stbl)].concat());
            // Une piste sans rapport devant (son) : ignorée.
            let sound = bx(b"trak", &bx(b"mdia", &full(b"mdhd", &[0; 20])));
            bx(b"moov", &[sound, bx(b"trak", &mdia)].concat())
        };
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let mdat_body: Vec<u8> = datas.concat();
        let mdat = bx(b"mdat", &mdat_body);
        // Les décalages dépendent de la taille du moov (qui ne dépend pas de leur valeur).
        let moov_len = build(&vec![0; layout.chunks.len()]).len();
        let data_start = if layout.moov_first {
            ftyp.len() + moov_len + 8
        } else {
            ftyp.len() + 8
        };
        let mut offsets = vec![];
        let (mut at, mut index) = (data_start as u64, 0);
        for per in &layout.chunks {
            offsets.push(at);
            for _ in 0..*per {
                at += datas[index].len() as u64;
                index += 1;
            }
        }
        let moov = build(&offsets);
        let file = if layout.moov_first {
            [ftyp, moov, mdat].concat()
        } else {
            [ftyp, mdat, moov].concat()
        };
        (file, datas)
    }

    fn simple() -> Layout {
        Layout {
            chunks: vec![7],
            moov_first: true,
            no_stss: false,
            wide: false,
        }
    }

    #[test]
    fn reads_the_server_layout() {
        let (file, datas) = clip(7, &simple());
        let track = parse(&file).unwrap();
        assert_eq!((track.width, track.height), (640, 360));
        assert_eq!(track.avcc, [1, 0x64, 0, 0x1f, 0xff]);
        assert_eq!(track.samples.len(), 7);
        assert_eq!(track.duration_ms, 700);
        let ts: Vec<u64> = track.samples.iter().map(|s| s.ts_ms).collect();
        assert_eq!(ts, [0, 100, 200, 300, 400, 500, 600]);
        let keys: Vec<bool> = track.samples.iter().map(|s| s.key).collect();
        assert_eq!(keys, [true, false, false, true, false, false, true]);
        for (i, data) in datas.iter().enumerate() {
            assert_eq!(track.sample_data(&file, i), Some(&data[..]));
        }
    }

    #[test]
    fn tolerates_other_layouts() {
        for layout in [
            Layout {
                chunks: vec![3, 3, 1],
                moov_first: false,
                no_stss: false,
                wide: true,
            },
            Layout {
                chunks: vec![2, 2, 2, 1],
                moov_first: true,
                no_stss: true,
                wide: false,
            },
        ] {
            let (file, datas) = clip(7, &layout);
            let track = parse(&file).unwrap();
            assert_eq!(track.samples.len(), 7);
            for (i, data) in datas.iter().enumerate() {
                assert_eq!(track.sample_data(&file, i), Some(&data[..]), "image {i}");
            }
            assert_eq!(
                track.samples.iter().all(|s| s.key),
                layout.no_stss,
                "sans stss, toutes les images sont des clés"
            );
        }
    }

    #[test]
    fn seek_goes_back_to_the_preceding_key_frame() {
        let (file, _) = clip(7, &simple());
        let track = parse(&file).unwrap();
        assert_eq!(track.index_at(0), 0);
        assert_eq!(track.index_at(99), 0);
        assert_eq!(track.index_at(100), 1);
        assert_eq!(track.index_at(10_000), 6);
        assert_eq!(track.keyframe_at_or_before(0), 0);
        assert_eq!(track.keyframe_at_or_before(2), 0);
        assert_eq!(track.keyframe_at_or_before(3), 3);
        assert_eq!(track.keyframe_at_or_before(5), 3);
        assert_eq!(track.keyframe_at_or_before(99), 6);
    }

    #[test]
    fn a_truncated_file_keeps_its_whole_frames_and_garbage_is_refused() {
        // `mdat` en dernier et coupé : le `moov` est devant, donc entier.
        let (file, _) = clip(7, &simple());
        let cut = &file[..file.len() - 9];
        // La boîte `mdat` déborde : elle est ignorée, mais le `moov` reste lisible et les images
        // dont les octets manquent sont écartées.
        let track = parse(cut).unwrap();
        assert!(track.samples.len() < 7 && !track.samples.is_empty());
        assert!(track.duration_ms > 0);
        // Rien de lisible.
        assert_eq!(parse(&[]), Err(Mp4Error::Truncated));
        assert_eq!(parse(&[0xAB; 100]), Err(Mp4Error::Truncated));
        // `moov` sans piste vidéo.
        assert_eq!(parse(&bx(b"moov", &[])), Err(Mp4Error::NoVideoTrack));
        // Tailles absurdes : jamais de panique ni d'allocation démesurée.
        let (mut file, _) = clip(7, &simple());
        let stsz = file.windows(4).position(|w| w == b"stsz").unwrap();
        file[stsz + 12..stsz + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse(&file), Err(Mp4Error::BadTables));
    }

    #[test]
    fn every_truncation_and_bit_flip_is_an_error_or_a_track_never_a_panic() {
        let (file, _) = clip(7, &simple());
        for cut in 0..file.len() {
            let _ = parse(&file[..cut]);
        }
        for i in 0..file.len() {
            let mut damaged = file.clone();
            damaged[i] ^= 0xFF;
            if let Ok(track) = parse(&damaged) {
                for k in 0..track.samples.len() {
                    let _ = track.sample_data(&damaged, k);
                }
            }
        }
    }
}
