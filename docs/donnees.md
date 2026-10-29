# Données et confidentialité

[← Retour au README](../README.md)

Toutes les données restent sur votre Mac, dans :

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

L'application ne contacte aucun serveur. Seuls les webhooks que vous configurez dans vos
automatisations envoient des données vers les adresses que vous avez choisies.

## Historique des mesures

L'historique est écrit au fil de l'eau et relu au démarrage. Pour qu'il ne grossisse pas
indéfiniment :

- les mesures de plus de **7 jours** sont supprimées ;
- chaque appareil est limité à **5 Mo** ; au-delà, les plus anciennes mesures partent ;
- le nettoyage a lieu au démarrage puis toutes les 6 heures ;
- **« Effacer l'historique »** sur la fiche d'un appareil supprime tout son historique ;
- « Oublier » un appareil supprime aussi son historique.

L'interface affiche au plus 5000 points par mesure : pour une mesure très fréquente comme
la puissance, seules les plus récentes apparaissent.

## Sauvegarder et repartir de zéro

- **Sauvegarde** : copiez le dossier ci-dessus. Le fichier le plus précieux est
  `controller.json` : sans lui, il faut réappairer tous les appareils.
- **Repartir de zéro** : supprimez `controller.json`. Une nouvelle identité est créée au
  lancement suivant ; pensez à retirer l'ancienne des appareils dans l'app Maison.

## Ancien nom

Les premières versions s'appelaient « matter-demo » et rangeaient leurs données dans
`dev.matter-demo.MatterDemo`. Ce dossier est renommé automatiquement au premier lancement de
Matter Home Desktop ; si le renommage échoue, l'ancien dossier continue d'être utilisé.
