//! Validation du plan et des règles reçus d'un client : tailles bornées, identifiants
//! uniques, valeurs plausibles. Le serveur écrit ces données sur disque et les exécute.

use std::collections::HashSet;

use protocol::automation::{Action, Condition, RuleSet, Trigger};
use protocol::home::{HomePlan, ItemKind, MAX_CAMERA_RANGE, MAX_FOV, MIN_CAMERA_RANGE, MIN_FOV};
use protocol::wire::{ApiError, ErrorCode};

pub const MAX_RULES: usize = 500;
const MAX_PARTS: usize = 50;
const MAX_NAME: usize = 200;
const MAX_TEXT: usize = 2_000;
const MAX_URL: usize = 2_048;
const MAX_BODY: usize = 8_192;
pub const MAX_DELAY_S: u64 = 3_600;
const MAX_FLOORS: usize = 20;
const MAX_ROOMS: usize = 200;
const MAX_ITEMS: usize = 1_000;
/// Coordonnées du plan : 100 km, largement assez pour une maison.
const MAX_COORD: f32 = 100_000.0;

fn bad(detail: &str) -> ApiError {
    ApiError::with_detail(ErrorCode::BadRequest, detail)
}

fn check_len(text: &str, max: usize, what: &str) -> Result<(), ApiError> {
    if text.chars().count() > max {
        return Err(bad(&format!("{what} trop long")));
    }
    Ok(())
}

