//! Validation du plan et des règles reçus d'un client : tailles bornées, identifiants
//! uniques, valeurs plausibles. Le serveur écrit ces données sur disque et les exécute.

use std::collections::HashSet;

use protocol::automation::{Action, Condition, RuleSet, Trigger};
use protocol::home::{HomePlan, ItemKind};
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
                    if !crate::automation::is_webhook_url(url) {
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
    if plan.floors.len() > MAX_FLOORS {
        return Err(bad("trop d'étages"));
    }
    let mut ids = HashSet::new();
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
            if let ItemKind::Decor { label, .. } = &item.kind {
                check_len(label, MAX_NAME, "libellé")?;
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
        set.rules[0].actions = vec![Action::Delay { seconds: 99_999 }];
        assert!(rules(&set).is_err());
        let mut p = HomePlan::default();
        assert!(plan(&p).is_ok());
        p.floors[0].name = "x".repeat(MAX_NAME + 1);
        assert!(plan(&p).is_err());
    }
}
