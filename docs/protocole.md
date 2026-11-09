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
(`create-account`) ; le mot de passe est haché en argon2id. Après 5 échecs depuis une adresse,
la connexion de **cette adresse** est mise en attente 30 s, doublée à chaque échec (15 min au
plus) : `too_many_attempts` (HTTP 429, `Retry-After`). Il n'y a **pas** de blocage global : une
attaque répartie sur plusieurs adresses ne peut pas enfermer le propriétaire dehors. Elle est
seulement ralentie : une seule vérification de mot de passe à la fois pour tout le serveur
(les demandes font la queue dans l'ordre d'arrivée) et 250 ms de pause avant la réponse à un
échec. Un jeton est aléatoire (256 bits), stocké haché, expire après 30 jours sans usage
(180 jours au plus) et se révoque (`POST /api/logout`).

**Limites de connexion** : 128 connexions en tout, 16 au plus par adresse IP (les suivantes sont
fermées sans réponse), poignée de main TLS en 10 s, en-têtes en 10 s, et **15 s sans recevoir
un octet du corps** d'une requête (réponse `request_timeout`, HTTP 408 ; le délai repart à zéro
à chaque morceau reçu, un gros import régulier passe). Une requête entière dure 120 s au plus.

**Écrire en TLS** : un client qui écrit une requête (surtout un corps volumineux) doit appeler
`flush` sur son flux TLS avant d'attendre la réponse. Les bibliothèques asynchrones (par exemple
tokio-rustls) gardent la fin des données dans un tampon qui ne part que sur `flush` ou à la
lecture suivante ; sans lui, le serveur attend la suite du corps jusqu'à son délai (408 après
15 s) pendant que le client attend la réponse.

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
| `POST /api/import?dry_run=1` | même corps ; inspecte sans rien engager : 200 avec le résumé (voir ci-dessous) |

Les commandes répondent `202 {"accepted": true}` dès que le serveur les a transmises ; leur
effet (ou leur échec) arrive en événements. Les erreurs possibles : `not_found` (appareil ou
règle inconnu), `bad_request`, `pairing_busy`, `payload_too_large` (413), `method_not_allowed`
(405, avec l'en-tête `Allow`), `request_timeout` (408), `import_pending` (409), `invalid_import`
(400), `unavailable`… Toute erreur, y compris 405 et 413, a le corps `{"error": {"code": …}}`.

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
   Une mesure plus ancienne que la dernière retenue pour cet appareil (historique rejoué au
   démarrage du serveur) n'est ni retenue ni diffusée : le serveur ne numérote que ce qu'il
   envoie, `seq` n'a pas de trou.

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
être gardé comme un secret).

`POST /api/import` valide **à fond** la sauvegarde : identité du contrôleur (clés et certificats
lisibles), `maison.json` et `regles.json` (désérialisés, puis mêmes bornes que `PUT /api/plan` et
`PUT /api/rules`). Une sauvegarde invalide est refusée (`invalid_import`, 400) sans rien écrire.
Sinon elle est déposée dans le dossier de données (`import-en-attente.matterhome`, créée en 0600,
écriture atomique), la réponse est 202, puis le serveur s'arrête **avec le code 75** (non nul,
pour qu'un launchd configuré pour ne relancer que les échecs le relance aussi). Un second import
pendant ce temps est refusé : `import_pending` (409).

**Inspecter sans importer** : `POST /api/import?dry_run=1` (même corps) fait les mêmes contrôles et
répond 200 `{"summary": {"created": "2026-11-09T10:00:00+01:00", "devices": 12, "rooms": 7,
"rules": 4, "history_files": 12, "has_plan": true, "has_rules": true}, "restarting": false}`
sans rien écrire ni arrêter le serveur. Le client l'appelle pour montrer « 12 appareils, 7 pièces,
4 règles, sauvegarde du … » avant de demander confirmation, puis refait l'appel sans `dry_run`.

