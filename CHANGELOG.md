# Journal des modifications

Les changements notables de chaque version sont décrits ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le projet
respecte le [versionnage sémantique](https://semver.org/lang/fr/).

Les téléchargements de chaque version sont sur la
[page des versions](https://github.com/killerwolf/matter-home-desktop/releases).

## [Unreleased]

- Agents d'IA (étape E1a, lecture seule) : Ostal Server embarque un serveur MCP (`POST /mcp`, Streamable HTTP en JSON, sans état, sur l'écouteur HTTPS existant), **éteint par défaut** : tant que l'interrupteur n'est pas allumé, `/mcp` répond 404 comme une route inconnue. Jetons d'agent `ostal_agent_…` (256 bits, seule l'empreinte est gardée dans `serveur/agents.json`, 0600), rattachés à un compte, de durée limitée, révocables sans redémarrage ; ils ne valent que sur `/mcp` et aucun jeton de session, de mode ou de lecture n'y vaut. `ostal-server agent add|list|revoke|enable|disable` en est la seule interface pour l'instant : `add` ne crée que des jetons en lecture seule. Trois outils : `ostal_home_summary`, `ostal_list_devices`, `ostal_get_device` (ni mode, ni présence, ni adresse de caméra ou de webhook ; noms nettoyés champ par champ). Toute requête avec `Origin` : 403 ; 60 appels d'outil par minute et par jeton ; la création, la révocation et l'interrupteur sont au journal d'audit. Voir `docs/agents-ia.md`.
- Ostal Server : la première installation montre d'abord les prérequis (Mac allumé en permanence, appareils Matter déjà dans Apple Maison, HomePod ou Apple TV comme hub et routeur Thread, Tailscale pour l'extérieur) et la promesse de confidentialité avec ses exceptions.
- Ostal Server : « Essayer avec des appareils simulés » dans la première installation lance la démonstration (`--demo`) dans un autre processus, sans Terminal et sans toucher aux données réelles ; sa fenêtre porte un bandeau avec l'adresse, le compte de démonstration et « Quitter la démonstration » (sans confirmation) ; la démonstration s'arrête avec l'application qui l'a lancée.
- Documentation : « tout reste en local, sans cloud » devient « aucun cloud de l'éditeur ; rien de la maison ne part sans une option que vous activez », avec le tableau de ce qui sort (descriptions par IA, notifications push Apple, Google, Mozilla ou Microsoft, Tailscale, webhooks) et l'historique de 7 jours.
- Ostal Server : quand tous les appareils (ou 80 % au moins, et trois au minimum) sont injoignables en même temps, la phrase d'état dit « Tous les appareils ont décroché en même temps » et renvoie au Wi-Fi du Mac, pas à l'alimentation ; la logique commune est dans `protocol::status::devices_point`, reprise par l'app Mac et l'app web.
- Ostal Server : Réglages › Dépannage donne trois cas (tout injoignable, un seul injoignable, « La maison ne répond pas »), le journal technique en dernier.
- Ostal Server : les étapes de connexion citent la découverte automatique d'abord, l'adresse en secours, et ne parlent plus de Windows ni de Linux.
- Ostal Server : la vue d'ensemble montre la box de la maison (reliée, injoignable, mot de passe refusé, présence figée) avec un renvoi vers Réglages › Présence, qui dit où trouver le mot de passe de la Bbox.
- Ostal Server : l'onglet « Journal » devient « Activité » ; textes périmés corrigés (« d'Ostal Server », sauvegarde faite depuis l'app Mac).
- App Mac et web : plus d'erreur brute pour un appareil injoignable. Le serveur ne laisse passer que « pas d'adresse sur le réseau », « ne répond pas » ou « occupé » (`protocol::status::unreachable_reason`) ; le texte détaillé reste dans le journal technique.
- App Mac : « Changer de serveur » sur « Plus de connexion à la maison » revient à l'écran de connexion sans révoquer la session (le jeton reste dans le fichier de connexion) ; seule « Se déconnecter » la ferme.
- App Mac : la phrase d'état dit « Tous les appareils ont décroché en même temps » (Wi-Fi du Mac) quand presque tous sont injoignables ; l'état vide des caméras renvoie à l'onglet « Caméras » d'Ostal Server, sans « menu Caméras… » ni « flux RTSP » ; le menu Fichier montre « Compte : … » (le compte ouvert).
- App web : la phrase d'état dit aussi « Tous les appareils ont décroché en même temps » (jumelle de `protocol::status`, mêmes seuils).
- App web : la carte « Installer sur l'iPhone » ne s'affiche plus que sur iPhone et iPad, a un bouton « Plus tard » (une semaine) et ne rend plus les notifications obligatoires.
- App web : « La maison ne répond pas » est annoncé comme une alerte, avec deux pistes : Mac éteint, en veille ou hors du Wi-Fi à la maison, Tailscale de loin.
- App web : « Mode nuit » et « Mode vacances » dans la carte d'installation, l'état vide dit où ajouter un appareil, et « Mes connexions » montre le compte ouvert.
- Présence : un téléphone absent du Wi-Fi peut être relié à son compte à distance ; la liste montre d'abord ceux qui sont sur le Wi-Fi, puis les autres appareils connus de la box.
- App Mac : après « Changer de serveur », l'écran de connexion dit que la session de l'ancien serveur n'est pas fermée et propose « Revenir à ce serveur ».
- Appareil injoignable : la raison « le pont ne le voit plus » s'ajoute aux trois autres quand le pont le déclare injoignable.
- App web : l'état vide dit d'ajouter un appareil depuis l'app Ostal sur le Mac ; « Plus tard » sur l'étape facultative des notifications (la seule restante) masque la carte d'installation pour de bon.

### Présence par le Wi-Fi de la box et mode automatique (multi-comptes, étape 6)

