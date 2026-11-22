# App web mobile (PWA)

## Refonte « Nuit et Lueur » (préversion, sans changement de version)

- **Coquille** : en-tête de 52 px (logotype de 32 px, pastille de mode à cible de 44 px) et barre du
  bas à cinq entrées : Appareils, Plan, Temp., Caméras et **Mode**. Mode montre le mode actuel avec
  son icône et ouvre la feuille des modes (quatre lignes, l'effet de chaque mode d'après `effect`
  de `mode.js`, « Actif » sur le mode du serveur). Le choix ne s'affiche qu'après la réponse du
  serveur. Le menu déroulant n'existe plus.
- **Appareils** : carte de phrase d'état (`statusSummary` de `tiles.js`, jumelle de
  `protocol::status`, essayée par `tiles.test.mjs`) avec « À jour à HH:MM » ; carte « Installer sur
  l'iPhone » (étapes : raccourcis, widget, notifications, d'après `GET /api/tokens` et l'abonnement
  push) qui disparaît quand tout est fait ; pièces en cartes sur deux colonnes, une ligne par appareil.
- **La maison ne répond pas** : bandeau « La maison ne répond pas. » avec la piste (Mac en veille,
  Tailscale) et « Réessayer » ; les cartes restent à 0,55 d'opacité avec « Dernières valeurs
  connues. Lu à HH:MM. » ; la pastille de mode passe en gris. Le texte vient de `api.js`
  (`HOUSE_SILENT`, `HOUSE_HINT`), repris par l'écran de connexion.
- **Confirmations** : un dialogue `[Annuler][Révoquer]` dans la page (`askConfirm`) remplace
  `confirm()` pour supprimer un événement, révoquer un jeton ou un abonnement.
- **Vocabulaire** : « injoignable » partout (jamais « hors ligne » ni « en ligne »), un seul nom : « Ostal ».
- **Plan** : s'ouvre sur le plan entier (ajustement complet), noms courts toujours visibles, légende, boutons de 44 px, feuille à demi ouverte.
- Les couleurs viennent toutes de `var(--color-…)` ; aucun `style=`, aucune URL externe dans `index.html`.

[← Retour au README](../README.md) · [Protocole](protocole.md) · [Design system](design-system.md)

Ticket n° 15. Le serveur sert lui-même une app web pensée pour le mobile (iPhone, iPad, et aussi
un ordinateur) sur **`https://<serveur>:<port>/app/`**. Les fichiers sont **intégrés au binaire**
(`include_str!`, `include_bytes!`) : pas de dossier à déployer, pas d'étape de build JavaScript,
pas de framework, pas de nouvelle dépendance Rust.

## Ce qu'elle fait

- **Style** : « tableau de bord », tout est collé et droit. Coins carrés (rayon 0), marges
  minimales, en-têtes compacts, tuiles jointives séparées par un trait de 1 px
  (`--color-line`). Les valeurs propres au web (rayon, espacements serrés, hauteurs) sont des
  variables `--web-*` en tête de `app.css` ; `design/tokens.css`, partagé avec le client de
  bureau, n'est pas touché. Thème clair ou sombre selon le système.
