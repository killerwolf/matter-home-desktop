//! Le détecteur ONNX : la caisse `ort` (ONNX Runtime) avec l'exécuteur **CoreML** (GPU de la
//! Radeon sur le Mac serveur), repli sur le processeur. Voir `docs/detection.md`.
//!
//! # Rien n'est lié à la compilation
//!
//! `ort` est compilé en `load-dynamic` : la bibliothèque ONNX Runtime (`libonnxruntime.dylib`) est
//! **chargée au lancement**, pas liée. Les binaires que `ort` sait télécharger n'existent pas pour
//! les Mac Intel (le serveur), et un binaire universel reste ainsi du seul code Rust.
//!
//! # Où sont les fichiers
//!
//! L'application du serveur les **embarque** (`scripts/bundle-server-app.sh`, empreintes SHA-256
//! vérifiées) : `Contents/Frameworks/libonnxruntime.dylib` et `Contents/Resources/models/`. Ils
//! l'emportent. Sans eux (serveur en ligne de commande, version de développement), on prend ceux
//! de `<données>/models/`, puis `ORT_DYLIB_PATH` pour la bibliothèque.
//!
//! | Fichier | Rôle |
//! |---|---|
//! | `detector.onnx` | modèle principal, sur CoreML (YOLOX-s 640 : ~50 ms par image, ~0,4 cœur) |
//! | `detector-cpu.onnx` | modèle léger, si CoreML ne démarre pas (YOLOX-nano 416, 2 fils : ~23 ms) |
//! | `detector.json` | réglages facultatifs ([`ModelSpec`]) |
//! | `libonnxruntime.dylib` | ONNX Runtime (version universelle de Microsoft, avec CoreML) |
//!
//! Un fichier qui n'appartient pas à l'utilisateur (ou à root), ou que le groupe ou les autres
//! peuvent modifier, est **refusé** : c'est du code que le serveur exécute.
//!
//! Le gros modèle ne tourne **jamais** sur le processeur (330 ms par image pour YOLOX-s) : sans
//! CoreML et sans `detector-cpu.onnx`, la détection reste coupée, sauf `"engine": "cpu"` dans
//! `detector.json` (pour un petit modèle seul).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::ep::coreml::{ComputeUnits, ModelFormat};
use ort::session::Session;
use ort::value::Tensor;

use matter_home_video::Image;

use super::detector::{letterbox, postprocess, Detection, Detector, ModelSpec};

pub const MODEL: &str = "detector.onnx";
pub const CPU_MODEL: &str = "detector-cpu.onnx";
pub const SPEC: &str = "detector.json";
pub const DYLIB: &str = "libonnxruntime.dylib";

/// Le dossier `Contents` de l'application, si le programme est dans `Contents/MacOS`.
fn bundle_contents() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    (macos.file_name()? == "MacOS").then(|| macos.parent().map(Path::to_path_buf))?
}

/// Les dossiers de modèles, dans l'ordre : celui de l'application, puis `<données>/models/`.
pub fn model_dirs(models: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(contents) = bundle_contents() {
        out.push(contents.join("Resources/models"));
    }
    out.push(models.to_path_buf());
    out
}

/// Où chercher ONNX Runtime : l'application (`Contents/Frameworks`) d'abord, puis à côté du
/// programme, puis le dossier des modèles, puis `ORT_DYLIB_PATH`.
pub fn dylib_candidates(models: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(contents) = bundle_contents() {
        out.push(contents.join("Frameworks").join(DYLIB));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        out.push(exe_dir.join(DYLIB));
    }
    out.push(models.join(DYLIB));
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH") {
        out.push(PathBuf::from(path));
    }
    out
}

/// Un fichier qu'on peut charger : à l'utilisateur (ou à root), non modifiable par le groupe ni
/// par les autres. `Err` : la raison.
pub fn trusted(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|e| format!("illisible ({})", e.kind()))?;
        // SAFETY : `getuid` n'a ni argument ni effet de bord.
        let me = unsafe { libc::getuid() };
        if meta.uid() != me && meta.uid() != 0 {
            return Err("n'appartient pas à cet utilisateur".into());
        }
        if meta.mode() & 0o022 != 0 {
            return Err("modifiable par d'autres comptes (droits trop larges)".into());
        }
    }
    let _ = path;
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// ONNX Runtime ne se charge qu'une fois par processus ; seul un **succès** est retenu : un
/// échec (bibliothèque absente) est réessayé au prochain chargement du détecteur.
fn init_runtime(models: &Path) -> Result<(), String> {
    static LOADED: Mutex<bool> = Mutex::new(false);
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if *loaded {
        return Ok(());
    }
    let mut refused = Vec::new();
    let path = dylib_candidates(models)
        .into_iter()
        .filter(|p| p.is_file())
        .find(|p| match trusted(p) {
            Ok(()) => true,
            Err(why) => {
                refused.push(format!("{} refusée : {why}", file_name(p)));
                false
            }
        });
    let Some(path) = path else {
        return Err(match refused.first() {
            Some(why) => format!("ONNX Runtime {why}"),
            None => format!("ONNX Runtime absent ({DYLIB} introuvable)"),
        });
    };
    match ort::init_from(&path) {
        Ok(builder) => {
            builder.with_name("matter-home").commit();
            *loaded = true;
            Ok(())
        }
        Err(e) => Err(format!("ONNX Runtime illisible : {e}")),
    }
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
            let _ = crate::persistence::ensure_private_dir(&cache);
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
        // Le premier dossier qui a un modèle : celui de l'application, sinon `<données>/models/`.
        let dir = model_dirs(models)
            .into_iter()
            .find(|d| d.join(MODEL).is_file() || d.join(CPU_MODEL).is_file())
            .ok_or_else(|| format!("aucun modèle ({MODEL} absent du dossier des modèles)"))?;
        let (main, light) = (dir.join(MODEL), dir.join(CPU_MODEL));
        for path in [&main, &light].into_iter().filter(|p| p.is_file()) {
            trusted(path).map_err(|why| format!("{} refusé : {why}", file_name(path)))?;
        }
        // Les réglages : ceux de `<données>/models/` l'emportent (utilisateur avancé), sinon ceux
        // du dossier du modèle.
        let spec = if models.join(SPEC).is_file() {
            read_spec(models)?
        } else {
            read_spec(&dir)?
        };
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
