# Caméras : contrat entre les tickets n° 5, 6 et 7

[← Retour au README](../README.md) · [Protocole](protocole.md)

> **État : la phase parallèle est terminée.** Les tickets n° 5, 6 et 7 sont fusionnés (branche
> `cameras-v1`), la route du flux est faite, et `CHANGELOG.md` et `docs/protocole.md` sont à jour.
> Cette page garde la trace de la répartition.

Le ticket n° 4 a posé le socle (stockage, protocole, fenêtre du serveur). Les tickets n° 5
(lecture), n° 6 (plan) et n° 7 (liste) se sont construits **en parallèle** dessus. Cette page dit qui
possède quoi, pour que leurs fusions ne se marchent pas dessus, et ce qui est déjà en place.

## Règles communes

- **Le protocole reste en version 2** pendant toute la série. Personne ne la relève. Pour qu'un
  ajout reste lisible par un client plus ancien : un champ nouveau est `#[serde(default)]` ; les
  énumérations d'événement, d'état et de motif de caméra ont une variante de repli
  (`WireEvent::Unknown`, `CameraStatus::Unknown`, `CameraProblem::Failed`, lues par `#[serde(other)]`)
  et le client les ignore en comptant quand même leur `seq`. `ItemKind` n'en a **pas** : un plan
  illisible ne se « tolère » pas.
- **Jamais d'adresse RTSP, de mot de passe ou d'identifiant de caméra** dans un type du protocole,
  un événement, un message d'erreur ou un journal. `Target`, `StoredCamera`, `CameraDraft` ont un
  `Debug` masqué : ne pas le contourner. Journal : le nom donné à la caméra et son identifiant en
  hexadécimal, rien d'autre.
- **Une caméra regardée a un spectateur** (`Hub::acquire_camera_viewer`). Sans spectateur, le
  serveur n'ouvre aucune connexion vers elle ; avec, il n'en ouvre qu'une par flux.
- **Personne ne touche `CHANGELOG.md` ni `docs/protocole.md`** pendant la phase parallèle : chaque
  agent note ce qu'il aurait écrit dans son compte rendu, la fusion les rédige.
- **Seul le ticket n° 5 modifie `Cargo.toml` et `Cargo.lock`** (décodeur vidéo). Si #6 ou #7 a besoin
  d'une dépendance, il la demande à #5 ou la dit dans son compte rendu.
- Les textes d'interface sont en français ; les identifiants de caméra sont du texte décimal dans
  le JSON (`text_id`), des `u64` en Rust.

## Qui possède quels fichiers

| Ticket | Fichiers à soi | Peut aussi toucher (une ligne ou un bloc, sans réécrire) |
|---|---|---|
| **#5** lecture | `crates/server/src/cameras/live.rs`, `crates/server/src/net/camera_stream.rs`, `crates/client/src/net/stream.rs`, `crates/client/src/video/*`, `crates/client/src/ui/player.rs`, `Cargo.toml` et `Cargo.lock` | une ligne de route dans `net/routes.rs` ; `lib.rs` du client pour `pub mod video;` |
| **#6** plan | `crates/protocol/src/home.rs` (au-delà du socle), `crates/server/src/validate.rs`, l'emplacement du plan dans `crates/server/src/hub/cameras.rs`, les fichiers de l'éditeur de plan du client (`ui/plan.rs`) | `crates/protocol/src/wire/mod.rs` pour un test |
| **#7** liste | `crates/client/src/ui/cameras.rs` | rien d'autre : tout ce qu'il lui faut est dans `Model` |
| **#4** (ce socle) | tout le reste de `crates/server/src/cameras/` et `hub/cameras.rs`, `crates/protocol/src/wire/camera.rs`, `crates/server-app/src/cameras*.rs` | — |

Un besoin hors de ses fichiers (un champ de plus dans `CameraView`, une méthode du hub) : le dire
dans le compte rendu plutôt que de le faire en parallèle d'un autre.

## Ce qui est en place

### Protocole (`crates/protocol`)

- `wire::CameraView { id, name, status, has_sub_stream, stream }`, `CameraStatus`, `CameraProblem`,
  `StreamInfo`, `CamerasResponse`, `WireEvent::CamerasChanged { cameras }` (liste complète),
  `Snapshot::cameras`.
- `wire::StreamKind { Main, Sub }` (`label()`, `key()`, `from_key()`). Le flux réduit est celui qu'on
  lit par défaut quand il existe.
- `wire::CAMERA_STREAM_PATH` = `/api/cameras/{id}/stream` et `camera_stream_path(id, kind)` =
  `/api/cameras/<id>/stream?kind=main|sub` : la route **réservée** du flux (**faite par #5** ; 404 sans `detail` sur un
  serveur ancien), sous le même jeton que le reste.
- `wire::is_camera_id(id)` : **un contrôle de plage, rien de plus.** Des appareils pontés tirés
  avant les caméras ont leurs identifiants dans la même plage (environ la moitié d'entre eux). La
  preuve qu'un identifiant est une caméra, c'est sa présence dans `Model::cameras` côté client ou
  dans le carnet du serveur (`Hub::camera_ids`). Le serveur ne tire jamais pour une caméra
  l'identifiant d'un appareil connu, et l'import d'une sauvegarde refuse un recoupement.
- `home::ItemKind::Camera { camera_id }`, `Item::rotation` (degrés, `0 <= r < 360`, sens horaire, 0
  vers le haut du plan, `#[serde(default)]`), `HomePlan::camera_room(id)`, `placed_cameras()`,
  `remove_camera(id)`. `camera_id` est du texte dans le JSON du plan (comme `node_id`).
- `validate::plan` contrôle la rotation et la plage de `camera_id`. **Il ne contrôle pas
  l'existence** de la caméra (un plan envoyé juste avant une suppression ne doit pas être refusé) :
  c'est à #6 de décider où et comment, et `delete_camera` retire déjà l'objet du plan.

### Serveur (`crates/server`)

- `cameras/{store,probe,monitor,viewers,live}.rs` : données et carnet ; `DESCRIBE` et test annulable
  (`CameraTest`) ; surveillance ; compte des spectateurs ; `live.rs` : le relais du direct (#5, fait).
- `hub/cameras.rs` : `add_camera`, `update_camera`, `delete_camera` (**une seule transaction** :
  caméras, puis plan, sous un seul passage par `write_lock`, jamais de verrou réentrant),
  `camera_target(id, kind)`, `camera_viewers(id)`, `acquire_camera_viewer(id) -> ViewerLease`,
  `camera_probe_lock(id)`, `start_camera_test`.
- La surveillance saute une caméra regardée ou en cours de test ; « Tester » est refusé pour une
  caméra regardée et prend le verrou de la caméra ; « Annuler » arrête le fil tout de suite.
- Premier échec d'une caméra joignable : revérifiée aussitôt ; deux d'affilée : injoignable.

### Client (`crates/client`)

- `Model::cameras`, `Model::camera(id)`, `Change::CamerasChanged`.
- Onglet **Caméras** (`View::Cameras`) branché dans la barre d'onglets et le `match` central ; il
  appelle `ui/cameras.rs` (texte d'attente).
- `ui::player::player(ui, model, camera_id, StreamKind)` : le contrat d'affichage d'un flux, texte
  d'attente pour l'instant. #6 l'appelle au clic sur une caméra du plan, #7 au clic dans la
  mosaïque. Elle ne change pas de signature.
- L'éditeur de plan (`ui/plan.rs`) dessine et édite une caméra par un simple texte (« Caméra ») :
  #6 remplace ces deux endroits (repères `#6`).
