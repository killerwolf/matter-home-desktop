# Guide d'utilisation

[← Retour au README](../README.md)

## Se connecter au serveur

Au premier lancement, l'application demande l'adresse du serveur (par exemple
`192.168.1.20:8443` ou `mac-mini.local`), l'identifiant et le mot de passe du compte créé sur le
serveur (`matter-home-server create-account`).

1. L'application lit le certificat du serveur et affiche son **empreinte SHA-256**. Comparez-la à
   celle affichée par le serveur à son lancement (ou par `matter-home-server fingerprint`), puis
   cliquez **Approuver et se connecter**. Seul ce certificat sera accepté ensuite.
2. Les fois suivantes, la connexion est automatique (jeton conservé, pas le mot de passe).
   Si la session expire ou est révoquée, l'écran de connexion revient avec l'adresse remplie.
3. Si l'empreinte change (serveur réinstallé, ou autre appareil qui se fait passer pour lui), la
   connexion est **refusée** avec les deux empreintes affichées ; un geste explicite est nécessaire
   pour approuver le nouveau certificat.

Sans serveur, l'application affiche « DÉCONNECTÉ » et se reconnecte seule (attente de 1 s
jusqu'à 30 s). Rien n'est affiché ni commandé tant que le serveur n'est pas joint.
**Fichier → Se déconnecter** révoque le jeton et efface les informations de connexion.

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

## Ponts : volets Somfy derrière une TaHoma

Un **pont Matter** (Somfy TaHoma Switch, par exemple) est un seul nœud qui expose plusieurs
appareils. Appairez le pont comme n'importe quel appareil : si la TaHoma est dans Apple
Maison via Matter, ses réglages proposent « Activer le mode jumelage » ; sinon, l'app TaHoma
génère un code d'appairage Matter dans ses réglages.

Après la lecture de sa description, le pont apparaît comme « Passerelle » et **chaque volet
devient un appareil à part entière** : sa propre fiche, son nom (celui donné dans l'app
TaHoma, modifiable ici), sa place sur le plan, ses automatisations. La fiche du pont liste
ses appareils ; la fiche d'un volet indique son pont. « Oublier » n'existe que sur le pont :
il retire tous ses appareils.

Un moteur **io-homecontrol** remonte sa position ; un moteur **RTS** (radio à sens unique)
ne la connaît pas : la fiche le signale, seules les commandes sont possibles.

## Fiche d'un appareil

Sélectionnez un appareil dans la liste de gauche. La fiche montre son état (en ligne,
connexion, injoignable avec la prochaine tentative), le niveau de sa pile, puis une section
par type de mesure :

- **Thermomètre** : grande valeur, humidité, mode de réception (souscription ou interrogation
  périodique), choix de la fenêtre (15 min, 1 h, 7 jours), min / max / moyenne,
  graphique, **Exporter en CSV** (fichier produit par le serveur, enregistré à l'endroit que vous
  choisissez, colonnes `timestamp_unix`, `datetime_local`, `temperature_c`, `humidity_pct`).
- **Détecteur d'ouverture** : « Ouvert » ou « Fermé », depuis quand, nombre d'ouvertures sur
  la fenêtre, derniers changements.
- **Prise** : puissance instantanée, état allumée / éteinte, compteur d'énergie de la prise,
  énergie estimée sur la fenêtre, graphique en escalier.
- **Volet** : ouverture (« Fermé », « Ouvert à 35 % », « Ouvert ») et mouvement en cours,
  boutons **▲ Ouvrir**, **■ Stop**, **▼ Fermer**, curseur « Ouverture » puis **Aller à cette
  position** (0 % = fermé, 100 % = tout ouvert), nombre d'ouvertures et graphique de
  l'ouverture sur la fenêtre. Le résultat d'une commande s'affiche dans le journal.

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
- **Icône** : porte, fenêtre, volet, lumière, prise, thermomètre, bouton, ou « Automatique »
  (déduite de ce que mesure l'appareil). Pour une prise qui alimente une lampe, choisissez
  « Lumière » : le plan affiche une lumière.

Le nom et l'icône s'appliquent partout (liste, fiche, plan, automatisations).

## Options de lancement

| Commande | Effet |
|---|---|
| `matter-home-desktop` | lance l'application et se connecte au serveur |
| `matter-home-desktop --plan` | ouvre directement le plan de la maison |
| `matter-home-desktop --rules` | ouvre directement les automatisations |
| `matter-home-desktop --device X` | ouvre la fiche du premier appareil dont le nom contient X |
| `RUST_LOG=debug matter-home-desktop` | journaux détaillés |

Les appareils simulés et le diagnostic mDNS sont des options du serveur :
`matter-home-server --demo` (compte `demo`, mot de passe affiché une fois, adresse
`127.0.0.1:8443`) et `matter-home-server --discover`.

Avec l'application installée depuis le DMG, le programme se trouve dans le paquet :

```bash
"/Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop" --plan
```
