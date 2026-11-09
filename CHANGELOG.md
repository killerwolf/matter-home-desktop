# Journal des modifications

Les changements notables de chaque version sont décrits ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le projet
respecte le [versionnage sémantique](https://semver.org/lang/fr/).

Les téléchargements de chaque version sont sur la
[page des versions](https://github.com/killerwolf/matter-home-desktop/releases).

## [Unreleased]

### Ajouté

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
