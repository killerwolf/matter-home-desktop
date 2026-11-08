//! Types partagés entre le client (interface) et le serveur (Matter, règles, historique).
//!
//! Ce crate ne dépend ni d'egui ni de matc : il ne contient que des types de données
//! (avec serde) et des fonctions pures. Le format de transmission sur le réseau n'est
//! pas défini ici à ce stade.

pub mod automation;
pub mod events;
pub mod format;
pub mod home;
