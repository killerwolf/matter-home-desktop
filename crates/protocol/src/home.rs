//! Plan de la maison : étages, pièces rectangulaires et objets placés.
//! Coordonnées en mètres, origine en haut à gauche, y vers le bas.
//! Sauvegardé par le serveur dans `maison.json` (ou `maison-demo.json` en mode démo).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    /// Volet roulant ou store.
    Shutter,
}

impl Symbol {
    pub const ALL: [Symbol; 7] = [
        Symbol::Door,
        Symbol::Window,
        Symbol::Shutter,
        Symbol::Light,
        Symbol::Plug,
        Symbol::Thermometer,
        Symbol::Button,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Symbol::Door => "Porte",
            Symbol::Window => "Fenêtre",
            Symbol::Shutter => "Volet",
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
        /// Porte ou fenêtre, pour un capteur d'ouverture (Matter ne les distingue pas). Absent :
        /// porte, ou fenêtre si le pictogramme choisi en est une. Anciens plans : absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opening: Option<OpeningKind>,
    },
    /// Objet non connecté, pour la lisibilité du plan.
    Decor { symbol: Symbol, label: String },
    /// Caméra du serveur (`CameraView::id`), posée et orientée avec `Item::rotation`. Ticket n° 6.
    /// Un client plus ancien ne connaît pas cette variante : le protocole est en version 2 pour
    /// cette raison, et il n'y a pas de variante de repli ici (un plan reçu serait illisible).
    /// `fov`, `range` et `shape` décrivent le champ filmé ; absents d'un plan écrit avec la
    /// version 1 de l'objet (sans champ), ils prennent leurs valeurs par défaut.
    Camera {
        camera_id: u64,
        /// Ouverture du champ, en degrés (`MIN_FOV..=MAX_FOV`).
        #[serde(default = "default_fov")]
        fov: f32,
        /// Portée dessinée, en mètres (`MIN_CAMERA_RANGE..=MAX_CAMERA_RANGE`).
        #[serde(default = "default_camera_range")]
        range: f32,
        #[serde(default)]
        shape: FovShape,
    },
}

/// Ce que ferme un capteur d'ouverture posé sur le plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningKind {
    #[default]
    Door,
    Window,
    /// Un type que ce programme ne connaît pas (plan écrit par une version plus récente) : lu et
    /// dessiné comme une porte. Jamais proposé au choix.
    #[serde(other)]
    Other,
}

impl OpeningKind {
    pub const ALL: [OpeningKind; 2] = [OpeningKind::Door, OpeningKind::Window];

    pub fn label(self) -> &'static str {
        match self {
            OpeningKind::Door => "Porte",
            OpeningKind::Window => "Fenêtre",
            OpeningKind::Other => "Porte",
        }
    }
}

/// Ouverture minimale et maximale du champ d'une caméra (degrés).
pub const MIN_FOV: f32 = 10.0;
pub const MAX_FOV: f32 = 180.0;
/// Portée minimale et maximale dessinée pour une caméra (m).
pub const MIN_CAMERA_RANGE: f32 = 0.5;
pub const MAX_CAMERA_RANGE: f32 = 50.0;
pub const DEFAULT_FOV: f32 = 90.0;
pub const DEFAULT_CAMERA_RANGE: f32 = 3.0;

fn default_fov() -> f32 {
    DEFAULT_FOV
}

fn default_camera_range() -> f32 {
    DEFAULT_CAMERA_RANGE
}

/// Forme sous laquelle le champ d'une caméra est dessiné sur le plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FovShape {
    /// Secteur de disque : l'ouverture est l'angle du secteur.
    #[default]
    Cone,
    /// Rectangle de la profondeur de la portée, aussi large que la corde du secteur.
    Rectangle,
}

impl FovShape {
    pub const ALL: [FovShape; 2] = [FovShape::Cone, FovShape::Rectangle];

    pub fn label(self) -> &'static str {
        match self {
            FovShape::Cone => "Cône",
            FovShape::Rectangle => "Rectangle",
        }
    }
}