- Ostal Server se relie à la box (Réglages › Présence) avec son mot de passe d'administration, gardé
  dans `serveur/box.json` (0600) : jamais affiché, journalisé ni exporté. Il relève les appareils actifs
  toutes les 2 minutes, en lecture seule ; seuls les **noms** des téléphones sont gardés
  (`telephones.json`), jamais une adresse MAC ni une IP. Box injoignable ou mot de passe refusé : le mode
  est figé, et un mot de passe refusé n'est plus rejoué.
- À sa première connexion, l'app web demande « Lequel est votre téléphone ? » (noms seulement, « plus
  tard » possible) ; « Mon téléphone » permet de le changer. iPhone et Android. L'administrateur corrige
  dans Ostal Server (Réglages › Présence, fiche d'un compte).
- La présence de chacun vient de son téléphone vu sur le Wi-Fi (un départ compte après 10 minutes) ou de
  « Je pars » / « Je rentre » : le plus récent l'emporte. L'accueil de l'app web et la barre de l'app Mac
  disent qui est à la maison.
- Nouvel interrupteur « Changer le mode de la maison selon la présence », **éteint par défaut** (aussi
  après la mise à jour) : Absent quand la dernière personne part, Présent dès qu'une arrive ; Nuit et
  Vacances restent à la main (une absence ne les quitte jamais, une arrivée pendant Vacances remet
  Présent). « Je pars » et « Je rentre » : interrupteur éteint, comme avant (« Je pars » met la maison
  en Absent, « Je rentre » en Présent, Nuit et Vacances respectés), et la présence est quand même
  notée ; allumé, un départ n'arme la maison que si personne d'autre n'est à la maison. L'interrupteur
  peut être allumé sans box reliée : la règle suit alors les seuls raccourcis (« aucun téléphone relié,
  jamais » ne vaut que pour le Wi-Fi), et délier la box ne l'éteint plus. Le raccourci dit ce que la
  maison est devenue. Chaque changement automatique est au journal avec la personne concernée, ainsi que
  le changement du délai de départ.
- La box : la vérification du certificat n'est désactivée que pour `mabbox.bytel.fr` ou une adresse IP
  privée ; toute autre adresse https garde la vérification. Un départ vu pendant qu'une déclaration « à la
  maison » arrive n'arme plus la maison (la décision se prend sous le verrou du mode).

### Préparation des droits (multi-comptes, étape 5)

- Invisible pour l'instant : chaque route qui modifie la maison (commande d'un équipement, mode,
  présence, plan, automatisations, appairage, export et import, réglage des notifications) demande
  d'abord si le compte en a le droit. Tous les comptes gardent les mêmes droits ; le code d'erreur
  `forbidden` (403) existe déjà pour le jour où l'on restreindra un compte. Rien à migrer.

### Journal d'audit (multi-comptes, étape 4)

- Un journal note qui a fait quoi, depuis quel téléphone ou ordinateur : connexion réussie ou
  refusée, nouvelle connexion, connexion coupée, mot de passe changé, compte créé, désactivé ou
  réactivé, changement de mode (avec sa source : personne, raccourci, automatisation ou automatique),
  commande d'un équipement, automatisations modifiées ou testées, appairage et suppression d'un
  équipement, export et import, « Je pars » et « Je rentre ». Chaque ligne porte la date, la
  personne, la connexion, l'adresse, l'action, la cible et le résultat. Jamais de mot de passe, de
  jeton, de code d'appairage ni de mesure ; un identifiant tapé lors d'une connexion refusée n'est
  gardé que s'il désigne un compte.
- Un fichier par mois, `serveur/audit/AAAA-MM.jsonl`, en ajout seul (dossier 0700, fichiers 0600),
  gardé 6 mois : les fichiers plus anciens sont supprimés. Le dossier reste dans `serveur/` : jamais
  exporté, jamais remplacé par un import. Rien d'ancien n'est modifié : le dossier se crée à côté des
  fichiers existants au premier événement, et les anciens clients continuent de se connecter (ligne
  « connexion inconnue »).
- Ostal Server : nouvel onglet **Journal** (filtres Personne et Période, par jour) ; la ligne de
  commande `ostal-server audit [--user COMPTE] [--days N]` relit les mêmes lignes.

### Raccourcis et notifications par personne (multi-comptes, étape 3)

- Raccourcis, widget et notifications deviennent personnels. Les jetons de mode et de lecture se
  rangent dans `jetons.json` : un par téléphone, avec un libellé (« iPhone de Hamdi »), révocables un
  par un sans toucher à ceux des autres. Chaque jeton est rattaché à la connexion d'où il a été
  préparé : réinstaller depuis la même connexion remplace son jeton (deux téléphones nommés
  « iPhone » gardent chacun le leur), et couper une connexion coupe aussi ses raccourcis et son
  widget. Les raccourcis et le widget d'un compte désactivé cessent de marcher tant qu'il l'est ; ils
  remarchent à la réactivation.
- Migration sans casse : `jeton-mode.json` et `jeton-lecture.json` deviennent des jetons du premier
  utilisateur (même empreinte) ; les raccourcis et le widget déjà installés continuent de marcher.
  Les anciens fichiers restent en place, intacts. Les abonnements aux notifications sont rattachés
  au premier utilisateur au démarrage (un champ s'ajoute dans `push.json`, rien d'autre ne change).
- Notifications : chaque personne voit, règle et révoque ses téléphones et ordinateurs ; chacun reçoit
  selon ses catégories. L'interrupteur général reste un réglage commun à toute la maison. Un
  téléphone qui change de compte passe à la nouvelle personne.
- App web, « Widgets et raccourcis » : « Installer mes raccourcis » (« Je pars », « Je rentre »,
  Mode nuit, Mode vacances) et « Votre widget », au jeton de la personne ; « Je pars » et « Je rentre »
  disent où elle est ; une note remplace les deux sections sur Android. Le widget propose
  « Je pars » et « Je rentre ». « Mode absent » et « Mode présent », déjà installés, marchent encore.
