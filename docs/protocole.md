# Protocole client-serveur

[← Retour au README](../README.md) · [Architecture](architecture.md)

Le client (interface) et le serveur (`matter-home-server`) parlent en **HTTPS** (requêtes) et en
**WebSocket** (temps réel), sur le même port (8443 par défaut). Les types sont dans
`crates/protocol/src/wire/` : ce crate n'a ni tokio, ni axum, ni TLS, pour que le client le
réutilise tel quel. Les types internes `Command` et `Event` ne sont jamais envoyés tels quels.

## Règles

- **Version** : `protocol_version` (2 aujourd'hui). `GET /api/version` la donne sans jeton ;
  la connexion et le WebSocket (`?protocol=2`) l'exigent identique, sinon l'erreur
  `version_incompatible` (HTTP 426). Ajouter un champ optionnel ne change pas la version ;
  ajouter une **variante** d'énumération (un type d'événement, un code d'erreur) la change, car
  `serde` refuse les variantes inconnues. Historique : **2** = caméras (ticket n° 4, événement
  `cameras_changed`) ; 1 = la v1.0.0.
- **Identifiants en texte** : appareils, règles, étages, pièces, objets sont des chaînes
  décimales (`"18446744073709551615"`), car un `u64` dépasse 2^53 et JavaScript l'arrondirait.
  Un identifiant numérique, ou avec zéro initial, est refusé. Les compteurs (`seq`) restent
  des nombres. Le plan et les règles gardent le format de `maison.json` et `regles.json`,
  seuls les identifiants (`id`, `node_id`, `rule_id`) passent en texte.
- **Erreurs** : `{"error": {"code": "...", "detail": "...", "retry_after_s": 30}}`. Le client
  se fie au `code` (stable, voir `ErrorCode`) ; `detail` est un texte humain facultatif, sans
  chemin local.
- **Jeton** : `Authorization: Bearer <jeton>` sur toute route et sur le WebSocket, jamais
  dans l'URL. Seules `GET /api/version` et `POST /api/login` s'en passent. **Navigateur** (app web,
  `/app/`, voir [web-app.md](web-app.md)) : un `WebSocket` ne pose pas d'en-tête, donc le jeton
  peut aussi venir de `Sec-WebSocket-Protocol: matter-home.v1, bearer.<jeton>`, accepté seulement
  sur un upgrade WebSocket ; le serveur répond `matter-home.v1`. Le client natif n'est pas touché.
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
| `GET /api/ws?protocol=2` | temps réel (WebSocket) |
| `GET /api/cameras` | les caméras, **sans adresse ni mot de passe** (voir « Caméras ») |
| `GET /api/cameras/{id}/snapshot` | une image JPEG récente de la caméra (voir « L'image fixe d'une caméra ») |
| `POST /api/pairing` | `{code}` : appairer (202 ; la suite arrive en événements) |
| `POST /api/devices/{id}/retry` | réessayer la connexion (202) |
| `DELETE /api/devices/{id}` | oublier l'appareil (202) |
| `PUT /api/devices/{id}/customization` | `{name, icon}` ; `null` = automatique (202) |
| `POST /api/devices/{id}/on-off` | `{command: "On" \| "Off" \| "Toggle"}` (202) |
| `POST /api/devices/{id}/cover` | `{command: "Open" \| "Close" \| "Stop" \| {"Position": 40}}` (202) |
| `GET /api/devices/{id}/history` | historique à la demande (voir ci-dessous) |
| `GET /api/devices/{id}/history.csv` | CSV température et humidité (même fenêtre, 7 jours par défaut) |
| `DELETE /api/devices/{id}/history` | effacer l'historique (202) |
| `GET`, `PUT /api/plan` | lire, remplacer le plan de la maison (au moins un étage : sinon `plan_without_floor`, 400) |
| `GET`, `PUT /api/rules` | lire, remplacer les règles |
| `POST /api/rules/{id}/test` | exécuter une règle maintenant (202 ; résultat en `rule_run`) |
| `GET /api/rule-runs` | journal des 200 dernières exécutions de règles |
| `GET /api/export` | sauvegarde `.matterhome` (**clés privées et mots de passe des caméras en clair**) |
| `POST /api/import` | corps = fichier `.matterhome` ; 202, puis le serveur quitte (code 75). Une autre identité Matter est refusée (`identity_mismatch`, 409) sans `?replace_identity=1` |
| `POST /api/import?dry_run=1` | même corps ; inspecte sans rien engager : 200 avec le résumé (voir ci-dessous) |

Les commandes répondent `202 {"accepted": true}` dès que le serveur les a transmises ; leur
effet (ou leur échec) arrive en événements. Les erreurs possibles : `not_found` (appareil ou
règle inconnu), `bad_request`, `plan_without_floor` (400, plan sans étage), `pairing_busy`, `payload_too_large` (413), `method_not_allowed`
(405, avec l'en-tête `Allow`), `request_timeout` (408), `import_pending` (409), `identity_mismatch` (409), `invalid_import`
(400), `unavailable`… Toute erreur, y compris 405 et 413, a le corps `{"error": {"code": …}}`.

### Historique

`GET /api/devices/{id}/history?since=…&until=…&metrics=temperature,humidity&limit=…` (instants
en secondes Unix). Par défaut : la dernière heure, toutes les grandeurs, 5 000 points. La
fenêtre est bornée à 7 jours (la rétention du serveur) et `limit` à 20 000. S'il y a plus de
points, les **plus récents** sont renvoyés avec `"truncated": true` : relire avec `until`
égal à l'instant du premier point pour remonter. Les points sont `{at, metric, value}` ; les
grandeurs sont `temperature`, `humidity`, `power`, `energy`, `battery`, `position`, et les
états binaires `open`, `on_off`, `battery_low` (valeur 0 ou 1).

**Réduction par tranches** (optionnelle, ajoutée après la v1.0.0-rc.4) : `buckets=N` (au plus
2 000) découpe la fenêtre en N tranches égales ; chaque grandeur ne garde que le **minimum et
le maximum** de chaque tranche (au plus 2 N + 2 points par grandeur, extrémités comprises). Avec
`buckets`, `limit` compte **par grandeur** (et non pour l'ensemble des points) et `truncated`
vaut `true` si une seule grandeur a dû être coupée.
Toute la fenêtre est donc couverte, même pour un capteur qui rapporte toutes les 10 s, et les
pointes survivent. Aucune valeur n'est calculée (pas de moyenne) et un trou de données reste un
trou. Un serveur plus ancien ignore le paramètre et renvoie les mesures brutes (éventuellement
tronquées) ; un client plus ancien ne l'envoie pas : la réponse garde la même forme.

## WebSocket

Aucun message ne part du client (ses commandes passent par l'API). Le serveur envoie :

1. `{"type": "hello", "protocol_version": 2, "server_version": "…"}`
2. `{"type": "snapshot", "seq": N, "devices": […], "plan": {…}, "rules": {…}, "rule_runs": […], "pairing_busy": false, "cameras": […]}` :
   l'**état complet** (description, état de liaison et dernière valeur de chaque grandeur ; pas
   de séries, qui se lisent en historique) ;
3. `{"type": "event", "seq": N+1, "event": {"type": "reading", …}}` pour chaque changement.
   Une mesure plus ancienne que la dernière retenue pour cet appareil (historique rejoué au
   démarrage du serveur) n'est ni retenue ni diffusée : le serveur ne numérote que ce qu'il
   envoie, `seq` n'a pas de trou.

Événements (`event.type`) : `device_updated`, `device_removed`, `status`, `seen`, `mode`,
`reading` (mesure ou état), `cover_motion`, `rule_run`, `pairing_progress`, `pairing_done`,
`pairing_failed`, `notice`, `notification`, `plan_changed`, `rules_changed`, `cameras_changed`.

Le `seq` croît de 1 à chaque événement : un client qui voit un trou rouvre une connexion.
**Client lent** : le serveur ne l'attend jamais. S'il prend du retard, il reçoit un nouveau
`snapshot` (à appliquer en remplaçant tout, puis à suivre les événements de `seq` supérieur) ;
s'il y retombe plus de 3 fois par minute, ou n'avance plus (écriture bloquée 10 s), il est
déconnecté. Il se reconnecte alors et repart d'un état complet. Un client fermé ne reçoit
rien, notifications comprises : le serveur n'a pas de file d'attente pour les absents.

## Caméras

Ticket n° 4. Le serveur est le **seul** à parler aux caméras (RTSP). Le client ne reçoit que de
quoi les lister et dire si elles répondent : **jamais l'adresse RTSP ni le mot de passe**, qui
restent sur le serveur (`cameras.json`, voir plus bas). Ce n'est pas une règle de discipline : les
types de `crates/protocol/src/wire/camera.rs` n'ont aucun champ qui puisse les porter (un test
fige la liste des clés).

```json
{"cameras": [
  {"id": "18446744004990074881", "name": "Entrée",
   "status": {"state": "online"},
   "has_sub_stream": true,
   "stream": {"codec": "h264", "width": 1280, "height": 720}},
  {"id": "18446744004990074883", "name": "Garage",
   "status": {"state": "unreachable", "problem": "unreachable",
              "detail": "La caméra ne répond pas (éteinte, hors réseau, RTSP désactivé ou mauvais port)."},
   "has_sub_stream": true}
]}
```

- **Où** : `GET /api/cameras` (`{"cameras": […]}`, triées par nom), le champ `cameras` de l'état
  complet (HTTPS et WebSocket) et l'événement `{"type": "cameras_changed", "cameras": […]}`.
  L'événement porte **la liste complète** (32 caméras au plus) : le client remplace la sienne. Il
  part à chaque ajout, modification, suppression, et quand l'état d'une caméra change (jamais si
  une vérification redonne le même résultat).
- **Identifiant** (`id`) : texte décimal d'un `u64`, tiré dans la plage
  `0xFFFF_FFF8_0000_0000..=0xFFFF_FFFF_FFFF_FFFF` (`CAMERA_ID_MIN`, `CAMERA_ID_MAX`), que Matter
  réserve (jamais un identifiant de nœud appairé). **La plage n'est pas exclusive** : avant les
  caméras, les appareils derrière un pont tiraient les leurs sur toute la plage
  `0xFFFF_FFF0_0000_0000..`, donc environ la moitié de ceux déjà attribués s'y trouvent. Aujourd'hui
  les nouveaux appareils pontés sont tirés juste en dessous. `is_camera_id` n'est qu'un contrôle de
  plage ; la preuve qu'un identifiant est une caméra est sa présence dans la liste du serveur. Le
  serveur ne donne jamais à une caméra l'identifiant d'un appareil connu, et l'import d'une
  sauvegarde refuse une caméra dont l'identifiant est celui d'un appareil de `controller.json`.
- **État** (`status.state`) : `unknown` (pas encore vérifiée), `online`, ou `unreachable` avec
  `problem` (code stable : `unreachable`, `unauthorized`, `not_found`, `protocol`, `failed`) et
  `detail`, une phrase française sans adresse ni mot de passe. Le serveur **ne garde aucune
  connexion ouverte** vers une caméra : il fait un `DESCRIBE` bref (jamais `SETUP` ni `PLAY`),
  une caméra à la fois, à l'ajout puis environ chaque minute, avec 8 s de délai. Une caméra
  injoignable n'a aucun effet sur les appareils Matter (autre fil, autre runtime).
- **Depuis quand** (`status.since`) : pour `unreachable`, l'heure (secondes Unix) du passage à
  injoignable, fixée **après les 2 échecs d'affilée** d'une caméra jusque-là joignable (ou au
  premier échec d'une caméra jamais vue joignable), puis **inchangée** tant que la caméra reste
  injoignable, même si le motif change. Elle disparaît quand la caméra revient. Facultatif
  (`#[serde(default)]`, non écrit s'il est absent) : un client plus ancien l'ignore, un serveur plus
  ancien ne l'envoie pas (« depuis quand » inconnu). Les états ne sont pas gardés sur disque : après
  un redémarrage du serveur, l'heure repart du premier échec constaté. Version du protocole : **2**.
- **Flux** (`stream`) : ce que la caméra annonce pour le flux surveillé (le réduit s'il existe,
  sinon le principal) : `codec` en minuscules (`h264`, `h265`, `jpeg`), et `width`/`height` si la
  description les contient (SPS). Il garde la dernière valeur connue quand la caméra devient
  injoignable. Un client qui ne décode pas le codec doit le dire (« H.265 non pris en charge »).
- **Lecture seule** : aucune route n'ajoute, ne modifie ni ne supprime une caméra (`POST`, `PUT`,
  `DELETE` sur `/api/cameras` : 405). On les gère dans la fenêtre du serveur (« Caméras… »). Un
  porteur de jeton ne peut donc ni lire un mot de passe de caméra, ni faire connecter le serveur à
  une adresse de son choix.
- **Compatibilité** : un serveur de version 1 n'a pas `cameras` (à lire comme vide) ; ce champ,
  et ceux qu'on ajouterait aux types de caméra, sont ignorés par qui ne les connaît pas. Mais
  `cameras_changed` est une nouvelle variante d'événement : un client de version 1 la refuserait.
  C'est pourquoi la version du protocole passe à **2** (client et serveur sortent ensemble).
  Elle **ne bougera plus pendant les tickets n° 4 à 7**. Pour la suite, les énumérations qui
  vont grandir ont une variante de repli : un événement inconnu est lu `Unknown` (le client
  l'ignore mais compte son `seq`), un état de caméra inconnu `unknown`, un motif inconnu `failed`.
  `ItemKind` n'en a pas (un plan qu'on ne comprend pas ne s'affiche pas à moitié).

**Données du serveur** (jamais envoyées) : `cameras.json` dans le dossier de données, créé en
**0600** (dossier en 0700), écriture atomique. Pour chaque caméra : `id` (numérique dans le
fichier, comme `maison.json`), `name`, `url` (flux principal, **sans** `user:mdp@`), `sub_url`
(flux réduit, facultatif), `username`, `password`. On peut coller `rtsp://user:mdp@ip:554/…` dans le
formulaire : les identifiants sont rangés dans leurs champs et retirés de l'adresse (`retina` refuse
d'ailleurs une adresse qui en porte). Aqara G100 (recherche, non vérifié sur l'appareil) :
`rtsp://ip:554/live/ch00_0` (principal, environ 2304 × 1296, signalé instable) et `ch00_1` (720p,
stable) ; ne pas lire plus de deux flux à la fois par caméra. `Debug` masque tout (adresse,
identifiant, mot de passe) ; ni le journal ni un message d'erreur ne reprennent ce qui a été saisi.
Le fichier est dans la sauvegarde `.matterhome` : **les mots de passe des caméras y sont en clair**,
comme les clés de la maison (la fenêtre « Caméras… » et le message d'export du client le disent).
L'import le valide (version, plage et unicité des identifiants, adresses `rtsp://` sans
identifiants) et refuse la sauvegarde sans répéter d'adresse ni de mot de passe. Le résumé
(`ImportSummary.cameras`, 0 si absent) compte les caméras.

**Pour #5 (lecture), #6 (plan), #7 (liste)** :

- *Transport de la vidéo, non décidé ici.* Le ticket n° 5 propose que le serveur se connecte à la
  caméra seulement tant que quelqu'un regarde et **relaie les images H.264 telles quelles**
  (WebSocket binaire par caméra, sans décodage ni réencodage), le client décodant. L'autre option
  est une connexion RTSP directe du client à la caméra, qui exigerait de lui donner l'adresse et le
  mot de passe : contraire à la règle ci-dessus. Le point d'entrée côté serveur est
  `Hub::camera_target(id, StreamKind::{Main, Sub})` (adresse sans identifiants + identifiants),
  qui ne sort jamais du serveur ; le flux réduit est le défaut (`StoredCamera::preferred_kind`).
  La route de flux est décrite plus bas (« Le flux d'une caméra »).
- *Placement* : il ne va **pas** dans la caméra mais dans le plan (`home::Item`) : #6 ajoute
  `ItemKind::Camera { camera_id }` et `Item::rotation` (`#[serde(default)]`). Contrôle côté serveur :
  `is_camera_id` et existence de la caméra ; à la suppression, `Hub::delete_camera` est l'endroit
  où la retirer du plan (déjà fait, dans la même transaction). La version du protocole est **déjà à
  2** : ne pas la relever de nouveau pour un ticket de la même livraison.
- *Route du flux, réservée* : `wire::CAMERA_STREAM_PATH` (`/api/cameras/{id}/stream`) et
  `camera_stream_path(id, StreamKind::Main | Sub)` (`?kind=main|sub`).
- *Plan* : `ItemKind::Camera { camera_id }`, `Item::rotation` (degrés, 0 à 360 exclu, sens horaire,
  0 vers le haut ; absent des anciens plans : 0), `HomePlan::camera_room`, `placed_cameras`,
  `remove_camera`. `PUT /api/plan` refuse une rotation hors plage et un `camera_id` hors de la plage
  des caméras. Supprimer une caméra la retire aussi du plan (`plan_changed` suit `cameras_changed`).

### Le flux d'une caméra

`GET /api/cameras/{id}/stream?kind=main|sub` (`sub` par défaut) : un **WebSocket** sous le jeton
Bearer, qui compte parmi les 32 emplacements WebSocket. La version du protocole reste **2**.

- **Refus** : 401 (jeton) ; 404 avec `detail` si la caméra est inconnue ; 404 **sans** `detail` si
  la route n'existe pas (serveur ancien) ; 400 si `kind` ou l'identifiant est invalide, ou s'il n'y
  a pas de flux réduit ; 503 s'il y a trop de WebSocket.
- **Fermeture** : 1001 (caméra modifiée ou serveur à l'arrêt), 1008 (session terminée), 1000
  (codec non pris en charge).
- **Messages texte** : `StreamStatus` : `connecting`, `live`, `retrying{problem}`,
  `unsupported{codec}`, `unknown`.
- **Messages binaires** :
  - `Config`, étiquette `0x01` : codec (u8), largeur (u16), hauteur (u16), rapport d'aspect du
    pixel n/d (u16 chacun), puis l'`avcC`.
  - `Frame`, étiquette `0x02` : drapeaux (u8, bit 0 = image clé), horodatage (u32, en ms), puis les
    données AVCC (longueurs sur 4 octets). **Chaque image clé porte son SPS/PPS.**
- **Livraison** : une image perdue n'est jamais rejouée ; un client lent reprend à l'image clé
  suivante. Le délai d'écriture de 10 s ne s'applique pas au flux : il a son propre seuil de 30 s
  d'écriture bloquée.

### L'image fixe d'une caméra

`GET /api/cameras/{id}/snapshot` (ticket n° 12) : une image **JPEG** récente (`Content-Type:
image/jpeg`, `Age` en secondes), sous le jeton Bearer comme le reste. Les clients s'en servent pour
une vignette quand la mosaïque ne lit pas de flux : caméra sans flux réduit, ou au-delà du plafond
de flux simultanés (4). La version du protocole reste **2** : une route de plus, aucun type ni code
d'erreur nouveau (`wire::CAMERA_SNAPSHOT_PATH`, `wire::camera_snapshot_path(id)`).

- **Fabrication** : le serveur décode une image clé H.264 (VideoToolbox, la caisse `matter-home-video`
  partagée avec le client) et l'encode en JPEG (crate `jpeg-encoder`, Rust pur, sans FFmpeg ni
  libjpeg), réduite à 640 px de large au plus. La forme non carrée des pixels n'est pas corrigée.
  Une caméra H.265 n'a pas d'image fixe (503).
- **Cache partagé** : une image par caméra, valable **15 s** pour tous les clients. Un échec est
  gardé 3 s (dix vignettes ne font pas dix essais). Une caméra supprimée n'a plus d'image.
- **Une capture à la fois** par caméra : les demandes simultanées attendent et reçoivent le même
  résultat. Délai maximal d'une demande, file d'attente comprise : **12 s**.
- **Spectateurs** : la capture passe par le registre de la lecture. Caméra déjà regardée (n'importe
  quel flux) : on se joint à la session, sans connexion de plus, et l'image vient de la dernière
  image clé gardée. Sinon une session s'ouvre sur le flux préféré (le réduit s'il existe) avec un
  témoin de spectateur (la surveillance et « Tester » l'évitent), on attend la première image clé,
  puis la session se ferme après sa grâce de 5 s. Jamais plus de deux connexions à la caméra. Avant
  d'ouvrir ou de rejoindre, la capture prend sans attendre le **verrou de la caméra** (celui de la
  surveillance et du testeur) : pris, la réponse est 503 et rien n'est ouvert.
- **Erreurs** (corps `{"error": {"code", "detail", "retry_after_s"}}`) :

  | statut | code | quand |
  |---|---|---|
  | 401 | `unauthorized` | jeton absent ou invalide |
  | 400 | `bad_request` | identifiant invalide |
  | 404 | `not_found` avec `detail` | caméra inconnue |
  | 404 | `not_found` **sans** `detail` | route inconnue : serveur plus ancien (le client le dit et n'insiste pas) |
  | 502 | `device_unreachable` | pas d'image clé à temps, caméra éteinte ou muette (`retry_after_s` 10) |
  | 503 | `unavailable` | test ou vérification en cours (`retry_after_s` 5), caméra modifiée, codec non pris en charge, décodeur absent |

- **Client de bureau** : une tuile sans flux lu (pas de flux réduit, ou au-delà de
  `MAX_LIVE_TILES`) dont la caméra est en ligne affiche cette image, rafraîchie toutes les **20 s**
  tant qu'elle est à l'écran (30 s après un échec ; la dernière image reste affichée).
- **App web** : même règle pour les vignettes à l'écran qui ne lisent pas de flux (au-delà de 4).
  `fetch` avec l'en-tête `Authorization`, puis `Blob`, `createImageBitmap` et dessin dans le canvas
  de la vignette. **La page ne charge jamais d'URL `blob:`** : la politique de sécurité (CSP) ne
  change pas (`img-src 'self'`, ni `blob:` ni `media-src`). Rien n'est demandé tant que l'onglet est
  caché.
- **Compatibilité** : un client récent face à un serveur ancien reçoit le 404 sans `detail` et garde
  son texte « ouvrez la caméra » ; un client ancien ne connaît pas la route et ne l'appelle pas.

### Porte ou fenêtre sur le plan

Un objet `Device` du plan peut porter `"opening": "Door" | "Window"` : Matter ne distingue pas une
porte d'une fenêtre, c'est l'utilisateur qui le dit (inspecteur du plan, « Porte / Fenêtre »). Le
champ est facultatif (`#[serde(default)]`, absent des plans anciens ; il n'est pas écrit quand il
vaut `null`). Absent : porte, ou fenêtre si le pictogramme choisi pour l'appareil en est une. Une
valeur inconnue d'une version plus récente est lue comme une porte. La version du protocole reste
**2**.

**Mettre à jour client et serveur ensemble** : un client plus ancien ignore le champ, donc il
l'efface quand il enregistre le plan. Pour limiter les dégâts, `PUT /api/plan` garde le réglage
déjà enregistré pour un objet (même identifiant, même appareil) dont le nouveau plan n'a pas
`opening`. (On ne peut donc pas « effacer » le réglage : on le change.)

### Une caméra sur le plan

`{"type":"Camera","camera_id":"<texte>","fov":90.0,"range":3.0,"shape":"Cone"|"Rectangle"}`.
`fov`, `range` et `shape` sont facultatifs (90°, 3 m, `Cone`).

- `PUT /api/plan` : `fov` entre 10 et 180 ; `range` entre 0,5 et 50 ; une caméra ne figure qu'**une
  fois**, tous étages confondus ; une référence à une caméra que le serveur ne connaît pas est
  acceptée, et le client l'affiche grisée.
- **Compatibilité** : un client ou un serveur plus ancien ne sait pas lire un plan qui contient un
  objet `Camera`.

- Répartition des fichiers entre les tickets : [cameras-contrat.md](cameras-contrat.md).
- *Client* : `Model::cameras` (remplacé par `cameras_changed`) et `Model::camera(id)`.

## Export et import

`GET /api/export` renvoie le fichier `.matterhome` (appareils, plan, règles, caméras, historique, **les
clés privées du contrôleur et les mots de passe des caméras en clair** : le canal est chiffré et authentifié, le fichier doit
être gardé comme un secret).

`POST /api/import` valide **à fond** la sauvegarde : identité du contrôleur (clés et certificats
lisibles), `maison.json` et `regles.json` (désérialisés, puis mêmes bornes que `PUT /api/plan` et
`PUT /api/rules`), `cameras.json` (voir « Caméras »). Une sauvegarde invalide est refusée (`invalid_import`, 400) sans rien écrire. Une vraie
sauvegarde dont le plan n'a aucun étage (`floors: []`) n'est pas refusée : son plan est remplacé
par le plan par défaut, avant d'être déposée (le même remplacement a lieu au chargement d'un
`maison.json` sans étage).
Sinon elle est déposée dans le dossier de données (`import-en-attente.matterhome`, créée en 0600,
écriture atomique), la réponse est 202, puis le serveur s'arrête **avec le code 75** (non nul,
pour qu'un launchd configuré pour ne relancer que les échecs le relance aussi). Un second import
pendant ce temps est refusé : `import_pending` (409).

**Inspecter sans importer** : `POST /api/import?dry_run=1` (même corps) fait les mêmes contrôles et
répond 200 `{"summary": {"created": "2026-11-09T10:00:00+01:00", "devices": 12, "rooms": 7,
"rules": 4, "history_files": 12, "has_plan": true, "has_rules": true,
"disabled_rules": ["Alerte porte"]}, "restarting": false, "identity": {"incoming":
{"fabric_id": "0xff", "devices": 3}, "current": {"fabric_id": "0x1", "devices": 1}, "differs": true}}`
sans rien écrire ni arrêter le serveur. Le client l'appelle pour montrer « 12 appareils, 7 pièces,
4 règles, sauvegarde du … » avant de demander confirmation, puis refait l'appel sans `dry_run`.

**Garde-fou d'identité** : l'import par l'API compare le numéro de fabric de la sauvegarde à celui du
serveur (comme `import --replace-identity` en ligne de commande). `identity` donne les deux
identités (`fabric_id` en texte hexadécimal, nombre d'appareils ; `current` est absent si le serveur
n'a pas encore d'identité) et `differs` vaut `true` quand elles diffèrent, ou quand celle du serveur
est illisible. Dans ce cas, `POST /api/import` sans `replace_identity=1` est refusé avec le code
stable `identity_mismatch` (409) : rien n'est écrit, le serveur ne s'arrête pas. Le client prévient
clairement dans son dialogue et n'ajoute `replace_identity=1` qu'après une confirmation séparée
(« Remplacer l'identité du serveur… » puis « Oui, remplacer l'identité et importer »). Les
appareils appairés avec l'ancienne identité ne seraient plus reconnus ; l'ancien dossier reste
conservé dans « …avant-import-<date> ». Les valeurs acceptées de `dry_run` et `replace_identity` :
`1`, `true`, `0`, `false`.

**Règles à webhook incomplet** (`disabled_rules`) : la v0.2.1 enregistrait des règles dont l'adresse
de l'action « Appeler une URL » était vide, sans `http` ou avec un espace, alors que `PUT /api/rules`
les refuse quand la règle est **activée**. Plutôt que de refuser toute la sauvegarde, l'import
**désactive** ces règles (`enabled: false`, tout le reste est conservé tel quel) et en donne les
noms dans `disabled_rules` (liste vide sinon ; champ absent des anciens serveurs, à lire comme
vide). Le client doit les montrer avant la confirmation. Une règle **désactivée** peut garder une
adresse incomplète (`PUT /api/rules` l'accepte, bornes de taille comprises) : elle ne s'exécute
jamais, et l'appel du webhook revérifie l'adresse, même pour `POST /api/rules/{id}/test`. Dès qu'on
la réactive, l'adresse doit être complète.

**Import en ligne de commande**, serveur arrêté : `matter-home-server import FICHIER [--yes]
[--replace-identity]` (voir [migration.md](migration.md)). Même validation (les deux certificats
DER sont décodés), même application atomique ; il prend le verrou d'instance, applique tout de
suite puis quitte, sans jamais démarrer Matter ni le réseau. Relancé après une coupure avec le même
fichier, il reprend l'import resté en attente ; une fabric différente de celle du dossier n'est
remplacée qu'avec `--replace-identity`. `import --abandon` retire la sauvegarde en attente et met
de côté le dossier préparé `D.import-nouveau` sous `D.import-abandonne-<date>` (jamais supprimé).

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

**Reprise** : si `D` est absent, ou présent sans `controller.json` (par exemple recréé par
`create-account` entre la coupure et la relance, avec le seul `serveur/`) alors que `D.import-nouveau`
contient une identité, l'échange est terminé : `D` (sans identité) est mis de côté sous
`D.sans-identite-<date>`, jamais supprimé, puis `D.import-nouveau` devient `D`. `create-account`
et `import` font cette reprise avant de toucher au dossier. `create-account` ne prend pas le
verrou d'instance de force (`--replace` s'utilise serveur allumé) : si un serveur le tient, il a
déjà fait la reprise à son démarrage, et la commande ne reprend rien.

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
  v0.2.1). Jamais le mot de passe. Jeton expiré ou révoqué, ou « Se déconnecter » : le jeton est
  effacé (et révoqué sur le serveur), l'adresse et l'empreinte sont **conservées** ; un geste à part
  (« oublier son certificat », indisponible pendant une connexion) efface aussi l'empreinte. Une
  requête déjà en file au moment de la déconnexion ne part pas avec l'ancien jeton.
- **Empreinte changée** : connexion refusée, rien n'est envoyé, aucune reconnexion. Un geste
  explicite (« Oublier l'ancien certificat », avec case à cocher) repasse par la confirmation.
- **Version** : `GET /api/version` avant toute connexion ; un protocole différent donne un message
  clair (« mettez à jour l'application ou le serveur »), sans boucle de reconnexion.
- **Temps réel** : l'état complet remplace tout ; ensuite `seq` doit croître de 1. Un événement de
  `seq` déjà vu est ignoré, un trou rouvre la connexion. Fermeture 1013 (client en retard) : simple
  reconnexion, l'affichage reste. Autre coupure : « déconnecté » (les données sont effacées) et
  nouvel essai après 1, 2, 4, 8, 16 puis 30 s. Une mesure plus ancienne que la valeur connue est
  ignorée.
- **Échéances** : chaque lecture et écriture de la socket recalcule le temps restant avant une
  échéance absolue (connexion + poignée de main TLS 8 s, requête entière selon sa nature, mise à
  niveau WebSocket 10 s, état complet 30 s, 60 s au plus entre deux messages) : un serveur qui goutte
  ses octets ne les allonge pas. Un fil réseau ne meurt jamais sans répondre (une panique devient une
  erreur pour le demandeur, ou une « liaison perdue » avec nouvel essai pour le WebSocket).
- **Journal** : l'en-tête `Authorization` n'est jamais écrit, même avec `RUST_LOG=trace`.
- **Requêtes** : 4 au plus en même temps (une connexion chacune, `Connection: close`, `flush` du flux
  TLS avant d'attendre la réponse), plus le WebSocket : bien sous les 16 connexions par adresse.
- **Plan et règles** : un seul envoi en vol par document, modifications regroupées (400 ms de calme,
  pas pendant un glisser), dernière modification gagnante. Si un autre client modifie pendant une
  édition locale, l'édition est gardée, **rien n'est envoyé** et un bandeau propose d'afficher l'autre
  version ou de garder la sienne. Un document que le serveur refuserait d'avance (action « Appeler une
  URL » sans adresse valable) reste un brouillon visible, non envoyé. Un refus 400 garde le brouillon
  avec le motif, sans le renvoyer tant qu'il ne change pas ; un autre refus 4xx revient à la version
  du serveur ; un délai dépassé, un 503 ou une coupure gardent le document à envoyer (nouvel essai
  après 3 s, doublé jusqu'à 30 s). Une coupure garde le brouillon : à la reprise il repart si le
  serveur n'a pas changé entre-temps, sinon le bandeau prévient ; il est abandonné si c'est un autre
  serveur, ou après une déconnexion. À la fermeture de la fenêtre, un dernier envoi borné (1,5 s)
  part sans attendre les 400 ms. « Tester » une règle attend l'enregistrement de la dernière version.
- **Historique** : lu à la demande quand une fiche s'ouvre ou que la fenêtre change (« 7 jours » est
  le maximum que le serveur garde), complété par les mesures reçues, vidé à la déconnexion.
- **Export et import** : le serveur produit le `.matterhome` et le CSV, le client les enregistre
  avec le sélecteur de fichier (sauvegarde en 0600) ; l'écriture, et la lecture d'une sauvegarde à
  importer (128 Mo au plus), se font dans les fils réseau, sans copie du contenu. Une sauvegarde ne
  part que vers le serveur qui l'a validée (`dry_run`). Import : `?dry_run=1` pour le résumé, confirmation,
  puis import réel ; le serveur redémarre et le client se reconnecte seul.
- **Notifications** : quand le client est ouvert, une règle qui notifie affiche un toast et une
  notification macOS locale (titre et texte passés en arguments d'`osascript`, jamais dans son
  script).
- **Déconnexion** : le journal, les notifications affichées, le code d'appairage et toute fenêtre
  en attente d'une réponse sont effacés avec les données.
- **Erreurs** : le client traduit chaque `code` en français (correspondance exhaustive : un nouveau
  code oblige à écrire sa phrase) ; le mot de passe, le jeton et les codes d'appairage ne sont jamais
  journalisés.

## Hors protocole pour l'instant

Pas de pagination du journal des règles (200 entrées, en mémoire : perdu au redémarrage), pas
de file d'attente de commandes côté serveur, pas de WebSocket vers la page web (le jeton passe
dans l'en-tête `Authorization`, que les navigateurs ne permettent pas sur un WebSocket).
