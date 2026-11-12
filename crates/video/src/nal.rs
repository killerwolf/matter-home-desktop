//! Les unités NAL du H.264, côté client : lecture de l'enregistrement `avcC` (SPS et PPS), découpage
//! d'une image « AVCC » (chaque NAL précédée de sa longueur sur 4 octets) et vérification qu'elle
//! est saine **avant** de la donner au décodeur du système : une image tronquée ou incohérente ne
//! doit jamais l'atteindre (un décodeur matériel n'est pas fait pour être nourri de n'importe quoi).

/// Type d'une NAL H.264 (les 5 bits de poids faible du premier octet).
pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

/// Les jeux de paramètres d'un flux (NAL complètes, en-tête compris, sans longueur).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParameterSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalError {
    /// L'enregistrement `avcC` est trop court, de mauvaise version, ou sans SPS ou PPS.
    BadConfig,
    /// Une longueur déborde de l'image, ou est nulle.
    BadLength,
    /// Une NAL a son bit « interdit » levé, ou un type inconnu pour une image.
    BadHeader,
}

impl std::fmt::Display for NalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NalError::BadConfig => "configuration vidéo invalide",
            NalError::BadLength => "image vidéo tronquée",
            NalError::BadHeader => "image vidéo invalide",
        })
    }
}

impl std::error::Error for NalError {}

/// Lit un `AVCDecoderConfigurationRecord` (ISO 14496-15) : premier SPS et premier PPS.
///
/// Le serveur annonce toujours des longueurs de NAL sur 4 octets ; un autre `lengthSizeMinusOne`
/// est refusé plutôt que deviné.
pub fn parse_avcc(record: &[u8]) -> Result<ParameterSets, NalError> {
    // version, profil, compatibilité, niveau, 6 bits réservés + longueur-1, 3 bits + nombre de SPS
    if record.len() < 7 || record[0] != 1 || record[4] & 0x03 != 3 {
        return Err(NalError::BadConfig);
    }
    let sps_count = usize::from(record[5] & 0x1f);
    let mut at = 6;
    let mut sps = None;
    for _ in 0..sps_count {
        let len = read_u16(record, at).ok_or(NalError::BadConfig)?;
        let nal = record
            .get(at + 2..at + 2 + len)
            .ok_or(NalError::BadConfig)?;
        sps.get_or_insert(nal.to_vec());
        at += 2 + len;
    }
    let pps_count = usize::from(*record.get(at).ok_or(NalError::BadConfig)?);
    at += 1;
    let mut pps = None;
    for _ in 0..pps_count {
        let len = read_u16(record, at).ok_or(NalError::BadConfig)?;
        let nal = record
            .get(at + 2..at + 2 + len)
            .ok_or(NalError::BadConfig)?;
        pps.get_or_insert(nal.to_vec());
        at += 2 + len;
    }
    match (sps, pps) {
        (Some(sps), Some(pps))
            if sps.first().is_some_and(|b| b & 0x1f == NAL_SPS)
                && pps.first().is_some_and(|b| b & 0x1f == NAL_PPS) =>
        {
            Ok(ParameterSets { sps, pps })
        }
        _ => Err(NalError::BadConfig),
    }
}

fn read_u16(data: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
    ])))
}

