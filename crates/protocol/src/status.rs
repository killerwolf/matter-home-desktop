//! La phrase d'état de la maison : la même sur le Mac, le bureau, l'iPhone et le widget.
//!
//! Fonctions pures, sans donnée ni horloge : chaque surface a déjà la liste de ses appareils et
//! de ses caméras, elle en tire les noms des injoignables et appelle ces fonctions. L'unité de
//! compte est la **catégorie** (un [`Point`]), jamais l'appareil : deux lampes injoignables font
//! « Une chose à regarder », pas deux. Une jumelle en JavaScript (`tiles.js`, `ostal.js`) reprend
//! les mêmes textes, mot pour mot (voir `docs/protocole.md`).

/// Une chose à regarder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Point {
    /// Au moins un appareil injoignable : leurs noms, dans l'ordre d'affichage.
    Devices(Vec<String>),
    /// Au moins une caméra injoignable : leurs noms.
    Cameras(Vec<String>),
    /// Tous les appareils (ou presque) ont décroché en même temps : voir [`devices_point`]. Le
    /// booléen vaut `true` quand ils sont tous injoignables. Passe avant les caméras comme
    /// [`Point::Devices`].
    DevicesTogether(bool),
    /// Autre point (serveur seulement : accès à distance, ancienne application, chaleur…) :
    /// sa phrase, déjà écrite.
    Other(String),
}

impl Point {
    /// Rien à dire : une catégorie sans nom ou une phrase vide ne compte pas.
    pub fn is_empty(&self) -> bool {
        match self {
            Point::Devices(names) | Point::Cameras(names) => names.is_empty(),
            Point::DevicesTogether(_) => false,
            Point::Other(text) => text.trim().is_empty(),
        }
    }

    /// La phrase de ce point (vide si [`Point::is_empty`]).
    pub fn phrase(&self) -> String {
        match self {
            Point::Devices(names) => unreachable_phrase(names, false),
            Point::Cameras(names) => unreachable_phrase(names, true),
            Point::DevicesTogether(all) => dropped_together_phrase(*all),
            Point::Other(text) => text.trim().to_owned(),
        }
    }
}

/// Le seuil d'une panne d'ensemble : au moins 3 appareils, dont 80 % ou plus injoignables au même
/// moment. Avec moins de 3 appareils, deux injoignables ne disent rien sur le réseau du Mac.
pub fn dropped_together(unreachable: usize, total: usize) -> bool {
    total >= 3 && unreachable <= total && unreachable * 10 >= total * 8
}

/// La cause probable d'une panne d'ensemble (sans le début de phrase).
const NETWORK_CAUSE: &str =
    "le Mac n'est sans doute plus sur le réseau de la maison (Wi-Fi, IPv6). Vérifiez le Wi-Fi du Mac.";

/// « Tous les appareils ont décroché en même temps : … » (ou « La plupart des appareils … »).
pub fn dropped_together_phrase(all: bool) -> String {
    let lead = if all { "Tous les" } else { "La plupart des" };
    format!("{lead} appareils ont décroché en même temps : {NETWORK_CAUSE}")
}

/// Le point des appareils injoignables : [`Point::DevicesTogether`] quand ils ont presque tous
/// décroché ([`dropped_together`], `total` = tous les appareils du serveur), sinon la liste de
/// leurs noms. Vide quand aucun n'est injoignable. Les surfaces (Ostal Server, app Mac) l'appellent
/// à la place de `Point::Devices`.
pub fn devices_point(unreachable: Vec<String>, total: usize) -> Point {
    if dropped_together(unreachable.len(), total) {
        Point::DevicesTogether(unreachable.len() == total)
    } else {
        Point::Devices(unreachable)
    }
}

/// Le conseil sous les appareils injoignables : le Wi-Fi du Mac pour une panne d'ensemble,
/// l'alimentation pour un ou deux appareils. Vide quand aucun n'est injoignable.
pub fn unreachable_advice(unreachable: usize, total: usize) -> &'static str {
    match unreachable {
        0 => "",
        _ if dropped_together(unreachable, total) => {
            "Vérifiez le Wi-Fi du Mac : les appareils reviennent tout seuls dès qu'il est de nouveau sur le réseau de la maison."
        }
        1 => "Vérifiez son alimentation : elle revient toute seule dès qu'elle répond.",
        _ => "Vérifiez leur alimentation : ils reviennent tout seuls dès qu'ils répondent.",
    }
}

