# Journal des modifications

Les changements notables de chaque version sont décrits ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le projet
respecte le [versionnage sémantique](https://semver.org/lang/fr/).

Les téléchargements de chaque version sont sur la
[page des versions](https://github.com/killerwolf/matter-home-desktop/releases).

## [Unreleased]

### Ajouté

- **Accès à distance via Tailscale** (ticket n° 14). Le client de bureau accepte **plusieurs
  adresses par serveur** (champ « Adresse à distance » de l'écran de connexion, `alt_urls` dans
  `connexion.json`, `alt_urls` écrit après une connexion réussie) : il prend la première qui
  répond (empreinte du serveur, ou certificat public pour `https://nom.ts.net/`), la maison d'abord, et bascule seul à la reconnexion. « à distance » est indiqué
  dans la barre d'état ; les caméras restent sur le flux réduit par défaut, avec un rappel si
  « Haute » est choisie. **Certificats** : l'épinglage par empreinte ignore le nom (accès direct
  `nom.ts.net:8443`) ; une adresse `https://nom.ts.net/` (port 443, `tailscale serve`) est
  validée par la chaîne de confiance publique (nouvelle dépendance `webpki-roots`), sans
  épinglage (vérifiée par le nom, pas par l'empreinte du serveur) ; tout autre certificat
  invalide reste refusé. README : section « Accès à
  distance », et [docs/acces-distant.md](docs/acces-distant.md).
- **App web : refonte du style et des pages** (ticket n° 15). Style « tableau de bord » : coins
  carrés, marges minimales, tuiles jointives séparées par un trait de 1 px (variables `--web-*`
  dans `app.css`, `design/tokens.css` inchangé). **Appareils** : la pastille de température est
  à côté du nom de la pièce, et les pièces circulent en blocs (CSS grid) de 320 à 2560 px.
  **Températures** : tous les capteurs sur un seul graphique SVG (une couleur stable par
  capteur), durée 1 h / 24 h / 1 semaine, légende avec case à cocher (mémorisée), valeur
  actuelle, minimum et maximum, et curseur au survol ou au toucher. **Caméras** : plus de feuille
  modale ; un clic passe la vignette en plein écran (flux principal, retour au flux réduit en
  sortant), avec repli en plein écran CSS sur iPhone.
- **Accès web par Tailscale depuis le menu de Matter Home Server** : une case « Accès web
  (Tailscale) » active ou coupe `tailscale serve` pour le port du serveur, sans Terminal (jamais
  `funnel`). Elle se grise avec la raison quand Tailscale n'est pas installé ou déconnecté, ou
  quand un autre `serve` occupe déjà le port 443 (il n'est jamais écrasé) ; si Serve n'est pas
  autorisé sur le tailnet, un élément ouvre le lien d'autorisation. Actif, le menu propose
  d'ouvrir ou de copier `https://<nom>.ts.net/app/`. Voir [docs/web-app.md](docs/web-app.md).
- **Version dans le menu de Matter Home Server** : une ligne « Matter Home Server 1.0.0-rc.7 » en
  haut du menu. Le nom complet vient de `MATTER_HOME_VERSION`, posée par la CI sur un tag ; un
  build local affiche « 1.0.0 (dev) ».
- **Client de bureau : l'adresse du serveur dans la barre du haut** (« ● mac.ts.net:8443 · 1.0.0 »),
  tronquée par « … » si elle est longue (infobulle avec l'adresse complète). Un clic ouvre un petit
  menu : « Copier l'adresse » et « Se déconnecter / changer de serveur ».
- **App web mobile (PWA)** (ticket n° 15). Le serveur sert `/app/` : appareils en tuiles par pièce
  (tap sur une lumière ou un volet, appui long pour le détail), courbes de température sur 24 h,
  caméras en direct (WebCodecs), barre d'onglets iOS, thème clair ou sombre, installable. Fichiers
  intégrés au binaire, sans framework ni build ; couleurs et tailles de `design/tokens.css`. La
  coquille se sert sans jeton, avec une CSP stricte et sans CORS ; le WebSocket d'un navigateur
  s'authentifie par sous-protocole (`bearer.<jeton>`), jamais par l'URL. Le protocole reste en
  version 2 et le client de bureau ne change pas. Voir [docs/web-app.md](docs/web-app.md).
- **Caméras RTSP, 1/5 : les ajouter depuis le serveur** (ticket n° 4). Le menu de **Matter Home
  Server** gagne **« Caméras… »** : une fenêtre pour ajouter, modifier et supprimer des caméras
  (nom, adresse du flux principal, adresse du flux réduit facultative, identifiant, mot de passe)
  et un bouton **Tester**, qui se connecte à la caméra (un `DESCRIBE`, jamais de flux) et dit le
  codec et la résolution, ou une erreur compréhensible (caméra éteinte, mot de passe refusé,
  mauvaise adresse de flux). On peut coller `rtsp://identifiant:mot-de-passe@adresse/…` : les
  identifiants sont rangés dans leurs champs. Le serveur vérifie ensuite chaque caméra environ une
  fois par minute, sans garder de connexion ouverte, et le client voit pour chacune « joignable »
  ou « injoignable ». Une caméra qui ne répond pas n'a aucun effet sur les appareils Matter.
  Bibliothèque RTSP : [`retina`](https://docs.rs/retina) (Rust pur, MIT ou Apache-2.0).
- **Le mot de passe d'une caméra ne quitte jamais le serveur** : le client ne reçoit que
  l'identifiant, le nom, l'état et le codec, jamais l'adresse RTSP ni le mot de passe (`GET
  /api/cameras`, l'état complet et l'événement `cameras_changed`), et aucune route de l'API ne
  les modifie. Ils sont dans `cameras.json` (droits 0600, écriture atomique), masqués dans
  `Debug`, jamais dans les journaux. Ils sont **inclus en clair dans la sauvegarde `.matterhome`**,
  comme les clés de la maison : la fenêtre « Caméras… » et le message d'export du client le disent.
  L'export puis l'import conservent les caméras. Détails dans `docs/protocole.md`.
- Mode démo : trois caméras fictives (une injoignable, une sans flux réduit) pour essayer la
  fenêtre et les clients sans caméra ni trafic réseau.
- **Portes et fenêtres sur le plan** : un capteur d'ouverture s'affiche comme une porte ou une
  fenêtre, **ouverte ou fermée** (porte fermée, porte entrouverte avec son arc, fenêtre à
  croisillon, fenêtre aux battants ouverts), dessinée sur le plan. Seul l'état ouvert ressort (disque
  orange) ; sans mesure, une icône neutre. Matter ne distingue pas une porte d'une fenêtre : le
  réglage **« Porte / Fenêtre »** est dans l'inspecteur du plan (porte par défaut ; une fenêtre déjà
  choisie par son pictogramme le reste). Les plans existants s'ouvrent tels quels.
- **« Hors ligne depuis 14:02 »** : une caméra injoignable dit depuis quand, sur sa vignette, dans la
  vue en grand, au survol du plan et dans le lecteur (« depuis hier 22:15 », ou la date si c'est plus
  ancien). Le serveur note l'heure quand la caméra devient injoignable (après 2 échecs d'affilée) et
  la garde jusqu'au retour en ligne.
- **Caméras, 2/5 : le direct dans le client** (ticket n° 5). Le serveur relaie le H.264 de la
  caméra **sans le transcoder**. Une seule connexion à la caméra sert tous les spectateurs **d'un
  même flux** (la session est partagée par caméra et par type de flux, réduit ou principal) ; elle
  se ferme 5 s après le départ du dernier. Le client décode avec VideoToolbox. Le lecteur propose
  le flux réduit ou le flux principal, des messages d'erreur en français et une reprise
  automatique. Les journaux de `retina` sont coupés par défaut, parce qu'ils écrivaient les
  adresses des caméras.
- **Caméras, 3/5 : sur le plan** (ticket n° 6). Les caméras se placent et s'orientent sur le plan.
  Chacune montre son champ filmé en **cône ou en rectangle**, avec un angle et une portée
  réglables ; la rotation est enregistrée avec le plan. Le plan indique l'état de chaque caméra
  (en ligne, injoignable, inconnu), et un clic ouvre le direct dans une fenêtre. Les plans
  enregistrés avant cette version s'ouvrent tels quels : rotation 0, angle 90° et portée 3 m.
- **Caméras, 4/5 : la page Caméras** (ticket n° 7). Un nouvel onglet liste toutes les caméras par
  pièce, avec leur état et un aperçu sur le flux réduit. Un clic ouvre la caméra en grand, avec la
  possibilité de passer au flux principal. Au plus **4 caméras** lisent en même temps (une Aqara
  G100 ne tolère qu'environ deux canaux RTSP) ; les autres montrent une image d'attente. Fermer la
  fenêtre du plan ou quitter la page coupe le flux au bout de quelques secondes, et deux endroits
  qui montrent la même caméra sur le même flux partagent une seule session côté serveur (une par
  caméra et par flux). La mosaïque ne lit jamais le flux principal ; après une reconnexion, les
  lecteurs repartent d'eux-mêmes.

- **Onglet Températures** (ticket n° 3) : toutes les températures sur **un seul graphique**, une
  courbe de couleur par thermomètre, nommée d'après l'appareil et sa pièce sur le plan, pour
  comparer les pièces et vérifier le réglage de la chaudière. Période au choix : **1 h, 24 h ou
  1 semaine**. Légende cliquable (masquer ou afficher une courbe, avec la température actuelle),
  et au survol l'heure et la valeur de chaque courbe à cet instant. Les courbes sont coupées là où
  il manque des mesures (capteur injoignable, piles vides), jamais reliées à travers un trou. Les
  nouvelles mesures s'ajoutent en direct. Option `--temperatures` (avec `--period 1h|24h|1w`) pour ouvrir directement la page.
  La fiche d'un thermomètre gagne aussi la fenêtre « 24 h ».
- **Historique réduit par tranches** : `GET /api/devices/{id}/history` accepte `buckets=N` (au plus
  2 000). La fenêtre est découpée en N tranches et chaque grandeur garde le **minimum et le
  maximum** de chaque tranche, avant la limite de points. Une semaine d'un capteur qui rapporte
  toutes les 10 s (environ 60 000 mesures) tient ainsi en moins de 1 600 points, sans perdre les
  pointes de chauffe, au lieu de ne montrer que les derniers jours. **Compatible** : sans
  `buckets` la réponse est inchangée ; un serveur rc.4 ignore le paramètre (le client réduit alors
  lui-même et signale que la période peut être incomplète) et un client rc.4 ne l'envoie pas.
  Détails dans `docs/protocole.md`.
- Mode démo : une semaine de températures (salon, chambre avec une panne de 3 heures) est déjà
  là au démarrage, pour la page Températures.
- **Matter Home Server**, le serveur en application de la barre de menus (`crates/server-app`) :
  un DMG à glisser dans Applications, une icône en haut de l'écran (sans icône dans le Dock), toute
  la configuration dans une petite fenêtre, **jamais de Terminal ni de mot de passe
  administrateur**. Le serveur tourne dans le processus de l'application. Le menu montre l'état
  (« En marche · 12 appareils sur 13 en ligne »), les appareils injoignables, l'adresse du serveur
  et l'empreinte du certificat à copier, le journal, « Ouvrir à la connexion » et « Quitter »
  (avec confirmation). Fenêtre de première configuration : reprise d'une sauvegarde `.matterhome`
  (résumé, comparaison des identités, confirmation) ou nouvelle maison, puis compte ; mêmes gardes
  que `import`, `init` et `create-account`, désormais partagées dans la bibliothèque du serveur
  (`setup`). Démarrage direct quand l'identité et le compte existent déjà.
- **Cohabitation** : l'application ne démarre pas le serveur et l'explique si un autre serveur,
  le service launchd installé ou la v0.2.x de l'application de bureau tient les données ; une seule
  instance de l'application. Le verrou d'instance contient maintenant le numéro du processus.
- **Ouverture à la connexion** sans droits administrateur (LaunchAgent d'utilisateur, relance
  après plantage, pas de boucle), et **veille** empêchée tant que le serveur tourne
  (`caffeinate`) ; le capot fermé reste hors de portée.
- `scripts/bundle-server-app.sh` : `.app` universelle signée « ad hoc » (macOS 10.15 / 11.0
  minimum) et `MatterHomeServer-<version>-universal.dmg`, construits et publiés par la CI.
  L'archive en ligne de commande reste, comme option avancée.
- **Étiquette de fabric « Matter Home Serveur »** : le serveur l'écrit sur chaque appareil
  (commande UpdateFabricLabel), pour qu'Apple Maison l'affiche dans « Services connectés » à côté du
  numéro de fabricant « 101 ». Sans réappairage ni action : appareils déjà appairés et nouveaux,
  à la première connexion. Purement cosmétique : elle ne démarre qu'une fois la souscription
  établie (jamais en interrogation périodique ni sans souscription), dans la même tâche que la
  session (donc arrêtée avec elle, y compris quand l'appareil est oublié) ; une panique dans ce
  travail est journalisée sans toucher à la session ; elle n'influe ni sur l'état de l'appareil ni
  sur sa souscription ni sur un appairage. Les appareils sur pile sont ménagés : lecture de
  l'étiquette d'abord, écriture seulement si elle diffère (jamais si la lecture échoue), et plus
  aucun essai tant que le serveur tourne une fois l'étiquette posée ou refusée par l'appareil (tout
  statut IM, BUSY compris, vaut refus définitif : moins de trafic) ; entrée introuvable, lecture
  en échec, réponse illisible et échecs de transport sont retentés, 3 essais au total par appareil.

### Corrigé

- **Matter Home Server : le menu de la barre de menus ne répondait plus une fois la fenêtre
  fermée.** « Quitter », « Ouvrir le journal », « Compte et réglages… », les copies et
  « Ouvrir à la connexion » ne faisaient rien, et l'icône, l'infobulle et la ligne d'état ne se
  mettaient plus à jour : eframe 0.37 ne lance plus `ui()` pour une fenêtre cachée, alors que tout
  ce travail s'y faisait. Il se fait désormais dans `App::logic`, que eframe appelle aussi fenêtre
  cachée (réveil par les clics du menu, puis toutes les 2 s). La confirmation « Quitter » s'ouvre
  hors de ce rappel, la fenêtre s'ouvre avec son contenu, et ne reste plus à l'écran, noire et vide, au lancement
  (eframe l'y montrait après sa première image : elle est recachée tout de suite, sans boucler).

### Modifié

- **Refonte de l'interface du client, phase 1** (ticket n° 13). Nouveau design system :
  `design/tokens.json` (couleurs claires et sombres, échelle de température de 16 à 24 °C par
  paliers de 2 °C, espacements, rayons, tailles de texte) est lu par le client et le sera par la
  future app web ; `docs/design-system.md` l'explique. Le client suit le thème clair ou sombre du
  système. Une seule navigation : les onglets du haut, **Plan en premier** ; la colonne d'appairage
  permanente disparaît au profit d'un bouton **« + Ajouter »** qui ouvre l'appairage dans une
  fenêtre. L'onglet **Appareils** est en pleine largeur : une ligne par appareil (un nom, une
  valeur), rangée par pièce, le pont à part ; seuls « pile faible » et « hors ligne » sont signalés.
  Le détail (état, pile, type, nœud, dernière communication, fabricant, commandes, courbes)
  s'ouvre dans un panneau à droite au clic, jamais tout seul. Un appareil injoignable montre
  « — » et le mot « injoignable » (le même sur le plan). Rien n'est supprimé. Les autres onglets
  changent seulement de thème. `design/tokens.css` (généré, vérifié par un test) prépare l'app
  web ; `HomePlan::room_of` est la règle unique de la pièce d'un appareil.
- **Refonte de l'interface du client, phase 2** (ticket n° 13) : **Plan** et **Températures**.
  Sur le plan, chaque pièce est teintée selon la température moyenne de ses capteurs (paliers de
  2 °C, bleu à 16 °C, rouge à 24 °C), avec la valeur écrite ; un capteur injoignable ne compte pas,
  une pièce sans capteur reste grise. Caméras et volets sont plus discrets, le champ d'une caméra
  n'apparaît qu'au survol, à la sélection ou en mode Modifier. Un clic sur un appareil ouvre, à
  droite du plan, le même panneau de détail que l'onglet Appareils. Le dessin et le mode Modifier ne
  changent pas ; les couleurs du plan suivent les tokens (clair et sombre). Températures : le
  graphique prend tout l'écran, l'en-tête donne pour chaque pièce la valeur actuelle en grand avec
  le minimum et le maximum de la période (calculés par le client), des heures rondes sur l'axe, peu
  de lignes de grille, des noms de pièce courts et des couleurs de courbe issues des tokens. Un
  volet sans position n'a plus de valeur vide dans la liste (« — »). Options d'essai :
  `--edit-plan`, et `--device` combinable avec `--plan`. Le plan se recadre quand la largeur change
  (panneau de détail, fenêtre), une légende de l'échelle de température est sous le plan, les
  petites étiquettes sont sur une pastille lisible (contraste testé), les heures rondes suivent le
  changement d'heure et l'axe vertical a un repère tous les 1 ou 2 °C.
- **Écran Appareils en tuiles** : à la place d'une ligne par appareil (nom à gauche, valeur tout à
  droite sur une fenêtre large), une grille de cartes par pièce, d'environ 180 px, avec une grande
  icône d'état (ampoule allumée ou éteinte, prise, porte ou fenêtre ouverte ou fermée, volet et son
  pourcentage, thermomètre, télécommande), le nom et la valeur en grand. L'en-tête de pièce porte
  l'étage et la température moyenne, teintée comme sur le plan. Les pièces se rangent côte à côte
  (blocs de 3 ou 4 colonnes au plus). Un clic **allume ou éteint** une lumière et **ouvre, ferme ou
  arrête** un volet, avec un retour immédiat (un refus remet l'affichage et passe par le message
  d'erreur habituel ; un second clic trop proche ou pendant une commande est ignoré) ; pour le
  reste, prise comprise, il ouvre le détail, comme les trois points (au survol ou au focus) et le
  clic droit. Une pile faible remplace l'humidité sur la carte ; la fenêtre signale un appareil
  injoignable par un bandeau et défile quand l'écran est bas ; Échap annule d'abord la saisie du
  nom. Une carte injoignable est estompée, une pile faible
  a un petit point orange. Le détail devient une **fenêtre centrée** (Échap, « Fermer » ou un clic à
  côté la ferment), à la place du panneau latéral : en-tête avec la commande principale, la courbe
  seulement si l'appareil en a une, les actions, puis « Infos techniques » repliées. Le plan ouvre la
  même fenêtre et garde toute sa largeur. Nouveaux tokens `tile`. Le mode démo du serveur gagne,
  avec `MATTER_HOME_DEMO_HOUSE=1` seulement, des lampes, une télécommande et un capteur injoignable
  (pour les captures, lampes commandables) ; `MATTER_HOME_WINDOW=LxH` (800 à 8000) règle la taille de la fenêtre du client.
- **Refonte de l'interface du client, phase 3** (ticket n° 13) : **Caméras, Connexion,
  Automatisations**. Caméras : une grille de vignettes qui remplit la largeur (image, nom, pièce en
  légende ; seul « injoignable » est signalé, avec « Hors ligne depuis … ») ; en grand, une seule
  ligne d'en-tête (retour, nom, pièce) avec le réglage **Fluide / Haute** à la place de « flux
  réduit / principal », l'image sur toute la largeur et le codec et la résolution dans une barre
  d'état. Le lecteur ne change pas. Connexion : un formulaire centré, la dernière adresse
  préremplie, une aide qui dit où la trouver (menu du serveur, « Connecter un client »), les
  erreurs sous le champ concerné et une note de sécurité d'une ligne avec « Détails ».
  Automatisations : chaque règle se lit comme une phrase, avec un interrupteur et sa dernière
  exécution sur la ligne ; vocabulaire « Seulement si », « Essayer maintenant », « Appeler une
  adresse web » ; l'historique s'ouvre depuis la barre d'état. Une porte ou une fenêtre est
  « Ouverte » ou « Fermée » partout (liste et plan). Températures : la ligne a la même épaisseur
  sur toute la période (une moyenne par tranche de trois pixels, au lieu du minimum et du maximum
  qui faisaient des barres épaisses là où les mesures sont serrées et un trait fin ailleurs ; le
  minimum et le maximum affichés restent exacts, et un vrai trou de mesures reste un trou).
  Options d'essai : `--cameras`, `--camera X`. Corrections de revue : la ligne n'est plus coupée
  en morceaux sur une semaine (elle ne l'est que sur une vraie plage sans mesure) et une bande
  pâle du minimum au maximum garde les pics visibles ; l'interrupteur d'une règle bascule
  vraiment (le clic de la ligne ne le recouvre plus) ; une caméra hors ligne est signalée en
  orange et son nom grisé ; un seul champ d'heure « 22:00 » validé ; les règles disent leurs
  conditions et leur heure dans la phrase ; l'erreur de connexion est placée d'après sa nature.
- **Protocole version 2** : l'état complet gagne `cameras` et le WebSocket l'événement
  `cameras_changed` (une variante de plus : les énumérations `serde` refusent les variantes
  inconnues, un client de version 1 n'est donc plus accepté). Client et serveur sortent ensemble.
  Les nouveaux appareils derrière un pont ne sont plus tirés dans le dernier huitième de leur plage,
  où le serveur tire les caméras (les identifiants déjà attribués restent valables, et une caméra ne
  prend jamais l'identifiant d'un appareil connu). Les énumérations qui vont grandir ont une
  variante de repli (événement, état de caméra, motif) : un client ignore une nouveauté qu'il ne
  connaît pas au lieu de se déconnecter. Le plan gagne `ItemKind::Camera` et `rotation` (pour le
  ticket n° 6), la suppression d'une caméra la retire du plan, et un onglet **Caméras** (vide pour
  l'instant) apparaît dans le client.
- Caméras : un premier échec de vérification ne change pas l'état affiché, il en faut deux
  d'affilée ; le bouton **Annuler** du test arrête aussitôt la connexion ; une caméra regardée
  n'est ni vérifiée ni testée ; un mot de passe enregistré n'est pas réutilisé si l'adresse change
  de machine (il faut le ressaisir) ; un fichier de caméras illisible ou des caméras écartées au
  chargement sont conservés à part, jamais écrasés.

## [1.0.0] - 2026-11-09

Version 1 : l'application est séparée en deux parties, **sans aucun cloud**. Le **serveur**
tourne en continu sur un Mac du réseau local (service launchd, sans fenêtre) : il pilote les
appareils Matter, exécute les automatisations et garde les données. Le **client** (l'application
de bureau) affiche et commande, sans stocker de données. Reprise des données de la v0.2.1 sans
réappairer un seul appareil : [docs/migration.md](docs/migration.md). Installation du serveur :
[docs/installation-serveur.md](docs/installation-serveur.md).

### Ajouté

- **Mise en paquet** (étape 6). Chaque version publie le DMG du client et une archive du serveur,
  `matter-home-server-<version>-macos.tar.gz` : binaire universel (arm64 et x86_64), signé « ad
  hoc », compilé pour un ancien Mac (macOS 10.15 minimum pour Intel), avec `install.sh`,
  `uninstall.sh`, le gabarit du service launchd et une doc courte. `scripts/package-server.sh`
  la fabrique.
- **Service launchd** (LaunchDaemon, `dev.matter-home-desktop.server`) : démarre avec le Mac sans
  session ouverte, sous le compte de l'utilisateur qui l'installe (jamais root), relancé quand
  il sort en erreur (plantage, import appliqué, code 75) avec un délai de 30 s. `install.sh` et
  `uninstall.sh` (en français) retirent la quarantaine, demandent `sudo` seulement pour les
  gestes qui en ont besoin, valident le fichier avec `plutil -lint`, affichent l'empreinte du
  certificat, et ne règlent jamais la veille ni le pare-feu en silence : ils vérifient l'état
  quand c'est possible sans droits et affichent les commandes à lancer vous-même. Modes
  `--a-blanc` et `--prefixe` pour les essayer sans rien modifier. `uninstall.sh` ne supprime
  jamais les données sauf `--supprimer-donnees`, avec sa propre confirmation.
- Serveur : refuse de démarrer sans identité Matter (code 78, hors `--demo`) au lieu d'en créer
  une nouvelle par accident ; `--require-identity` reste accepté sans effet. Sous-commande `init`
  (installation neuve sans sauvegarde, seule façon de créer une identité). `import`, `init`,
  `create-account` et le serveur refusent de tourner en root, `--log-file` (journal en 0600 avec rotation à 5 Mo), `--version` (aussi
  sur le client).
- Import par l'API : une sauvegarde d'une autre identité (fabric) que celle du serveur est refusée
  (`identity_mismatch`, 409) sans `replace_identity=1` ; le client le dit clairement et demande une
  confirmation séparée. `install.sh` reprend le dossier de données du service installé, et
  refuse de continuer si l'application « Matter Home Desktop » (v0.2.1) est ouverte.
- `import --abandon` : renoncer à un import en attente ou interrompu ; `import --replace-identity` :
  seul moyen de remplacer une identité (fabric) différente de celle du dossier. Après une coupure
  en plein échange, `--abandon` remet les anciennes données en place.
- **Serveur : API réseau sécurisée** (étape 3 du découpage client-serveur). HTTPS (certificat
  auto-signé créé au premier lancement, empreinte SHA-256 affichée et journalisée) et
  WebSocket pour le temps réel ; compte unique créé en ligne de commande
  (`matter-home-server create-account`, mot de passe haché en argon2id, tentatives limitées),
  jetons de session stockés hachés. Le serveur tient l'état complet des appareils, le plan et
  les règles, diffuse chaque changement à tous les clients et envoie les notifications des
  règles aux clients connectés (plus de notification macOS sur le Mac du serveur). Export,
  import, historique et CSV sont servis à la demande. Protocole décrit dans
  `docs/protocole.md`.
- Options du serveur `--listen` et `--data-dir`.
- **Migration des données** (étape 5). `matter-home-server import FICHIER [--yes]` importe un
  `.matterhome` de la v0.2.1 (même identité Matter, sans réappairage), y compris dans un dossier
  vide ; il prend le verrou d'instance, affiche un résumé, puis quitte sans démarrer Matter ni le
  réseau. Les règles dont le webhook est incomplet (brouillons enregistrés par la v0.2.1) sont
  désactivées à l'import au lieu de le faire refuser, et nommées dans le résumé. Une coupure entre
  les deux renommages de l'import n'est plus suivie de la perte du dossier complet si
  `create-account` a été lancé avant la relance. Procédure : `docs/migration.md`.
- **Client branché sur le serveur** (étape 4). Écran de connexion (adresse, identifiant, mot de
  passe) ; à la première connexion l'empreinte SHA-256 du certificat est montrée puis épinglée,
  une empreinte changée est refusée sans contournement silencieux. Le client ne garde plus aucune
  donnée (seulement adresse, empreinte et jeton, en 0600, dans un dossier distinct de la v0.2.1) :
  état complet puis événements numérotés, historique lu à la demande, reconnexion automatique,
  « déconnecté » sans serveur. Appairage, volets, effacement d'historique, personnalisation, plan et
  règles (dernière modification gagnante), export `.matterhome` et CSV, import en deux temps
  (résumé puis confirmation) passent tous par le serveur ; notifications affichées dans
  l'interface.

### Modifié

- Import (`matter-home-server import`) : le rappel dit d'arrêter la v0.2.1 sur **tous** les Mac,
  y compris celui-ci (elle ne prend pas le verrou d'instance), et de partir du Mac qui détient
  les données les plus récentes. Il affiche l'identité du fichier et celle du dossier. Le dossier
  de données est toujours en 0700. Les certificats DER de la sauvegarde sont décodés à la
  validation.
- Un dossier préparé par un import auquel on a renoncé est mis de côté
  (`…import-abandonne-<date>`) au lieu d'être supprimé.

### Corrigé

- Relancer `import` après une coupure reprend l'import du même fichier au lieu de répondre
  « import demandé par l'API ? » ; un autre fichier en attente est refusé avec la marche à suivre.
- `--data-dir donnees` ou `donnees/` (chemin relatif) échouait à la synchronisation du dossier
  parent : le chemin est rendu absolu.
- `import --yes` ne remplace plus une identité Matter différente : il faut `--replace-identity`.
- Le serveur refuse de créer une nouvelle identité si un dossier `…avant-import-…` voisin en
  porte une.
- `fingerprint` pendant la reprise d'un import dit qu'un import attend d'être repris, au lieu de
  « le serveur n'a jamais été lancé ? ».
- Test du client : les points d'historique sont attendus avec un délai borné.
- Test du serveur « client lent » : il attend la fin de la rafale de démarrage de la démo avant
  d'ouvrir ses WebSockets (échec sporadique de la file de diffusion saturée).
- Client : plus de perte silencieuse de modifications. La fermeture de la fenêtre demande
  confirmation (« Rester » / « Quitter sans enregistrer ») quand un brouillon ne peut pas partir
  (adresse de webhook incomplète, liaison coupée, version d'un autre client en attente) ; un
  brouillon ou un choix en attente se voit dans la barre du haut depuis toutes les vues, avec un
  bouton « Revenir à la version du serveur » (règles et plan). Un jeton expiré, un certificat
  changé ou un changement de serveur qui abandonne un brouillon le dit sur l'écran de connexion,
  avec l'échec éventuel d'effacement du fichier de connexion. Les notifications du système passent
  par un seul fil et une file bornée : une rafale ne lance plus un processus par notification,
  le surplus est remplacé par un résumé. Cmd+Q ne passe pas par la demande de fermeture de la
  fenêtre (macOS), donc sans question : l'envoi borné à la sortie reste le filet.

## [0.2.1] - 2026-11-03

La 0.2.0 n'a jamais été publiée (échec de la fabrication du DMG sur la CI) ; elle est
reprise ici telle quelle.

### Ajouté

- **Changer de Mac** : menu Fichier → Exporter mes données… / Importer des données…. Un seul
  fichier `.matterhome` emporte appareils, plan, règles et historique ; l'import est appliqué
  au lancement suivant et garde une copie des anciennes données.
- **Volets et stores** (cluster Window Covering) : fiche avec ouverture, mouvement en cours,
  boutons ouvrir / stop / fermer, curseur de position, historique ; icône « Volet » ; état
  sur le plan ; action « Ouvrir / fermer un volet » et mesure « ouverture du volet » dans
  les automatisations.
- **Ponts Matter** (Somfy TaHoma…) : chaque appareil exposé par un pont devient un appareil
  à part entière, avec son nom, sa fiche, sa place sur le plan et ses règles ; la fiche du
  pont liste ses appareils ; un appareil déclaré injoignable par le pont est signalé.
  Écrit d'après la spécification, à valider avec une TaHoma réelle.
- Mode démo : une TaHoma et trois volets, dont un moteur RTS sans retour de position.

### Modifié

- **Fenêtre d'installation du DMG** : taille fixe, fond avec la consigne et une flèche vers
  le dossier Applications, grandes icônes bien placées, fond net sur écran Retina
  ([#1](https://github.com/killerwolf/matter-home-desktop/issues/1)).

## [0.1.0] - 2026-10-29

Première version publiée : Matter Home Desktop se télécharge en DMG, une seule application
universelle pour les Mac Apple Silicon et Intel.

### Ajouté

- **Appairage** des appareils Matter over Thread ou Wi-Fi déjà installés dans Apple Maison,
  avec le code à 11 chiffres ou le code QR, sans Bluetooth.
- **Liste des appareils** avec leur état (en ligne, injoignable), reconnexion automatique,
  nom et icône personnalisables.
- **Thermomètres** : température et humidité en direct, graphique, min / max / moyenne sur
  15 min, 1 h ou tout l'historique, export CSV.
- **Détecteurs d'ouverture** : ouvert / fermé, depuis quand, chronologie des changements.
- **Prises** : puissance en watts, énergie consommée, état allumée / éteinte.
- **Piles** : niveau et alerte de pile faible.
- **Plan de la maison** : étages, pièces, appareils placés avec leur état en direct.
- **Automatisations** « Quand… Si… Alors… » : notifications macOS, marche / arrêt des prises
  et lampes, webhooks.
- **Historique** des mesures conservé 7 jours sur le Mac.
- **Mode démo** (`--demo`) pour découvrir l'application sans appareil.
