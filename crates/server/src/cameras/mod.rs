//! Caméras RTSP : stockage, vérification et surveillance (ticket #4).
//!
//! # Ce que le serveur sait, ce que les clients savent
//!
//! Le serveur est le **seul** à parler aux caméras. Il garde pour chacune une adresse RTSP
//! (flux principal), une adresse facultative de flux réduit, un identifiant et un mot de passe
//! ([`StoredCamera`]). Les clients ne reçoivent que `protocol::wire::CameraView` : identifiant,
//! nom, état, codec et taille du flux. Ce type n'a pas de champ pour une adresse ou un secret.
//!
//! # Secrets
//!
//! - **Disque** : `cameras.json` dans le dossier de données (0700), créé en **0600** dès sa
//!   création et écrit de façon atomique (fichier temporaire, `fsync`, renommage), comme les clés
//!   de `controller.json`. Le mot de passe y est en clair : le fichier est un secret au même
//!   titre. Il est **inclus dans la sauvegarde `.matterhome`** (`backup.rs`), comme ces clés.
//! - **Adresse sans identifiants** : un utilisateur colle souvent `rtsp://user:mdp@ip:554/…`.
//!   L'adresse enregistrée n'a **jamais** de `user:mdp@` : les identifiants passent dans leurs
//!   champs (voir [`CameraDraft`]). `retina` refuse d'ailleurs une adresse qui en porte.
//! - **Mémoire et journaux** : `Debug` masque le mot de passe ([`StoredCamera`], [`CameraDraft`],
//!   [`Credentials`], [`Target`]), les messages d'erreur ne reprennent jamais ce que
//!   l'utilisateur a saisi, et rien de ce module n'écrit une adresse dans le journal (seulement
//!   le nom donné à la caméra et un identifiant). `masked_url` sert à afficher une adresse
//!   inconnue sans ses identifiants.
//!
//! # Connexions
//!
//! Le serveur ne garde **aucune connexion ouverte** vers une caméra. Il la vérifie par un
//! `DESCRIBE` bref (jamais de `SETUP` ni de `PLAY`), dans un thread à part avec son propre
//! runtime, une caméra à la fois, avec un délai strict ([`PROBE_TIMEOUT`]) : une caméra qui ne
//! répond pas ne ralentit ni ne bloque rien d'autre, et les appareils Matter l'ignorent. À
//! l'ajout ou la modification, la vérification est immédiate ; ensuite toutes les
//! [`MONITOR_EVERY`].
//!
//! # Organisation du module
//!
//! - `store` : données, saisie, validation, fichier, carnet (caméras + état) ;
//! - `probe` : le `DESCRIBE` et le bouton « Tester » (annulable) ;
//! - `monitor` : la surveillance ;
//! - `viewers` : le compte des spectateurs ; `live` : la lecture (ticket n° 5, faite : un relais H.264 partagé par caméra et flux) ;
//! - le côté hub (ajout, modification, suppression…) est dans `hub/cameras.rs`.
//!
//! # Pour #5 (lecture), #6 (plan) et #7 (liste)
//!
//! - **Lecture** : [`Hub::camera_target`](crate::hub::Hub::camera_target) rend l'adresse (sans
//!   identifiants) et les identifiants du flux voulu ([`StreamKind`]) : c'est le point d'entrée
//!   interne, il ne sort jamais du serveur. Par défaut on lit le flux réduit
//!   ([`StoredCamera::preferred_kind`]). Que le serveur relaie le flux ou que le client se connecte
//!   en direct n'est **pas décidé ici** (voir `docs/protocole.md`, « Caméras »).
//! - **Plan** : le placement (pièce, position, orientation) n'est pas dans la caméra mais dans le
//!   plan (`home::Item`, `ItemKind::Camera`, `rotation`). `Hub::delete_camera` retire déjà la
//!   caméra supprimée du plan, dans la même transaction (voir `hub/cameras.rs`).
//! - **Liste** : `GET /api/cameras`, l'état complet et `cameras_changed` donnent tout ce qu'il faut.

pub mod live;
mod monitor;
mod probe;
pub mod snapshot;
mod store;
#[cfg(test)]
mod tests;
mod viewers;

pub use monitor::*;
pub use probe::*;
pub use protocol::wire::StreamKind;
pub use store::*;
pub use viewers::{ViewerLease, ViewerRegistry};
