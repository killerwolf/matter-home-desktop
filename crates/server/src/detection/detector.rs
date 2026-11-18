//! Le détecteur d'objets, derrière le trait [`Detector`] (le modèle peut changer), et tout ce qui
//! ne dépend pas du moteur d'inférence : mise au format de l'image (« letterbox »), lecture de la
//! sortie du modèle, suppression des doublons (NMS), correspondance des classes COCO.
//!
//! # Formats de sortie pris en charge ([`HeadFormat`])
//!
//! - **YOLOv8 / YOLO11** : `1 × (4 + nc) × N` (ou `1 × N × (4 + nc)`), `cx, cy, w, h` en pixels de
//!   l'entrée puis un score par classe ; image RGB divisée par 255.
//! - **YOLOX** : `1 × N × (5 + nc)` : `cx, cy, w, h, objectness`, puis un score par classe ; le score
//!   est `objectness × classe`. Les modèles officiels rendent la sortie **brute** de la grille
//!   (pas 8, 16, 32) : `cx = (x + gx) × pas`, `w = exp(w) × pas`. Image BGR, bordure 114, **sans**
//!   division par 255.
//!
//! Le format est déduit de la forme de la sortie ; `models/detector.json` peut l'imposer (voir
//! [`ModelSpec`]). Un autre format s'ajoute ici, dans [`decode_output`], sans toucher au reste.

use protocol::wire::DetectedClass;
use serde::Deserialize;

use matter_home_video::Image;

/// Une détection, dans l'image d'origine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub class: DetectedClass,
    pub score: f32,
    /// Boîte normalisée (0 à 1) : `x0, y0, x1, y1`.
    pub bbox: [f32; 4],
}

impl Detection {
    /// Le bas de la boîte, au milieu : là où sont les pieds (ou les roues). C'est ce point qui doit
    /// être dans une zone, et hors des masques.
    pub fn anchor(&self) -> (f32, f32) {
        ((self.bbox[0] + self.bbox[2]) / 2.0, self.bbox[3])
    }
}

/// Un détecteur d'objets. Appelé depuis un seul fil, une image à la fois.
pub trait Detector: Send {
    /// Les objets reconnus dans l'image (avant les seuils par caméra, après un seuil plancher).
    fn detect(&mut self, image: &Image) -> Result<Vec<Detection>, String>;
    /// Le moteur, en clair (« CoreML (GPU), YOLOX 416 »).
    fn engine(&self) -> String;
}

/// Correspondance des 80 classes COCO vers les nôtres ; les autres sont ignorées.
pub fn coco_class(id: usize) -> Option<DetectedClass> {
    match id {
        0 => Some(DetectedClass::Personne),
        // vélo, voiture, moto, bus, camion
        1 | 2 | 3 | 5 | 7 => Some(DetectedClass::Vehicule),
        15 => Some(DetectedClass::Chat),
        16 => Some(DetectedClass::Chien),
        _ => None,
    }
}

/// La forme de la sortie du modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeadFormat {
    /// Déduite de la forme de la sortie.
    Auto,
    /// YOLOv8, YOLO11 (sans objectness).
    Yolov8,
    /// YOLOX (avec objectness).
    Yolox,
}

/// Le réglage du modèle : `models/detector.json`, facultatif. Les champs absents prennent la
/// valeur du format (`bgr`, `normalize`, `decode_grid`) ou de l'entrée du modèle (`input_size`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ModelSpec {
    pub format: HeadFormat,
    /// Taille de l'entrée (carrée) si le modèle ne la donne pas : 640 (YOLO11, YOLOX-s), 416
    /// (YOLOX-nano, YOLOX-tiny).
    pub input_size: Option<usize>,
    /// Canaux dans l'ordre B, V, R.
    pub bgr: Option<bool>,
    /// Pixels divisés par 255.
    pub normalize: Option<bool>,
    /// YOLOX : sortie brute de la grille à décoder.
    pub decode_grid: Option<bool>,
    /// Gris de la bordure.
    pub pad: u8,
    /// Nombre de classes du modèle (80 pour COCO).
    pub num_classes: usize,
    /// Moteur : `auto` (CoreML, puis CPU), `coreml` ou `cpu`.
    pub engine: String,
    /// Fils du moteur CPU.
    pub cpu_threads: usize,
    /// Score plancher avant les seuils par caméra.
    pub floor: f32,
    /// Chevauchement au-delà duquel deux boîtes de même classe n'en font qu'une.
    pub iou: f32,
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self {
            format: HeadFormat::Auto,
            input_size: None,
            bgr: None,
            normalize: None,
            decode_grid: None,
            pad: 114,
            num_classes: 80,
            engine: "auto".into(),
            cpu_threads: 2,
            floor: 0.25,
            iou: 0.45,
        }
    }
}

