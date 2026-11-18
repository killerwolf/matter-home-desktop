//! Le détecteur ONNX : la caisse `ort` (ONNX Runtime) avec l'exécuteur **CoreML** (GPU de la
//! Radeon sur le Mac serveur), repli sur le processeur. Voir `docs/detection.md`.
//!
//! # Rien n'est lié, rien n'est téléchargé
//!
//! `ort` est compilé en `load-dynamic` : la bibliothèque ONNX Runtime (`libonnxruntime.dylib`) est
//! **chargée au lancement**, pas liée. Les binaires que `ort` sait télécharger n'existent pas pour
//! les Mac Intel (le serveur), et un binaire universel reste ainsi du seul code Rust. Ni le modèle
//! ni la bibliothèque ne sont dans le dépôt : on les dépose dans `<données>/models/`.
//!
//! | Fichier | Rôle |
//! |---|---|
//! | `detector.onnx` | modèle principal, sur CoreML (YOLOX-s 640 : ~50 ms par image, ~0,4 cœur) |
//! | `detector-cpu.onnx` | modèle léger, si CoreML ne démarre pas (YOLOX-nano 416, 2 fils : ~23 ms) |
//! | `detector.json` | réglages facultatifs ([`ModelSpec`]) |
//! | `libonnxruntime.dylib` | ONNX Runtime (version universelle de Microsoft, avec CoreML) |
//!
//! Le gros modèle ne tourne **jamais** sur le processeur (330 ms par image pour YOLOX-s) : sans
//! CoreML et sans `detector-cpu.onnx`, la détection reste coupée, sauf `"engine": "cpu"` dans
//! `detector.json` (pour un petit modèle seul).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::ep::coreml::{ComputeUnits, ModelFormat};
use ort::session::Session;
use ort::value::Tensor;

use matter_home_video::Image;

use super::detector::{letterbox, postprocess, Detection, Detector, ModelSpec};

pub const MODEL: &str = "detector.onnx";
pub const CPU_MODEL: &str = "detector-cpu.onnx";
pub const SPEC: &str = "detector.json";
pub const DYLIB: &str = "libonnxruntime.dylib";

/// Où chercher ONNX Runtime : le dossier des modèles, puis à côté du programme (et dans
/// `Contents/Frameworks` d'une application), puis `ORT_DYLIB_PATH`.
pub fn dylib_candidates(models: &Path) -> Vec<PathBuf> {
    let mut out = vec![models.join(DYLIB)];
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        out.push(exe_dir.join(DYLIB));
        out.push(exe_dir.join("../Frameworks").join(DYLIB));
    }
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH") {
        out.push(PathBuf::from(path));
    }
    out
}

/// ONNX Runtime ne se charge qu'une fois par processus.
fn init_runtime(models: &Path) -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let Some(path) = dylib_candidates(models).into_iter().find(|p| p.is_file()) else {
            return Err(format!(
                "ONNX Runtime absent ({DYLIB} introuvable dans le dossier des modèles)"
            ));
        };
        match ort::init_from(&path) {
            Ok(builder) => {
                builder.with_name("matter-home").commit();
                Ok(())
            }
            Err(e) => Err(format!("ONNX Runtime illisible : {e}")),
        }
    })
    .clone()
}

pub struct OnnxDetector {
    session: Session,
    input_name: String,
    input: (usize, usize),
    spec: ModelSpec,
    engine: String,
    /// Disposition de la sortie, connue à la première image.
    layout: Option<super::detector::Layout>,
}

fn read_spec(models: &Path) -> Result<ModelSpec, String> {
    match std::fs::read(models.join(SPEC)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{SPEC} illisible : {e}")),
        Err(_) => Ok(ModelSpec::default()),
    }
}

enum Engine {
    CoreMl,
    Cpu,
}

fn open(path: &Path, engine: &Engine, spec: &ModelSpec, models: &Path) -> Result<Session, String> {
    let text = |e: ort::Error<_>| e.to_string();
    let builder = Session::builder().map_err(|e| e.to_string())?;
    let mut builder = match engine {
        Engine::CoreMl => {
            let cache = models.join("coreml-cache");
            let _ = std::fs::create_dir_all(&cache);
            let ep = ort::ep::CoreML::default()
                .with_compute_units(ComputeUnits::All)
                .with_model_format(ModelFormat::MLProgram)
                .with_model_cache_dir(cache.display())
                .build()
                .error_on_failure();
            builder.with_execution_providers([ep]).map_err(text)?
        }
        Engine::Cpu => builder
            .with_intra_threads(spec.cpu_threads.clamp(1, 4))
            .map_err(text)?,
    };
    builder.commit_from_file(path).map_err(|e| e.to_string())
}

