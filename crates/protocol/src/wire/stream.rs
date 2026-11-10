//! Le flux vidéo d'une caméra (ticket n° 5) : ce que le serveur envoie sur le WebSocket
//! `GET /api/cameras/{id}/stream?kind=main|sub` (voir [`super::camera_stream_path`]).
//!
//! Le serveur relaie le H.264 de la caméra **sans le décoder ni le modifier** ; le client le
//! décode. Deux sortes de messages WebSocket :
//!
//! - **texte** : un [`StreamStatus`] en JSON, quand l'état de la liaison avec la caméra change ;
//! - **binaire** : un [`StreamPacket`], dans l'ordre : d'abord une configuration
//!   ([`StreamPacket::Config`]), puis des images ([`StreamPacket::Frame`]).
//!
//! # Format binaire (grand-boutiste)
//!
//! ```text
//! configuration : 0x01 | codec u8 | largeur u16 | hauteur u16 | rapport de pixel n u16 | d u16 | avcC…
//! image         : 0x02 | drapeaux u8 (bit 0 : image clé) | horodatage u32 (ms) | données AVCC…
//! ```
//!
//! - `codec` : 1 = H.264 (le seul aujourd'hui) ;
//! - `avcC` : l'enregistrement `AVCDecoderConfigurationRecord` (ISO 14496-15) : profil, SPS et PPS ;
//! - données : les unités NAL de l'image, chacune précédée de sa longueur sur 4 octets (format
//!   « AVCC », celui qu'attend VideoToolbox). **Chaque image clé porte aussi ses SPS et PPS** :
//!   un client qui a perdu des images reprend à la clé suivante sans autre information ;
//! - horodatage : en millisecondes depuis le début de la session du serveur, il boucle (49 jours).
//!
//! Une nouvelle configuration est envoyée à chaque changement de paramètres de la caméra, et à
//! l'arrivée d'un spectateur (suivie, quand le serveur l'a gardée, de la dernière suite d'images
//! depuis la dernière image clé, pour qu'il n'attende pas la suivante).

use serde::{Deserialize, Serialize};

use super::camera::CameraProblem;

/// Codec H.264 dans [`StreamPacket::Config`].
pub const CODEC_H264: u8 = 1;

const TAG_CONFIG: u8 = 1;
const TAG_FRAME: u8 = 2;
const FLAG_KEY: u8 = 1;
const CONFIG_HEADER: usize = 1 + 1 + 2 + 2 + 2 + 2;
const FRAME_HEADER: usize = 1 + 1 + 4;

/// État de la liaison entre le serveur et la caméra, envoyé en texte (JSON) à chaque changement.
/// Un client qui ne connaît pas un état le lit comme [`StreamStatus::Unknown`] et continue
/// d'attendre des images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StreamStatus {
    /// Le serveur se connecte à la caméra.
    Connecting,
    /// Les images arrivent.
    Live,
    /// La liaison avec la caméra a été perdue : le serveur réessaie tout seul.
    Retrying { problem: CameraProblem },
    /// La caméra envoie un codec que le serveur ne relaie pas (H.265…). Définitif : le serveur
    /// ferme ensuite le flux, inutile de réessayer.
    Unsupported { codec: String },
    #[serde(other)]
    Unknown,
}

/// Un message binaire du flux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamPacket {
    Config {
        codec: u8,
        width: u16,
        height: u16,
        /// Rapport d'aspect d'un pixel (numérateur, dénominateur) ; `(1, 1)` pour un pixel carré.
        pixel_aspect: (u16, u16),
        /// `AVCDecoderConfigurationRecord`.
        avcc: Vec<u8>,
    },
    Frame {
        key: bool,
        timestamp_ms: u32,
        /// Unités NAL précédées de leur longueur sur 4 octets.
        data: Vec<u8>,
    },
}

/// Pourquoi un message binaire est refusé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    Empty,
    UnknownTag(u8),
    Truncated,
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PacketError::Empty => "message vidéo vide",
            PacketError::UnknownTag(_) => "message vidéo inconnu",
            PacketError::Truncated => "message vidéo tronqué",
        })
    }
}

impl std::error::Error for PacketError {}