/// Le format effectif, une fois la sortie connue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub format: HeadFormat,
    /// Les valeurs d'une boîte sont contiguës (`1 × N × C`) ; sinon `1 × C × N`.
    pub rows: bool,
    pub boxes: usize,
    pub channels: usize,
    pub decode_grid: bool,
}

impl ModelSpec {
    pub fn bgr_for(&self, format: HeadFormat) -> bool {
        self.bgr.unwrap_or(format == HeadFormat::Yolox)
    }

    pub fn normalize_for(&self, format: HeadFormat) -> bool {
        self.normalize.unwrap_or(format != HeadFormat::Yolox)
    }

    /// Déduit la disposition de la sortie de sa forme (`[1, a, b]` ou `[a, b]`). `input` : taille
    /// (largeur, hauteur) de l'entrée, pour reconnaître une grille YOLOX brute.
    pub fn layout(&self, shape: &[usize], input: (usize, usize)) -> Result<Layout, String> {
        let dims: Vec<usize> = shape.iter().copied().skip_while(|d| *d == 1).collect();
        let (a, b) = match dims.as_slice() {
            [a, b] => (*a, *b),
            _ => return Err(format!("sortie du modèle inattendue : {shape:?}")),
        };
        let nc = self.num_classes;
        let pick = |format: HeadFormat| -> Option<Layout> {
            let channels = match format {
                HeadFormat::Yolox => 5 + nc,
                _ => 4 + nc,
            };
            let (rows, boxes) = if b == channels {
                (true, a)
            } else if a == channels {
                (false, b)
            } else {
                return None;
            };
            let decode_grid = format == HeadFormat::Yolox
                && self
                    .decode_grid
                    .unwrap_or_else(|| grid_size(input, &STRIDES) == boxes);
            Some(Layout {
                format,
                rows,
                boxes,
                channels,
                decode_grid,
            })
        };
        let found = match self.format {
            HeadFormat::Auto => pick(HeadFormat::Yolox).or_else(|| pick(HeadFormat::Yolov8)),
            format => pick(format),
        };
        found.ok_or_else(|| format!("sortie du modèle inattendue : {shape:?} pour {nc} classes"))
    }
}

/// Les pas de la grille YOLOX.
pub const STRIDES: [usize; 3] = [8, 16, 32];

fn grid_size((w, h): (usize, usize), strides: &[usize]) -> usize {
    strides.iter().map(|s| (w / s) * (h / s)).sum()
}

/// Comment l'image d'origine a été placée dans l'entrée du modèle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub src_w: u32,
    pub src_h: u32,
}

