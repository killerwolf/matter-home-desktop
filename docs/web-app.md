# App web mobile (PWA)

[← Retour au README](../README.md) · [Protocole](protocole.md) · [Design system](design-system.md)

Ticket n° 15. Le serveur sert lui-même une app web pensée pour le mobile (iPhone, iPad, et aussi
un ordinateur) sur **`https://<serveur>:<port>/app/`**. Les fichiers sont **intégrés au binaire**
(`include_str!`, `include_bytes!`) : pas de dossier à déployer, pas d'étape de build JavaScript,
pas de framework, pas de nouvelle dépendance Rust.

## Ce qu'elle fait

- **Appareils** : des tuiles rangées par pièce (même règle que le client : `HomePlan::room_of`),
  2 colonnes sur un iPhone, davantage sur tablette ou ordinateur, avec la pastille de température
  de la pièce. Une tuile = une icône, le nom, **une** valeur ; seuls « injoignable » et « pile
  faible » sont signalés ; « Ouverte » / « Fermée » pour une porte ou une fenêtre. Mise à jour en
  direct par le WebSocket d'événements.
  - Tap sur une **lumière** : allume ou éteint. Tap sur un **volet** : ouvre, ferme, ou arrête
    s'il bouge. Mêmes routes que le client (`POST /api/devices/{id}/on-off` et `…/cover`).
  - Tap sur tout le reste, ou **appui long** sur n'importe quelle tuile : feuille de détail (valeur,
    commandes, courbe 24 h d'un thermomètre, informations techniques repliées).
  - Garde-fous : retour visuel immédiat (la tuile montre l'effet attendu et un contour pendant
    l'envoi), pas de double envoi (un nouveau tap est ignoré tant que la commande n'est pas
    acquittée et pendant 400 ms), l'attente cesse à l'événement attendu, au refus, ou après 5 s.
- **Températures** : une courbe par pièce sur 24 h (SVG dessiné à la main), avec la valeur
  actuelle, le minimum et le maximum. Un trou de données reste un trou.
- **Caméras** : une grille de vignettes (flux `sub`) et le grand format (flux `main`), décodés avec
  WebCodecs (`VideoDecoder`, H.264 AVCC) et dessinés dans un canvas. Sans WebCodecs (iOS avant
  17), l'app l'écrit clairement. **Non vérifié avec une vraie caméra** : seul le chemin de
  connexion et de reprise a été vu (les caméras de démo n'ont pas de flux).
- Barre d'onglets en bas (style iOS), `safe-area-inset` respecté, thème clair ou sombre selon le
  système. Installable (manifeste + service worker qui garde la coquille ; **jamais** `/api/`).
- Hors périmètre : le plan et les automatisations.

## Fichiers

Sources : `crates/server/web/` (`index.html`, `app.css`, `app.js` et ses modules `api.js`,
`tiles.js`, `icons.js`, `charts.js`, `cameras.js`, `sw.js`, `manifest.webmanifest`, icônes PNG
192 et 512). Les **tokens** ne sont pas recopiés : `design/tokens.css` est intégré tel quel
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
  `Sec-WebSocket-Protocol: matter-home.v1, bearer.<jeton>`. Le serveur ne lit cette entrée que sur
  `GET /api/ws` et `GET /api/cameras/{id}/stream` avec un vrai upgrade (`Connection: upgrade`, `Sec-WebSocket-Key`, version 13), jamais ailleurs ; un `Origin` qui ne correspond pas à `Host` est refusé (403), vérifie le jeton comme
  d'habitude, et **répond toujours `matter-home.v1`** (jamais l'entrée qui porte le jeton). Le
  client natif, qui envoie `Authorization`, n'est pas touché ; `PROTOCOL_VERSION` reste **2**. Même
  mécanisme pour `GET /api/cameras/{id}/stream`.
- **Certificat** : le client de bureau épingle l'empreinte SHA-256 du certificat auto-signé. Un
  navigateur ne peut pas épingler : il applique sa propre confiance. Avec le certificat
  auto-signé, Safari refuse (ou fait cliquer une exception, sans PWA installable ni WebSocket
  fiable). Pour l'iPhone, il faut un **certificat reconnu** : celui de Tailscale (`*.ts.net`),
  ticket n° 14. Les essais en local utilisent Chrome avec `--ignore-certificate-errors` vers
  `127.0.0.1` seulement.

## Accès web depuis le menu de l'app serveur (Tailscale)

La case **« Accès web (Tailscale) »** du menu de Matter Home Server publie le serveur sur le
tailnet, sans Terminal. **Activer** lance `tailscale serve --bg https+insecure://localhost:<port>`
(le port est celui du serveur), **désactiver** lance `tailscale serve --https=443 off`. Seule la
publication privée au tailnet existe : la publication sur tout Internet (`funnel`) n'est jamais
lancée, et un test le garantit. L'état est porté par Tailscale (`serve --bg` est persistant) :
l'app ne range rien.

- **Binaire** : `/Applications/Tailscale.app/Contents/MacOS/Tailscale`, puis
  `/opt/homebrew/bin/tailscale`, `/usr/local/bin/tailscale`, puis le `PATH` (chemins absolus
  seulement). Lancé directement, arguments fixes, jamais à travers un shell.
- **État** : relu à l'ouverture et à la fermeture du menu, dans un fil à part (3 s au plus) :
  `tailscale status --json` (`BackendState`, `Self.DNSName`) puis `tailscale serve status --json`.
  Le menu n'est jamais bloqué ; un changement de structure (les deux lignes de l'accès actif)
  attend la fermeture du menu, comme pour les appareils injoignables.
- **Libellés** : « Accès web : Tailscale non installé » et « Accès web : Tailscale déconnecté »
  (case grisée) ; « Accès web : à autoriser dans Tailscale » avec un élément qui ouvre le lien
  fourni par la réponse « Serve is not enabled » ; « Accès web : port 443 déjà utilisé par
  Tailscale » (case grisée) quand un autre `serve` occupe le 443 : il n'est jamais écrasé ni retiré.
- **Funnel** : si `AllowFunnel` est vrai sur le 443, la case est grisée (« Accès web : Funnel actif
  (public), à couper dans Tailscale ») et l'app n'active ni ne coupe rien. Une session `Foreground`
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
- **Essais** : `MATTER_HOME_TAILSCALE_BIN=/chemin/absolu/faux-tailscale` remplace tout le reste
  (aucune recherche ailleurs) ; elle n'est lue qu'en `--demo` ou dans un build de développement. En `--demo`, sans cette variable, Tailscale est vu comme absent :
  le menu d'un essai ne lance jamais le vrai `tailscale`. Les tests unitaires s'appuient sur des
  fixtures JSON (`crates/server-app/tests/fixtures/tailscale/`) et ne lancent jamais le vrai binaire.

## Essayer

`matter-home-server --demo --listen 127.0.0.1:18443 --data-dir <dossier jetable>`, puis ouvrir
`https://127.0.0.1:18443/app/` ; le mot de passe du compte `demo` est affiché une fois au premier
lancement. `MATTER_HOME_DEMO_HOUSE=1` donne une maison plus riche.