- Présence déclarée : `PUT /api/presence` (`{"state": "home" | "away"}`, jeton de mode ou session)
  garde ce que chacun a dit dans `presence.json`. En attendant la présence automatique, « Je pars » et
  « Je rentre » (jeton de mode) font ce que faisaient les raccourcis d'avant, sans rien à réinstaller,
  mais selon la règle de la maison : la maison passe en Absent seulement si personne d'autre n'est
  à la maison, et en Présent seulement depuis Absent ou Vacances (une arrivée la nuit ne réveille pas
  la maison). Une session ne fait que déclarer.
- « Je pars » et « Je rentre » disent ce que la maison est devenue : `PUT /api/presence` répond aussi
  `mode` (le mode actuel) et `message` (par exemple « Maison en mode Absent. » ou « Sam est encore à
  la maison : la maison reste en Présent. »), et la notification du raccourci affiche cette phrase au
  lieu d'un « C'est noté » fixe : on sait si la maison est armée. Les raccourcis déjà installés
  marchent encore (ils affichent l'ancien texte).
- Couper une connexion : les confirmations (app web, fenêtre du serveur) disent que les raccourcis et
  le widget installés depuis elle cessent aussi de marcher ; dans la fenêtre du serveur, la coupure
  se fait sans figer la fenêtre (« Coupure de … »). Une coupure qui arrive pendant la préparation de
  raccourcis ne laisse plus de jeton neuf pour la connexion coupée. Un identifiant de connexion ne
  peut plus avoir la forme d'un identifiant interne (32 caractères hexadécimaux).
- Ostal Server : le code secret et le widget de la fenêtre sont ceux du premier compte.
- Les commandes `user add`, `user passwd`, `user disable` et `user enable`, et la fenêtre du serveur,
  prennent un verrou (`serveur/comptes.lock`) : deux modifications simultanées des comptes ne se
  perdent plus. `create-account --replace` dit combien de jetons et d'abonnements des anciens comptes
  restent dans les fichiers, sans effet.

### Connexions (multi-comptes, étape 2)

- Chaque téléphone ou ordinateur où un compte est ouvert est une « connexion » : le login envoie
  son identifiant (gardé par l'app), son type (app web, app Mac) et son nom (« iPhone de Hamdi »).
  Protocole 4 : le serveur répond dans la version que parle le client, donc la 2.0.0 et la 2.1.0-beta.1
  (version 3, sans connexion) continuent de marcher et apparaissent comme « connexion inconnue ».
- Les connexions se rangent dans `sessions.json` (dernier accès, sur place ou par Tailscale), à côté
  de ce qui existe ; rien n'est migré ni supprimé. Se reconnecter depuis la même installation remplace
  sa session. Routes `GET /api/me/installations` et `DELETE /api/me/installations/{id}`.
- App web : feuille « Mes connexions » (nom, dernier accès, où, « Couper »). Le cache du service
  worker passe à `ostal-shell-v16`.
- App Ostal sur Mac : Fichier › « Mes connexions… » (page « ‹ Maison · Mes connexions »).
- Ostal Server : « Voir toutes les connexions » dans Réglages › Comptes, avec « Couper », et les
  connexions de chaque personne dans sa fiche.
- Le serveur donne au client ses adresses Tailscale directes après la connexion (même certificat,
  rien de nouveau à approuver) ; l'app Mac revient sur l'adresse de la maison dès qu'elle répond de
  nouveau.

### Découverte du serveur (multi-comptes, étape 2)

- Ostal Server s'annonce sur le réseau de la maison (Bonjour, service `_ostal._tcp`, nom du Mac,
  adresses privées seulement, version du protocole et empreinte du certificat en TXT), par l'API mDNS
  de `matc` : ni nouvelle caisse, ni sous-processus. Jamais d'annonce sur la boucle locale (démo,
  essais) ; « au revoir » envoyé à l'arrêt.
- App Mac : à la première ouverture, « Ostal Server trouvé » (nom du Mac, rien à taper) ;
  « Saisir une adresse à la main » en secours. Le certificat se confirme comme avant, par son
  empreinte. Mes connexions : carte « Comment ce Mac joint la maison » (réseau de la maison,
  Tailscale, détails, « Chercher à nouveau »).

### Comptes nominatifs (multi-comptes, étape 1)

- Serveur : plusieurs comptes, un par personne du foyer (`comptes.json` : identifiant, nom affiché,
  mot de passe haché argon2id, rôle administrateur pour tous en V1, actif ou désactivé). Au premier
  démarrage, l'ancien compte unique devient le premier utilisateur, avec le même identifiant
  interne : ses sessions, ses raccourcis et les anciens clients continuent de marcher. Les anciens
  fichiers (`compte.json`, `sessions.json`) restent en place.
- Sessions rattachées à leur utilisateur (32 au plus par personne). Un nouveau mot de passe ou une
  désactivation coupe tout de suite les sessions de ce compte, et d'elles seules, même serveur ouvert.
- Limiteur d'échecs de connexion aussi par identifiant, en plus de l'adresse.
- Ligne de commande : `ostal-server user add | list | passwd | disable | enable` (`create-account`
  reste, pour le premier compte).
- Ostal Server : Réglages › Comptes (liste, « Ajouter un compte », fiche d'un compte : nouveau mot de
  passe, désactiver ou réactiver). « Remplacer le compte » disparaît : le serveur ne redémarre plus.

### Refonte (préversion, finitions)

- App web : le plan de la maison est affiné (`plan.js` et ses tests).
- Notifications et caméra injoignable : le serveur (« Ostal Server ») signale mieux une caméra qui ne
  répond plus (`cameras.rs`, `cameras_ui.rs`, page des caméras).
- Intégration : formatage, clippy (`-D warnings`), tests Rust et tests node au vert.

### Refonte (préversion, itération 3)

