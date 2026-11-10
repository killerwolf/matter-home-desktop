//! Identifiants en texte : adaptateurs serde.

use std::fmt;

use serde::de::{DeserializeOwned, Error as _, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Identifiant mal formé (pas du texte décimal canonique d'un `u64`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(pub String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "identifiant invalide : {}", self.0)
    }
}

impl std::error::Error for IdError {}

/// Lit un identifiant : chiffres décimaux seulement, sans zéro initial (sauf « 0 »), pour
/// qu'un identifiant n'ait qu'une écriture.
pub fn parse_id(text: &str) -> Result<u64, IdError> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return Err(IdError(text.chars().take(24).collect()));
    }
    text.parse()
        .map_err(|_| IdError(text.chars().take(24).collect()))
}

/// `#[serde(with = "text_id")]` : un `u64` écrit en chaîne dans le JSON.
pub mod text_id {
    use super::*;

    pub fn serialize<S: Serializer>(id: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&id.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = u64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("un identifiant en texte décimal")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u64, E> {
                parse_id(v).map_err(E::custom)
            }
        }
        d.deserialize_str(V)
    }
}

/// Clés JSON qui portent un identifiant dans le plan et les règles.
const ID_KEYS: [&str; 4] = ["id", "node_id", "rule_id", "camera_id"];

fn stringify(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if ID_KEYS.contains(&key.as_str()) {
                    if let Some(n) = v.as_u64() {
                        *v = Value::String(n.to_string());
                        continue;
                    }
                }
                stringify(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(stringify),
        _ => {}
    }
}

fn numberify(value: &mut Value) -> Result<(), IdError> {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if ID_KEYS.contains(&key.as_str()) {
                    match v {
                        Value::String(text) => {
                            *v = Value::from(parse_id(text)?);
                            continue;
                        }
                        // Un identifiant numérique serait arrondi côté JavaScript : refusé.
                        Value::Number(n) => return Err(IdError(n.to_string())),
                        _ => {}
                    }
                }
                numberify(v)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                numberify(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `#[serde(with = "as_text_ids")]` pour un document déjà sérialisable tel quel (le plan,
/// les règles) : ses identifiants (`id`, `node_id`, `rule_id`, à toute profondeur) passent
/// en texte sur le fil, sans changer le format des fichiers `maison.json` et `regles.json`.
pub mod as_text_ids {
    use super::*;

    pub fn serialize<T: Serialize, S: Serializer>(doc: &T, s: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(doc).map_err(serde::ser::Error::custom)?;
        stringify(&mut value);
        value.serialize(s)
    }

    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        d: D,
    ) -> Result<T, D::Error> {
        let mut value = Value::deserialize(d)?;
        numberify(&mut value).map_err(D::Error::custom)?;
        serde_json::from_value(value).map_err(D::Error::custom)
    }
}

/// Enveloppe qui applique [`as_text_ids`] : `TextIds<HomePlan>`, `TextIds<RuleSet>`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextIds<T>(pub T);

impl<T: Serialize> Serialize for TextIds<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        as_text_ids::serialize(&self.0, s)
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for TextIds<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        as_text_ids::deserialize(d).map(TextIds)
    }
}
