//! Vérification d'un flux RTSP (`DESCRIBE`) et bouton « Tester ».

use std::fmt;
use std::time::Duration;

use url::Url;

use protocol::wire::{CameraProblem, StreamInfo, StreamKind};

use super::store::StoredCamera;

/// Délai maximal d'une vérification (connexion comprise).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

// ---------------------------------------------------------------------------
// Vérification d'un flux (DESCRIBE)
// ---------------------------------------------------------------------------

/// Identifiants d'une caméra. `Debug` masque tout.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials { … }")
    }
}

/// Ce qu'il faut pour parler à un flux : l'adresse (sans identifiants) et les identifiants.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub url: Url,
    pub credentials: Option<Credentials>,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Target { … }")
    }
}

/// Une vérification échouée : le type de problème et une phrase pour l'utilisateur (sans
/// adresse ni identifiant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub problem: CameraProblem,
    pub message: String,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProbeError {}

fn probe_error(problem: CameraProblem, message: &str) -> ProbeError {
    ProbeError {
        problem,
        message: message.to_string(),
    }
}

/// Classe une erreur de `retina` (opaque) d'après son texte. Le texte brut ne sort jamais : il
/// contient les adresses de la connexion.
pub(super) fn classify(error: &retina::Error) -> ProbeError {
    let text = error.to_string();
    if text.contains("401 response") || text.contains("403 response") {
        probe_error(
            CameraProblem::Unauthorized,
            "La caméra refuse l'identifiant ou le mot de passe.",
        )
    } else if text.contains("404 response") {
        probe_error(
            CameraProblem::NotFound,
            "La caméra ne connaît pas cette adresse de flux (vérifiez le chemin, par exemple /live/ch00_1).",
        )
    } else if text.contains("Unable to connect") {
        probe_error(
            CameraProblem::Unreachable,
            "La caméra ne répond pas (éteinte, hors réseau, RTSP désactivé ou mauvais port).",
        )
    } else if text.contains("Error reading from RTSP peer")
        || text.contains("Error writing to RTSP peer")
    {
        probe_error(
            CameraProblem::Unreachable,
            "La connexion avec la caméra s'est interrompue.",
        )
    } else if text.contains("RTSP framing error") || text.contains("Unparseable") {
        probe_error(
            CameraProblem::Protocol,
            "La caméra a répondu, mais pas comme une caméra RTSP.",
        )
    } else if text.contains("response to DESCRIBE") {
        probe_error(
            CameraProblem::Failed,
            "La caméra a refusé de décrire ce flux.",
        )
    } else {
        probe_error(CameraProblem::Failed, "Échec de la connexion à la caméra.")
    }
}

/// Décrit un flux : un `DESCRIBE`, lu par `retina` (authentification Basic ou Digest comprise),
/// puis la connexion est lâchée. Ni `SETUP` ni `PLAY` : la caméra ne démarre aucun flux.
///
/// Rend le codec et, quand la caméra les annonce dans sa description (SPS H.264 ou H.265), les
/// dimensions du premier flux vidéo.
pub async fn probe(target: &Target, timeout: Duration) -> Result<StreamInfo, ProbeError> {
    let mut options = retina::client::SessionOptions::default()
        .user_agent(format!("MatterHomeServer/{}", env!("CARGO_PKG_VERSION")));
    if let Some(c) = &target.credentials {
        options = options.creds(Some(retina::client::Credentials {
            username: c.username.clone(),
            password: c.password.clone(),
        }));
    }
    let described = tokio::time::timeout(
        timeout,
        retina::client::Session::describe(target.url.clone(), options),
    )
    .await
    .map_err(|_| {
        probe_error(
            CameraProblem::Unreachable,
            "La caméra n'a pas répondu à temps.",
        )
    })?
    .map_err(|e| classify(&e))?;
    let stream = described
        .streams()
        .iter()
        .find(|s| s.media() == "video")
        .ok_or_else(|| {
            probe_error(
                CameraProblem::Protocol,
                "Aucun flux vidéo dans la réponse de la caméra.",
            )
        })?;
    let (width, height) = match stream.parameters() {
        Some(retina::codec::ParametersRef::Video(v)) => {
            let (w, h) = v.pixel_dimensions();
            (Some(w), Some(h))
        }
        _ => (None, None),
    };
    Ok(StreamInfo {
        codec: stream.encoding_name().to_ascii_lowercase(),
        width,
        height,
    })
}

/// [`probe`] pour un appelant sans runtime (la fenêtre du serveur) : bloque jusqu'au résultat
/// ou au délai.
pub fn probe_blocking(target: &Target, timeout: Duration) -> Result<StreamInfo, ProbeError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| probe_error(CameraProblem::Failed, "Vérification impossible."))?;
    let outcome = runtime.block_on(probe(target, timeout));
    // Pas de `drop` ordinaire : la résolution DNS passe par un fil de `spawn_blocking` qui peut
    // rester bloqué, et l'attendre figerait l'appelant bien au-delà du délai.
    runtime.shutdown_background();
    outcome
}

