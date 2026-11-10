//! Le décodeur H.264 matériel de macOS : VideoToolbox, par les liaisons `objc2-*`.
//!
//! Le flux de données : une image AVCC (sans SPS ni PPS, voir `nal::prepare_sample`) devient un
//! `CMBlockBuffer` puis un `CMSampleBuffer` portant la description du format (SPS et PPS), que
//! `VTDecompressionSession` décode **de façon synchrone** (images rendues dans l'ordre où elles
//! arrivent : les caméras n'envoient pas d'images B, et le direct ne réordonne rien). La fonction de
//! rappel reçoit un `CVPixelBuffer` en BGRA, converti en RGBA et déposé dans le `FrameSink`.
//!
//! Seules des API de macOS 10.15 sont utilisées (le client est universel : 10.15 en x86_64, 11.0 en
//! arm64).
//!
//! Tout le code `unsafe` de la lecture vidéo est ici. Les invariants :
//! - chaque objet Core Foundation créé (`+1`) est pris dans un `CFRetained` et relâché à sa chute ;
//! - le pointeur donné à la fonction de rappel (`refcon`) est un `Arc<FrameSink>` rendu à la
//!   session par `Arc::into_raw` et repris **après** `invalidate` (plus aucun rappel possible).

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_media::{
    kCMBlockBufferAssureMemoryNowFlag, kCMTimeInvalid, CMBlockBuffer, CMSampleBuffer,
    CMSampleTimingInfo, CMTime, CMVideoFormatDescription,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVImageBuffer,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

use eframe::egui::Color32;

use super::nal::ParameterSets;
use super::{BackendError, FrameSink, H264Backend, Image};

/// `kVTInvalidSessionErr` : la session n'existe plus (réveil du Mac, décodeur réinitialisé).
const ERR_INVALID_SESSION: i32 = -12903;
/// `kVTVideoDecoderMalfunctionErr`.
const ERR_MALFUNCTION: i32 = -12911;
/// `kVTVideoDecoderNotAvailableNowErr` : le décodeur matériel est pris ; réessayer plus tard.
const ERR_NOT_AVAILABLE_NOW: i32 = -12915;

pub struct VideoToolbox {
    sink: Arc<FrameSink>,
    session: Option<Session>,
}

impl VideoToolbox {
    pub fn new(sink: Arc<FrameSink>) -> Self {
        Self {
            sink,
            session: None,
        }
    }
}

struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMVideoFormatDescription>,
    /// `Arc<FrameSink>` rendu par `Arc::into_raw`, repris à la chute.
    refcon: *const FrameSink,
}

// Une session VideoToolbox s'utilise depuis n'importe quel fil (mais pas en parallèle : le décodeur
// vit dans le fil du flux). Le `refcon` est un `Arc` de contenu `Send + Sync`.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY : après `invalidate`, la fonction de rappel n'est plus appelée ; le `refcon` vient
        // d'`Arc::into_raw` dans `Session::new` et n'est repris qu'ici.
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
            drop(Arc::from_raw(self.refcon));
        }
    }
}

fn format_description(
    parameters: &ParameterSets,
) -> Result<CFRetained<CMVideoFormatDescription>, BackendError> {
    let pointers = [
        NonNull::from(&parameters.sps[0]),
        NonNull::from(&parameters.pps[0]),
    ];
    let sizes = [parameters.sps.len(), parameters.pps.len()];
    let mut out: *const CMVideoFormatDescription = ptr::null();
    // SAFETY : les deux tableaux ont 2 éléments valides pendant l'appel ; la description rendue
    // (+1) est prise par `CFRetained`.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            2,
            NonNull::new_unchecked(pointers.as_ptr() as *mut NonNull<u8>),
            NonNull::new_unchecked(sizes.as_ptr() as *mut usize),
            4,
            NonNull::from(&mut out),
        )
    };
    match (status, NonNull::new(out as *mut CMVideoFormatDescription)) {
        (0, Some(ptr)) => Ok(unsafe { CFRetained::from_raw(ptr) }),
        // Jeux de paramètres refusés : le flux est abîmé, pas le décodeur.
        _ => Err(BackendError::BadData),
    }
}

impl Session {
    fn new(
        format: CFRetained<CMVideoFormatDescription>,
        sink: &Arc<FrameSink>,
    ) -> Result<Session, BackendError> {
        // On demande du BGRA : VideoToolbox convertit lui-même (sur le décodeur matériel, pas sur
        // le processeur), un seul passage reste à faire ici (BGRA vers RGBA).
        // SAFETY : lecture d'une constante du système.
        let key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
        let value = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
        let attributes = CFDictionary::from_slices(&[key], &[&*value]);
        let refcon = Arc::into_raw(sink.clone());
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_frame),
            decompressionOutputRefCon: refcon as *mut c_void,
        };
        let mut out: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY : tous les pointeurs sont valides pendant l'appel ; l'enregistrement est copié par
        // VideoToolbox.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(attributes.as_opaque()),
                &record,
                NonNull::from(&mut out),
            )
        };
        match (status, NonNull::new(out)) {
            (0, Some(session)) => Ok(Session {
                // SAFETY : `create` rend une référence +1.
                session: unsafe { CFRetained::from_raw(session) },
                format,
                refcon,
            }),
            _ => {
                // SAFETY : la session n'a pas été créée : aucun rappel ne viendra.
                drop(unsafe { Arc::from_raw(refcon) });
                Err(BackendError::Unavailable(format!(
                    "VideoToolbox refuse de créer le décodeur (code {status})"
                )))
            }
        }
    }
}

