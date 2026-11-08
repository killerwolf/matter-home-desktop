//! Types partagés entre le client (interface) et le serveur (Matter, règles, historique).
//!
//! Ce crate ne dépend ni d'egui ni de matc : il ne contient que des types de données
//! (avec serde) et des fonctions pures. `events` et `automation` sont les types internes ;
//! `wire` définit ce qui circule réellement sur le réseau (voir `docs/protocole.md`).

pub mod automation;
pub mod events;
pub mod format;
pub mod home;
pub mod wire;