pub fn rules(set: &RuleSet) -> Result<(), ApiError> {
    if set.version != 1 {
        return Err(bad("version des règles non prise en charge"));
    }
    if set.rules.len() > MAX_RULES {
        return Err(bad("trop de règles"));
    }
    let mut ids = HashSet::new();
    for rule in &set.rules {
        if !ids.insert(rule.id) {
            return Err(bad("identifiant de règle en double"));
        }
        check_len(&rule.name, MAX_NAME, "nom de règle")?;
        if rule.conditions.len() > MAX_PARTS || rule.actions.len() > MAX_PARTS {
            return Err(bad("trop de conditions ou d'actions"));
        }
        if let Trigger::Time { hour, minute, .. } = rule.trigger {
            if hour > 23 || minute > 59 {
                return Err(bad("heure invalide"));
            }
        }
        for cond in &rule.conditions {
            if let Condition::TimeBetween { from, to } = cond {
                if [from, to].iter().any(|(h, m)| *h > 23 || *m > 59) {
                    return Err(bad("heure invalide"));
                }
            }
        }
        for action in &rule.actions {
            match action {
                Action::Notify { title, message } => {
                    check_len(title, MAX_TEXT, "titre")?;
                    check_len(message, MAX_TEXT, "message")?;
                }
                Action::Webhook { url, body } => {
                    check_len(url, MAX_URL, "adresse")?;
                    check_len(body, MAX_BODY, "corps de requête")?;
                    // Une règle désactivée peut garder un brouillon d'adresse (vide, sans
                    // « http », avec un espace) : la v0.2.1 les enregistrait, et l'import les
                    // désactive au lieu de refuser toute la sauvegarde. Elle ne s'exécute jamais
                    // (le moteur ignore les règles désactivées, et l'appel `webhook` revérifie
                    // l'adresse, même pour « Tester »). Une règle activée, elle, doit être complète.
                    if rule.enabled && !crate::automation::is_webhook_url(url) {
                        return Err(bad(
                            "adresse de webhook invalide (http:// ou https:// attendu, sans espace)",
                        ));
                    }
                }
                Action::Delay { seconds } if *seconds > MAX_DELAY_S => {
                    return Err(bad("attente trop longue (une heure au plus)"));
                }
                Action::SetCover {
                    command: protocol::automation::CoverCommand::Position(p),
                    ..
                } if *p > 100 => return Err(bad("position de volet invalide")),
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn plan(plan: &HomePlan) -> Result<(), ApiError> {
    if plan.version != 1 {
        return Err(bad("version du plan non prise en charge"));
    }
    if plan.floors.is_empty() {
        return Err(ApiError::with_detail(
            ErrorCode::PlanWithoutFloor,
            "le plan doit avoir au moins un étage",
        ));
    }
    if plan.floors.len() > MAX_FLOORS {
        return Err(bad("trop d'étages"));
    }
    let mut ids = HashSet::new();
    let mut cameras = HashSet::new();
    let mut unique = |id: u64| ids.insert(id);
    let coord_ok = |v: f32| v.is_finite() && v.abs() <= MAX_COORD;
    for floor in &plan.floors {
        if !unique(floor.id) {
            return Err(bad("identifiant en double dans le plan"));
        }
        check_len(&floor.name, MAX_NAME, "nom d'étage")?;
        if floor.rooms.len() > MAX_ROOMS || floor.items.len() > MAX_ITEMS {
            return Err(bad("trop de pièces ou d'objets"));
        }
        for room in &floor.rooms {
            if !unique(room.id) {
                return Err(bad("identifiant en double dans le plan"));
            }
            check_len(&room.name, MAX_NAME, "nom de pièce")?;
            if ![room.x, room.y, room.w, room.h].into_iter().all(coord_ok) {
                return Err(bad("dimensions de pièce invalides"));
            }
        }
        for item in &floor.items {
            if !unique(item.id) {
                return Err(bad("identifiant en double dans le plan"));
            }
            if !coord_ok(item.x) || !coord_ok(item.y) {
                return Err(bad("position d'objet invalide"));
            }
            if !item.rotation.is_finite() || !(0.0..360.0).contains(&item.rotation) {
                return Err(bad("orientation d'objet invalide (0 à 360 degrés)"));
            }
            match &item.kind {
                ItemKind::Decor { label, .. } => check_len(label, MAX_NAME, "libellé")?,
                // Plage de tirage seulement, **pas l'existence** : un plan envoyé juste avant la
                // suppression d'une caméra (ou par un client pas encore au courant) ne doit pas
                // être refusé. `Hub::delete_camera` retire l'objet orphelin, et le client grise
                // ceux qui subsistent (sauvegarde importée…).
                ItemKind::Camera {
                    camera_id,
                    fov,
                    range,
                    ..
                } => {
                    if !protocol::wire::is_camera_id(*camera_id) {
                        return Err(bad("identifiant de caméra invalide"));
                    }
                    if !fov.is_finite() || !(MIN_FOV..=MAX_FOV).contains(fov) {
                        return Err(bad("ouverture de caméra invalide (10 à 180 degrés)"));
                    }
                    if !range.is_finite() || !(MIN_CAMERA_RANGE..=MAX_CAMERA_RANGE).contains(range)
                    {
                        return Err(bad("portée de caméra invalide (0,5 à 50 m)"));
                    }
                    if !cameras.insert(*camera_id) {
                        return Err(bad("caméra placée deux fois sur le plan"));
                    }
                }
                ItemKind::Device { .. } => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::automation::Rule;

    #[test]
    fn rejects_duplicates_and_bad_values() {
        let mut set = RuleSet::default();
        set.rules.push(Rule::new(1));
        assert!(rules(&set).is_ok());
        set.rules.push(Rule::new(1));
        assert!(rules(&set).is_err());
        set.rules.pop();
        set.rules[0].actions = vec![Action::Webhook {
            url: "file:///etc/passwd".into(),
            body: String::new(),
        }];
        assert!(rules(&set).is_err());
        for bad_url in ["http://a/ -K /etc/passwd", "http://a/\nb", "-K http://a"] {
            set.rules[0].actions = vec![Action::Webhook {
                url: bad_url.into(),
                body: String::new(),
            }];
            assert!(rules(&set).is_err(), "{bad_url:?}");
        }
        // Désactivée, la règle peut garder une adresse incomplète ; activée, jamais.
        set.rules[0].enabled = false;
        for draft in ["", "exemple.fr/hook", "http://a b"] {
            set.rules[0].actions = vec![Action::Webhook {
                url: draft.into(),
                body: String::new(),
            }];
            assert!(rules(&set).is_ok(), "{draft:?}");
        }
        // Les bornes de taille restent valables pour une règle désactivée.
        set.rules[0].actions = vec![Action::Webhook {
            url: "x".repeat(MAX_URL + 1),
            body: String::new(),
        }];
        assert!(rules(&set).is_err());
        set.rules[0].enabled = true;
        set.rules[0].actions = vec![Action::Delay { seconds: 99_999 }];
        assert!(rules(&set).is_err());
        let mut p = HomePlan::default();
        assert!(plan(&p).is_ok());
        // Un plan sans étage plante les clients : refusé, avec un code à part.
        let none = HomePlan {
            floors: vec![],
            ..HomePlan::default()
        };
        assert_eq!(plan(&none).unwrap_err().code, ErrorCode::PlanWithoutFloor);
        p.floors[0].name = "x".repeat(MAX_NAME + 1);
        assert!(plan(&p).is_err());
    }

    #[test]
    fn camera_items_are_checked() {
        let item = |camera_id: u64, rotation: f32| protocol::home::Item {
            id: 9,
            kind: ItemKind::camera(camera_id),
            x: 1.0,
            y: 1.0,
            rotation,
        };
        let with = |item| {
            let mut p = HomePlan::default();
            p.floors[0].items.push(item);
            p
        };
        let cam = protocol::wire::CAMERA_ID_MIN + 4;
        assert!(plan(&with(item(cam, 0.0))).is_ok());
        assert!(plan(&with(item(cam, 359.5))).is_ok());
        assert!(
            plan(&with(item(7, 0.0))).is_err(),
            "hors de la plage des caméras"
        );
        for bad_rotation in [-1.0, 360.0, f32::NAN, f32::INFINITY] {
            assert!(
                plan(&with(item(cam, bad_rotation))).is_err(),
                "{bad_rotation}"
            );
        }
    }

    #[test]
    fn camera_field_of_view_and_duplicates_are_checked() {
        use protocol::home::{FovShape, Item};
        let cam = protocol::wire::CAMERA_ID_MIN + 4;
        let item = |id: u64, camera_id: u64, fov: f32, range: f32| Item {
            id,
            kind: ItemKind::Camera {
                camera_id,
                fov,
                range,
                shape: FovShape::Rectangle,
            },
            x: 1.0,
            y: 1.0,
            rotation: 45.0,
        };
        let with = |items: Vec<Item>| {
            let mut p = HomePlan::default();
            p.floors[0].items = items;
            p
        };
        assert!(plan(&with(vec![item(9, cam, 10.0, 0.5)])).is_ok());
        assert!(plan(&with(vec![item(9, cam, 180.0, 50.0)])).is_ok());
        for (fov, range) in [
            (9.9, 3.0),
            (180.1, 3.0),
            (f32::NAN, 3.0),
            (90.0, 0.4),
            (90.0, 50.1),
            (90.0, f32::INFINITY),
            (90.0, f32::NAN),
        ] {
            assert!(
                plan(&with(vec![item(9, cam, fov, range)])).is_err(),
                "{fov} {range}"
            );
        }
        // Une même caméra posée deux fois est refusée ; deux caméras différentes, non.
        assert!(plan(&with(vec![
            item(9, cam, 90.0, 3.0),
            item(10, cam, 90.0, 3.0)
        ]))
        .is_err());
        assert!(plan(&with(vec![
            item(9, cam, 90.0, 3.0),
            item(10, cam + 1, 90.0, 3.0)
        ]))
        .is_ok());
    }

    #[test]
    fn a_camera_that_does_not_exist_is_not_refused_here() {
        // Décision du ticket n° 6 : `validate::plan` ne connaît pas la liste des caméras, et ne
        // doit pas : un plan envoyé avant une suppression passe. L'orphelin est grisé côté client.
        let mut p = HomePlan::default();
        p.floors[0].items.push(protocol::home::Item {
            id: 9,
            kind: ItemKind::camera(protocol::wire::CAMERA_ID_MIN + 123_456),
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        });
        assert!(plan(&p).is_ok());
    }
}