impl H264Backend for VideoToolbox {
    fn start(&mut self, parameters: &ParameterSets) -> Result<(), BackendError> {
        let format = format_description(parameters)?;
        // Les mêmes paramètres avec une autre résolution, par exemple : si la session accepte la
        // nouvelle description, on la garde ; sinon on la recrée.
        if let Some(current) = &mut self.session {
            // SAFETY : les deux objets sont valides.
            let accepted = unsafe { current.session.can_accept_format_description(&format) };
            if accepted {
                current.format = format;
                return Ok(());
            }
        }
        self.session = None;
        self.session = Some(Session::new(format, &self.sink)?);
        Ok(())
    }

    fn decode(&mut self, sample: &[u8], timestamp_ms: u32) -> Result<(), BackendError> {
        let Some(session) = &self.session else {
            return Err(BackendError::SessionLost);
        };
        let length = sample.len();
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        // SAFETY : mémoire allouée par Core Media (`AssureMemoryNow`), puis remplie avec une copie des
        // données : rien ne référence `sample` après cet appel.
        let block = unsafe {
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                length,
                None,
                ptr::null(),
                0,
                length,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            );
            let Some(block) = NonNull::new(block).filter(|_| status == 0) else {
                return Err(BackendError::BadData);
            };
            let block = CFRetained::from_raw(block);
            let copied = CMBlockBuffer::replace_data_bytes(
                NonNull::new_unchecked(sample.as_ptr() as *mut c_void),
                &block,
                0,
                length,
            );
            if copied != 0 {
                return Err(BackendError::BadData);
            }
            block
        };
        let timing = CMSampleTimingInfo {
            duration: unsafe { kCMTimeInvalid },
            presentationTimeStamp: unsafe { CMTime::new(i64::from(timestamp_ms), 1000) },
            decodeTimeStamp: unsafe { kCMTimeInvalid },
        };
        let mut buffer: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY : les pointeurs (une durée, une taille) valent pour l'appel ; l'échantillon rendu
        // (+1) est pris par `CFRetained`.
        let buffer = unsafe {
            let status = CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&session.format),
                1,
                1,
                &timing,
                1,
                &length,
                NonNull::from(&mut buffer),
            );
            let Some(buffer) = NonNull::new(buffer).filter(|_| status == 0) else {
                return Err(BackendError::BadData);
            };
            CFRetained::from_raw(buffer)
        };
        let mut info = VTDecodeInfoFlags::empty();
        // Flags à 0 : synchrone. La fonction de rappel a été appelée quand l'appel rend la main.
        // SAFETY : session et échantillon valides.
        let status = unsafe {
            session
                .session
                .decode_frame(&buffer, VTDecodeFrameFlags(0), ptr::null_mut(), &mut info)
        };
        match status {
            0 => Ok(()),
            ERR_INVALID_SESSION | ERR_MALFUNCTION | ERR_NOT_AVAILABLE_NOW => {
                // Jamais de réemploi d'une session morte (réveil du Mac) : `start` en recrée une
                // à la prochaine image clé au lieu de la juger « acceptable ».
                self.session = None;
                Err(BackendError::SessionLost)
            }
            _ => Err(BackendError::BadData),
        }
    }
}

/// La fonction de rappel de VideoToolbox : une image décodée (BGRA), convertie et déposée.
unsafe extern "C-unwind" fn on_frame(
    refcon: *mut c_void,
    _source_frame: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    // Une panique ne doit pas traverser Core Media.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(image) = NonNull::new(image) else {
            return;
        };
        if status != 0 || refcon.is_null() {
            return;
        }
        // SAFETY : `refcon` est le `FrameSink` de la session, vivant tant qu'elle existe ; l'image est
        // valide pendant la durée du rappel.
        let (sink, buffer) = unsafe { (&*(refcon as *const FrameSink), image.as_ref()) };
        if let Some(converted) = to_rgba(buffer) {
            sink.put(converted);
        }
    }));
}

/// Un `CVPixelBuffer` BGRA en image opaque (la seule conversion de la chaîne).
fn to_rgba(buffer: &CVImageBuffer) -> Option<Image> {
    // SAFETY : l'adresse de base n'est valable que verrouillée ; elle est lue entre le verrou et le
    // déverrouillage, sur `height` lignes de `bytes_per_row` octets.
    unsafe {
        if CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return None;
        }
        let width = CVPixelBufferGetWidth(buffer);
        let height = CVPixelBufferGetHeight(buffer);
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
        let converted =
            (!base.is_null() && width > 0 && height > 0 && stride >= width * 4).then(|| {
                let source = std::slice::from_raw_parts(base, stride * height);
                let mut pixels = Vec::with_capacity(width * height);
                for row in source.chunks_exact(stride) {
                    pixels.extend(
                        row[..width * 4]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|bgra| Color32::from_rgb(bgra[2], bgra[1], bgra[0])),
                    );
                }
                Image {
                    width: width as u32,
                    height: height as u32,
                    pixels,
                }
            });
        CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        converted
    }
}