/// Les NAL d'une image AVCC, sans leur longueur. Refuse une image dont une longueur déborde, une
/// NAL vide, un bit interdit levé, ou qui n'a aucune NAL.
pub fn split_avcc(data: &[u8]) -> Result<Vec<&[u8]>, NalError> {
    let mut nals = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = rest
            .get(..4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or(NalError::BadLength)?;
        let nal = rest.get(4..4 + len).ok_or(NalError::BadLength)?;
        let header = *nal.first().ok_or(NalError::BadLength)?;
        if header & 0x80 != 0 {
            return Err(NalError::BadHeader);
        }
        nals.push(nal);
        rest = &rest[4 + len..];
    }
    if nals.is_empty() {
        return Err(NalError::BadLength);
    }
    Ok(nals)
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// Une image AVCC prête pour le décodeur : sans SPS ni PPS (ils vont dans la description du
/// format, pas dans les données), avec ceux qu'elle portait.
#[derive(Debug, PartialEq, Eq)]
pub struct Sample {
    pub data: Vec<u8>,
    /// SPS et PPS trouvés dans l'image (une image clé les porte), s'il y en a un de chaque.
    pub parameters: Option<ParameterSets>,
    /// L'image contient une image IDR.
    pub idr: bool,
}

/// Valide une image AVCC et la sépare de ses jeux de paramètres.
pub fn prepare_sample(data: &[u8]) -> Result<Sample, NalError> {
    let nals = split_avcc(data)?;
    let mut sps = None;
    let mut pps = None;
    let mut idr = false;
    let mut out = Vec::with_capacity(data.len());
    for nal in nals {
        match nal_type(nal) {
            NAL_SPS => {
                sps.get_or_insert(nal.to_vec());
            }
            NAL_PPS => {
                pps.get_or_insert(nal.to_vec());
            }
            kind => {
                idr |= kind == NAL_IDR;
                out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
    }
    if out.is_empty() {
        // Rien que des jeux de paramètres : pas une image.
        return Err(NalError::BadLength);
    }
    Ok(Sample {
        data: out,
        parameters: match (sps, pps) {
            (Some(sps), Some(pps)) => Some(ParameterSets { sps, pps }),
            _ => None,
        },
        idr,
    })
}

/// Un flux Annex B (codes de début `00 00 01`) en images AVCC : une `Vec` par image (un groupe de
/// NAL se termine à une NAL d'image). Sert aux tests et à l'exemple de démonstration.
pub fn annex_b_to_avcc_frames(data: &[u8]) -> Vec<Vec<u8>> {
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
    let mut frames = Vec::new();
    let mut current = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        let nal = &data[start..end];
        current.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        current.extend_from_slice(nal);
        if matches!(nal_type(nal), 1 | 5) {
            frames.push(std::mem::take(&mut current));
        }
    }
    frames
}

/// Un `avcC` à partir d'un SPS et d'un PPS.
pub fn build_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 6] = [0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x80];
    const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];

    fn avcc_of(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn avcc_record_gives_the_parameter_sets() {
        let record = build_avcc(&SPS, &PPS);
        assert_eq!(
            parse_avcc(&record),
            Ok(ParameterSets {
                sps: SPS.to_vec(),
                pps: PPS.to_vec()
            })
        );
    }

    #[test]
    fn a_bad_avcc_record_is_refused() {
        let good = build_avcc(&SPS, &PPS);
        assert_eq!(parse_avcc(&[]), Err(NalError::BadConfig));
        assert_eq!(
            parse_avcc(&good[..good.len() - 1]),
            Err(NalError::BadConfig)
        );
        assert_eq!(parse_avcc(&good[..8]), Err(NalError::BadConfig));
        let mut version = good.clone();
        version[0] = 2;
        assert_eq!(parse_avcc(&version), Err(NalError::BadConfig));
        // Longueur de NAL sur 2 octets : pas ce que le serveur annonce.
        let mut short_len = good.clone();
        short_len[4] = 0xfd;
        assert_eq!(parse_avcc(&short_len), Err(NalError::BadConfig));
        // Sans SPS.
        let mut no_sps = good.clone();
        no_sps[5] = 0xe0;
        assert_eq!(parse_avcc(&no_sps), Err(NalError::BadConfig));
        // Un « SPS » qui n'en est pas un.
        let wrong = build_avcc(&[0x68, 0x42, 0xc0, 0x1e], &PPS);
        assert_eq!(parse_avcc(&wrong), Err(NalError::BadConfig));
    }

    #[test]
    fn frames_are_split_on_their_lengths_and_truncation_is_caught() {
        let idr = [0x65, 0x88, 0x84, 0x80];
        let frame = avcc_of(&[&SPS, &PPS, &idr]);
        let nals = split_avcc(&frame).unwrap();
        assert_eq!(nals, vec![&SPS[..], &PPS[..], &idr[..]]);
        assert_eq!(
            split_avcc(&frame[..frame.len() - 1]),
            Err(NalError::BadLength)
        );
        assert_eq!(split_avcc(&frame[..2]), Err(NalError::BadLength));
        assert_eq!(split_avcc(&[]), Err(NalError::BadLength));
        // Longueur nulle, ou énorme.
        assert_eq!(split_avcc(&[0, 0, 0, 0]), Err(NalError::BadLength));
        assert_eq!(
            split_avcc(&[0xff, 0xff, 0xff, 0xff, 0x65]),
            Err(NalError::BadLength)
        );
        // Bit interdit.
        assert_eq!(
            split_avcc(&avcc_of(&[&[0xe5, 1]])),
            Err(NalError::BadHeader)
        );
    }

    #[test]
    fn a_sample_loses_its_parameter_sets_but_reports_them() {
        let idr = [0x65, 0x88, 0x84, 0x80];
        let sample = prepare_sample(&avcc_of(&[&SPS, &PPS, &idr])).unwrap();
        assert!(sample.idr);
        assert_eq!(sample.data, avcc_of(&[&idr]));
        assert_eq!(
            sample.parameters,
            Some(ParameterSets {
                sps: SPS.to_vec(),
                pps: PPS.to_vec()
            })
        );
        let delta = [0x41, 0x9a, 0x01];
        let sample = prepare_sample(&avcc_of(&[&delta])).unwrap();
        assert!(!sample.idr);
        assert_eq!(sample.parameters, None);
        assert_eq!(sample.data, avcc_of(&[&delta]));
        // Que des jeux de paramètres : ce n'est pas une image.
        assert!(prepare_sample(&avcc_of(&[&SPS, &PPS])).is_err());
    }

    #[test]
    fn annex_b_frames_group_their_parameter_sets_with_the_picture() {
        let stream = [
            &[0, 0, 0, 1][..],
            &SPS,
            &[0, 0, 0, 1],
            &PPS,
            &[0, 0, 1],
            &[0x65, 1, 2],
            &[0, 0, 0, 1],
            &[0x41, 3],
        ]
        .concat();
        let frames = annex_b_to_avcc_frames(&stream);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], avcc_of(&[&SPS, &PPS, &[0x65, 1, 2]]));
        assert_eq!(frames[1], avcc_of(&[&[0x41, 3]]));
    }
}