**Application au lancement suivant**, atomique et reprise après coupure : le nouveau dossier est
construit à côté du dossier de données `D` (`D.import-partiel` : fichiers créés en 0600 et
synchronisés, copie de `serveur/`), renommé en `D.import-nouveau` (c'est le marqueur « complet »),
puis `D` devient `D.avant-import-<date>` et `D.import-nouveau` devient `D`. Au démarrage, le
serveur reprend un échange coupé entre ces deux renommages (`D` absent, `D.import-nouveau`
présent) et nettoie les restes des autres étapes ; il ne crée jamais de dossier ni d'identité à
la place. Un échec passager (disque, droits) remet tout en place, garde la sauvegarde en attente
et, après trois essais, **empêche le serveur de démarrer** en le disant dans le journal (un
import annoncé n'est jamais perdu en silence, ni appliqué par surprise plus tard). Le dossier
`serveur/` (compte, sessions, certificat) n'est ni exporté ni remplacé, seulement copié : l'empreinte
et le compte ne changent pas.

## Côté client (`crates/client`)

Le client n'a ni cache ni mode local : tout ce qu'il affiche vient du serveur, et il l'oublie à
la déconnexion. Sa couche réseau (`crates/client/src/net/`) n'utilise pas tokio : des fils
d'exécution et des sockets bloquants, hors du fil de l'interface, qui n'attend jamais le réseau.

- **Première connexion** : le client fait la poignée de main TLS pour lire l'empreinte du
  certificat, ferme sans rien envoyer, et la montre à l'utilisateur (qui la compare à celle du
  serveur). Après approbation, la connexion suivante, la seule à porter le mot de passe, n'accepte
  que ce certificat (la signature de la poignée de main est vérifiée pour de bon).
- **Fichier de connexion** : adresse, empreinte et jeton, en 0600, dans
  `dev.matter-home-desktop.MatterHomeClient/connexion.json` (jamais dans le dossier de données de la
  v0.2.1). Jamais le mot de passe. Jeton expiré ou révoqué : retour à l'écran de connexion, adresse
  et empreinte conservées.
- **Empreinte changée** : connexion refusée, rien n'est envoyé, aucune reconnexion. Un geste
  explicite (« Oublier l'ancien certificat », avec case à cocher) repasse par la confirmation.
- **Version** : `GET /api/version` avant toute connexion ; un protocole différent donne un message
  clair (« mettez à jour l'application ou le serveur »), sans boucle de reconnexion.
- **Temps réel** : l'état complet remplace tout ; ensuite `seq` doit croître de 1. Un événement de
  `seq` déjà vu est ignoré, un trou rouvre la connexion. Fermeture 1013 (client en retard) : simple
  reconnexion, l'affichage reste. Autre coupure : « déconnecté » (les données sont effacées) et
  nouvel essai après 1, 2, 4, 8, 16 puis 30 s. Une mesure plus ancienne que la valeur connue est
  ignorée.
- **Requêtes** : 4 au plus en même temps (une connexion chacune, `Connection: close`, `flush` du flux
  TLS avant d'attendre la réponse), plus le WebSocket : bien sous les 16 connexions par adresse.
- **Plan et règles** : un seul envoi en vol par document, modifications regroupées (400 ms de calme),
  dernière modification gagnante. Si un autre client modifie pendant une édition locale, l'édition
  est gardée et un bandeau propose d'afficher l'autre version.
- **Historique** : lu à la demande quand une fiche s'ouvre ou que la fenêtre change (« 7 jours » est
  le maximum que le serveur garde), complété par les mesures reçues, vidé à la déconnexion.
- **Export et import** : le serveur produit le `.matterhome` et le CSV, le client les enregistre
  avec le sélecteur de fichier (sauvegarde en 0600). Import : `?dry_run=1` pour le résumé, confirmation,
  puis import réel ; le serveur redémarre et le client se reconnecte seul.
- **Erreurs** : le client traduit chaque `code` en français (correspondance exhaustive : un nouveau
  code oblige à écrire sa phrase) ; le mot de passe, le jeton et les codes d'appairage ne sont jamais
  journalisés.

## Hors protocole pour l'instant

Pas de pagination du journal des règles (200 entrées, en mémoire : perdu au redémarrage), pas
de file d'attente de commandes côté serveur, pas de WebSocket vers la page web (le jeton passe
dans l'en-tête `Authorization`, que les navigateurs ne permettent pas sur un WebSocket).
