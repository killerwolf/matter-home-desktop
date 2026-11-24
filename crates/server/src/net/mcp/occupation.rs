//! Les outils du droit « occupation » (étape E1b) : le mode de la maison et qui est là. « Vacances »
//! dit que la maison est vide ; une caméra intérieure qui surveille dit que personne n'y est. Ces
//! outils ne sont vus et appelés que par un jeton qui porte ce droit (éteint par défaut, jamais
//! donné par la ligne de commande), et chacun de leurs appels est écrit **un par un** au journal.
//!
//! Présence : les personnes **actives** seulement, leur prénom nettoyé, `home` ou `away` et depuis
//! quand. Jamais le nom d'un téléphone, une adresse, l'état de la box ni le réglage du mode
//! automatique.

use protocol::mode::HomeMode;
use serde_json::{json, Value};

use super::reads::behavior_word;
use super::tools::{name_of, Ctx};
use crate::hub::Hub;
use crate::presence::Whereabouts;

/// Le mode, en clair : sa clé, son libellé et la phrase d'effet par défaut.
pub(super) fn mode_json(mode: HomeMode) -> Value {
    json!({
        "key": mode.key(),
        "label": mode.label(),
        "effect": mode.effect(),
    })
}

pub(super) fn get_mode(hub: &Hub) -> Value {
    let snapshot = hub.current_snapshot();
    let clean = hub.push_cleaner();
    let mode = snapshot.mode.unwrap_or_else(|| hub.mode());
    // Ce que fait chaque caméra dans CE mode, d'après son propre tableau (une caméra réglée à la
    // main s'écarte de la phrase d'effet par défaut).
    let cameras: Vec<Value> = snapshot
        .cameras
        .iter()
        .map(|c| {
            json!({
                "id": c.id.to_string(),
                "name": name_of(&clean, &c.name),
                "behavior_now": behavior_word(c.modes.get(mode)),
            })
        })
        .collect();
    let mut out = mode_json(mode);
    out["cameras"] = json!(cameras);
    out
}

/// Le prénom : le premier mot du nom affiché (jamais le nom de famille, qu'un compte peut y mettre).
fn first_name(display_name: &str) -> String {
    display_name
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

pub(super) fn get_presence(hub: &Hub, ctx: &Ctx<'_>) -> Value {
    let clean = hub.push_cleaner();
    let mut people: Vec<(String, Whereabouts, f64, bool)> = hub
        .presence_of(ctx.accounts)
        .into_iter()
        .filter_map(|d| {
            let name = ctx.accounts.display_name(&d.user_id)?;
            // Le prénom seulement : le premier mot du nom affiché.
            Some((
                first_name(&name_of(&clean, &name)),
                d.state,
                d.since,
                d.user_id == ctx.agent.user_id,
            ))
        })
        .collect();
    // Ceux qui sont là d'abord, puis par prénom : un ordre qui ne change pas d'un appel à l'autre.
    people.sort_by(|a, b| {
        (a.1 != Whereabouts::Home, a.0.to_lowercase())
            .cmp(&(b.1 != Whereabouts::Home, b.0.to_lowercase()))
    });
    json!({
        "people": people
            .iter()
            .map(|(name, state, since, me)| json!({
                "name": name,
                "state": match state {
                    Whereabouts::Home => "home",
                    Whereabouts::Away => "away",
                },
                "since": since,
                "me": me,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::first_name;

    #[test]
    fn presence_gives_the_first_word_of_the_display_name_only() {
        assert_eq!(first_name("Hamdi LAADHARI"), "Hamdi");
        assert_eq!(first_name("  Léa   Martin-Dupont "), "Léa");
        assert_eq!(first_name("Sam"), "Sam");
        assert_eq!(first_name(""), "");
    }
}
