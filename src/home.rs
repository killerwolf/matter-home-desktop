//! Plan de la maison : étages, pièces rectangulaires et objets placés.
//! Coordonnées en mètres, origine en haut à gauche, y vers le bas.
//! Sauvegardé dans `maison.json` (ou `maison-demo.json` en mode démo).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "maison.json";
pub const DEMO_FILE_NAME: &str = "maison-demo.json";

/// Pas de la grille magnétique des pièces (m).
pub const ROOM_SNAP: f32 = 0.5;
/// Pas de placement des objets (m).
pub const ITEM_SNAP: f32 = 0.25;
/// Taille minimale d'une pièce (m).
pub const MIN_ROOM_SIZE: f32 = 1.0;

/// Pictogramme d'un objet sur le plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Symbol {
    Door,
    Window,
    Light,
    Plug,
    Thermometer,
    Button,
}

impl Symbol {
    pub const ALL: [Symbol; 6] = [
        Symbol::Door,
        Symbol::Window,
        Symbol::Light,
        Symbol::Plug,
        Symbol::Thermometer,
        Symbol::Button,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Symbol::Door => "Porte",
            Symbol::Window => "Fenêtre",
            Symbol::Light => "Lumière",
            Symbol::Plug => "Prise",
            Symbol::Thermometer => "Thermomètre",
            Symbol::Button => "Bouton",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ItemKind {
    /// Appareil Matter appairé ; `symbol` remplace le pictogramme déduit de ses capacités
    /// (ex. un détecteur d'ouverture posé sur une fenêtre).
    Device {
        node_id: u64,
        symbol: Option<Symbol>,
    },
    /// Objet non connecté, pour la lisibilité du plan.
    Decor { symbol: Symbol, label: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub kind: ItemKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub id: u64,
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Room {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Floor {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub rooms: Vec<Room>,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Floor {
    /// Pièce la plus haute (dernière dessinée) qui contient ce point.
    pub fn room_at(&self, x: f32, y: f32) -> Option<&Room> {
        self.rooms.iter().rev().find(|r| r.contains(x, y))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomePlan {
    pub version: u32,
    pub floors: Vec<Floor>,
}

impl Default for HomePlan {
    fn default() -> Self {
        Self {
            version: 1,
            floors: vec![Floor {
                id: 1,
                name: "Rez-de-chaussée".into(),
                rooms: Vec::new(),
                items: Vec::new(),
            }],
        }
    }
}

impl HomePlan {
    /// Nouvel identifiant unique dans tout le plan.
    pub fn next_id(&self) -> u64 {
        let ids = self.floors.iter().flat_map(|f| {
            std::iter::once(f.id)
                .chain(f.rooms.iter().map(|r| r.id))
                .chain(f.items.iter().map(|i| i.id))
        });
        ids.max().unwrap_or(0) + 1
    }

    /// Appareils déjà placés, tous étages confondus.
    pub fn placed_devices(&self) -> HashSet<u64> {
        self.floors
            .iter()
            .flat_map(|f| &f.items)
            .filter_map(|i| match i.kind {
                ItemKind::Device { node_id, .. } => Some(node_id),
                ItemKind::Decor { .. } => None,
            })
            .collect()
    }

    /// Retire un appareil oublié ; `true` si le plan a changé.
    pub fn remove_device(&mut self, node_id: u64) -> bool {
        let mut changed = false;
        for f in &mut self.floors {
            let before = f.items.len();
            f.items
                .retain(|i| !matches!(i.kind, ItemKind::Device { node_id: n, .. } if n == node_id));
            changed |= f.items.len() != before;
        }
        changed
    }
}

/// Arrondit au pas de grille le plus proche.
pub fn snap(v: f32, step: f32) -> f32 {
    (v / step).round() * step
}

pub fn path(demo: bool) -> PathBuf {
    crate::persistence::data_dir().join(if demo { DEMO_FILE_NAME } else { FILE_NAME })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(id: u64, x: f32, y: f32, w: f32, h: f32) -> Room {
        Room {
            id,
            name: format!("R{id}"),
            x,
            y,
            w,
            h,
        }
    }

    #[test]
    fn snapping() {
        assert_eq!(snap(1.2, 0.5), 1.0);
        assert_eq!(snap(1.3, 0.5), 1.5);
        assert_eq!(snap(-0.3, 0.5), -0.5);
        assert_eq!(snap(2.13, 0.25), 2.25);
    }

    #[test]
    fn room_lookup_prefers_topmost() {
        let floor = Floor {
            id: 1,
            name: "RDC".into(),
            rooms: vec![room(2, 0.0, 0.0, 6.0, 4.0), room(3, 4.0, 2.0, 2.0, 2.0)],
            items: vec![],
        };
        assert_eq!(floor.room_at(1.0, 1.0).map(|r| r.id), Some(2));
        assert_eq!(floor.room_at(5.0, 3.0).map(|r| r.id), Some(3));
        assert_eq!(floor.room_at(7.0, 1.0), None);
    }

    #[test]
    fn ids_and_devices() {
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room(5, 0.0, 0.0, 3.0, 3.0));
        plan.floors[0].items.push(Item {
            id: 6,
            kind: ItemKind::Device {
                node_id: 42,
                symbol: None,
            },
            x: 1.0,
            y: 1.0,
        });
        plan.floors[0].items.push(Item {
            id: 7,
            kind: ItemKind::Decor {
                symbol: Symbol::Window,
                label: "Baie".into(),
            },
            x: 2.0,
            y: 0.0,
        });
        assert_eq!(plan.next_id(), 8);
        assert!(plan.placed_devices().contains(&42));
        assert!(plan.remove_device(42));
        assert!(!plan.remove_device(42));
        assert_eq!(plan.floors[0].items.len(), 1);
    }

    #[test]
    fn json_round_trip() {
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room(2, 0.0, 0.0, 4.0, 3.0));
        plan.floors[0].items.push(Item {
            id: 3,
            kind: ItemKind::Device {
                node_id: 9,
                symbol: Some(Symbol::Window),
            },
            x: 1.5,
            y: 0.0,
        });
        let text = serde_json::to_string(&plan).unwrap();
        assert!(text.contains("\"type\":\"Device\""));
        let back: HomePlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
    }
}