- **Appareils** : des blocs par pièce (même règle que le client : `HomePlan::room_of`), puis un
  bloc « Ponts ». La pastille de température est **juste à côté du nom de la pièce**. Les blocs
  circulent avec CSS grid (`auto-fill` et `minmax`), de 320 à 2560 px, et chaque bloc range ses
  tuiles dans sa propre grille (2 colonnes sous 600 px ; au-delà, `minmax(240px, 1fr)` et les ponts
  sur une carte double). Une tuile = une icône, le nom **sur une ligne** (points de suspension, jamais
  coupé mot par mot) et **dessous** une valeur ; seuls « injoignable » (suivi de « depuis HH:MM »,
  d'après `last_seen`) et « pile faible » sont signalés ; « Ouverte » / « Fermée » pour une porte ou
  une fenêtre. Mise à jour en direct par le WebSocket d'événements.
  - Tap sur une **lumière** : allume ou éteint. Tap sur un **volet** : ouvre, ferme, ou arrête
    s'il bouge. Mêmes routes que le client (`POST /api/devices/{id}/on-off` et `…/cover`).
  - Tap sur tout le reste, ou **appui long** sur n'importe quelle tuile : feuille de détail (valeur,
    commandes, courbe 24 h d'un thermomètre, informations techniques repliées).
  - Garde-fous : retour visuel immédiat (la tuile montre l'effet attendu et un contour pendant
    l'envoi), pas de double envoi (un nouveau tap est ignoré tant que la commande n'est pas
    acquittée et pendant 400 ms), l'attente cesse à l'événement attendu, au refus, ou après 5 s.
  - Tout en bas : **Notifications** (alertes sur l'iPhone ou l'iPad même app fermée, catégories,
    appareils abonnés ; voir [Notifications](notifications.md)), **Widgets et raccourcis** (voir
    plus bas) et « Se déconnecter ».
- **Températures** : **tous les capteurs sur un seul graphique** (SVG dessiné à la main, sans
  bibliothèque), une courbe par capteur, chacune avec sa couleur stable (`--color-series-1` à
  `-8`, selon le rang de l'identifiant ; pointillés au-delà de 8). Un choix de durée (**1 h,
  24 h, 1 semaine**) et une **légende** avec une case à cocher par capteur, qui donne la valeur
  actuelle, le minimum et le maximum de la période ; capteur injoignable signalé. Le choix des
  courbes et de la durée est gardé dans le `localStorage` (clé `ostal.temps`, avec
  `try/catch`). Un **curseur** au survol ou au toucher (flèches du clavier aussi) affiche l'heure
  et la valeur de chaque courbe. Mêmes règles que le client de bureau (`temperatures.rs`) :
  lecture `GET /api/devices/{id}/history?…&metrics=temperature&limit=20000&buckets=N` (N entre
  400 et 1600 selon la largeur), une ligne à un point par tranche d'environ 3 px, repères en
  heures rondes locales, courbe coupée sur les vrais trous (au moins 20 min, 2,5 fois l'intervalle
  de rapport de l'appareil, 3 tranches), virgule décimale. Relecture chaque minute (toutes les 10 min sur 1 semaine); en plein écran, seule la panne de la caméra affichée fait sortir.
- **Caméras** : une grille de vignettes (flux `sub`) décodées avec WebCodecs (`VideoDecoder`,
  H.264 AVCC) et dessinées dans un canvas. **Un clic sur une vignette la passe en plein écran**
  (plus de feuille modale) : l'API Fullscreen sur la vignette, flux **principal** (`kind=main`) ;
  en sortant, retour au flux réduit (`sub`). Deux petites icônes SVG en surimpression : plein
  écran (⤢) et quitter (⤡). Échap et le bouton retour du navigateur quittent. Sur iPhone,
  Safari n'offre pas l'API Fullscreen pour un élément qui n'est pas une vidéo : **repli en plein
  écran CSS** (la vignette devient fixe et couvre tout l'écran, avec une entrée d'historique pour
  que le bouton retour la ferme). Limites gardées : 4 vignettes au plus, seulement celles à
  l'écran, tout s'arrête quand la page est cachée (le plein écran est quitté). Sans WebCodecs
  (iOS avant 17), l'app l'écrit clairement. **Non vérifié avec une vraie caméra** : seuls le
  chemin de connexion, le repli CSS et le retour au flux réduit ont été vus (les caméras de démo
  n'ont pas de flux) ; l'API Fullscreen native n'a pas pu être essayée en navigateur sans écran.
  **Image fixe** (ticket n° 12) : une vignette à l'écran qui ne lit pas de flux (caméra sans flux
  réduit, ou au-delà des 4 flux) montre `GET /api/cameras/{id}/snapshot`, rafraîchie toutes les 20 s
  (rien tant que l'onglet est caché). `fetch` avec `Authorization`, `Blob`, `createImageBitmap`,
  dessin dans le canvas : **aucune URL `blob:` n'est chargée, la CSP ne change pas**.
  **Coût côté caméra** : chaque rafraîchissement (toutes les 20 s environ par vignette concernée, au
  plus une capture à la fois par caméra, cache de 15 s partagé) ouvre une **courte session RTSP**
  du serveur vers la caméra : connexion, attente d'une image clé, fermeture après 5 s. Une caméra
  qui a un flux réduit mais dont la vignette dépasse les 4 flux en subit autant. Elle est évitée si la
  caméra est déjà regardée (la capture se joint au flux ouvert) ou testée.
- **Plan** (lecture seule) : `plan.js`. Le plan et les pièces arrivent déjà aux clients : dans le
  `snapshot` du WebSocket (`plan`) puis `plan_changed` (voir [protocole](protocole.md)), donc **aucune
  route de plus**. Les pièces sont des rectangles SVG construits par le DOM (`createElementNS`,
  `textContent` : aucune donnée de la maison dans `innerHTML`, seuls les pictogrammes fixes y passent),
  teintés par la température moyenne de la pièce (mêmes paliers que la pastille de l'onglet
  Appareils) ; leur nom est suivi de cette température. Chaque appareil placé est un pictogramme avec
  sa valeur (« Allumée », « 21,5 °C », « Ouverte »… : le texte de la tuile, `tileOf`), mis à jour par
  les événements du WebSocket ; son nom apparaît quand on zoome. Le pictogramme choisi sur le plan
  (`symbol`) et le réglage porte ou fenêtre (`opening`) sont respectés. Les objets décoratifs sont
  grisés ; une caméra montre son champ (cône ou rectangle) et un tap mène à l'onglet Caméras.
  **Toucher un appareil ouvre la même feuille de détail** que la page Appareils (commandes
  « Allumer », « Éteindre », « Ouvrir »… comprises) ; la feuille reste à jour pendant que le plan est
  affiché. Les textes gardent une taille d'écran fixe ; les pictogrammes grossissent un peu avec le
  zoom (0,65 à l'ajustement, 1 dès 1,6 fois ; cible tactile de 56 px, 36 px à l'ajustement) ; seules
  les pièces suivent le zoom pour de bon. Un tap ouvre la feuille sans que le `click` qui suit ne la
  referme (il est avalé).
  - **Gestes** (pointer events, `touch-action: none` sur le SVG) : un doigt déplace, deux doigts
    pincent (le point sous le milieu des doigts reste sous eux), un tap sans déplacement (moins de
    8 px) ouvre l'appareil. Molette ou pavé tactile : zoom au curseur. Boutons « − » « + » et
    **« Recentrer »** ; au clavier, `+`, `-`, `0` et les flèches quand le plan a le focus.
    L'échelle va de 0,6 à 14 fois l'ajustement et un bout du plan reste toujours à l'écran.
  - **Mise en page** : le plan prend la hauteur libre entre la barre du haut et les onglets
    (`100dvh`, zones sûres comprises) ; un changement de taille (rotation, iPad) réajuste la vue tant
    qu'on ne l'a pas déplacée. Plusieurs étages : un sélecteur dans la barre du plan, le premier étage
    garni d'abord. Même style que les autres pages (coins carrés `--web-radius`, traits `--color-line`,
    jetons de `tokens.css`, thème clair ou sombre).
  - **Plan vide** (aucune pièce ni objet) : « Le plan se dessine dans l'application de bureau ».
  - **Essais** : `node crates/server/web/plan.test.mjs` (géométrie : ajustement, pincement, bornes,
    comme `charts.test.mjs`, à la main) et des tests du serveur (fichier servi avec la CSP stricte,
    service worker, onglet).
- **Mode de la maison** (ticket n° 8) : `mode.js` (libellés, caméra éteinte ou non, sans dépendance). La
  pastille « Mode : … » de la barre du haut suit le `mode` du snapshot et l'événement `mode_changed` ;
  un tap ouvre un petit menu de quatre choix (`PUT /api/mode` avec le jeton de session de la page, la
  pastille ne change qu'à la confirmation du serveur). Une caméra éteinte dans le mode actuel n'ouvre ni
  flux ni image fixe (« Éteinte en mode Nuit »). Essai : `node crates/server/web/mode.test.mjs`. Le
  cache du service worker passe en `v4`.
- Barre d'onglets en bas (style iOS), `safe-area-inset` respecté, thème clair ou sombre selon le
  système. Installable (manifeste + service worker qui garde la coquille ; **jamais** `/api/`).
- Hors périmètre : la **modification** du plan (elle reste dans l'application de bureau) et les automatisations.

## Widgets et raccourcis

Feuille ouverte par « Widgets et raccourcis », en bas de l'onglet Appareils. Elle installe, sur
l'iPhone de **la personne connectée**, ses quatre **raccourcis** (« Je pars », « Je rentre », « Mode
nuit », « Mode vacances ») et son **widget Scriptable** (état de la maison sur l'écran d'accueil)
**sans rien taper** : ni adresse, ni jeton, ni mot de passe. Le serveur prépare chaque fichier avec son
adresse publique et un **jeton à part, au nom de la personne et de ce téléphone** (voir
[Modes](modes.md#les-jetons-de-liphone)). Chacun ne voit et ne révoque que les siens.

1. **Installer mes raccourcis** : `POST /api/tokens/mode/shortcuts` crée le **jeton de mode** de ce
   téléphone (celui du même téléphone est remplacé : la page demande confirmation s'il en existe un ;
   ceux des autres téléphones et des autres personnes ne bougent pas), l'écrit avec l'adresse dans les
   quatre raccourcis, les signe sur le Mac (`/usr/bin/shortcuts sign --mode anyone`), puis affiche les
   boutons à toucher. « Je pars » et « Je rentre » disent où la personne est (et, en attendant la présence
   automatique, passent la maison en Absent et en Présent comme les raccourcis d'avant) ; Nuit et
   Vacances changent le mode de toute la maison, à la main. « Mode absent » et « Mode présent »,
   installés avant, marchent encore. Plus aucune question à l'import.
2. **Installer le widget** : `POST /api/tokens/read/widget` crée le **jeton de lecture** de ce téléphone
   et rend le script `ostal.js` avec `SERVEUR` et `TOKEN` déjà remplis ; la page le copie
   (`ClipboardItem` confié au presse-papiers pendant le geste, comme l'exige Safari ; repli : le texte
   dans une zone sélectionnée). On le colle une fois dans Scriptable.
3. **Révoquer** : un bouton par jeton (`DELETE /api/tokens/{id}` ; un seul jeton : « Révoquer mes
   raccourcis » ou « Révoquer le widget », plusieurs : une ligne par téléphone), avec la date de création
   de chacun (`GET /api/tokens` : mes jetons avec leur libellé, jamais le jeton lui-même).
   `DELETE /api/tokens/mode` et `/read` révoquent tous mes jetons de ce type.

Le **libellé** d'un jeton est le nom de la connexion qui le demande (« iPhone de Hamdi »), ou celui que
la page envoie (`label`, facultatif). **Sur Android**, il n'y a ni Raccourcis ni Scriptable : une note
(« Pas de raccourcis : la présence par le Wi-Fi suffit. ») remplace les deux sections.

Toutes ces routes exigent une **session** : ni le jeton de mode ni le jeton de lecture ne les ouvrent.
**Une préparation à la fois** : une autre demande pendant ce temps (autre onglet, autre personne, fenêtre
du serveur) répond aussitôt `409 busy` (« préparation déjà en cours »), sans faire la queue. Si la requête
est abandonnée pendant la signature (page fermée, délai de 120 s), le nouveau jeton **n'est pas
enregistré** : personne n'aurait reçu les fichiers.

**L'adresse.** La page envoie `location.origin` ; le serveur n'accepte que `https://<nom>.ts.net`, sans
port (400 sinon, avec « Ouvre l'app par son adresse en .ts.net »). L'adresse directe en `:8443` a un
certificat auto-signé que Raccourcis et Scriptable refusent : elle n'est jamais écrite dans un fichier.
Elle doit aussi être **celle de ce Mac** : l'en-tête `Origin` du navigateur doit être le même, et si
`Host` (ou `X-Forwarded-Host`) est un nom en .ts.net, ce doit être ce nom-là. `tailscale serve`
transmet le `Host` d'origine (le contrôle `Origin`/`Host` des WebSocket passe derrière lui, ce qui le
montre) ; un relais qui le réécrirait (`localhost:8443`) laisse l'adresse validée telle quelle. Non
essayé derrière un vrai `tailscale serve` par les tests.

**Les liens des raccourcis.** Un lien de Safari ne peut pas poser d'en-tête `Authorization`, et les
raccourcis portent le jeton de mode. Ils sont donc servis par une adresse **imprévisible** :

| Route | Type | Content-Disposition |
|---|---|---|
| `/app/raccourcis/perso/<128 bits>/Je%20pars.shortcut` | `application/octet-stream` | `attachment; filename="Je pars.shortcut"` |
| `/app/raccourcis/perso/<128 bits>/Je%20rentre.shortcut` | `application/octet-stream` | `attachment; filename="Je rentre.shortcut"` |
| `…/Mode%20nuit.shortcut`, `…/Mode%20vacances.shortcut` | idem | idem |

- Gardés **en mémoire seulement** (dans le `Hub`), au nom de la personne qui les a demandés, valables
  **5 minutes**, remplacés par sa préparation suivante (jamais ceux des autres) et oubliés quand son
  jeton de mode change ou est révoqué, depuis l'app web comme depuis la fenêtre du serveur. Tout cela se fait sous le même verrou que le changement de jeton :
  un lien ne porte jamais un jeton révoqué ; `Cache-Control: no-store`, et le service worker
  n'intercepte rien sous `/app/raccourcis/`. Un autre nom sous le même identifiant : 404.
- **Pas à usage unique**, à dessein : l'app installée sur l'écran d'accueil peut confier le
  téléchargement à Safari, qui redemande le fichier ; un lien mort obligerait à tout recommencer (et
  à changer encore de jeton). Le jeton ne sert qu'à changer le mode, et le lien expire vite.
- `HEAD` répond sans corps.

**Noms sans accent.** Le nom d'un raccourci importé vient du nom du fichier, qu'iOS enregistre en
Unicode **décomposé** (« e » suivi de l'accent) ; un lien `shortcuts://run-shortcut?name=…` écrit avec
un « é » composé ne le retrouve pas (Raccourcis répondait qu'il ne trouvait pas « Mode présent »). Les
noms sont donc en ASCII, les mêmes dans le serveur (`shortcuts.rs`), la page (`widgets.js`) et le
widget (`ostal.js`) ; un test le vérifie.

**La signature.** Elle tourne sur le Mac du serveur, dans un dossier temporaire privé (0700, supprimé
quoi qu'il arrive : le plist d'entrée porte le jeton ; ceux de plus de 10 minutes restés d'un arrêt
brutal sont supprimés au démarrage), 25 s au plus par raccourci. Elle demande une
session ouverte et un compte iCloud connecté ; sinon : 503 avec un message clair, et **rien ne
change** (le nouveau jeton n'est enregistré qu'une fois les deux signatures réussies). Les raccourcis
sont fabriqués en Rust (plist XML, portage de `mode_shortcut` de `build_shortcuts.py`) : `PUT
<adresse>/api/presence` (« Je pars » : `away`, « Je rentre » : `home`) ou `PUT <adresse>/api/mode`
(Nuit, Vacances), `Authorization: Bearer <jeton de mode>`, corps JSON ; notification de réussite
seulement si la réponse a la clé attendue (`state` ou `mode`), sinon la réponse du serveur. Essai à la main de la vraie
signature (valeurs factices) : `cargo test -p ostal-server --lib -- --ignored
real_apple_signature`. Les autres essais remplacent la signature par le plist tel quel
(`OSTAL_TEST_UNSIGNED_SHORTCUTS=1` pour le binaire) : ils ne dépendent ni d'iCloud ni d'Apple.

**Le widget.** `ostal.js` n'a plus de connexion par mot de passe ni de jeton de session dans le
trousseau : il lit `GET /api/state` et `GET /api/events?limit=1` avec le jeton de lecture écrit dedans.
Un 401 affiche « Jeton révoqué : recopie le widget depuis l'app web ». Ses boutons ouvrent les
raccourcis par leur nom (`shortcuts://run-shortcut?name=Mode%20present`). L'en-tête est posé d'un
bloc (`req.headers = headers`) : Scriptable ignore une clé ajoutée après coup.

- **Non vérifié sur un vrai iPhone** : l'import des raccourcis personnalisés, le téléchargement depuis
  l'app installée sur l'écran d'accueil (la page invite à ouvrir Safari si rien ne se passe), la copie
  du script après la réponse du serveur, et le lancement par nom sans accent depuis le widget.
- Logique sans navigateur : `widgets.js` (essai : `node --test crates/server/web/widgets.test.mjs`).

## Fichiers

Sources : `crates/server/web/` (`index.html`, `app.css`, `app.js` et ses modules `api.js`,
`tiles.js`, `icons.js`, `charts.js`, `temps.js`, `cameras.js`, `plan.js`, `mode.js`, `push.js`, `widgets.js`, `sw.js`, `manifest.webmanifest`, icônes PNG
192 et 512, et `raccourcis/ostal.js`, le modèle du script du widget, rempli par le serveur). Les **tokens** ne sont pas recopiés : `design/tokens.css` est intégré tel quel
(`/app/tokens.css`), et un test vérifie que le fichier servi est celui du dépôt.

## Sécurité

- `/app/` se sert **sans jeton** : c'est la coquille, sans aucune donnée de la maison. Tout le
  reste passe par l'API, qui exige le jeton comme avant.
- En-têtes de la coquille : `Content-Security-Policy` stricte (`default-src 'self'`, aucun script
  ni style en ligne, `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, Les réponses de `/api/` portent `default-src 'none'; frame-ancestors 'none'`. `Cross-Origin-Resource-Policy:
  same-origin`, `Cache-Control: no-store`. **Pas de CORS** : l'app est de la même origine que l'API.
- **Connexion** : mêmes identifiants et même route que le client de bureau (`POST /api/login`,
  mêmes limites d'essais, argon2). Le **mot de passe n'est jamais gardé**. Le **jeton** (révocable,
  expire côté serveur) est gardé dans le `localStorage` de l'origine de l'app, l'équivalent de
  `connexion.json` du client ; la CSP sans script en ligne en réduit l'exposition. « Se
  déconnecter » appelle `POST /api/logout` (jeton révoqué) puis l'efface.
- **WebSocket d'un navigateur** : un `WebSocket` ne peut pas poser d'en-tête `Authorization`, et le
  jeton ne va jamais dans l'URL. L'app propose donc
  `Sec-WebSocket-Protocol: ostal.v1, bearer.<jeton>`. Le serveur ne lit cette entrée que sur
  `GET /api/ws` et `GET /api/cameras/{id}/stream` avec un vrai upgrade (`Connection: upgrade`, `Sec-WebSocket-Key`, version 13), jamais ailleurs ; un `Origin` qui ne correspond pas à `Host` est refusé (403), vérifie le jeton comme
  d'habitude, et **répond toujours `ostal.v1`** (jamais l'entrée qui porte le jeton). Le
  client natif, qui envoie `Authorization`, n'est pas touché ; `PROTOCOL_VERSION` reste **2**. Même
  mécanisme pour `GET /api/cameras/{id}/stream`.
- **Certificat** : le client de bureau épingle l'empreinte SHA-256 du certificat auto-signé. Un
  navigateur ne peut pas épingler : il applique sa propre confiance. Avec le certificat
  auto-signé, Safari refuse (ou fait cliquer une exception, sans PWA installable ni WebSocket
  fiable). Pour l'iPhone, il faut un **certificat reconnu** : celui de Tailscale (`*.ts.net`),
  ticket n° 14. Les essais en local utilisent Chrome avec `--ignore-certificate-errors` vers
  `127.0.0.1` seulement.

## Accès à distance depuis l'app serveur (Tailscale)

Le bouton **« Activer l'accès à distance »** de la fenêtre de Ostal Server (onglet
Connexion, « Depuis l'iPhone, même hors de la maison ») publie le serveur sur le tailnet, sans
Terminal. Le menu n'en montre plus que l'état (« Accès à distance : activé »). **Activer** lance `tailscale serve --bg https+insecure://localhost:<port>`
(le port est celui du serveur), **désactiver** lance `tailscale serve --https=443 off`. Seule la
publication privée au tailnet existe : la publication sur tout Internet (`funnel`) n'est jamais
lancée, et un test le garantit. L'état est porté par Tailscale (`serve --bg` est persistant) :
l'app ne range rien.

- **Binaire** : `/Applications/Tailscale.app/Contents/MacOS/Tailscale`, puis
  `/opt/homebrew/bin/tailscale`, `/usr/local/bin/tailscale`, puis le `PATH` (chemins absolus
  seulement). Lancé directement, arguments fixes, jamais à travers un shell.
- **État** : relu à l'ouverture et à la fermeture du menu, à l'ouverture de l'onglet Connexion et
  au bouton « C'est fait, vérifier », dans un fil à part (3 s au plus) :
  `tailscale status --json` (`BackendState`, `Self.DNSName`) puis `tailscale serve status --json`.
  Le menu n'est jamais bloqué.
- **Étapes** : la fenêtre dit où on en est. Tailscale non installé ou déconnecté : étape 1 à faire.
  Autorisation manquante (réponse « Serve is not enabled ») : un bouton ouvre le lien fourni par
  Tailscale. Un autre `serve` sur le 443 : c'est dit, il n'est jamais écrasé ni retiré.
- **Funnel** : si `AllowFunnel` est vrai sur le 443, la fenêtre et le menu le signalent (« ce Mac
  est ouvert à tout Internet par Tailscale ») et l'app n'active ni ne coupe rien. Une session `Foreground`
  ou un `Services` sur le 443 compte comme « port déjà utilisé ». `null` ou une sortie vide valent
  « rien de configuré ».
- **Lancement** : `TAILSCALE_BE_CLI=true` (le binaire de l'app devine sinon entre interface et
  ligne de commande), son propre groupe de processus (un délai dépassé tue tout le groupe) ;
  `status --json --peers=false`. Le lien d'autorisation est la première adresse `https` dont
  l'hôte est exactement `login.tailscale.com` dans la sortie d'un `serve --bg` en échec ; l'app
  n'ouvre jamais d'autre hôte que celui-là ou un nom `.ts.net`. La fixture de cette sortie est
  plausible, reconstituée, non capturée.
- **Actif** : la case est cochée et deux éléments apparaissent, « Ouvrir https://<nom>.ts.net/app/ »
  et « Copier l'adresse ».
- **Essais** : `OSTAL_TAILSCALE_BIN=/chemin/absolu/faux-tailscale` remplace tout le reste
  (aucune recherche ailleurs) ; elle n'est lue qu'en `--demo` ou dans un build de développement. En `--demo`, sans cette variable, Tailscale est vu comme absent :
  le menu d'un essai ne lance jamais le vrai `tailscale`. Les tests unitaires s'appuient sur des
  fixtures JSON (`crates/server-app/tests/fixtures/tailscale/`) et ne lancent jamais le vrai binaire.

## Essayer

`ostal-server --demo --listen 127.0.0.1:18443 --data-dir <dossier jetable>`, puis ouvrir
`https://127.0.0.1:18443/app/` ; le mot de passe du compte `demo` est affiché une fois au premier
lancement. `OSTAL_DEMO_HOUSE=1` donne une maison plus riche.
