//! Décodage vidéo des caméras (ticket n° 5) : le serveur relaie du H.264 en format AVCC
//! (`protocol::wire::stream`), le client le décode et en fait des images prêtes à afficher.
//!
//! # Le décodeur : VideoToolbox
//!
//! Le décodeur H.264 matériel de macOS (`videotoolbox`) : rien à embarquer dans le DMG ni à
//! installer, peu de processeur, ce qui compte sur un vieux Mac. Il est appelé par les liaisons
//! générées `objc2-video-toolbox`, `objc2-core-media` et `objc2-core-video` (maintenues avec le reste
//! d'`objc2`, déjà dans l'arbre). Hors macOS, le même décodeur existe mais refuse de démarrer
//! (« non disponible ») : le client se compile partout, la lecture non.
//!
//! # Robustesse
//!
//! La politique est dans [`StreamDecoder`], indépendante de VideoToolbox (et donc testée sans
//! lui) :
//!
//! - on **attend une image clé** avant de décoder, au démarrage comme après toute perte ;
//! - chaque image est **vérifiée** avant le décodeur (`nal`) : longueurs, bit interdit ;
//! - les SPS et PPS de chaque image clé sont lus : un changement de paramètres en cours de route
//!   (caméra qui change de résolution, flux principal Aqara qui renvoie des PPS différents)
//!   recrée la session **à cette clé**, jamais au milieu d'une suite d'images ;
//! - une erreur de décodage, ou une session perdue (réveil du Mac), fait attendre la clé suivante
//!   au lieu d'afficher des images abîmées ;
//! - seule la **dernière** image décodée est gardée ([`FrameSink`]) : l'interface n'accumule jamais
//!   de retard.

pub mod nal;

#[cfg(target_os = "macos")]
mod videotoolbox;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use protocol::wire::{StreamPacket, CODEC_H264};

use nal::ParameterSets;

/// Une image décodée : des pixels opaques, déjà dans le format d'`egui` (convertis une seule fois,
/// dans le fil du décodeur ; l'interface n'a plus qu'à les prendre).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// `width × height` pixels, ligne par ligne.
    pub pixels: Vec<eframe::egui::Color32>,
}

/// Là où le décodeur dépose ses images : seule la dernière est gardée.
pub struct FrameSink {
    latest: Mutex<Option<Image>>,
    frames: AtomicU64,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl FrameSink {
    /// `notify` est appelée à chaque image (depuis un fil du décodeur) : l'interface s'en sert pour
    /// se redessiner.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Arc<FrameSink> {
        Arc::new(FrameSink {
            latest: Mutex::new(None),
            frames: AtomicU64::new(0),
            notify: Box::new(notify),
        })
    }

    pub fn put(&self, image: Image) {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(image);
        self.frames.fetch_add(1, Ordering::SeqCst);
        (self.notify)();
    }

    /// La dernière image, une seule fois.
    pub fn take(&self) -> Option<Image> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Nombre d'images décodées depuis le début.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::SeqCst)
    }
}

/// Pourquoi le décodeur ne peut pas continuer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    /// Pas de décodeur sur ce système, ou il refuse de démarrer. Définitif.
    Unavailable(String),
    /// Le flux n'est pas du H.264.
    UnsupportedCodec(u8),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoError::Unavailable(why) => write!(f, "décodage vidéo indisponible : {why}"),
            VideoError::UnsupportedCodec(_) => f.write_str("format vidéo non pris en charge"),
        }
    }
}

impl std::error::Error for VideoError {}

/// Ce qu'un décodeur de bas niveau peut répondre à une image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// La session de décodage n'existe plus (réveil du Mac, décodeur réinitialisé) : à recréer.
    SessionLost,
    /// Image refusée : on attend la prochaine image clé.
    BadData,
    /// Plus de décodeur : définitif.
    Unavailable(String),
}

/// Un décodeur H.264 de bas niveau : VideoToolbox, ou autre chose dans les tests.
pub trait H264Backend {
    /// Prépare le décodage avec ces jeux de paramètres, en remplaçant la session en cours.
    fn start(&mut self, parameters: &ParameterSets) -> Result<(), BackendError>;
    /// Décode une image AVCC **sans** SPS ni PPS. L'image décodée est déposée dans le
    /// [`FrameSink`], pas rendue ici.
    fn decode(&mut self, sample: &[u8], timestamp_ms: u32) -> Result<(), BackendError>;
}

/// Compteurs de diagnostic.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecoderStats {
    /// Images reçues.
    pub received: u64,
    /// Images écartées (attente d'une image clé).
    pub dropped: u64,
    /// Images refusées par le décodeur, ou illisibles.
    pub errors: u64,
    /// Sessions de décodage (re)créées.
    pub sessions: u64,
}

/// Taille et forme des pixels annoncées par le serveur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Rapport d'aspect d'un pixel (numérateur, dénominateur).
    pub pixel_aspect: (u32, u32),
}

