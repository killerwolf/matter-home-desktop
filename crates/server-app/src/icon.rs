//! Les quatre icônes de la barre de menus, dessinées en noir sur fond transparent : macOS les
//! teinte lui-même selon le thème (icône « template »), donc elles ne se distinguent que par la
//! forme, jamais par la couleur : maison pleine (tout va bien), maison pleine avec une pastille
//! en haut à droite (en marche, un point à regarder), maison creuse (démarrage), maison pleine
//! avec un point d'exclamation (le serveur ne tourne pas).

use crate::status::IconKind;

pub const SIZE: u32 = 44;

fn in_house(x: i32, y: i32) -> bool {
    // Toit : triangle de sommet (22, 5) et de base y = 21 entre x = 5 et x = 39.
    let roof = (5..=21).contains(&y) && (x - 22).abs() <= (y - 5) * 17 / 16;
    // Corps : rectangle.
    let body = (11..=33).contains(&x) && (20..=38).contains(&y);
    roof || body
}

fn exclamation(x: i32, y: i32) -> bool {
    ((20..=23).contains(&x) && (22..=31).contains(&y))
        || ((20..=23).contains(&x) && (34..=37).contains(&y))
}

/// La pastille « à regarder », en haut à droite : un disque plein, détouré de la maison par un
/// anneau transparent.
const BADGE: (i32, i32) = (35, 9);

fn badge_distance2(x: i32, y: i32) -> i32 {
    (x - BADGE.0).pow(2) + (y - BADGE.1).pow(2)
}

/// Pixels RGBA de l'icône, `SIZE` × `SIZE`.
pub fn rgba(kind: IconKind) -> Vec<u8> {
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE as i32 {
        for x in 0..SIZE as i32 {
            let solid = in_house(x, y);
            let on = match kind {
                IconKind::Running => solid,
                IconKind::Attention => {
                    let d2 = badge_distance2(x, y);
                    d2 <= 7 * 7 || (solid && d2 > 10 * 10)
                }
                IconKind::Problem => solid && !exclamation(x, y),
                IconKind::Starting => {
                    solid && !(-3..=3).all(|dy| (-3..=3).all(|dx| in_house(x + dx, y + dy)))
                }
            };
            out.extend_from_slice(&[0, 0, 0, if on { 255 } else { 0 }]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(kind: IconKind) -> usize {
        rgba(kind).chunks(4).filter(|p| p[3] == 255).count()
    }

    #[test]
    fn right_size_and_black_only() {
        for k in [
            IconKind::Running,
            IconKind::Attention,
            IconKind::Starting,
            IconKind::Problem,
        ] {
            let px = rgba(k);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert!(px.chunks(4).all(|p| p[..3] == [0, 0, 0]));
        }
    }

    #[test]
    fn states_differ_by_shape() {
        let (run, start, problem) = (
            opaque(IconKind::Running),
            opaque(IconKind::Starting),
            opaque(IconKind::Problem),
        );
        assert!(
            start > 0 && start < run,
            "creuse : {start} < pleine : {run}"
        );
        assert!(problem < run && problem > start);
        assert_ne!(rgba(IconKind::Running), rgba(IconKind::Problem));
        assert_ne!(rgba(IconKind::Running), rgba(IconKind::Starting));
        // La pastille : hors de la maison (coin haut droit), et détourée d'elle.
        let attention = rgba(IconKind::Attention);
        let at = |px: &[u8], x: i32, y: i32| px[((y * SIZE as i32 + x) * 4 + 3) as usize] == 255;
        assert!(at(&attention, BADGE.0, BADGE.1));
        assert!(!at(&rgba(IconKind::Running), BADGE.0, BADGE.1));
        assert!(!at(&attention, 28, 14), "anneau transparent attendu");
        for other in [IconKind::Running, IconKind::Starting, IconKind::Problem] {
            assert_ne!(attention, rgba(other));
        }
    }
}
