//! Les quatre icônes de la barre de menus, d'après le logo « Seuil » (une arche ouverte et un
//! point allumé : `assets/logo/serveur/e-seuil-{ok,alerte,demarrage,arret}.svg`). Dessinées en
//! noir sur fond transparent : macOS les teinte lui-même selon le thème (image « modèle »), donc
//! elles ne se distinguent que par la forme, jamais par la couleur :
//!
//! - en marche : arche épaisse et point (quelqu'un est là) ;
//! - en marche, une chose à regarder : la même, avec une pastille en haut à droite, détourée ;
//! - démarrage : arche fine, sans point (personne encore) ;
//! - le serveur ne tourne pas : arche fine et point d'exclamation sous elle.
//!
//! Les cotes sont celles des SVG (carré de 1024), ramenées à `SIZE` ; chaque pixel est
//! échantillonné 4 × 4 fois pour des bords lissés.

use crate::status::IconKind;

pub const SIZE: u32 = 44;

/// Arche des SVG : montants en x = 232 et 792, de y = 900 (bas) à y = 450 (naissance), demi-cercle
/// de centre (512, 450) et de rayon 280 par le haut ; bouts ronds.
const ARCH_LEFT: f32 = 232.0;
const ARCH_RIGHT: f32 = 792.0;
const ARCH_BOTTOM: f32 = 900.0;
const ARCH_SPRING: f32 = 450.0;
const ARCH_CENTER_X: f32 = 512.0;
const ARCH_RADIUS: f32 = 280.0;
const THICK: f32 = 120.0;
const THIN: f32 = 72.0;
/// La pastille « à regarder », et l'échancrure transparente qui la détoure.
const BADGE: (f32, f32) = (866.0, 158.0);
const BADGE_RADIUS: f32 = 120.0;
const BADGE_CLEARANCE: f32 = 200.0;

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Distance de `p` au tracé de l'arche (son axe).
fn arch_distance(p: (f32, f32)) -> f32 {
    let segment = |x: f32| {
        let y = p.1.clamp(ARCH_SPRING, ARCH_BOTTOM);
        dist(p, (x, y))
    };
    let arc = if p.1 <= ARCH_SPRING {
        (dist(p, (ARCH_CENTER_X, ARCH_SPRING)) - ARCH_RADIUS).abs()
    } else {
        f32::INFINITY
    };
    arc.min(segment(ARCH_LEFT)).min(segment(ARCH_RIGHT))
}

fn in_arch(p: (f32, f32), width: f32) -> bool {
    arch_distance(p) <= width / 2.0
}

fn in_disc(p: (f32, f32), center: (f32, f32), radius: f32) -> bool {
    dist(p, center) <= radius
}

/// Le point d'exclamation de l'état arrêté : barre arrondie (x 464 à 560, y 440 à 670) et point.
fn in_exclamation(p: (f32, f32)) -> bool {
    let bar = {
        let y = p.1.clamp(488.0, 622.0);
        dist(p, (512.0, y)) <= 48.0
    };
    bar || in_disc(p, (512.0, 740.0), 56.0)
}

/// Le point (quelqu'un est là).
fn in_dot(p: (f32, f32)) -> bool {
    in_disc(p, (512.0, 760.0), 110.0)
}

/// Le dessin, dans le repère des SVG (0 à 1024, y vers le bas).
fn covers(kind: IconKind, p: (f32, f32)) -> bool {
    match kind {
        IconKind::Running => in_arch(p, THICK) || in_dot(p),
        IconKind::Attention => {
            in_disc(p, BADGE, BADGE_RADIUS)
                || (!in_disc(p, BADGE, BADGE_CLEARANCE) && (in_arch(p, THICK) || in_dot(p)))
        }
        IconKind::Starting => in_arch(p, THIN),
        IconKind::Problem => in_arch(p, THIN) || in_exclamation(p),
    }
}

const SAMPLES: u32 = 4;

