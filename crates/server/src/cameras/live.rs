//! Lecture en direct d'une caméra, côté serveur : **à écrire par le ticket n° 5**.
//!
//! Ce fichier est vide exprès : #5 en est le seul propriétaire (voir `docs/cameras-contrat.md`).
//! Ce que #4 met à sa disposition :
//!
//! - `Hub::camera_target(id, kind)` : adresse (sans identifiants) et identifiants du flux
//!   `StreamKind::Main` ou `StreamKind::Sub`, qui ne sortent jamais du serveur ;
//! - `Hub::acquire_camera_viewer(id)` : un [`ViewerLease`](super::ViewerLease) par spectateur. La
//!   surveillance saute une caméra regardée et « Tester » lui est refusé. Sans spectateur, le
//!   serveur n'a **aucune** connexion ouverte vers la caméra : à #5 de la couper après le départ
//!   du dernier ;
//! - `Hub::camera_probe_lock(id)` : le verrou qui sérialise une connexion à la caméra (surveillance
//!   et test) : le prendre pour toute connexion de lecture n'est pas utile tant que le spectateur
//!   est compté ;
//! - `protocol::wire::CAMERA_STREAM_PATH` / `camera_stream_path` : la route réservée
//!   `GET /api/cameras/{id}/stream?kind=main|sub` (404 aujourd'hui). La brancher dans
//!   `net/routes.rs` (une ligne) avec le code dans `net/camera_stream.rs`.
//!
//! Règles : jamais le mot de passe ni l'adresse dans un message, un événement ou un journal
//! (`Debug` de `Target` les masque) ; une caméra modifiée ou supprimée pendant la lecture coupe
//! ses spectateurs.
