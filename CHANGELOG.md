# Journal des modifications

Les changements notables de chaque version sont décrits ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le projet
respecte le [versionnage sémantique](https://semver.org/lang/fr/).

Les téléchargements de chaque version sont sur la
[page des versions](https://github.com/killerwolf/matter-home-desktop/releases).

## [Unreleased]

### Ajouté

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