- Intégration : formatage, clippy (`-D warnings`), tests Rust et tests node repris sur les trois
  surfaces ; le cache du service worker de l'app web est à `ostal-shell-v15` (tests du serveur
  alignés).
- Préversion : la version n'est toujours pas incrémentée.

### Refonte (préversion, itération 2)

- Intégration : formatage, clippy et tests repris sur les trois surfaces ; le cache du service worker
  de l'app web passe à `ostal-shell-v14` (tests du serveur alignés).
- Préversion : la version n'est toujours pas incrémentée.

### Refonte (préversion, itération 1)

- **Refonte complète selon la nouvelle charte**, sur les trois surfaces à la fois : app web,
  fenêtre du serveur (« Ostal Server ») et app de bureau. Socle commun : jetons de design, thème
  clair et sombre, phrase d'état, raccourcis Nuit et Vacances, widget.
- **Tailles de fenêtre fixes** : app de bureau 1280×800, fenêtre du serveur 640×700.
- **Fenêtre du serveur** : écrans Caméras, formulaire de caméra, Descriptions, Notifications,
  Raccourcis, Réglages et Confirmer (nouveau module de confirmation).
- **App de bureau** : nouvelle coquille (`shell`), caméras, appareils, règles, capteurs,
  thermomètre et lecteur remis à la charte.
- **App web** : tuiles, plan, températures, notifications push, manifeste et service worker mis à
  jour ; tests node ajoutés pour les tuiles.
- Documentation mise à jour (web, serveur, plan, utilisation, automatisations, installation).
- Préversion : la version n'est pas incrémentée.

### Modifié

- **Nouvelle palette « Nuit et Lueur »**, celle du logo Seuil, dans l'app de bureau, l'app web et
  la fenêtre du serveur : neutres légèrement bleutés, texte Nuit (`#10232d`) en clair, fond nuit
  profonde en sombre. Nouveaux tokens `lueur`, `on_lueur` et `lueur_text` : une lampe allumée et
  le mode **Présent** (pastille du mode) prennent la Lueur (`#f5b33f`). L'alerte (`warn`) passe à
  un orange plus rouge (`#b4500e`), pour ne plus se confondre avec une lampe allumée : Lueur veut
  dire « allumé », jamais « attention ». L'échelle de température ne change pas. Tous les textes
  gardent un contraste d'au moins 4,5 (testé).

### Ajouté

- **Logotype Ostal** : le logo Seuil et le mot « Ostal » en Outfit SemiBold, en tracés, en
  variantes claire et sombre (`assets/logo/ostal-logotype-*.svg`). Il apparaît en haut du README et
  sur l'écran de connexion de l'app web.
- **Chaque changement de mode dit d'où il vient** : raccourci iPhone (jeton de mode), app web, app
  de bureau, fenêtre du serveur, automatisation (avec le nom de la règle) ou autre. Le journal du
  serveur et celui de l'app de bureau l'écrivent (« Mode : Absent (raccourci iPhone) ») ; l'app web
  montre une notification brève quand le mode change ailleurs ; une règle « Quand le mode
  devient… » peut l'écrire dans sa notification avec la nouvelle variable `{source}`. Protocole
  compatible : `mode_changed` et `PUT /api/mode` gagnent un champ facultatif `source`, que les
  anciens clients ignorent ; un client ne peut annoncer que ce que son jeton permet.

## [2.0.0] - 2026-11-21

### Modifié

- **Matter Home s'appelle désormais Ostal.** L'application de bureau devient « Ostal »
  (`dev.ostal.app`), l'application de la barre de menus « Ostal Server » (`dev.ostal.server`),
  avec un nouveau logo, « Seuil » (une arche ouverte et un point allumé), y compris dans la
  barre de menus et pour l'app web. Le mot « Matter » ne désigne plus que le protocole.
  Binaires `ostal`, `ostal-server`, `ostal-server-app` ; variables `OSTAL_*` (au lieu de
  `MATTER_HOME_*`) ; sauvegardes `.ostal` ; fichiers de version `Ostal-<version>-<arch>.dmg`,
  `OstalServer-<version>-universal.dmg` et `ostal-server-<version>-macos.tar.gz` ; widget
  Scriptable `ostal.js` ; étiquette de fabric « Ostal Server » (réécrite une fois sur chaque
  appareil).
