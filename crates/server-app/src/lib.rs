//! Matter Home Server : le serveur Matter Home sous forme d'application de la barre de menus.
//!
//! Tout ce qui se décide sans interface (texte d'état, décision de démarrage, LaunchAgent,
//! veille, modèle du menu) est ici et se teste ; `main.rs` ne fait que brancher l'interface
//! (icône de barre de menus et fenêtre egui) sur le [`controller`].

pub mod agent;
pub mod cameras;
pub mod controller;
pub mod icon;
pub mod menu;
pub mod signals;
pub mod sleep;
pub mod startup;
pub mod status;
pub mod system;
