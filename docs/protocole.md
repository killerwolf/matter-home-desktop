# Protocole client-serveur

[← Retour au README](../README.md) · [Architecture](architecture.md)

Le client (interface) et le serveur (`matter-home-server`) parlent en **HTTPS** (requêtes) et en
**WebSocket** (temps réel), sur le même port (8443 par défaut). Les types sont dans
`crates/protocol/src/wire/` : ce crate n'a ni tokio, ni axum, ni TLS, pour que le client le
réutilise tel quel. Les types internes `Command` et `Event` ne sont jamais envoyés tels quels.

## Règles

- **Version** : `protocol_version` (1 aujourd'hui). `GET /api/version` la donne sans jeton ;
  la connexion et le WebSocket (`?protocol=1`) l'exigent identique, sinon l'erreur
  `version_incompatible` (HTTP 426). Ajouter un champ optionnel ne change pas la version.
- **Identifiants en texte** : appareils, règles, étages, pièces, objets sont des chaînes
  décimales (`"18446744073709551615"`), car un `u64` dépasse 2^53 et JavaScript l'arrondirait.
  Un identifiant numérique, ou avec zéro initial, est refusé. Les compteurs (`seq`) restent
  des nombres. Le plan et les règles gardent le format de `maison.json` et `regles.json`,
  seuls les identifiants (`id`, `node_id`, `rule_id`) passent en texte.
- **Erreurs** : `{"error": {"code": "...", "detail": "...", "retry_after_s": 30}}`. Le client
  se fie au `code` (stable, voir `ErrorCode`) ; `detail` est un texte humain facultatif, sans
  chemin local.
- **Jeton** : `Authorization: Bearer <jeton>` sur toute route et sur le WebSocket, jamais
  dans l'URL. Seules `GET /api/version` et `POST /api/login` s'en passent.
- **Concurrence** : plusieurs clients à la fois ; pour le plan et les règles, la dernière
  modification gagne et chaque changement est diffusé à tous (`plan_changed`, `rules_changed`).

## Sécurité de la connexion

Le serveur génère au premier lancement un certificat auto-signé (`serveur/tls-cert.pem`, clé
en 0600), le garde d'un lancement à l'autre, affiche et journalise son **empreinte SHA-256**
(`matter-home-server fingerprint` la redonne). Le client la mémorise à la première connexion
et refuse tout autre certificat. Le compte unique se crée sur le serveur
(`create-account`) ; le mot de passe est haché en argon2id. Après 5 échecs depuis une adresse
(20 au total), la connexion est mise en attente 30 s, doublée à chaque échec (15 min au plus) :
`too_many_attempts` (HTTP 429, `Retry-After`). Un jeton est aléatoire (256 bits), stocké haché,
expire après 30 jours sans usage (180 jours au plus) et se révoque (`POST /api/logout`).

## Essayer sans appareil

`matter-home-server --demo` expose la même API avec des appareils simulés, sur
`127.0.0.1:8443`, dans un dossier temporaire supprimé à la sortie (ou celui de `--data-dir`).
S'il n'y a pas encore de compte, il crée le compte `demo` et affiche son mot de passe, tiré au
sort, **une seule fois sur la sortie standard** (jamais dans le journal). L'historique simulé
est écrit dans `history-demo/`, le plan et les règles de démonstration restent en mémoire.
`--listen ADRESSE:PORT` (le port `0` laisse le système choisir) et `--data-dir DOSSIER` servent
aux essais et aux tests.

## Routes

| Méthode et chemin | Rôle |
|---|---|
| `GET /api/version` | version du protocole et du serveur (sans jeton) |
| `POST /api/login` | `{username, password, protocol_version}` → `{token, expires_at, …}` (sans jeton) |
| `POST /api/logout` | révoque le jeton (204) ; le WebSocket de cette session se ferme |
| `GET /api/state` | état complet (même contenu que le premier message du WebSocket) |
| `GET /api/ws?protocol=1` | temps réel (WebSocket) |
| `POST /api/pairing` | `{code}` : appairer (202 ; la suite arrive en événements) |
| `POST /api/devices/{id}/retry` | réessayer la connexion (202) |
| `DELETE /api/devices/{id}` | oublier l'appareil (202) |
| `PUT /api/devices/{id}/customization` | `{name, icon}` ; `null` = automatique (202) |
| `POST /api/devices/{id}/on-off` | `{command: "On" \| "Off" \| "Toggle"}` (202) |
| `POST /api/devices/{id}/cover` | `{command: "Open" \| "Close" \| "Stop" \| {"Position": 40}}` (202) |
| `GET /api/devices/{id}/history` | historique à la demande (voir ci-dessous) |
| `GET /api/devices/{id}/history.csv` | CSV température et humidité (même fenêtre, 7 jours par défaut) |
| `DELETE /api/devices/{id}/history` | effacer l'historique (202) |
| `GET`, `PUT /api/plan` | lire, remplacer le plan de la maison |
| `GET`, `PUT /api/rules` | lire, remplacer les règles |
| `POST /api/rules/{id}/test` | exécuter une règle maintenant (202 ; résultat en `rule_run`) |
| `GET /api/rule-runs` | journal des 200 dernières exécutions de règles |
| `GET /api/export` | sauvegarde `.matterhome` (**clés privées en clair**) |
| `POST /api/import` | corps = fichier `.matterhome` ; 202, puis le serveur quitte (code 75) |

Les commandes répondent `202 {"accepted": true}` dès que le serveur les a transmises ; leur
effet (ou leur échec) arrive en événements. Les erreurs possibles : `not_found` (appareil ou
règle inconnu), `bad_request`, `pairing_busy`, `payload_too_large`, `unavailable`…

### Historique

`GET /api/devices/{id}/history?since=…&until=…&metrics=temperature,humidity&limit=…` (instants
en secondes Unix). Par défaut : la dernière heure, toutes les grandeurs, 5 000 points. La
fenêtre est bornée à 7 jours (la rétention du serveur) et `limit` à 20 000. S'il y a plus de
points, les **plus récents** sont renvoyés avec `"truncated": true` : relire avec `until`
égal à l'instant du premier point pour remonter. Les points sont `{at, metric, value}` ; les
grandeurs sont `temperature`, `humidity`, `power`, `energy`, `battery`, `position`, et les
états binaires `open`, `on_off`, `battery_low` (valeur 0 ou 1).

## WebSocket

Aucun message ne part du client (ses commandes passent par l'API). Le serveur envoie :

1. `{"type": "hello", "protocol_version": 1, "server_version": "…"}`
2. `{"type": "snapshot", "seq": N, "devices": […], "plan": {…}, "rules": {…}, "rule_runs": […], "pairing_busy": false}` :
   l'**état complet** (description, état de liaison et dernière valeur de chaque grandeur ; pas
   de séries, qui se lisent en historique) ;
3. `{"type": "event", "seq": N+1, "event": {"type": "reading", …}}` pour chaque changement.

Événements (`event.type`) : `device_updated`, `device_removed`, `status`, `seen`, `mode`,
`reading` (mesure ou état), `cover_motion`, `rule_run`, `pairing_progress`, `pairing_done`,
`pairing_failed`, `notice`, `notification`, `plan_changed`, `rules_changed`.

Le `seq` croît de 1 à chaque événement : un client qui voit un trou rouvre une connexion.
**Client lent** : le serveur ne l'attend jamais. S'il prend du retard, il reçoit un nouveau
`snapshot` (à appliquer en remplaçant tout, puis à suivre les événements de `seq` supérieur) ;
s'il y retombe plus de 3 fois par minute, ou n'avance plus (écriture bloquée 10 s), il est
déconnecté. Il se reconnecte alors et repart d'un état complet. Un client fermé ne reçoit
rien, notifications comprises : le serveur n'a pas de file d'attente pour les absents.

## Export et import

`GET /api/export` renvoie le fichier `.matterhome` (appareils, plan, règles, historique, **et les
clés privées du contrôleur en clair** : le canal est chiffré et authentifié, le fichier doit
être gardé comme un secret). `POST /api/import` valide la sauvegarde, la met de côté, répond
202 puis le serveur s'arrête **avec le code 75** (non nul, pour qu'un launchd configuré pour ne
relancer que les échecs le relance aussi). Au lancement suivant, il applique l'import, garde
les anciennes données dans un dossier voisin `…avant-import-<date>`, et reprend. Le dossier
`serveur/` (compte, sessions, certificat) n'est ni exporté ni remplacé : l'empreinte et le
compte ne changent pas.

## Hors protocole pour l'instant

Pas de pagination du journal des règles (200 entrées, en mémoire : perdu au redémarrage), pas
de file d'attente de commandes côté serveur, pas de WebSocket vers la page web (le jeton passe
dans l'en-tête `Authorization`, que les navigateurs ne permettent pas sur un WebSocket).