- **Rien n'est perdu au passage** : au premier lancement, Ostal Server **copie** le dossier de
  données de Matter Home (`dev.matter-home-desktop.MatterHomeDesktop` →
  `dev.ostal.OstalServer`), vérifie la copie et garde l'ancien dossier comme sauvegarde ; même
  identité Matter, aucun appareil à réappairer. Dans l'ancien dossier, seul `controller.json`
  est renommé (`controller.json.copie-vers-ostal`, réversible) : l'ancienne application rouverte
  par erreur ne repart pas avec la même fabric. L'application de bureau reprend de même sa
  connexion (`dev.ostal.OstalClient`), Ostal Server le réglage « ouverture à la connexion » (son
  agent remplace celui de Matter Home, qui est retiré). Les sauvegardes `.matterhome`
  s'importent toujours ; l'app web garde sa session et ses réglages. Voir
  [Données](docs/donnees.md#après-le-renommage-de-matter-home-en-ostal).
- **À faire après la mise à jour** : quitter Matter Home Server (tant qu'il tourne, Ostal
  Server refuse de démarrer et le dit), installer Ostal et Ostal Server depuis les DMG, ouvrir
  Ostal Server une fois, puis supprimer « Matter Home Server » et « Matter Home Desktop » du
  dossier Applications. Nouvel identifiant d'application : macOS redemande les autorisations
  (notifications, pare-feu, réseau local selon la version) et signale le nouvel élément ouvert
  à la connexion. Sur l'iPhone, l'adresse ne change pas : pour voir le nouveau nom et le
  nouveau logo, retirez l'app web de l'écran d'accueil puis ajoutez-la de nouveau (réabonnez
  ensuite l'appareil aux notifications), et réinstallez widget et raccourcis depuis l'app web
  si vous voulez qu'ils s'appellent Ostal (les anciens continuent de marcher). Pour le service
  installé au Terminal, `install.sh` remplace l'ancien service.

### Corrigé

- Ostal (application de bureau) : le Dock montre de nouveau l'icône de l'application au lieu
  du logo d'egui une fois l'application ouverte.

## [1.4.0] - 2026-11-21

### Ajouté

- **Raccourcis de mode en un geste** : dans l'app web, Widgets et raccourcis › « Installer les
  raccourcis » crée un nouveau jeton de mode et prépare « Mode absent » et « Mode present » avec
  l'adresse en .ts.net et le jeton écrits dedans, signés sur le Mac (`shortcuts sign`) : plus aucune
  question à l'import. Liens imprévisibles, en mémoire, valables 5 minutes. Si la signature échoue
  ou si la page est fermée avant la fin, l'ancien jeton reste valable. Une préparation à la fois
  (`409 busy` sinon, nouveau code d'erreur `busy`) ; l'adresse doit être celle de ce Mac.
- **Jeton de lecture** (`serveur/jeton-lecture.json`) pour le widget Scriptable : il n'ouvre que
  `GET /api/state` et `GET /api/events` (401 partout ailleurs, WebSocket et caméras compris).
  « Installer le widget » le crée et copie le script déjà rempli (adresse et jeton).
- Révocation de chaque jeton depuis l'app web ; état du jeton de lecture et bouton « Révoquer » dans
  la fenêtre du serveur (onglet Connexion).

### Modifié

- Les raccourcis s'appellent **« Mode absent »** et **« Mode present »** (sans accent) : le widget ne
  trouvait pas « Mode présent », iOS enregistrant l'accent du nom de fichier autrement. Supprimez les
  anciens raccourcis avant d'installer les nouveaux.
- Le widget Scriptable n'a plus de connexion par mot de passe ni de jeton de session gardé dans le
  trousseau ; sur un jeton révoqué, il affiche « Jeton révoqué : recopie le widget depuis l'app web ».
- Les raccourcis génériques (`/app/raccourcis/mode-absent.shortcut`, `mode-present.shortcut`) et le
  script vierge (`/app/raccourcis/matter-home.js`) ne sont plus servis. Service worker en version 9.

## [1.3.2] - 2026-11-20

### Corrigé

- Widget Scriptable : la connexion était redemandée sans fin (« Session expirée » juste après
  « Connecté ») : l'en-tête `Authorization` n'était pas envoyé, Scriptable ignorant les en-têtes
  ajoutés un par un.

## [1.3.1] - 2026-11-20

### Ajouté

- **Widgets et raccourcis** dans l'app web (Appareils › Widgets et raccourcis) : deux raccourcis
  Apple signés (« Mode absent », « Mode présent ») et le script du widget
  Scriptable, à installer depuis l'iPhone. La page affiche l'adresse de la maison (`location.origin`,
  avec « Copier »), explique le jeton de mode (onglet Connexion › Créer le code secret) et copie le
  script. Trois fichiers intégrés au binaire sous `/app/raccourcis/`, servis sans jeton (aucun
  secret dedans), avec le nom de fichier exact dans `Content-Disposition`. Aucune route d'API
  nouvelle. Service worker en version 8 (il
  ne garde pas les raccourcis). Voir `docs/web-app.md`.

## [1.3.0] - 2026-11-19

### Ajouté

- **Notifications sur iPhone et iPad** (ticket n° 17) : l'app web ajoutée à l'écran d'accueil
  (iOS 16.4 ou plus, adresse `.ts.net`) reçoit des alertes **même fermée**, par le web push standard
  (VAPID, message chiffré de bout en bout `aes128gcm`, RFC 8291 et 8292, avec `ring` déjà présent :
  aucune dépendance nouvelle). Quatre catégories, à cocher par appareil : actions « Notifier » des
  règles, **passages détectés** sur une caméra en Surveillance (un message par passage, avec sa
  description si elle est attendue ; le toucher ouvre la fiche de l'événement), **pile faible**
  (sous 15 %, une fois jusqu'à 20 %) et **appareil injoignable** depuis 10 minutes. Réglages dans
  l'app web (Appareils › Notifications : activer, essai, catégories, appareils abonnés révocables)
  et dans la fenêtre du serveur (onglet Connexion : interrupteur général, révocation). Envoi par le `curl` du système
  vers les seuls services de push connus (Apple, Google, Mozilla, Microsoft) ; jamais d'adresse, de
  mot de passe de caméra ni d'identifiant d'appareil brut dans le texte. Clé VAPID et abonnements
  dans `serveur/` (0600), jamais exportés. Nouvelles routes `/api/push…` (protocole inchangé,
  version 3) ; service worker de l'app web en version 7. Voir `docs/notifications.md`.

### Modifié

- **Client : les clips des événements se lisent dans l'application** (ticket n° 22). « Lire le clip »
  ne télécharge plus le fichier pour l'ouvrir dans QuickTime : le clip se lit sous l'événement, avec
  lecture / pause, barre de progression (on peut se déplacer dans le clip), temps écoulé et durée
  totale, boucle facultative, image à la largeur du panneau. Un démultiplexeur MP4 minimal lit le
  fichier du serveur et le décodeur VideoToolbox des flux en direct fait le reste, dans un fil à
  part : l'interface ne se bloque pas, les images sont montrées aux instants du MP4, et seules
  quelques images décodées sont gardées en mémoire. Un clip illisible ou un décodeur en échec
  affiche un message dans le panneau. « Ouvrir dans QuickTime » reste disponible. Aucune dépendance
  nouvelle.
- **Serveur : l'application de la barre de menus est réorganisée** (ticket n° 21). Le menu répond
  d'abord à « est-ce que tout va bien ? » : un titre (« Tout va bien », « 1 point à regarder »,
  « Le serveur est à l'arrêt »), une ligne par sujet (appareils, caméras et détection, accès à
  distance) et une seule action quand quelque chose ne va pas. L'icône gagne une forme « un point
  est à regarder » (maison avec une pastille). La fenêtre passe à cinq onglets, **Vue d'ensemble**,
  **Appareils**, **Caméras**, **Connexion**, **Réglages**, avec le même aspect que le client
  (tokens partagés). Les tâches rares sont guidées pas à pas : première installation en trois
  étapes, connecter un ordinateur, iPhone hors de la maison (Tailscale), changement de mode par
  Raccourcis, clé Anthropic, ajout d'un appareil. Vocabulaire revu (« accès à distance », « code de
  vérification », « code secret »). Rien n'est supprimé : copier l'adresse et l'empreinte, l'accès
  web, le journal, le lancement à l'ouverture de session et la version quittent le menu pour la
  fenêtre. Le comportement du serveur ne change pas. Détail : `docs/ux-serveur.md`.

## [1.2.1] - 2026-11-18

### Modifié

- **Client, macOS : barre de menus du Mac** (ticket n° 20). Le menu **Fichier** (Exporter mes
  données…, Importer des données…, Se déconnecter) quitte la fenêtre pour la barre de menus en haut
  de l'écran, avec le menu de l'application (À propos, Masquer, Quitter ⌘Q), **Édition** (Annuler,
  Couper, Copier, Coller, Tout sélectionner) et **Fenêtre** (Réduire ⌘M, Zoom). Les éléments de
  Fichier sont grisés comme avant (pas de connexion). Mêmes fonctions qu'avant, déclenchées depuis
  le menu natif (AppKit via objc2, aucune dépendance nouvelle). Les autres systèmes gardent le menu
  dans la fenêtre.

### Corrigé

- **Éditeur de règles** : le menu « Quand » ne propose plus « Inconnu (version plus récente du
  serveur) », que le serveur refusait avec un message vague. Une règle qui contient déjà un
  déclencheur, une condition ou une action d'une version plus récente du serveur reste un brouillon
  local, avec un message clair, au lieu d'être envoyée.

## [1.2.0] - 2026-11-18

### Ajouté

- **Descriptions des événements** (ticket n° 19) : une phrase en français (200 caractères au plus) et
  un niveau de menace (0 normal, 1 suspect, 2 critique) par événement de détection, comme la revue GenAI
  de Frigate, avec l'API Anthropic et la clé de l'utilisateur. **Désactivé par défaut.** Seule la capture
  JPEG (côté long 1 024 px au plus) part, avec le nom de la caméra et les classes : jamais le flux. Modèle
  au choix (Haiku 5.5, Sonnet 5.5, **Opus 5.5** par défaut, coût indiqué), caméras (toutes, extérieures,
  liste), classes, plafond quotidien (200 par défaut), « dès l'ouverture » facultatif (fenêtre du serveur,
  « Descriptions des événements… »). Appel HTTPS par `curl` (clé par l'entrée standard, jamais en
  argument), sortie structurée, un nouvel essai sur 429 / 5xx / réseau (`retry-after`), refus du modèle
  enregistré comme « description indisponible », file bornée et fil dédié (rien ne bloque l'analyse). La clé
  est dans `serveur/descriptions.json` (0600), jamais réaffichée ni journalisée, **jamais exportée** dans
  une sauvegarde. Champs facultatifs `description` et `niveau` de l'événement `detection` (protocole 3,
  nouvel envoi à l'arrivée), affichés dans l'app web et le client de bureau ; variables `{description}` et
  `{niveau}` des notifications et webhooks (la règle attend la description). Voir
  [docs/descriptions.md](docs/descriptions.md) (ce qui part, coûts, limite de dépense, désactivation).

- **Détection de personnes, d'animaux et de véhicules** (ticket n° 18) sur les caméras en **Surveillance**
  dans le mode actuel, sans enregistrement continu. Une seule session RTSP par caméra, **partagée** avec
  le direct et les vignettes (le registre des sessions est maintenant tenu par le hub) ; aucune connexion
  pour une caméra Éteinte ou en Diffusion. Pré-enregistrement des ~10 dernières secondes de H.264 brut en
  mémoire ; mouvement sur 2 images décodées par seconde (VideoToolbox décode tout mais ne convertit que
  celles-là) ; puis le modèle sur les images en mouvement : **ONNX Runtime + CoreML** par la caisse `ort`
  en chargement dynamique (rien de lié à la compilation ; `libonnxruntime.dylib` et les modèles sont
  embarqués dans l'application du serveur, `<données>/models/` reste un recours pour les utilisateurs
  avancés, voir [docs/detection.md](docs/detection.md)), YOLOX (sortie brute de la grille)
  et YOLOv8/YOLO11 reconnus, repli sur un petit modèle processeur (`detector-cpu.onnx`). Sans modèle : la
  détection est désactivée et le dit. Un passage = un événement (8 s sans détection le ferment) : fiche,
  capture JPEG et clip **MP4 sans réencodage** (avant + passage + après), dans `<données>/events/`, 7 jours
  et 2 Go au plus (réglable, `detection.json`), nettoyés au démarrage et toutes les heures ; pas dans la
  sauvegarde `.matterhome` (gardés à l'import). Classes, score minimal, zones et masques par caméra
  (fenêtre « Caméras… » du serveur, section « Détection », qui montre aussi le moteur et les caméras
  analysées). Pause de 30 s avec une ligne au journal si l'analyse ne suit plus.
- **Événements dans les clients** : `WireEvent::Detection` (`detection`) et les routes
  `GET /api/events`, `GET|DELETE /api/events/{id}`, `GET /api/events/{id}/snapshot`,
  `GET /api/events/{id}/clip` (avec `Range`), ajoutés **dans le protocole 3**. App web : liste
  « Événements » sous les caméras, capture et lecture du clip (cache du service worker en `v5`, la politique
  de sécurité autorise `media-src blob:` pour ce seul usage). Client de bureau : colonne « Événements » à
  côté de la mosaïque, le clip s'ouvre dans le lecteur du Mac.
- **Détection embarquée** : `Matter Home Server.app` contient les modèles YOLOX-s et YOLOX-nano
  (Apache-2.0) et ONNX Runtime 1.23.2 (MIT, roues PyPI officielles x86_64 et arm64 réunies, macOS 13
  minimum pour la détection), avec leurs licences (`Contents/Resources/licences`) :
  rien à installer. Ils sont téléchargés à la construction (`scripts/bundle-server-app.sh
  --with-detection`) et vérifiés par empreinte SHA-256 épinglée (`scripts/detection-assets.sha256`).
  Objets suivis d'une image à l'autre (un objet immobile depuis 10 s ne compte plus), détecteur
  lancé seulement s'il y a du mouvement, passage coupé à 2 minutes, événement annoncé « en cours »
  dès la première détection puis complété ; pause par caméra si l'analyse ne suit plus ou si le Mac
  chauffe ; pas de clip sur un disque presque plein.
- **Règles d'un serveur plus récent** : `Trigger`, `Condition` et `Action` ont une variante de repli
  (`Unknown`), refusée à l'enregistrement.
- **Automatisations** : « Quand une caméra détecte… » (`Trigger::Detection`, une caméra ou toutes, une ou
  plusieurs classes), avec `{appareil}` = la caméra et `{valeur}` = la classe ; utilisable avec « Seulement
  si le mode est… ».

- **Mode global de la maison** (ticket n° 8) : **Présent**, **Absent**, **Nuit** ou **Vacances** (Présent
  par défaut), tenu par le serveur (`mode.json`, atomique, droits 0600, inclus dans la sauvegarde
  `.matterhome`) et le même pour tous les clients. **Protocole en version 3** (les règles de mode n'ont pas
  de repli : un client 1.1.0 est refusé en 426 « version incompatible » au lieu de boucler sur une
  lecture impossible) : champ `mode` dans l'état complet (le sélecteur reste caché s'il manque),
  événement `mode_changed`, route `PUT /api/mode`. Une sauvegarde faite avec cette version est refusée
  par un serveur 1.1.0. Redemander le mode
  actuel ne change rien. Il se choisit dans la barre du haut du client de bureau, par la pastille « Mode : … »
  de l'app web (cache du service worker en `v4`), et par le sous-menu « Mode » de la barre de menus du serveur.
  Il apparaît au journal. Voir [docs/modes.md](docs/modes.md).
- **Jeton de mode pour Raccourcis (iPhone)** : un secret à part, généré par le serveur (fenêtre du serveur,
  affiché une seule fois, régénérable, révocable), qui ne sert **qu'à** `PUT /api/mode` (toute autre route le
  refuse en 401). Seule son empreinte SHA-256 est gardée (`serveur/jeton-mode.json`, 0600, jamais exportée),
  il n'est jamais journalisé, et il est vérifié en premier : un bon jeton n'est jamais refusé, un mauvais reçoit 401 après 250 ms
  (pas de blocage par adresse, qui aurait été partagé derrière `tailscale serve`).
  Guide pas à pas « Changer de mode automatiquement avec Raccourcis sur iPhone ».
- **Caméras selon le mode** : chaque caméra a un **emplacement** (Intérieure ou Extérieure) et un comportement par
  mode (**Éteinte**, **Diffusion**, **Surveillance**), réglés dans la fenêtre « Caméras… » du serveur ; choisir
  l'emplacement pré-remplit le tableau (Extérieure : Diffusion en Présent, Surveillance sinon ; Intérieure :
  Éteinte en Présent et Nuit, Surveillance en Absent et Vacances), modifiable ensuite. Pour une caméra éteinte,
  le serveur refuse le direct et l'image fixe (503 `unavailable`, « caméra éteinte en mode … ») et ferme les
  lectures en cours au changement de mode ; les clients n'ouvrent rien. « Surveillance » se comporte comme
  « Diffusion » (la détection viendra plus tard). Éteinte veut dire **aucune connexion** : plus de vérification de
  joignabilité non plus, l'état affiché est « Éteinte » (nouvel état `off`). Les caméras déjà enregistrées
  deviennent **Extérieures** : rien ne change pour elles.