/// La politique de décodage d'un flux (voir l'en-tête du module).
pub struct StreamDecoder<B> {
    backend: B,
    /// Paramètres de la dernière configuration reçue.
    config: Option<ParameterSets>,
    /// Paramètres avec lesquels la session de décodage tourne.
    active: Option<ParameterSets>,
    need_key: bool,
    info: Option<VideoInfo>,
    stats: DecoderStats,
}

impl<B: H264Backend> StreamDecoder<B> {
    pub fn with_backend(backend: B) -> Self {
        Self {
            backend,
            config: None,
            active: None,
            need_key: true,
            info: None,
            stats: DecoderStats::default(),
        }
    }

    pub fn stats(&self) -> DecoderStats {
        self.stats
    }

    pub fn info(&self) -> Option<VideoInfo> {
        self.info
    }

    /// Reprend à zéro (nouvelle connexion) : tout ce qui vient avant la prochaine configuration et
    /// la prochaine image clé est écarté.
    pub fn reset(&mut self) {
        self.config = None;
        self.need_key = true;
    }

    /// Traite un message du flux. Une erreur n'est rendue que si le décodage est impossible pour de
    /// bon ; tout le reste (image abîmée, session perdue) est absorbé.
    pub fn push(&mut self, packet: &StreamPacket) -> Result<(), VideoError> {
        match packet {
            StreamPacket::Config {
                codec,
                width,
                height,
                pixel_aspect,
                avcc,
            } => {
                if *codec != CODEC_H264 {
                    return Err(VideoError::UnsupportedCodec(*codec));
                }
                self.info = Some(VideoInfo {
                    width: u32::from(*width),
                    height: u32::from(*height),
                    pixel_aspect: (
                        u32::from(pixel_aspect.0.max(1)),
                        u32::from(pixel_aspect.1.max(1)),
                    ),
                });
                // Une configuration illisible n'est pas fatale : les images clés portent leurs
                // propres SPS et PPS.
                self.config = nal::parse_avcc(avcc).ok();
                Ok(())
            }
            StreamPacket::Frame {
                key,
                timestamp_ms,
                data,
            } => self.frame(*key, *timestamp_ms, data),
        }
    }

    fn frame(&mut self, key: bool, timestamp_ms: u32, data: &[u8]) -> Result<(), VideoError> {
        self.stats.received += 1;
        let sample = match nal::prepare_sample(data) {
            Ok(sample) => sample,
            Err(_) => {
                self.stats.errors += 1;
                self.need_key = true;
                return Ok(());
            }
        };
        if key && sample.idr {
            // Les paramètres de l'image clé font foi ; à défaut, ceux de la configuration.
            let Some(wanted) = sample.parameters.clone().or_else(|| self.config.clone()) else {
                self.stats.dropped += 1;
                return Ok(());
            };
            if self.active.as_ref() != Some(&wanted) {
                match self.backend.start(&wanted) {
                    Ok(()) => {
                        self.active = Some(wanted);
                        self.stats.sessions += 1;
                    }
                    Err(BackendError::Unavailable(why)) => {
                        return Err(VideoError::Unavailable(why))
                    }
                    Err(_) => {
                        self.active = None;
                        self.stats.errors += 1;
                        return Ok(());
                    }
                }
            }
            self.need_key = false;
        } else if self.need_key || self.active.is_none() {
            self.stats.dropped += 1;
            return Ok(());
        }
        match self.backend.decode(&sample.data, timestamp_ms) {
            Ok(()) => Ok(()),
            Err(BackendError::Unavailable(why)) => Err(VideoError::Unavailable(why)),
            Err(error) => {
                // Session perdue : on la recrée à la prochaine clé. Image refusée : on attend la clé.
                if error == BackendError::SessionLost {
                    self.active = None;
                }
                self.stats.errors += 1;
                self.need_key = true;
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub type Decoder = StreamDecoder<videotoolbox::VideoToolbox>;

#[cfg(target_os = "macos")]
impl Decoder {
    /// Un décodeur VideoToolbox qui dépose ses images dans `sink`.
    pub fn new(sink: Arc<FrameSink>) -> Result<Decoder, VideoError> {
        Ok(StreamDecoder::with_backend(
            videotoolbox::VideoToolbox::new(sink),
        ))
    }
}

/// Hors macOS : pas de décodeur.
#[cfg(not(target_os = "macos"))]
pub struct NoDecoder;

#[cfg(not(target_os = "macos"))]
impl H264Backend for NoDecoder {
    fn start(&mut self, _: &ParameterSets) -> Result<(), BackendError> {
        Err(BackendError::Unavailable(
            "VideoToolbox n'existe que sous macOS".into(),
        ))
    }

    fn decode(&mut self, _: &[u8], _: u32) -> Result<(), BackendError> {
        Err(BackendError::Unavailable(
            "VideoToolbox n'existe que sous macOS".into(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub type Decoder = StreamDecoder<NoDecoder>;

#[cfg(not(target_os = "macos"))]
impl Decoder {
    pub fn new(_sink: Arc<FrameSink>) -> Result<Decoder, VideoError> {
        Err(VideoError::Unavailable(
            "VideoToolbox n'existe que sous macOS".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