impl OnnxDetector {
    /// Charge le détecteur depuis le dossier des modèles. `Err` : la raison, en clair (pour le
    /// journal et la fenêtre du serveur) ; la détection reste alors coupée.
    pub fn load(models: &Path) -> Result<OnnxDetector, String> {
        let main = models.join(MODEL);
        let light = models.join(CPU_MODEL);
        if !main.is_file() && !light.is_file() {
            return Err(format!(
                "aucun modèle ({MODEL} absent du dossier des modèles)"
            ));
        }
        let spec = read_spec(models)?;
        init_runtime(models)?;
        let mut tried = Vec::new();
        let mut attempt = |path: &Path, engine: Engine, label: &str| -> Option<(Session, String)> {
            if !path.is_file() {
                return None;
            }
            match open(path, &engine, &spec, models) {
                Ok(session) => Some((session, label.to_string())),
                Err(e) => {
                    tried.push(format!("{label} : {e}"));
                    None
                }
            }
        };
        let threads = spec.cpu_threads.clamp(1, 4);
        let cpu_label = format!("CPU, {threads} fils");
        let opened = match spec.engine.as_str() {
            "cpu" => attempt(&main, Engine::Cpu, &cpu_label)
                .or_else(|| attempt(&light, Engine::Cpu, &cpu_label)),
            "coreml" => attempt(&main, Engine::CoreMl, "CoreML"),
            _ => attempt(&main, Engine::CoreMl, "CoreML")
                // Jamais le gros modèle sur le processeur : seulement le petit.
                .or_else(|| attempt(&light, Engine::Cpu, &cpu_label)),
        };
        let Some((session, engine)) = opened else {
            let why = if tried.is_empty() {
                format!("CoreML indisponible et pas de {CPU_MODEL} pour le processeur")
            } else {
                tried.join(" ; ")
            };
            return Err(why);
        };
        let input = session
            .inputs()
            .first()
            .ok_or("le modèle n'a pas d'entrée")?;
        let input_name = input.name().to_string();
        let shape: Vec<i64> = input
            .dtype()
            .tensor_shape()
            .map(|s| s.to_vec())
            .unwrap_or_default();
        let fallback = spec.input_size.unwrap_or(640);
        let dim = |i: usize| {
            shape
                .get(i)
                .copied()
                .filter(|d| *d > 0)
                .map_or(fallback, |d| d as usize)
        };
        let size = (dim(3), dim(2));
        Ok(OnnxDetector {
            session,
            input_name,
            input: size,
            engine: format!("{engine}, entrée {}×{}", size.0, size.1),
            spec,
            layout: None,
        })
    }
}

impl Detector for OnnxDetector {
    fn detect(&mut self, image: &Image) -> Result<Vec<Detection>, String> {
        // La disposition (et donc le format, qui décide BGR et normalisation) se lit sur la forme
        // de la sortie : déclarée par le modèle, sinon à la première image.
        if self.layout.is_none() {
            if let Some(shape) = self
                .session
                .outputs()
                .first()
                .and_then(|o| o.dtype().tensor_shape().map(|s| s.to_vec()))
                .filter(|s| s.iter().all(|d| *d > 0))
            {
                let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
                self.layout = Some(self.spec.layout(&dims, self.input)?);
            }
        }
        let format = self
            .layout
            .map_or(super::detector::HeadFormat::Yolox, |l| l.format);
        let format = match (self.layout, self.spec.format) {
            (None, super::detector::HeadFormat::Auto) => format,
            (None, f) => f,
            (Some(l), _) => l.format,
        };
        let (data, lb) = letterbox(
            image,
            self.input,
            self.spec.pad,
            self.spec.bgr_for(format),
            self.spec.normalize_for(format),
        );
        let (w, h) = self.input;
        let tensor = Tensor::from_array(([1usize, 3, h, w], data)).map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| e.to_string())?;
        let (shape, values) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let dims: Vec<usize> = shape.iter().map(|d| (*d).max(0) as usize).collect();
        let layout = match self.layout {
            Some(l) => l,
            None => {
                let l = self.spec.layout(&dims, self.input)?;
                self.layout = Some(l);
                l
            }
        };
        Ok(postprocess(values, &layout, self.input, &lb, &self.spec))
    }

    fn engine(&self) -> String {
        self.engine.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Essai manuel avec un vrai modèle (jamais en CI : ni modèle ni ONNX Runtime dans le dépôt) :
    /// `MH_MODELS=<dossier avec detector.onnx> ORT_DYLIB_PATH=<libonnxruntime.dylib>
    /// cargo test -p matter-home-server onnx -- --ignored --nocapture`.
    #[test]
    #[ignore = "manuel : MH_MODELS et ORT_DYLIB_PATH"]
    fn a_real_model_loads_and_runs() {
        let dir = PathBuf::from(std::env::var("MH_MODELS").expect("MH_MODELS"));
        let mut detector = OnnxDetector::load(&dir).expect("détecteur");
        let image = Image {
            width: 1280,
            height: 720,
            rgb: vec![90; 1280 * 720 * 3],
        };
        let started = std::time::Instant::now();
        let first = detector.detect(&image).expect("inférence");
        let warm = std::time::Instant::now();
        for _ in 0..5 {
            detector.detect(&image).expect("inférence");
        }
        eprintln!(
            "{} : première image {:?}, puis {:?} par image, {} détection(s) sur une image grise, \
             sortie {:?}",
            detector.engine(),
            warm - started,
            warm.elapsed() / 5,
            first.len(),
            detector.layout
        );
        assert!(first.is_empty(), "une image grise ne contient personne");
    }
}
