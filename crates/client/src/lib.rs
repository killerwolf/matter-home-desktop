//! Cœur du client Matter Home, sans interface : la connexion au serveur (`net`), l'état tel
//! que le protocole le décrit (`model`) et les séries lues à la demande (`series`).
//!
//! L'interface (egui) est dans le binaire `matter-home-desktop`. Cette bibliothèque existe
//! pour que la couche réseau soit testée contre le vrai serveur (`tests/`), sans fenêtre.

pub mod logging;
pub mod model;
pub mod net;
pub mod series;
