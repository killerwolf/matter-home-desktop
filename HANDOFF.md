# Passation de session (12/11/2026)

Fichier temporaire : à supprimer une fois les branches ci-dessous terminées et fusionnées.

## État du dépôt

- `main` = `2c34190` : la branche `v1-client-serveur` (92 commits, V1 client/serveur, PWA, accès web Tailscale) a été fusionnée en fast-forward et poussée. Le dernier tag est `v1.0.0-rc.8` ; **pas de tag `v1.0.0`**.
- Rien n'est fermé sur GitHub. Aucune PR ouverte.
- La branche `v1-client-serveur` existe encore sur le serveur (identique à `main`).

## Branches de travail à reprendre (poussées, non compilées, non testées)

Chaque branche porte un dernier commit « WIP … non compilé ni testé » : une session d'agents a été coupée en cours de route (disque plein). **Premier geste : `cargo build` puis `cargo test`, corriger, puis finir le ticket.**

| Ticket | Branche | Contenu déjà là |
|---|---|---|
| #16 refonte app serveur | `ticket-16-serveur-ui` | Thème partagé déplacé dans le crate `matter-home-theme` ; WIP sur `crates/server-app` (`menu.rs`, `status.rs`, `ui.rs`). |
| #14 accès à distance | `ticket-14-acces-distant` | WIP côté client : adresses multiples (`net/addr.rs`, `store.rs`, `backend.rs`, `ws.rs`, `stream.rs`), flux sub à distance (`ui/cameras.rs`), `ui/connect.rs`, `tests/connection.rs`. |
| #12 image fixe caméra | `ticket-12-snapshot` | Crate partagé `matter-home-video` (décodage, pixels RGB) ; WIP sur `protocol/wire/camera.rs`, `server/cameras/live.rs`, `server/hub/cameras.rs`. |

#14 et #16 touchent tous deux au client (`connect.rs` : l'aide doit dire « Connecter un client ») : prévoir un petit conflit de fusion.

## Ce qui reste, ticket par ticket (audit du code de `main`)

- **#2 V1** : code fait. Reste l'étape 2 (test de plusieurs jours de la v0.2.1 sur l'ancien Mac, sans trace dans le dépôt), puis tag `v1.0.0` et fermeture du ticket. Le CHANGELOG est encore en `[Unreleased]`.
- **#13 refonte client** : presque fait. Reste le détail d'appareil en panneau à droite (aujourd'hui une fenêtre modale) ; les écrans serveur 8 à 10 sont dans #16.
- **#16** : sous-menu « Connecter un client », état en une ligne, caméras en une ligne avec point de couleur, « Supprimer » dans le formulaire, mot de passe « enregistré / Changer / Effacer », aide Aqara dépliable, libellés « Adresse de la caméra » / « Adresse pour la mosaïque (facultatif) », pied de fenêtre en deux phrases.
- **#14** : adresses multiples dans le client, flux sub par défaut hors réseau local, section README « Accès à distance » (le README dit encore « pas d'accès à distance »). Point ouvert : le certificat auto-signé du serveur n'inclut pas le nom MagicDNS ; derrière `tailscale serve` le client verrait le certificat Tailscale, différent de l'empreinte épinglée. À tester pour de vrai avec Tailscale.
- **#15 PWA** : appareils, températures, caméras, manifest et service worker faits. Reste : plan en lecture, édition du plan, notifications web push, test avec une vraie caméra.
- **#12** : route `GET /api/cameras/{id}/snapshot`, cache partagé, respect du registre des spectateurs, vignette rafraîchie toutes les 10 à 30 s, sans FFmpeg.
- **#10** : seules les icônes porte/fenêtre sur un point libre existent ; murs et ouvertures accrochées aux murs absents.
- **#8** (modes Présent/Absent) et **#9** (pièces en L) : non commencés.

## Ordre proposé

1. Valider l'étape 2 de #2, puis taguer `v1.0.0` et fermer #2.
2. Terminer #16, #14, #12 (chacun sa branche, fusion dans `main` un par un, un commit par version qui marche).
3. Revue du code des trois branches par un modèle Opus 5.5 (consigne de l'utilisateur : Opus uniquement pour la revue ; Sonnet pour le code, Haiku pour la recherche d'information).
4. Ensuite : #13 (panneau à droite), #15 (plan, push), #10, #8, #9.

## Notes

- Disque limité sur le Mac précédent : les dossiers `target/` ont été supprimés ; la première compilation sera longue. Éviter de compiler plusieurs worktrees en parallèle sur un disque presque plein.
- Les worktrees (`MatterDemoApp-wt-12`, `-wt-14`, `-wt-16`) n'existent que sur l'ancien Mac : sur le nouveau, utiliser `git fetch` puis `git worktree add ../<nom> <branche>`.
