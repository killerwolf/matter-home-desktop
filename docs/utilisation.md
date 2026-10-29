# Guide d'utilisation

[← Retour au README](../README.md)

## Appairer un appareil

1. App **Maison** (iPhone, iPad ou Mac) → appui long sur l'appareil → **Réglages** (roue
   dentée en bas).
2. Faites défiler, puis touchez **Activer le mode jumelage** (« mode d'appairage » selon la
   version d'iOS). Maison affiche un **code à 11 chiffres** (ex. `1234-567-8901`), valable
   quelques minutes.
3. Dans Matter Home Desktop, collez le code dans « Appairer un appareil » puis cliquez
   **Appairer**. Tirets et espaces sont acceptés ; un code QR `MT:…` fonctionne aussi.

L'appareil reste dans Apple Maison : l'application devient un administrateur de plus, sur sa
propre « fabric » Matter. Pas de Bluetooth : l'appareil est déjà sur le réseau.

Chaque code ne sert qu'une fois, et un seul appairage peut être en cours à la fois.

**Capteurs sur pile** : réveillez-les (appui sur leur bouton) juste avant d'activer le mode
jumelage. Maison affiche le code tout de suite, mais le capteur n'ouvre sa fenêtre qu'à son
réveil, puis la republie via le HomePod. L'application cherche pendant 40 s, puis tente
directement les appareils qui se déclarent appairables. Voir aussi
[Dépannage](depannage.md).

Les appareils **Matter over Wi-Fi** s'appairent de la même façon.

## Fiche d'un appareil

Sélectionnez un appareil dans la liste de gauche. La fiche montre son état (en ligne,
connexion, injoignable avec la prochaine tentative), le niveau de sa pile, puis une section
par type de mesure :

- **Thermomètre** : grande valeur, humidité, mode de réception (souscription ou interrogation
  périodique), choix de la fenêtre (15 min, 1 h, tout l'historique), min / max / moyenne,
  graphique, **Exporter en CSV** (fichier dans `~/Downloads`, colonnes `timestamp_unix`,
  `datetime_local`, `temperature_c`, `humidity_pct`).
- **Détecteur d'ouverture** : « Ouvert » ou « Fermé », depuis quand, nombre d'ouvertures sur
  la fenêtre, derniers changements.
- **Prise** : puissance instantanée, état allumée / éteinte, compteur d'énergie de la prise,
  énergie estimée sur la fenêtre, graphique en escalier.

Boutons de la fiche :

| Bouton | Effet |
|---|---|
| ✏ Personnaliser le nom et l'icône | voir ci-dessous |
| Effacer l'historique | supprime les mesures enregistrées de cet appareil (avec confirmation) |
| Reconnecter | retente la connexion tout de suite |
| Oublier | retire l'appareil de l'application ; il reste dans Apple Maison |

« Oublier » ne retire pas l'application de l'appareil. Pour cela : Maison → appareil →
Réglages, section des services / applications connectés (libellé variable selon iOS).

## Nom et icône

« ✏ Personnaliser le nom et l'icône », sur la fiche ou dans le plan en mode modification :

- **Nom** : « Porte d'entrée », « Fenêtre de la cuisine »… Laissé vide, l'application
  reprend le nom annoncé par l'appareil, rappelé sous le titre de la fiche.
- **Icône** : porte, fenêtre, lumière, prise, thermomètre, bouton, ou « Automatique »
  (déduite de ce que mesure l'appareil). Pour une prise qui alimente une lampe, choisissez
  « Lumière » : le plan affiche une lumière.

Le nom et l'icône s'appliquent partout (liste, fiche, plan, automatisations).

## Options de lancement

| Commande | Effet |
|---|---|
| `matter-home-desktop` | mode normal, avec vos appareils |
| `matter-home-desktop --demo` | appareils simulés, aucune communication réseau |
| `matter-home-desktop --plan` | ouvre directement le plan de la maison |
| `matter-home-desktop --rules` | ouvre directement les automatisations |
| `matter-home-desktop --device X` | ouvre la fiche du premier appareil dont le nom contient X |
| `matter-home-desktop --discover` | liste les appareils Matter visibles sur le réseau, puis quitte |
| `RUST_LOG=debug matter-home-desktop` | journaux détaillés (`RUST_LOG=debug,matc=debug` pour le protocole) |

Les options `--plan`, `--rules` et `--device` se combinent avec `--demo`.

Avec l'application installée depuis le DMG, le programme se trouve dans le paquet :

```bash
"/Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop" --demo --plan
```
