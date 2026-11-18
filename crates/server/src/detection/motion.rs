//! Détection de mouvement, bon marché (comme Frigate) : chaque image analysée est réduite en une
//! petite image grise, comparée à la précédente ; les pixels qui changent de plus de
//! [`MotionConfig::threshold`] niveaux, hors masques, sont comptés. Au-delà de
//! [`MotionConfig::min_area`] (part de l'image), il y a mouvement et le détecteur d'objets est
//! appelé. Quelques dizaines de microsecondes par image.

use protocol::wire::DetectionSettings;

use matter_home_video::Image;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionConfig {
    /// Largeur de l'image grise (la hauteur suit les proportions).
    pub width: usize,
    /// Écart de niveau de gris (0 à 255) à partir duquel un pixel a changé.
    pub threshold: u8,
    /// Part minimale de l'image (non masquée) qui doit changer.
    pub min_area: f32,
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            width: 96,
            threshold: 25,
            min_area: 0.003,
        }
    }
}

/// Une petite image en niveaux de gris.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grey {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// Réduit une image RGB en gris de `width` pixels de large (moyenne de chaque bloc).
pub fn to_grey(image: &Image, width: usize) -> Grey {
    let (sw, sh) = (image.width as usize, image.height as usize);
    if sw == 0 || sh == 0 || image.rgb.len() < sw * sh * 3 {
        return Grey {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    }
    let w = width.clamp(1, sw);
    let h = (sh * w / sw).max(1);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let (y0, y1) = (y * sh / h, ((y + 1) * sh / h).max(y * sh / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * sw / w, ((x + 1) * sw / w).max(x * sw / w + 1));
            // Un pixel sur deux dans chaque sens : assez pour une moyenne, deux fois moins cher.
            let (mut sum, mut count) = (0u32, 0u32);
            for sy in (y0..y1).step_by(2) {
                let row = sy * sw * 3;
                for sx in (x0..x1).step_by(2) {
                    let p = &image.rgb[row + sx * 3..row + sx * 3 + 3];
                    // Luminance approchée (299, 587, 114) / 1000.
                    sum +=
                        (u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8;
                    count += 1;
                }
            }
            pixels.push((sum / count.max(1)) as u8);
        }
    }
    Grey {
        width: w,
        height: h,
        pixels,
    }
}

/// Le suivi du mouvement d'une caméra.
#[derive(Debug, Default)]
pub struct Motion {
    config: MotionConfig,
    previous: Option<Grey>,
    /// Masque calculé pour cette taille et ces réglages (`true` : ignoré).
    mask: Option<(usize, usize, Vec<bool>)>,
}

impl Motion {
    pub fn new(config: MotionConfig) -> Self {
        Self {
            config,
            previous: None,
            mask: None,
        }
    }

    /// Les réglages de la caméra ont changé : le masque est à recalculer.
    pub fn reset_mask(&mut self) {
        self.mask = None;
    }

    /// La part de l'image (hors masques) qui a changé depuis l'image précédente, et s'il y a
    /// mouvement. La première image ne bouge jamais.
    pub fn update(&mut self, image: &Image, settings: &DetectionSettings) -> (f32, bool) {
        let grey = to_grey(image, self.config.width);
        let Some(previous) = self.previous.take() else {
            self.previous = Some(grey);
            return (0.0, false);
        };
        if previous.width != grey.width || previous.height != grey.height || grey.pixels.is_empty()
        {
            self.previous = Some(grey);
            self.mask = None;
            return (0.0, false);
        }
        let (w, h) = (grey.width, grey.height);
        if self
            .mask
            .as_ref()
            .is_none_or(|(mw, mh, _)| (*mw, *mh) != (w, h))
        {
            let mask = (0..h)
                .flat_map(|y| {
                    (0..w).map(move |x| ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32))
                })
                .map(|(x, y)| settings.masked(x, y))
                .collect();
            self.mask = Some((w, h, mask));
        }
        let mask = &self.mask.as_ref().expect("masque calculé").2;
        let (mut changed, mut counted) = (0u32, 0u32);
        for ((a, b), masked) in previous.pixels.iter().zip(&grey.pixels).zip(mask) {
            if *masked {
                continue;
            }
            counted += 1;
            if a.abs_diff(*b) > self.config.threshold {
                changed += 1;
            }
        }
        self.previous = Some(grey);
        let part = if counted == 0 {
            0.0
        } else {
            changed as f32 / counted as f32
        };
        (part, part >= self.config.min_area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::Polygon;

    fn flat(width: u32, height: u32, level: u8) -> Image {
        Image {
            width,
            height,
            rgb: vec![level; (width * height * 3) as usize],
        }
    }

    /// Une image grise avec un carré blanc à (x, y), en pixels.
    fn with_square(x: u32, y: u32) -> Image {
        let mut image = flat(320, 180, 40);
        for yy in y..y + 30 {
            for xx in x..x + 30 {
                let at = ((yy * 320 + xx) * 3) as usize;
                image.rgb[at..at + 3].copy_from_slice(&[250, 250, 250]);
            }
        }
        image
    }

    #[test]
    fn grey_keeps_proportions() {
        let g = to_grey(&flat(1280, 720, 100), 96);
        assert_eq!((g.width, g.height), (96, 54));
        assert!(g.pixels.iter().all(|p| (98..=100).contains(p)));
    }

    #[test]
    fn a_moving_square_is_motion_a_still_image_is_not() {
        let settings = DetectionSettings::default();
        let mut motion = Motion::new(MotionConfig::default());
        assert!(
            !motion.update(&with_square(10, 10), &settings).1,
            "première image"
        );
        assert!(
            !motion.update(&with_square(10, 10), &settings).1,
            "rien ne bouge"
        );
        let (part, moved) = motion.update(&with_square(200, 100), &settings);
        assert!(moved, "le carré a bougé ({part})");
    }

    #[test]
    fn masked_motion_is_ignored() {
        let settings = DetectionSettings {
            masks: vec![Polygon {
                points: vec![(0.5, 0.0), (1.0, 0.0), (1.0, 1.0), (0.5, 1.0)],
            }],
            ..DetectionSettings::default()
        };
        let mut motion = Motion::new(MotionConfig::default());
        motion.update(&with_square(200, 20), &settings);
        assert!(
            !motion.update(&with_square(260, 120), &settings).1,
            "dans le masque"
        );
        assert!(
            motion.update(&with_square(20, 120), &settings).1,
            "hors du masque"
        );
    }

    #[test]
    fn a_resolution_change_restarts() {
        let settings = DetectionSettings::default();
        let mut motion = Motion::new(MotionConfig::default());
        motion.update(&flat(320, 180, 0), &settings);
        assert!(!motion.update(&flat(640, 480, 255), &settings).1);
    }
}
