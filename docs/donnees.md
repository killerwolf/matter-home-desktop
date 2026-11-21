# Données et confidentialité

[← Retour au README](../README.md)

## Ce que le client garde

Depuis la version 1, l'application de bureau (le client) **ne stocke aucune donnée de la
maison** : ni mesures, ni plan, ni règles, ni identité Matter. Sans serveur, elle affiche
« déconnecté ». Elle ne conserve qu'un fichier, `connexion.json`, en 0600 :

```
~/Library/Application Support/dev.ostal.OstalClient/connexion.json
```

| Champ | Contenu |
|---|---|
| `url` | adresse du serveur |
| `fingerprint` | empreinte SHA-256 de son certificat, approuvée à la première connexion |
| `token` | jeton de session (absent après une déconnexion ou un jeton refusé) |

Le mot de passe n'est jamais conservé. **Se déconnecter** (menu Fichier) révoque le jeton sur le
serveur et supprime ce fichier. Ce dossier est distinct de celui de la v0.2.1
(`…MatterHomeDesktop`), que le client ne lit, n'écrit ni ne supprime jamais.

## Après le renommage de « Matter Home » en Ostal

Au premier lancement, Ostal reprend les données de Matter Home **par copie** ; les anciens
dossiers restent tels quels, comme sauvegarde, et ne sont plus lus ensuite.