- **Automatisations** : déclencheur « Le mode de la maison change » (`Trigger::Mode`), condition « le mode est … »
  (`Condition::Mode`, un ou plusieurs modes) et action « Changer le mode de la maison » (`Action::SetMode`, par
  exemple « à 23:00, passer en Nuit »). Une règle de mode qui change le mode garde un délai d'au moins 10 s :
  deux règles qui se renvoient le mode ne bouclent pas.

### Corrigé

- **Détection** : à l'arrêt propre du serveur, les passages en cours sont clos et écrits (attente de 3 s
  au plus) au lieu de rester « en cours » ; au démarrage, tout événement resté « en cours » est clos quel
  que soit son âge ; la vignette d'un passage en cours est rafraîchie à chaque mise à jour (client de
  bureau et app web, cache du service worker en `v6`) ; `models/detector.json` ne s'applique plus qu'à un
  modèle pris dans `<données>/models/` (jamais à un modèle embarqué) et le journal dit quelle
  configuration est prise.

## [1.1.0] - 2026-11-17

### Ajouté

- **App web : le plan de la maison en lecture** (ticket n° 15). Nouvel onglet « Plan » : les pièces dessinées en SVG (teintées selon leur température moyenne), les appareils placés dessus avec leur état en direct (allumé, éteint, température, ouvert, fermé, injoignable), les caméras et leur champ. Un tap sur un appareil ouvre la même feuille de détail que la page Appareils, avec ses commandes. Zoom au pincement (molette et boutons « + » « − » aussi), déplacement au doigt, bouton « Recentrer », un choix d'étage s'il y en a plusieurs. Le plan arrive déjà par le WebSocket (snapshot et `plan_changed`) : aucune nouvelle route. Pas d'édition (elle reste dans l'application de bureau).