/// Le nom est entre guillemets français : « « Lampe d'Adam » est injoignable. »,
/// « « A » et « B » sont injoignables. », « « A », « B » et 1 autre sont injoignables. » ; pour des
/// caméras, « La caméra « X » est injoignable. » et « Les caméras … ».
fn unreachable_phrase(names: &[String], cameras: bool) -> String {
    let (one, many) = if cameras {
        ("La caméra ", "Les caméras ")
    } else {
        ("", "")
    };
    match names {
        [] => String::new(),
        [a] => format!("{one}« {a} » est injoignable."),
        [a, b] => format!("{many}« {a} » et « {b} » sont injoignables."),
        [a, b, rest @ ..] => {
            let others = rest.len();
            let plural = if others > 1 { "autres" } else { "autre" };
            format!("{many}« {a} », « {b} » et {others} {plural} sont injoignables.")
        }
    }
}

/// Le titre, pour `n` points (catégories) à regarder : « La maison va bien. », « Une chose à
/// regarder », « Deux choses à regarder », puis « 3 choses à regarder » en chiffres.
pub fn headline(n: usize) -> String {
    match n {
        0 => "La maison va bien.".into(),
        1 => "Une chose à regarder".into(),
        2 => "Deux choses à regarder".into(),
        n => format!("{n} choses à regarder"),
    }
}

/// Le détail : la phrase du premier point, les appareils passant d'abord, puis les caméras, puis
/// les autres dans l'ordre reçu. `None` quand il n'y a rien à regarder (la surface écrit alors
/// [`all_reachable`] si elle connaît le nombre d'appareils).
pub fn detail(points: &[Point]) -> Option<String> {
    let rank = |p: &Point| match p {
        Point::Devices(_) | Point::DevicesTogether(_) => 0,
        Point::Cameras(_) => 1,
        Point::Other(_) => 2,
    };
    points
        .iter()
        .filter(|p| !p.is_empty())
        .min_by_key(|p| rank(p)) // `min_by_key` garde le premier à égalité.
        .map(Point::phrase)
}

/// Combien de points comptent vraiment (ceux qui ne sont pas vides), pour [`headline`].
pub fn count(points: &[Point]) -> usize {
    points.iter().filter(|p| !p.is_empty()).count()
}