| Matter Home | Ostal | Repris par |
|---|---|---|
| `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop/` | `…/dev.ostal.OstalServer/` | le serveur (application ou ligne de commande), verrous pris |
| `~/Library/Application Support/dev.matter-home-desktop.MatterHomeClient/connexion.json` | `…/dev.ostal.OstalClient/connexion.json` | l'application de bureau |
| `~/Library/Application Support/Matter Home Server/ouverture-a-la-connexion` | `…/Ostal Server/` | Ostal Server |
| `~/Library/LaunchAgents/dev.matter-home-desktop.server-app.plist` | `dev.ostal.server.plist` | Ostal Server (l'ancien est retiré) |
| `/Library/LaunchDaemons/dev.matter-home-desktop.server.plist` | `dev.ostal.server-daemon.plist` | `install.sh` / `uninstall.sh` |

La copie du serveur n'a lieu que si l'ancien dossier a une identité Matter (`controller.json`)
et le nouveau non. Elle est vérifiée octet par octet avant d'être mise en place d'un seul
renommage ; en cas d'échec, rien n'est démarré et l'ancien dossier n'a pas bougé. Tant qu'Ostal
tourne, il tient aussi le verrou de Matter Home : l'ancienne application ne peut plus démarrer
sur l'ancienne copie (même identité Matter, les deux se disputeraient les appareils). Les
sauvegardes `.matterhome` s'importent toujours. Les journaux de Matter Home
(`~/Library/Logs/Matter Home Server`) restent où ils sont.

## Ce que le serveur garde

Le reste de cette page décrit le dossier de données **du serveur** (`ostal-server`) : toutes
les données restent sur le Mac qui le fait tourner, dans :

```
~/Library/Application Support/dev.ostal.OstalServer/
```

| Fichier | Contenu |
|---|---|
| `controller.json` | identité du contrôleur (fabric, clés et certificats), appareils appairés, noms et icônes choisis |
| `maison.json` | plan de la maison |
| `regles.json` | automatisations |
| `mode.json` | mode global de la maison (`present`, `absent`, `nuit` ou `vacances`) ; inclus dans la sauvegarde `.ostal` |
| `serveur/jeton-mode.json` | **empreinte** (SHA-256, jamais le jeton) du jeton de mode des Raccourcis de l'iPhone, droits 0600 ; propre à ce Mac, jamais exportée (comme le reste de `serveur/`) |
| `cameras.json` | caméras RTSP : nom, adresses, identifiant, emplacement et comportement par mode, et **mot de passe en clair** (droits 0600) ; inclus dans la sauvegarde `.ostal` |
| `history/<appareil>.jsonl` | historique des mesures, une ligne par mesure ou changement d'état |
| `events/<id>.json`, `.jpg`, `.mp4` | événements de détection (ticket n° 18) : fiche, capture, clip ; 7 jours et 2 Go au plus ; **pas** dans la sauvegarde `.ostal` (voir [Détection](detection.md)) |
| `models/` | facultatif (utilisateurs avancés) : modèle de détection et ONNX Runtime pris quand l'application n'embarque pas les siens ; `coreml-cache/` (modèle compilé par CoreML, 0700) ; pas dans la sauvegarde |
| `serveur/descriptions.json` | réglages des [descriptions](descriptions.md) (ticket n° 19) **et clé d'API Anthropic**, en clair, droits 0600 ; propre à ce Mac, **jamais exportée** dans la sauvegarde (la clé se ressaisit sur un autre Mac) |
| `serveur/push-vapid.json` | clé privée VAPID des [notifications](notifications.md) (ticket n° 17), droits 0600 ; propre à ce Mac, **jamais exportée** (une nouvelle clé oublie les abonnements : les appareils se réabonnent) |
| `serveur/push.json` | [notifications](notifications.md) : réglage général, abonnements des appareils (adresse d'abonnement, clés `p256dh` et `auth`, nom, catégories) et appareils déjà signalés pour leur pile, droits 0600 ; **jamais exporté** |
| `detection.json` | facultatif : plafond des événements, images analysées par seconde |

En mode démo, l'application utilise `maison-demo.json` et `regles-demo.json`, et n'écrit
aucun historique.

Les fichiers sont lisibles uniquement par votre compte (droits 0600). Les heures
d'ouverture des portes renseignent sur la présence dans la maison : ne partagez pas ce
dossier.

Le client ne parle qu'à votre serveur, sur votre réseau. Seuls les webhooks que vous configurez dans vos
automatisations envoient des données vers les adresses que vous avez choisies, et, si vous activez les
[descriptions](descriptions.md), la capture des événements choisis part vers l'API d'Anthropic (rien
d'autre, jamais le flux).

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
   fichier `.ostal` (le serveur le produit : appareils, plan, règles, historique). Il est
   créé en 0600.
2. Transférez-le par AirDrop ou clé USB. Il contient les clés privées de votre maison : ne le
   partagez pas et supprimez-le après l'import.
3. Pour importer sur un serveur : **Fichier → Importer des données…**, choisissez le fichier. Le
   serveur le vérifie et le client affiche le résumé (« 12 appareils, 7 pièces, 4 règles,
   sauvegarde du … ») ; après confirmation, le serveur redémarre pour appliquer l'import et le client
   se reconnecte seul.

Pour reprendre les données de la **v0.2.1** (l'application de bureau d'avant le serveur), voir
[migration.md](migration.md).

Les données précédentes du serveur sont gardées dans un dossier voisin « …avant-import-<date> ».
N'utilisez pas deux serveurs en même temps avec la même identité. Ces menus sont désactivés
sans connexion au serveur.

## Sauvegarder et repartir de zéro

- **Sauvegarde** : le menu d'export ci-dessus, ou copiez le dossier. Le fichier le plus précieux est
  `controller.json` : sans lui, il faut réappairer tous les appareils.
- **Repartir de zéro** : arrêtez le service, déplacez (ou supprimez) le dossier de données, puis
  lancez `ostal-server init` et recréez le compte (`create-account`). Le serveur ne crée
  jamais une nouvelle identité tout seul (hors `--demo`), et
  `init` refuse tant que des données ou un dossier « …avant-import-… » portant une identité
  traînent à côté. Pensez à retirer l'ancienne identité des appareils dans l'app Maison. Voir
  [installation-serveur.md](installation-serveur.md).

## Ancien nom

Les premières versions s'appelaient « matter-demo » et rangeaient leurs données dans
`dev.matter-demo.MatterDemo`. Ce dossier est renommé automatiquement au premier lancement de
Ostal ; si le renommage échoue, l'ancien dossier continue d'être utilisé.