/// Met l'image à la taille de l'entrée (`w × h`) sans la déformer, bordures grises en bas et à
/// droite (comme YOLOX ; YOLOv8 centre, mais l'écart est sans effet sur la détection), en tenseur
/// `1 × 3 × h × w` (plus proche voisin : assez pour la détection, et peu coûteux).
pub fn letterbox(
    image: &Image,
    (w, h): (usize, usize),
    pad: u8,
    bgr: bool,
    normalize: bool,
) -> (Vec<f32>, Letterbox) {
    let (sw, sh) = (image.width as usize, image.height as usize);
    let scale = (w as f32 / sw.max(1) as f32).min(h as f32 / sh.max(1) as f32);
    let (nw, nh) = (
        ((sw as f32 * scale).round() as usize).clamp(1, w),
        ((sh as f32 * scale).round() as usize).clamp(1, h),
    );
    let k = if normalize { 1.0 / 255.0 } else { 1.0 };
    let plane = w * h;
    let mut out = vec![f32::from(pad) * k; plane * 3];
    if sw > 0 && sh > 0 && image.rgb.len() >= sw * sh * 3 {
        let xs: Vec<usize> = (0..nw).map(|x| (x * sw / nw).min(sw - 1)).collect();
        for y in 0..nh {
            let sy = (y * sh / nh).min(sh - 1);
            let row = &image.rgb[sy * sw * 3..(sy + 1) * sw * 3];
            for (x, &sx) in xs.iter().enumerate() {
                let p = &row[sx * 3..sx * 3 + 3];
                let at = y * w + x;
                let (c0, c2) = if bgr { (p[2], p[0]) } else { (p[0], p[2]) };
                out[at] = f32::from(c0) * k;
                out[plane + at] = f32::from(p[1]) * k;
                out[2 * plane + at] = f32::from(c2) * k;
            }
        }
    }
    (
        out,
        Letterbox {
            scale,
            pad_x: 0.0,
            pad_y: 0.0,
            src_w: image.width,
            src_h: image.height,
        },
    )
}

/// Une boîte candidate, en pixels de l'entrée.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub class_id: usize,
    pub score: f32,
    /// `x0, y0, x1, y1`.
    pub bbox: [f32; 4],
}

/// Lit la sortie : une candidate par boîte (sa meilleure classe), au-dessus de `floor`. Seules les
/// classes que l'on suit ([`coco_class`]) sont gardées.
pub fn decode_output(
    data: &[f32],
    layout: &Layout,
    input: (usize, usize),
    floor: f32,
) -> Vec<Candidate> {
    let (n, c) = (layout.boxes, layout.channels);
    if data.len() < n * c {
        return Vec::new();
    }
    let at = |i: usize, j: usize| -> f32 {
        if layout.rows {
            data[i * c + j]
        } else {
            data[j * n + i]
        }
    };
    let grid = if layout.decode_grid {
        grid_cells(input, &STRIDES)
    } else {
        Vec::new()
    };
    let first_class = if layout.format == HeadFormat::Yolox {
        5
    } else {
        4
    };
    let mut out = Vec::new();
    for i in 0..n {
        let objectness = if layout.format == HeadFormat::Yolox {
            at(i, 4)
        } else {
            1.0
        };
        if objectness < floor {
            continue;
        }
        let mut best = (0usize, f32::MIN);
        for k in 0..(c - first_class) {
            if coco_class(k).is_none() {
                continue;
            }
            let s = at(i, first_class + k);
            if s > best.1 {
                best = (k, s);
            }
        }
        let score = best.1 * objectness;
        if score < floor {
            continue;
        }
        let (mut cx, mut cy, mut w, mut h) = (at(i, 0), at(i, 1), at(i, 2), at(i, 3));
        if let Some(&(gx, gy, stride)) = grid.get(i) {
            let s = stride as f32;
            cx = (cx + gx as f32) * s;
            cy = (cy + gy as f32) * s;
            w = w.exp() * s;
            h = h.exp() * s;
        }
        out.push(Candidate {
            class_id: best.0,
            score,
            bbox: [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0],
        });
    }
    out
}