### Modifié

- Écran Appareils (et plan) : le détail d'un appareil s'ouvre dans un panneau à droite, redimensionnable, à côté de la grille de tuiles, au lieu d'une fenêtre modale centrée. La tuile choisie est mise en évidence ; un clic sur une autre tuile change l'appareil, « Fermer » ou Échap referment le panneau. Sur une fenêtre étroite (moins de 800 px), le panneau prend toute la largeur à la place de la grille.

## [1.0.1] - 2026-11-17

### Modifié

- Capteurs sur pile : le rapport de vie de la souscription est demandé toutes les 15 min au plus (au lieu de 5 min), et le minimum passe à 60 s quand le nœud ne suit que des mesures lentes (température, humidité, pression, éclairement, niveau de pile), pour économiser les piles.

## [1.0.0] - 2026-11-13

Version 1 : l'application est séparée en deux parties, **sans aucun cloud**. Le **serveur**
tourne en continu sur un Mac du réseau local (service launchd, sans fenêtre) : il pilote les
appareils Matter, exécute les automatisations et garde les données. Le **client** (l'application
de bureau) affiche et commande, sans stocker de données. Reprise des données de la v0.2.1 sans
réappairer un seul appareil : [docs/migration.md](docs/migration.md). Installation du serveur :
[docs/installation-serveur.md](docs/installation-serveur.md).