/// Le détail quand tout va bien : « 10 appareils, tous joignables. »
pub fn all_reachable(devices: usize) -> String {
    match devices {
        0 => "Aucun appareil.".into(),
        1 => "1 appareil, joignable.".into(),
        n => format!("{n} appareils, tous joignables."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_point_the_house_is_fine() {
        assert_eq!(headline(0), "La maison va bien.");
        assert_eq!(detail(&[]), None);
        assert_eq!(count(&[]), 0);
        assert_eq!(all_reachable(10), "10 appareils, tous joignables.");
        assert_eq!(all_reachable(1), "1 appareil, joignable.");
        assert_eq!(all_reachable(0), "Aucun appareil.");
    }

    #[test]
    fn one_point_one_device() {
        let points = [Point::Devices(names(&["Lampe d'Adam"]))];
        assert_eq!(headline(count(&points)), "Une chose à regarder");
        assert_eq!(
            detail(&points).unwrap(),
            "« Lampe d'Adam » est injoignable."
        );
    }

    #[test]
    fn one_point_two_devices() {
        let points = [Point::Devices(names(&["Lampe d'Adam", "Prise TV"]))];
        assert_eq!(headline(count(&points)), "Une chose à regarder");
        assert_eq!(
            detail(&points).unwrap(),
            "« Lampe d'Adam » et « Prise TV » sont injoignables."
        );
    }

    #[test]
    fn one_point_three_devices_or_more() {
        let three = [Point::Devices(names(&[
            "Lampe d'Adam",
            "Prise TV",
            "Frigo",
        ]))];
        assert_eq!(
            detail(&three).unwrap(),
            "« Lampe d'Adam », « Prise TV » et 1 autre sont injoignables."
        );
        let four = [Point::Devices(names(&["A", "B", "C", "D"]))];
        assert_eq!(headline(count(&four)), "Une chose à regarder");
        assert_eq!(
            detail(&four).unwrap(),
            "« A », « B » et 2 autres sont injoignables."
        );
    }

    #[test]
    fn one_point_cameras() {
        let one = [Point::Cameras(names(&["Jardin"]))];
        assert_eq!(headline(count(&one)), "Une chose à regarder");
        assert_eq!(
            detail(&one).unwrap(),
            "La caméra « Jardin » est injoignable."
        );
        let two = [Point::Cameras(names(&["Entrée", "Jardin"]))];
        assert_eq!(
            detail(&two).unwrap(),
            "Les caméras « Entrée » et « Jardin » sont injoignables."
        );
        let three = [Point::Cameras(names(&["A", "B", "C"]))];
        assert_eq!(
            detail(&three).unwrap(),
            "Les caméras « A », « B » et 1 autre sont injoignables."
        );
    }

    #[test]
    fn two_points_devices_and_cameras_devices_come_first() {
        // Les caméras sont données avant les appareils : le détail commence quand même par eux.
        let points = [
            Point::Cameras(names(&["Jardin"])),
            Point::Devices(names(&["Lampe d'Adam", "Prise TV"])),
        ];
        assert_eq!(headline(count(&points)), "Deux choses à regarder");
        assert_eq!(
            detail(&points).unwrap(),
            "« Lampe d'Adam » et « Prise TV » sont injoignables."
        );
    }

    #[test]
    fn three_points_or_more_use_digits_and_the_first_point() {
        let points = [
            Point::Other("L'accès à distance est coupé.".into()),
            Point::Cameras(names(&["Jardin"])),
            Point::Other("La détection est suspendue par la chaleur.".into()),
        ];
        assert_eq!(headline(count(&points)), "3 choses à regarder");
        // Pas d'appareil : les caméras passent avant les autres points.
        assert_eq!(
            detail(&points).unwrap(),
            "La caméra « Jardin » est injoignable."
        );
        assert_eq!(headline(4), "4 choses à regarder");
        // Sans appareil ni caméra : la phrase du premier autre point.
        let others = [
            Point::Other("A.".into()),
            Point::Other("B.".into()),
            Point::Other("C.".into()),
        ];
        assert_eq!(detail(&others).unwrap(), "A.");
    }

    #[test]
    fn the_unit_is_the_category_not_the_device() {
        // Deux lampes et trois prises injoignables : un seul point.
        let points = [Point::Devices(names(&["L1", "L2", "P1", "P2", "P3"]))];
        assert_eq!(count(&points), 1);
        assert_eq!(headline(count(&points)), "Une chose à regarder");
    }

    #[test]
    fn empty_points_do_not_count() {
        let points = [
            Point::Devices(vec![]),
            Point::Cameras(vec![]),
            Point::Other("  ".into()),
        ];
        assert_eq!(count(&points), 0);
        assert_eq!(detail(&points), None);
        assert_eq!(headline(count(&points)), "La maison va bien.");
    }

    #[test]
    fn dropped_together_threshold() {
        assert!(!dropped_together(2, 2)); // moins de 3 appareils
        assert!(dropped_together(3, 3));
        assert!(!dropped_together(2, 3));
        assert!(dropped_together(4, 5)); // 80 %
        assert!(!dropped_together(3, 5));
        assert!(dropped_together(8, 10));
        assert!(!dropped_together(7, 10));
        assert!(!dropped_together(0, 0));
    }

    #[test]
    fn everything_dropped_names_the_network_not_the_power() {
        let point = devices_point(names(&["A", "B", "C"]), 3);
        assert_eq!(point, Point::DevicesTogether(true));
        assert_eq!(
            point.phrase(),
            "Tous les appareils ont décroché en même temps : le Mac n'est sans doute plus sur le réseau de la maison (Wi-Fi, IPv6). Vérifiez le Wi-Fi du Mac."
        );
        assert_eq!(headline(count(&[point])), "Une chose à regarder");
        let most = devices_point(names(&["A", "B", "C", "D"]), 5);
        assert!(most
            .phrase()
            .starts_with("La plupart des appareils ont décroché"));
        assert!(unreachable_advice(3, 3).contains("Wi-Fi du Mac"));
    }

    #[test]
    fn one_or_two_devices_keep_the_names_and_the_power_advice() {
        assert_eq!(
            devices_point(names(&["A"]), 10),
            Point::Devices(names(&["A"]))
        );
        // Deux appareils sur deux : trop peu pour parler du réseau.
        assert_eq!(
            devices_point(names(&["A", "B"]), 2),
            Point::Devices(names(&["A", "B"]))
        );
        assert_eq!(devices_point(vec![], 10), Point::Devices(vec![]));
        assert!(devices_point(vec![], 10).is_empty());
        assert!(unreachable_advice(1, 10).contains("alimentation"));
        assert!(unreachable_advice(2, 10).contains("leur alimentation"));
        assert_eq!(unreachable_advice(0, 10), "");
    }

    #[test]
    fn together_comes_before_cameras_in_the_detail() {
        let points = [
            Point::Cameras(names(&["Jardin"])),
            devices_point(names(&["A", "B", "C"]), 3),
        ];
        assert!(detail(&points).unwrap().starts_with("Tous les appareils"));
    }
}