/// Les cellules de la grille YOLOX, dans l'ordre de la sortie : (x, y, pas).
fn grid_cells((w, h): (usize, usize), strides: &[usize]) -> Vec<(usize, usize, usize)> {
    let mut cells = Vec::with_capacity(grid_size((w, h), strides));
    for &s in strides {
        for gy in 0..h / s {
            for gx in 0..w / s {
                cells.push((gx, gy, s));
            }
        }
    }
    cells
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = w * h;
    let area = |r: &[f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Suppression des doublons, classe par classe : la meilleure boîte gagne.
pub fn nms(mut candidates: Vec<Candidate>, threshold: f32) -> Vec<Candidate> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Candidate> = Vec::new();
    for c in candidates {
        if kept
            .iter()
            .all(|k| k.class_id != c.class_id || iou(&k.bbox, &c.bbox) <= threshold)
        {
            kept.push(c);
        }
    }
    kept
}

/// De la sortie brute aux détections dans l'image d'origine.
pub fn postprocess(
    data: &[f32],
    layout: &Layout,
    input: (usize, usize),
    letterbox: &Letterbox,
    spec: &ModelSpec,
) -> Vec<Detection> {
    let candidates = nms(decode_output(data, layout, input, spec.floor), spec.iou);
    let (sw, sh) = (letterbox.src_w.max(1) as f32, letterbox.src_h.max(1) as f32);
    let s = letterbox.scale.max(f32::EPSILON);
    candidates
        .into_iter()
        .filter_map(|c| {
            let class = coco_class(c.class_id)?;
            let fx = |v: f32| ((v - letterbox.pad_x) / s / sw).clamp(0.0, 1.0);
            let fy = |v: f32| ((v - letterbox.pad_y) / s / sh).clamp(0.0, 1.0);
            let bbox = [fx(c.bbox[0]), fy(c.bbox[1]), fx(c.bbox[2]), fy(c.bbox[3])];
            (bbox[2] > bbox[0] && bbox[3] > bbox[1]).then_some(Detection {
                class,
                score: c.score.min(1.0),
                bbox,
            })
        })
        .collect()
}

/// Un détecteur pour les tests : rend ce qu'on lui a dit, image après image.
#[cfg(test)]
pub struct MockDetector {
    pub script: std::collections::VecDeque<Vec<Detection>>,
    pub calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl Detector for MockDetector {
    fn detect(&mut self, _image: &Image) -> Result<Vec<Detection>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.script.pop_front().unwrap_or_default())
    }

    fn engine(&self) -> String {
        "essai".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NC: usize = 80;

    fn spec() -> ModelSpec {
        ModelSpec::default()
    }

    #[test]
    fn coco_classes_map_to_ours() {
        assert_eq!(coco_class(0), Some(DetectedClass::Personne));
        for id in [1, 2, 3, 5, 7] {
            assert_eq!(coco_class(id), Some(DetectedClass::Vehicule));
        }
        assert_eq!(coco_class(15), Some(DetectedClass::Chat));
        assert_eq!(coco_class(16), Some(DetectedClass::Chien));
        assert_eq!(coco_class(4), None, "avion");
        assert_eq!(coco_class(62), None, "télévision");
    }

    #[test]
    fn layouts_are_recognised_from_the_shape() {
        let s = spec();
        let v8 = s.layout(&[1, 84, 8400], (640, 640)).unwrap();
        assert_eq!(
            (v8.format, v8.rows, v8.boxes, v8.decode_grid),
            (HeadFormat::Yolov8, false, 8400, false)
        );
        let v8t = s.layout(&[1, 8400, 84], (640, 640)).unwrap();
        assert!(v8t.rows && v8t.format == HeadFormat::Yolov8);
        // YOLOX-nano 416 : 52² + 26² + 13² = 3549 boîtes, grille brute.
        let x = s.layout(&[1, 3549, 85], (416, 416)).unwrap();
        assert_eq!(
            (x.format, x.rows, x.boxes, x.decode_grid),
            (HeadFormat::Yolox, true, 3549, true)
        );
        // Une sortie déjà décodée (nombre de boîtes différent de la grille).
        let decoded = s.layout(&[1, 100, 85], (416, 416)).unwrap();
        assert!(!decoded.decode_grid);
        assert!(s.layout(&[1, 3, 7], (640, 640)).is_err());
        let forced = ModelSpec {
            decode_grid: Some(false),
            ..spec()
        };
        assert!(
            !forced
                .layout(&[1, 3549, 85], (416, 416))
                .unwrap()
                .decode_grid
        );
        assert!(s.bgr_for(HeadFormat::Yolox) && !s.normalize_for(HeadFormat::Yolox));
        assert!(!s.bgr_for(HeadFormat::Yolov8) && s.normalize_for(HeadFormat::Yolov8));
    }

    #[test]
    fn letterbox_keeps_proportions_and_channel_order() {
        // 4 × 2, rouge pur.
        let image = Image {
            width: 4,
            height: 2,
            rgb: [255u8, 0, 0].repeat(8),
        };
        let (data, lb) = letterbox(&image, (8, 8), 114, false, true);
        assert_eq!(lb.scale, 2.0);
        assert_eq!(data.len(), 3 * 64);
        assert_eq!(data[0], 1.0, "R en premier, divisé par 255");
        assert_eq!(data[64], 0.0);
        // Bordure du bas : grise.
        assert!((data[7 * 8] - 114.0 / 255.0).abs() < 1e-6);
        let (bgr, _) = letterbox(&image, (8, 8), 114, true, false);
        assert_eq!(bgr[0], 0.0, "B en premier");
        assert_eq!(bgr[128], 255.0, "R en dernier, sans division");
    }

    /// Une sortie YOLOv8 (canaux d'abord) avec une personne et un doublon.
    #[test]
    fn yolov8_output_is_decoded_with_nms() {
        let n = 3;
        let c = 4 + NC;
        let mut data = vec![0.0f32; c * n];
        let mut put = |i: usize, vals: [f32; 4], class: usize, score: f32| {
            for (j, v) in vals.iter().enumerate() {
                data[j * n + i] = *v;
            }
            data[(4 + class) * n + i] = score;
        };
        put(0, [320.0, 320.0, 100.0, 200.0], 0, 0.9);
        put(1, [322.0, 322.0, 100.0, 200.0], 0, 0.6); // doublon
        put(2, [100.0, 100.0, 50.0, 50.0], 62, 0.95); // télévision : ignorée
        let s = spec();
        let layout = s.layout(&[1, c, n], (640, 640)).unwrap();
        let lb = Letterbox {
            scale: 0.5,
            pad_x: 0.0,
            pad_y: 0.0,
            src_w: 1280,
            src_h: 1280,
        };
        let found = postprocess(&data, &layout, (640, 640), &lb, &s);
        assert_eq!(found.len(), 1);
        let d = found[0];
        assert_eq!(d.class, DetectedClass::Personne);
        assert!((d.score - 0.9).abs() < 1e-6);
        assert!((d.bbox[0] - 270.0 / 640.0).abs() < 1e-4);
        assert!((d.anchor().1 - 420.0 / 640.0).abs() < 1e-4);
    }

    /// Une sortie YOLOX brute : la grille et l'objectness sont appliquées.
    #[test]
    fn yolox_raw_grid_is_decoded() {
        let input = (64, 64);
        let cells = grid_size(input, &STRIDES); // 64 + 16 + 4
        assert_eq!(cells, 84);
        let c = 5 + NC;
        let mut data = vec![0.0f32; cells * c];
        // Cellule (2, 1) du pas 16 : rang 64 + 1 × 4 + 2.
        let i = 64 + 4 + 2;
        let row = &mut data[i * c..(i + 1) * c];
        row[0] = 0.5;
        row[1] = 0.5;
        row[2] = 0.0; // exp(0) × 16 = 16
        row[3] = (2.0f32).ln(); // 32
        row[4] = 0.8;
        row[5 + 16] = 0.9; // chien
                           // Objectness faible ailleurs : rien.
        data[5 * c + 4] = 0.1;
        data[5 * c + 5] = 1.0;
        let s = spec();
        let layout = s.layout(&[1, cells, c], input).unwrap();
        assert!(layout.decode_grid);
        let found = decode_output(&data, &layout, input, 0.25);
        assert_eq!(found.len(), 1);
        let f = found[0];
        assert_eq!(f.class_id, 16);
        assert!((f.score - 0.72).abs() < 1e-5, "objectness × classe");
        // cx = (0,5 + 2) × 16 = 40, cy = (0,5 + 1) × 16 = 24.
        assert_eq!(f.bbox, [32.0, 8.0, 48.0, 40.0]);
    }

    #[test]
    fn nms_keeps_distinct_classes_and_far_boxes() {
        let c = |class_id, score, x: f32| Candidate {
            class_id,
            score,
            bbox: [x, 0.0, x + 10.0, 10.0],
        };
        let kept = nms(
            vec![
                c(0, 0.5, 0.0),
                c(0, 0.9, 1.0),
                c(16, 0.4, 0.0),
                c(0, 0.3, 50.0),
            ],
            0.45,
        );
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].score, 0.9);
    }
}
