# Journal des modifications

Les changements notables de chaque version sont décrits ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le projet
respecte le [versionnage sémantique](https://semver.org/lang/fr/).

Les téléchargements de chaque version sont sur la
[page des versions](https://github.com/killerwolf/matter-home-desktop/releases).

## [Unreleased]

### Ajouté

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
- **Client branché sur le serveur** (étape 4). Écran de connexion (adresse, identifiant, mot de
  passe) ; à la première connexion l'empreinte SHA-256 du certificat est montrée puis épinglée,
  une empreinte changée est refusée sans contournement silencieux. Le client ne garde plus aucune
  donnée (seulement adresse, empreinte et jeton, en 0600, dans un dossier distinct de la v0.2.1) :
  état complet puis événements numérotés, historique lu à la demande, reconnexion automatique,
  « déconnecté » sans serveur. Appairage, volets, effacement d'historique, personnalisation, plan et
  règles (dernière modification gagnante), export `.matterhome` et CSV, import en deux temps
  (résumé puis confirmation) passent tous par le serveur ; notifications affichées dans
  l'interface.

### Corrigé

- Test du serveur « client lent » : il attend la fin de la rafale de démarrage de la démo avant
  d'ouvrir ses WebSockets (échec sporadique de la file de diffusion saturée).

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