/// « H.264 · 1280 × 720 » : le résultat d'une vérification, pour l'utilisateur.
pub fn describe_stream(info: &StreamInfo) -> String {
    let codec = match info.codec.as_str() {
        "h264" => "H.264".to_string(),
        "h265" => "H.265".to_string(),
        "jpeg" => "MJPEG".to_string(),
        other => other.to_ascii_uppercase(),
    };
    match (info.width, info.height) {
        (Some(w), Some(h)) => format!("{codec} · {w} × {h}"),
        _ => format!("{codec} · résolution annoncée au premier flux"),
    }
}

/// Résultat du bouton « Tester » pour un flux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTest {
    pub kind: StreamKind,
    pub outcome: Result<StreamInfo, ProbeError>,
}

impl StreamTest {
    /// « Flux principal : H.264 · 1280 × 720 » ou « Flux réduit : La caméra refuse… ».
    pub fn describe(&self) -> String {
        match &self.outcome {
            Ok(info) => format!("{} : {}", self.kind.label(), describe_stream(info)),
            Err(e) => format!("{} : {}", self.kind.label(), e.message),
        }
    }
}

/// Un test en cours du bouton « Tester » : un fil à part, **annulable** sur-le-champ (le bouton
/// « Annuler », la fermeture du formulaire, ou simplement lâcher ce témoin).
///
/// Pour une caméra déjà enregistrée, le test prend d'abord le verrou de la caméra
/// (`Hub::camera_probe_lock`), le même que la surveillance : jamais deux connexions à la même
/// caméra en même temps. Les flux sont testés l'un après l'autre.
pub struct CameraTest {
    rx: std::sync::mpsc::Receiver<Result<Vec<StreamTest>, super::CameraError>>,
    cancel: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

impl CameraTest {
    pub fn spawn(
        camera: StoredCamera,
        lock: Option<std::sync::Arc<std::sync::Mutex<()>>>,
    ) -> CameraTest {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
        let spawned = std::thread::Builder::new()
            .name("test-camera".into())
            .spawn(move || {
                let outcome = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let outcome =
                            runtime.block_on(run_test(&camera, lock.as_deref(), &mut cancelled));
                        runtime.shutdown_background();
                        outcome
                    }
                    Err(_) => Err(super::CameraError("Test impossible.".into())),
                };
                let _ = tx.send(outcome);
            });
        if spawned.is_err() {
            log::error!("fil du test de caméra impossible à lancer");
        }
        CameraTest {
            rx,
            cancel: std::sync::Arc::new(cancel),
        }
    }

    /// Un test déjà terminé (mode démo : aucun trafic).
    pub fn ready(outcome: Result<Vec<StreamTest>, super::CameraError>) -> CameraTest {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(outcome);
        let (cancel, _) = tokio::sync::watch::channel(false);
        CameraTest {
            rx,
            cancel: std::sync::Arc::new(cancel),
        }
    }

    /// Le résultat, s'il est arrivé (une seule fois).
    pub fn poll(&self) -> Option<Result<Vec<StreamTest>, super::CameraError>> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(super::CameraError("Le test s'est interrompu.".into())))
            }
        }
    }

    /// Abandonne le test : le fil s'arrête tout de suite et rend le verrou de la caméra.
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// Attend le résultat (tests, appels sans interface).
    pub fn wait(self) -> Result<Vec<StreamTest>, super::CameraError> {
        self.rx
            .recv()
            .unwrap_or_else(|_| Err(super::CameraError("Le test s'est interrompu.".into())))
    }
}

impl Drop for CameraTest {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn run_test(
    camera: &StoredCamera,
    lock: Option<&std::sync::Mutex<()>>,
    cancelled: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Vec<StreamTest>, super::CameraError> {
    let stop = || super::CameraError("Test annulé.".into());
    // La surveillance a peut-être une vérification en cours sur cette caméra : on attend son tour.
    let _guard = match lock {
        Some(lock) => loop {
            match lock.try_lock() {
                Ok(guard) => break Some(guard),
                Err(std::sync::TryLockError::Poisoned(e)) => break Some(e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    tokio::select! {
                        _ = cancelled.wait_for(|c| *c) => return Err(stop()),
                        () = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
            }
        },
        None => None,
    };
    let mut results = Vec::new();
    for kind in [StreamKind::Main, StreamKind::Sub] {
        let Some(target) = camera.target(kind) else {
            continue;
        };
        let outcome = tokio::select! {
            _ = cancelled.wait_for(|c| *c) => return Err(stop()),
            outcome = probe(&target, PROBE_TIMEOUT) => outcome,
        };
        results.push(StreamTest { kind, outcome });
    }
    Ok(results)
}