impl StreamPacket {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            StreamPacket::Config {
                codec,
                width,
                height,
                pixel_aspect,
                avcc,
            } => {
                let mut out = Vec::with_capacity(CONFIG_HEADER + avcc.len());
                out.push(TAG_CONFIG);
                out.push(*codec);
                out.extend_from_slice(&width.to_be_bytes());
                out.extend_from_slice(&height.to_be_bytes());
                out.extend_from_slice(&pixel_aspect.0.to_be_bytes());
                out.extend_from_slice(&pixel_aspect.1.to_be_bytes());
                out.extend_from_slice(avcc);
                out
            }
            StreamPacket::Frame {
                key,
                timestamp_ms,
                data,
            } => encode_frame(*key, *timestamp_ms, data),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<StreamPacket, PacketError> {
        let (&tag, rest) = bytes.split_first().ok_or(PacketError::Empty)?;
        match tag {
            TAG_CONFIG => {
                if bytes.len() < CONFIG_HEADER {
                    return Err(PacketError::Truncated);
                }
                let u16_at = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
                Ok(StreamPacket::Config {
                    codec: rest[0],
                    width: u16_at(2),
                    height: u16_at(4),
                    pixel_aspect: (u16_at(6), u16_at(8)),
                    avcc: bytes[CONFIG_HEADER..].to_vec(),
                })
            }
            TAG_FRAME => {
                if bytes.len() < FRAME_HEADER {
                    return Err(PacketError::Truncated);
                }
                Ok(StreamPacket::Frame {
                    key: bytes[1] & FLAG_KEY != 0,
                    timestamp_ms: u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
                    data: bytes[FRAME_HEADER..].to_vec(),
                })
            }
            other => Err(PacketError::UnknownTag(other)),
        }
    }
}

/// Encode une image sans passer par [`StreamPacket`] (pas de copie intermédiaire des données).
pub fn encode_frame(key: bool, timestamp_ms: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER + data.len());
    out.push(TAG_FRAME);
    out.push(if key { FLAG_KEY } else { 0 });
    out.extend_from_slice(&timestamp_ms.to_be_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        let config = StreamPacket::Config {
            codec: CODEC_H264,
            width: 1280,
            height: 720,
            pixel_aspect: (40, 33),
            avcc: vec![1, 0x64, 0, 0x1f, 0xff, 0xe1],
        };
        assert_eq!(StreamPacket::decode(&config.encode()), Ok(config));
        let frame = StreamPacket::Frame {
            key: true,
            timestamp_ms: 0xDEAD_BEEF,
            data: vec![0, 0, 0, 2, 0x65, 0x88],
        };
        assert_eq!(StreamPacket::decode(&frame.encode()), Ok(frame.clone()));
        assert_eq!(
            encode_frame(true, 0xDEAD_BEEF, &[0, 0, 0, 2, 0x65, 0x88]),
            frame.encode()
        );
        let delta = StreamPacket::Frame {
            key: false,
            timestamp_ms: 1,
            data: vec![],
        };
        assert_eq!(StreamPacket::decode(&delta.encode()), Ok(delta));
    }

    #[test]
    fn malformed_packets_are_refused_not_trusted() {
        assert_eq!(StreamPacket::decode(&[]), Err(PacketError::Empty));
        assert_eq!(
            StreamPacket::decode(&[9, 1, 2]),
            Err(PacketError::UnknownTag(9))
        );
        assert_eq!(
            StreamPacket::decode(&[1, 1, 0]),
            Err(PacketError::Truncated)
        );
        assert_eq!(
            StreamPacket::decode(&[2, 1, 0, 0]),
            Err(PacketError::Truncated)
        );
    }

    #[test]
    fn statuses_use_stable_codes_and_unknown_ones_fall_back() {
        let json = serde_json::to_string(&StreamStatus::Retrying {
            problem: CameraProblem::Unreachable,
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"retrying","problem":"unreachable"}"#);
        let json = serde_json::to_string(&StreamStatus::Unsupported {
            codec: "h265".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"unsupported","codec":"h265"}"#);
        // Un état d'un serveur plus récent n'est pas une erreur.
        let future: StreamStatus =
            serde_json::from_str(r#"{"state":"warming_up","eta":3}"#).unwrap();
        assert_eq!(future, StreamStatus::Unknown);
        // Un motif inconnu retombe sur `Failed`.
        let future: StreamStatus =
            serde_json::from_str(r#"{"state":"retrying","problem":"burnt_out"}"#).unwrap();
        assert_eq!(
            future,
            StreamStatus::Retrying {
                problem: CameraProblem::Failed
            }
        );
    }
}
