# Données et confidentialité

[← Retour au README](../README.md)

## Ce que le client garde

Depuis la version 1, l'application de bureau (le client) **ne stocke aucune donnée de la
maison** : ni mesures, ni plan, ni règles, ni identité Matter. Sans serveur, elle affiche
« déconnecté ». Elle ne conserve qu'un fichier, `connexion.json`, en 0600 :

```
~/Library/Application Support/dev.matter-home-desktop.MatterHomeClient/connexion.json
```

| Champ | Contenu |
|---|---|
| `url` | adresse du serveur |
| `fingerprint` | empreinte SHA-256 de son certificat, approuvée à la première connexion |
| `token` | jeton de session (absent après une déconnexion ou un jeton refusé) |

Le mot de passe n'est jamais conservé. **Se déconnecter** (menu Fichier) révoque le jeton sur le
serveur et supprime ce fichier. Ce dossier est distinct de celui de la v0.2.1
(`…MatterHomeDesktop`), que le client ne lit, n'écrit ni ne supprime jamais.

## Ce que le serveur garde

Le reste de cette page décrit le dossier de données **du serveur** (`matter-home-server`), qui
est celui de la v0.2.1 : toutes les données restent sur le Mac qui le fait tourner, dans :

```
~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop/
```

| Fichier | Contenu |
|---|---|
| `controller.json` | identité du contrôleur (fabric, clés et certificats), appareils appairés, noms et icônes choisis |
| `maison.json` | plan de la maison |
| `regles.json` | automatisations |
| `history/<appareil>.jsonl` | historique des mesures, une ligne par mesure ou changement d'état |

En mode démo, l'application utilise `maison-demo.json` et `regles-demo.json`, et n'écrit
aucun historique.

Les fichiers sont lisibles uniquement par votre compte (droits 0600). Les heures
d'ouverture des portes renseignent sur la présence dans la maison : ne partagez pas ce
dossier.

Le client ne parle qu'à votre serveur, sur votre réseau. Seuls les webhooks que vous configurez dans vos
automatisations envoient des données vers les adresses que vous avez choisies.

## Historique des mesures

L'historique est écrit au fil de l'eau et relu au démarrage. Pour qu'il ne grossisse pas
indéfiniment :

- les mesures de plus de **7 jours** sont supprimées ;
- chaque appareil est limité à **5 Mo** ; au-delà, les plus anciennes mesures partent ;
- le nettoyage a lieu au démarrage puis toutes les 6 heures ;
- **« Effacer l'historique »** sur la fiche d'un appareil supprime tout son historique ;
- « Oublier » un appareil supprime aussi son historique.

Le client lit l'historique à la demande (au plus 20 000 points par lecture, sur 7 jours) : pour une
mesure très fréquente comme la puissance, seules les plus récentes apparaissent. Ces lectures ne
sont gardées qu'en mémoire, le temps de l'affichage.

## Changer de Mac

1. Dans le client : menu **Fichier → Exporter mes données…**, puis choisissez où enregistrer le
   fichier `.matterhome` (le serveur le produit : appareils, plan, règles, historique). Il est
   créé en 0600.
2. Transférez-le par AirDrop ou clé USB. Il contient les clés privées de votre maison : ne le
   partagez pas et supprimez-le après l'import.
3. Pour importer sur un serveur : **Fichier → Importer des données…**, choisissez le fichier. Le
   serveur le vérifie et le client affiche le résumé (« 12 appareils, 7 pièces, 4 règles,
   sauvegarde du … ») ; après confirmation, le serveur redémarre pour appliquer l'import et le client
   se reconnecte seul.

Les données précédentes du serveur sont gardées dans un dossier voisin « …avant-import-<date> ».
N'utilisez pas deux serveurs en même temps avec la même identité. Ces menus sont désactivés
sans connexion au serveur.

## Sauvegarder et repartir de zéro

- **Sauvegarde** : le menu d'export ci-dessus, ou copiez le dossier. Le fichier le plus précieux est
  `controller.json` : sans lui, il faut réappairer tous les appareils.
- **Repartir de zéro** : supprimez `controller.json`. Une nouvelle identité est créée au
  lancement suivant ; pensez à retirer l'ancienne des appareils dans l'app Maison.

## Ancien nom

Les premières versions s'appelaient « matter-demo » et rangeaient leurs données dans
`dev.matter-demo.MatterDemo`. Ce dossier est renommé automatiquement au premier lancement de
Matter Home Desktop ; si le renommage échoue, l'ancien dossier continue d'être utilisé.