### Ajouté

- **Image fixe des caméras** (ticket n° 12). `GET /api/cameras/{id}/snapshot` rend un JPEG récent,
  fabriqué en Rust pur (décodage VideoToolbox, encodage `jpeg-encoder`, sans FFmpeg). Cache partagé
  de 15 s entre tous les clients, une seule capture à la fois par caméra, délai de 12 s ; si la
  caméra est déjà regardée la capture se joint à la session en cours, sinon elle ouvre une session
  courte (attend une image clé puis ferme) en respectant le registre des spectateurs et le verrou de
  la caméra (jamais pendant un test). Le client de bureau et l'app web affichent cette image,
  rafraîchie toutes les 20 s, dans les vignettes qui ne lisent pas de flux (pas de flux réduit, ou
  au-delà de 4 flux). La CSP de l'app web ne change pas (l'image passe par `createImageBitmap`, pas
  par une URL `blob:`). Protocole toujours en version 2. Voir [docs/protocole.md](docs/protocole.md).
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

- **Matter Home Server : refonte du menu et de la fenêtre Caméras** (ticket n° 16, suite du
  n° 13). **Menu** : l'état tient en une ligne (« 8 appareils en ligne » ; « 12 appareils en
  ligne, 1 injoignable » quand il en manque), un sous-menu « Connecter un client » regroupe
  l'adresse et les deux « Copier », puis « Caméras… », « Réglages… » (ex-« Compte et
  réglages… »), « Journal » (ex-« Ouvrir le journal »), « Ouvrir à la connexion » avec sa coche et
  « Quitter ». La ligne de version et la case « Accès web (Tailscale) » restent. Le menu ouvert
  n'est toujours jamais remplacé : les sous-menus se mettent à jour sur place, un changement de
  structure attend la fermeture du menu. **Liste des caméras** : une ligne par caméra (point de
  couleur et nom, seul un problème s'écrit ; le bouton « Modifier… » ouvre le formulaire), l'avertissement de
  sécurité en deux phrases en pied de fenêtre. **Formulaire** : le résultat du test sous chaque
  adresse, « Adresse de la caméra » et « Adresse pour la mosaïque (facultatif) », l'état détaillé
  de la caméra et la note « pas de flux réduit » dans le formulaire, l'aide Aqara repliable, mot
  de passe sur une ligne (« enregistré », « Changer », « Effacer »), et « Supprimer » (avec
  confirmation) dans le formulaire. Rien n'est retiré. L'app serveur utilise le design system
  partagé (`design/tokens.json`) via le crate `matter-home-theme`. L'aide de l'écran de connexion
  du client renvoie à « menu du serveur, Connecter un client ».

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
- Import (`matter-home-server import`) : le rappel dit d'arrêter la v0.2.1 sur **tous** les Mac,
  y compris celui-ci (elle ne prend pas le verrou d'instance), et de partir du Mac qui détient
  les données les plus récentes. Il affiche l'identité du fichier et celle du dossier. Le dossier
  de données est toujours en 0700. Les certificats DER de la sauvegarde sont décodés à la
  validation.
- Un dossier préparé par un import auquel on a renoncé est mis de côté
  (`…import-abandonne-<date>`) au lieu d'être supprimé.

### Corrigé

- **Matter Home Server : le menu de la barre de menus ne répondait plus une fois la fenêtre
  fermée.** « Quitter », « Ouvrir le journal », « Compte et réglages… », les copies et
  « Ouvrir à la connexion » ne faisaient rien, et l'icône, l'infobulle et la ligne d'état ne se
  mettaient plus à jour : eframe 0.37 ne lance plus `ui()` pour une fenêtre cachée, alors que tout
  ce travail s'y faisait. Il se fait désormais dans `App::logic`, que eframe appelle aussi fenêtre
  cachée (réveil par les clics du menu, puis toutes les 2 s). La confirmation « Quitter » s'ouvre
  hors de ce rappel, la fenêtre s'ouvre avec son contenu, et ne reste plus à l'écran, noire et vide, au lancement
  (eframe l'y montrait après sa première image : elle est recachée tout de suite, sans boucler).
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