impl ItemKind {
    /// Caméra posée avec le champ par défaut (cône de 90 degrés, 3 m).
    pub fn camera(camera_id: u64) -> Self {
        ItemKind::Camera {
            camera_id,
            fov: DEFAULT_FOV,
            range: DEFAULT_CAMERA_RANGE,
            shape: FovShape::Cone,
        }
    }
}

/// Direction (vecteur unitaire, y vers le bas) d'une orientation en degrés : 0 = vers le haut du
/// plan, sens des aiguilles d'une montre.
pub fn rotation_vector(degrees: f32) -> (f32, f32) {
    let r = degrees.to_radians();
    (r.sin(), -r.cos())
}

/// Orientation (`0 <= r < 360`) du vecteur `(dx, dy)` (y vers le bas) ; `None` s'il est nul.
pub fn rotation_of(dx: f32, dy: f32) -> Option<f32> {
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    Some(normalize_rotation(dx.atan2(-dy).to_degrees()))
}

/// Ramène un angle quelconque dans `[0, 360)`.
pub fn normalize_rotation(degrees: f32) -> f32 {
    let r = degrees.rem_euclid(360.0);
    // `rem_euclid` d'un tout petit négatif peut rendre exactement 360.0 en `f32`.
    if r >= 360.0 {
        0.0
    } else {
        r
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub kind: ItemKind,
    pub x: f32,
    pub y: f32,
    /// Orientation en degrés, `0.0 <= rotation < 360.0`, dans le sens des aiguilles d'une montre,
    /// 0 = vers le haut du plan (y négatif). Utile aux caméras (direction filmée). Absent des plans
    /// enregistrés avant les caméras : 0.
    #[serde(default)]
    pub rotation: f32,
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
    /// Reprend de `old` le réglage `opening` des appareils que le nouveau plan (envoyé par un
    /// client plus ancien, qui ne connaît pas ce champ et l'efface) ne porte pas : même objet
    /// (identifiant) et même appareil.
    pub fn keep_openings_from(&mut self, old: &HomePlan) {
        let stored: std::collections::HashMap<u64, (u64, OpeningKind)> = old
            .floors
            .iter()
            .flat_map(|f| &f.items)
            .filter_map(|i| match i.kind {
                ItemKind::Device {
                    node_id,
                    opening: Some(o),
                    ..
                } => Some((i.id, (node_id, o))),
                _ => None,
            })
            .collect();
        for item in self.floors.iter_mut().flat_map(|f| &mut f.items) {
            if let ItemKind::Device {
                node_id,
                opening: opening @ None,
                ..
            } = &mut item.kind
            {
                if let Some((stored_node, o)) = stored.get(&item.id) {
                    if stored_node == node_id {
                        *opening = Some(*o);
                    }
                }
            }
        }
    }

    /// Nom de la pièce où l'appareil est posé sur le plan, s'il y est et si le point tombe dans
    /// une pièce.
    pub fn room_name_of(&self, node_id: u64) -> Option<&str> {
        self.room_of(node_id).map(|(_, r)| r.name.as_str())
    }

    /// Étage et pièce où l'appareil est posé : la première pose (étages dans l'ordre, puis
    /// poses dans l'ordre) dont le point tombe dans une pièce. Une pose hors de toute pièce est
    /// ignorée, une seconde pose ne change rien. C'est la seule règle de « pièce d'un
    /// appareil » : l'onglet Appareils et les Températures s'en servent.
    pub fn room_of(&self, node_id: u64) -> Option<(&Floor, &Room)> {
        self.floors.iter().find_map(|f| {
            f.items
                .iter()
                .filter(|i| matches!(i.kind, ItemKind::Device { node_id: n, .. } if n == node_id))
                .find_map(|i| f.room_at(i.x, i.y))
                .map(|r| (f, r))
        })
    }

    /// Un plan a toujours au moins un étage. Remplace un plan sans étage (`floors: []`) par le
    /// plan par défaut ; `true` si c'était le cas. Sert à lire une ancienne sauvegarde ou un
    /// ancien fichier, jamais à accepter un plan envoyé par un client (refusé).
    pub fn ensure_floor(&mut self) -> bool {
        if !self.floors.is_empty() {
            return false;
        }
        *self = Self::default();
        true
    }

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
                ItemKind::Decor { .. } | ItemKind::Camera { .. } => None,
            })
            .collect()
    }

    /// Nom de la pièce où la caméra est posée sur le plan (même règle que [`Self::room_name_of`]).
    pub fn camera_room(&self, camera_id: u64) -> Option<&Room> {
        self.floors.iter().find_map(|f| {
            f.items
                .iter()
                .find(|i| matches!(i.kind, ItemKind::Camera { camera_id: c, .. } if c == camera_id))
                .and_then(|i| f.room_at(i.x, i.y))
        })
    }

    /// Caméras déjà placées, tous étages confondus.
    pub fn placed_cameras(&self) -> HashSet<u64> {
        self.floors
            .iter()
            .flat_map(|f| &f.items)
            .filter_map(|i| match i.kind {
                ItemKind::Camera { camera_id, .. } => Some(camera_id),
                _ => None,
            })
            .collect()
    }

    /// Retire une caméra supprimée du serveur ; `true` si le plan a changé.
    pub fn remove_camera(&mut self, camera_id: u64) -> bool {
        let mut changed = false;
        for f in &mut self.floors {
            let before = f.items.len();
            f.items.retain(
                |i| !matches!(i.kind, ItemKind::Camera { camera_id: c, .. } if c == camera_id),
            );
            changed |= f.items.len() != before;
        }
        changed
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
                opening: None,
            },
            x: 1.0,
            y: 1.0,
            rotation: 0.0,
        });
        plan.floors[0].items.push(Item {
            id: 7,
            kind: ItemKind::Decor {
                symbol: Symbol::Window,
                label: "Baie".into(),
            },
            x: 2.0,
            y: 0.0,
            rotation: 0.0,
        });
        assert_eq!(plan.next_id(), 8);
        assert!(plan.placed_devices().contains(&42));
        assert!(plan.remove_device(42));
        assert!(!plan.remove_device(42));
        assert_eq!(plan.floors[0].items.len(), 1);
    }

    #[test]
    fn room_name_of_device() {
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room(5, 0.0, 0.0, 3.0, 3.0));
        let put = |id, node_id, x| Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
                opening: None,
            },
            x,
            y: 1.0,
            rotation: 0.0,
        };
        plan.floors[0].items.push(put(6, 42, 1.0));
        plan.floors[0].items.push(put(7, 43, 9.0));
        assert_eq!(plan.room_name_of(42), Some("R5"));
        // Posé hors de toute pièce, ou absent du plan : pas de pièce.
        assert_eq!(plan.room_name_of(43), None);
        assert_eq!(plan.room_name_of(44), None);
    }

    #[test]
    fn room_of_is_the_first_pose_that_lands_in_a_room() {
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room(5, 0.0, 0.0, 3.0, 3.0));
        plan.floors.push(Floor {
            id: 2,
            name: "Étage".into(),
            rooms: vec![room(8, 0.0, 0.0, 3.0, 3.0)],
            items: vec![],
        });
        let put = |id, node_id, x| Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
                opening: None,
            },
            x,
            y: 1.0,
            rotation: 0.0,
        };
        // Double pose : hors pièce d'abord, puis dans la pièce 5 : la seconde compte.
        plan.floors[0].items.push(put(1, 42, 9.0));
        plan.floors[0].items.push(put(2, 42, 1.0));
        // Deux poses dans des pièces : la première gagne.
        plan.floors[0].items.push(put(3, 43, 2.0));
        plan.floors[1].items.push(put(4, 43, 1.0));
        // Posé hors de toute pièce.
        plan.floors[0].items.push(put(5, 44, 9.0));
        // Seulement à l'étage.
        plan.floors[1].items.push(put(6, 45, 1.0));
        let of = |n| plan.room_of(n).map(|(f, r)| (f.id, r.id));
        assert_eq!(of(42), Some((1, 5)));
        assert_eq!(of(43), Some((1, 5)));
        assert_eq!(of(44), None);
        assert_eq!(of(46), None);
        assert_eq!(of(45), Some((2, 8)));
        assert_eq!(plan.room_name_of(45), Some("R8"));
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
                opening: None,
            },
            x: 1.5,
            y: 0.0,
            rotation: 0.0,
        });
        let text = serde_json::to_string(&plan).unwrap();
        assert!(text.contains("\"type\":\"Device\""));
        let back: HomePlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
    }

    #[test]
    fn camera_items_round_trip_and_old_plans_stay_valid() {
        let cam = 0xFFFF_FFF8_0000_0001u64;
        let mut plan = HomePlan::default();
        plan.floors[0].rooms.push(room(10, 0.0, 0.0, 4.0, 3.0));
        plan.floors[0].items.push(Item {
            id: 20,
            kind: ItemKind::camera(cam),
            x: 1.0,
            y: 1.0,
            rotation: 135.0,
        });
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<HomePlan>(&json).unwrap(), plan);
        assert_eq!(plan.camera_room(cam).unwrap().id, 10);
        assert_eq!(plan.camera_room(cam + 1), None);
        assert_eq!(plan.placed_cameras(), HashSet::from([cam]));
        assert!(plan.placed_devices().is_empty());
        // Un plan d'avant les caméras (sans `rotation`) se lit, rotation 0.
        let old = r#"{"version":1,"floors":[{"id":1,"name":"RDC","items":[
            {"id":2,"kind":{"type":"Decor","symbol":"Door","label":""},"x":1.0,"y":2.0}]}]}"#;
        let old: HomePlan = serde_json::from_str(old).unwrap();
        assert_eq!(old.floors[0].items[0].rotation, 0.0);
        assert!(plan.remove_camera(cam));
        assert!(!plan.remove_camera(cam));
    }

    #[test]
    fn camera_field_of_view_round_trips_and_defaults() {
        let cam = 0xFFFF_FFF8_0000_0002u64;
        let mut plan = HomePlan::default();
        plan.floors[0].items.push(Item {
            id: 4,
            kind: ItemKind::Camera {
                camera_id: cam,
                fov: 70.0,
                range: 5.5,
                shape: FovShape::Rectangle,
            },
            x: 0.5,
            y: 0.5,
            rotation: 270.0,
        });
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("\"Rectangle\""));
        assert_eq!(serde_json::from_str::<HomePlan>(&json).unwrap(), plan);
        // Sur le fil, l'identifiant de caméra est du texte, le reste du champ reste numérique.
        let wired = serde_json::to_string(&crate::wire::TextIds(plan.clone())).unwrap();
        assert!(
            wired.contains("\"camera_id\":\"18446744039349813250\""),
            "{wired}"
        );
        assert!(wired.contains("\"fov\":70.0"), "{wired}");
        let back: crate::wire::TextIds<HomePlan> = serde_json::from_str(&wired).unwrap();
        assert_eq!(back.0, plan);
        // Une caméra posée sans ouverture (premier format du ticket n° 4) reçoit les valeurs par défaut.
        let bare = r#"{"version":1,"floors":[{"id":1,"name":"RDC","items":[
            {"id":2,"kind":{"type":"Camera","camera_id":18446744004990074882},"x":1.0,"y":2.0,"rotation":10.0}]}]}"#;
        let plan: HomePlan = serde_json::from_str(bare).unwrap();
        match &plan.floors[0].items[0].kind {
            ItemKind::Camera {
                fov, range, shape, ..
            } => {
                assert_eq!(
                    (*fov, *range, *shape),
                    (DEFAULT_FOV, DEFAULT_CAMERA_RANGE, FovShape::Cone)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rotation_math() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        // 0 = haut du plan (y négatif), sens horaire.
        assert_eq!(rotation_of(0.0, -1.0), Some(0.0));
        assert!(close(rotation_of(1.0, 0.0).unwrap(), 90.0));
        assert!(close(rotation_of(0.0, 1.0).unwrap(), 180.0));
        assert!(close(rotation_of(-1.0, 0.0).unwrap(), 270.0));
        assert_eq!(rotation_of(0.0, 0.0), None);
        let (x, y) = rotation_vector(90.0);
        assert!(close(x, 1.0) && close(y, 0.0));
        let (x, y) = rotation_vector(180.0);
        assert!(close(x, 0.0) && close(y, 1.0));
        for d in [0.0, 33.0, 179.0, 271.0, 359.0] {
            let (x, y) = rotation_vector(d);
            assert!(close(rotation_of(x, y).unwrap(), d), "{d}");
        }
        assert!(close(normalize_rotation(-90.0), 270.0));
        assert!(close(normalize_rotation(725.0), 5.0));
        assert!(normalize_rotation(-1e-9) < 360.0);
        assert!((0.0..360.0).contains(&normalize_rotation(-1e-9)));
    }

    #[test]
    fn the_opening_kind_is_optional_and_round_trips() {
        // Un plan écrit avant ce réglage se charge : rien, donc le défaut (porte).
        let old = r#"{"id":1,"kind":{"type":"Device","node_id":42,"symbol":null},"x":1.0,"y":2.0}"#;
        let item: Item = serde_json::from_str(old).unwrap();
        assert_eq!(
            item.kind,
            ItemKind::Device {
                node_id: 42,
                symbol: None,
                opening: None
            }
        );
        assert_eq!(OpeningKind::default(), OpeningKind::Door);
        // Sans réglage, rien n'est écrit (le plan reste lisible par un client plus ancien).
        let json = serde_json::to_string(&item).unwrap();
        assert!(!json.contains("opening"), "{json}");
        // Avec un réglage : aller-retour.
        let window = Item {
            kind: ItemKind::Device {
                node_id: 42,
                symbol: None,
                opening: Some(OpeningKind::Window),
            },
            ..item
        };
        let json = serde_json::to_string(&window).unwrap();
        assert!(json.contains(r#""opening":"Window""#), "{json}");
        assert_eq!(serde_json::from_str::<Item>(&json).unwrap(), window);
    }

    #[test]
    fn an_unknown_opening_kind_reads_as_a_door_and_an_old_client_save_keeps_the_setting() {
        let item: Item = serde_json::from_str(
            r#"{"id":1,"kind":{"type":"Device","node_id":42,"symbol":null,"opening":"Skylight"},"x":1.0,"y":2.0}"#,
        )
        .unwrap();
        assert!(matches!(
            item.kind,
            ItemKind::Device {
                opening: Some(OpeningKind::Other),
                ..
            }
        ));
        // Un plan enregistré par un client plus ancien (sans `opening`) garde le réglage connu.
        let device = |id, node_id, opening| Item {
            id,
            kind: ItemKind::Device {
                node_id,
                symbol: None,
                opening,
            },
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
        };
        let plan = |items| {
            let mut p = HomePlan::default();
            p.floors[0].items = items;
            p
        };
        let old = plan(vec![
            device(10, 1, Some(OpeningKind::Window)),
            device(11, 2, Some(OpeningKind::Window)),
        ]);
        let mut incoming = plan(vec![
            device(10, 1, None),
            device(11, 3, None), // autre appareil, même objet : rien à reprendre
            device(12, 4, None),
        ]);
        incoming.keep_openings_from(&old);
        let openings: Vec<_> = incoming.floors[0]
            .items
            .iter()
            .map(|i| match i.kind {
                ItemKind::Device { opening, .. } => opening,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(openings, vec![Some(OpeningKind::Window), None, None]);
        // Un réglage explicite du nouveau plan n'est jamais écrasé.
        let mut explicit = plan(vec![device(10, 1, Some(OpeningKind::Door))]);
        explicit.keep_openings_from(&old);
        assert!(matches!(
            explicit.floors[0].items[0].kind,
            ItemKind::Device {
                opening: Some(OpeningKind::Door),
                ..
            }
        ));
    }
}