/// Pixels RGBA de l'icône, `SIZE` × `SIZE` : noir, opacité = part du pixel couverte.
pub fn rgba(kind: IconKind) -> Vec<u8> {
    let scale = 1024.0 / SIZE as f32;
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * scale;
                    let py = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * scale;
                    if covers(kind, (px, py)) {
                        hits += 1;
                    }
                }
            }
            let alpha = (hits * 255 / (SAMPLES * SAMPLES)) as u8;
            out.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [IconKind; 4] = [
        IconKind::Running,
        IconKind::Attention,
        IconKind::Starting,
        IconKind::Problem,
    ];

    fn ink(kind: IconKind) -> u32 {
        rgba(kind).chunks(4).map(|p| p[3] as u32).sum()
    }

    fn alpha_at(px: &[u8], x: f32, y: f32) -> u8 {
        let s = SIZE as f32 / 1024.0;
        let (x, y) = ((x * s) as i32, (y * s) as i32);
        px[((y * SIZE as i32 + x) * 4 + 3) as usize]
    }

    #[test]
    fn right_size_and_black_only() {
        for k in ALL {
            let px = rgba(k);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert!(px.chunks(4).all(|p| p[..3] == [0, 0, 0]));
            // Le bord du carré reste transparent : rien n'est coupé.
            for i in 0..SIZE as usize {
                for (x, y) in [(i, 0), (0, i), (i, SIZE as usize - 1)] {
                    assert_eq!(px[(y * SIZE as usize + x) * 4 + 3], 0, "{k:?} ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn states_differ_by_shape() {
        let (run, start, problem) = (
            ink(IconKind::Running),
            ink(IconKind::Starting),
            ink(IconKind::Problem),
        );
        assert!(start > 0 && start < run, "fine : {start} < épaisse : {run}");
        assert!(problem < run && problem > start);
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(rgba(*a), rgba(*b), "{a:?} = {b:?}");
            }
        }
        // Le point : présent en marche, absent au démarrage.
        assert_eq!(alpha_at(&rgba(IconKind::Running), 512.0, 760.0), 255);
        assert_eq!(alpha_at(&rgba(IconKind::Starting), 512.0, 760.0), 0);
        // Le point d'exclamation : sa barre sous l'arche.
        assert_eq!(alpha_at(&rgba(IconKind::Problem), 512.0, 560.0), 255);
        // La pastille : hors de l'arche (coin haut droit), détourée par un anneau transparent.
        let attention = rgba(IconKind::Attention);
        assert_eq!(alpha_at(&attention, BADGE.0, BADGE.1), 255);
        assert_eq!(alpha_at(&rgba(IconKind::Running), BADGE.0, BADGE.1), 0);
        assert_eq!(alpha_at(&rgba(IconKind::Running), 728.0, 272.0), 255);
        assert_eq!(alpha_at(&attention, 728.0, 272.0), 0, "échancrure attendue");
    }
}

/// Planche de contrôle (pour l'œil, pas un test) : écrit les quatre états en PGM 8 bits
/// (opacité → gris foncé sur fond clair) dans `$OSTAL_ICON_DUMP`, à 44, 32 et 16 px.
/// `OSTAL_ICON_DUMP=/tmp/x cargo test -p ostal-server-app --lib icon::dump -- --ignored`
#[cfg(test)]
mod dump {
    use super::*;

    fn downscale(px: &[u8], to: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..to {
            for x in 0..to {
                let (x0, x1) = (x * SIZE / to, ((x + 1) * SIZE).div_ceil(to));
                let (y0, y1) = (y * SIZE / to, ((y + 1) * SIZE).div_ceil(to));
                let mut sum = 0u32;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        sum += px[((yy * SIZE + xx) * 4 + 3) as usize] as u32;
                    }
                }
                let a = sum / ((x1 - x0) * (y1 - y0));
                out.push((235 - a * 215 / 255) as u8);
            }
        }
        out
    }

    #[test]
    #[ignore]
    fn dump() {
        let dir = std::path::PathBuf::from(std::env::var("OSTAL_ICON_DUMP").unwrap());
        for (name, k) in [
            ("ok", IconKind::Running),
            ("alerte", IconKind::Attention),
            ("demarrage", IconKind::Starting),
            ("arret", IconKind::Problem),
        ] {
            for size in [44, 32, 16] {
                let mut data = format!("P5\n{size} {size}\n255\n").into_bytes();
                data.extend(downscale(&rgba(k), size));
                std::fs::write(dir.join(format!("{name}-{size}.pgm")), data).unwrap();
            }
        }
    }
}
