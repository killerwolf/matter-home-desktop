# Détection (personnes, animaux, véhicules)

[← Retour au README](../README.md) · [Modes](modes.md) · [Automatisations](automatisations.md) · [Données](donnees.md)

Ticket n° 18. Une caméra en **Surveillance** dans le mode actuel de la maison (voir [Modes](modes.md))
est analysée en continu par le serveur : quand une **personne**, un **chat**, un **chien** ou un
**véhicule** (voiture, camion, bus, moto, vélo) passe devant, le serveur en fait **un événement** avec
une capture et un clip vidéo (quelques secondes avant, le passage, quelques secondes après). Comme
Frigate, mais **sans enregistrement continu** : rien n'est écrit sur le disque tant que rien ne passe.

## Rien à installer

**L'application du serveur embarque tout** (`Matter Home Server.app`, depuis la version qui
contient ce ticket) : les modèles **YOLOX-s 640** (CoreML) et **YOLOX-nano 416** (processeur), et
**ONNX Runtime 1.23.2** (avec CoreML), tiré des roues officielles publiées par Microsoft sur PyPI
(`onnxruntime`, une par architecture, réunies en une bibliothèque universelle ; **macOS 13**
minimum : sur un système plus ancien, la détection reste désactivée et le dit). Au lancement, le
journal dit `détection prête : CoreML, entrée 640×640` ; la fenêtre du serveur (« Caméras… »,
section « Détection ») affiche le moteur et les caméras analysées.

| Dans l'application | Contenu | Licence |
|---|---|---|
| `Contents/Resources/models/detector.onnx` | `yolox_s.onnx`, version 0.1.1rc0 de `Megvii-BaseDetection/YOLOX` | Apache-2.0 |
| `Contents/Resources/models/detector-cpu.onnx` | `yolox_nano.onnx`, même version | Apache-2.0 |
| `Contents/Frameworks/libonnxruntime.dylib` | `onnxruntime/capi/libonnxruntime.1.23.2.dylib` des roues PyPI `onnxruntime-1.23.2-cp312-cp312-macosx_13_0_{x86_64,arm64}.whl` (`lipo -create`) | MIT |
| `Contents/Resources/licences/` | `YOLOX-LICENSE` (copie dans `assets/licences/`), `onnxruntime-LICENSE` et `onnxruntime-ThirdPartyNotices.txt` (tirés de la roue) | — |

Ces fichiers ne sont **pas** dans le dépôt : `scripts/bundle-server-app.sh --with-detection` (la CI
de publication le passe) les télécharge depuis leurs versions officielles avec
`scripts/fetch-detection-assets.sh`, vérifie leur empreinte **SHA-256 épinglée**
(`scripts/detection-assets.sha256`) et s'arrête au moindre écart. La bibliothèque est signée (ad hoc)
avec l'application, qui scelle aussi les modèles.

### Épingler une empreinte

Une ligne `A_EPINGLER` dans `scripts/detection-assets.sha256` arrête la construction. Pour la
remplir : `scripts/fetch-detection-assets.sh --pin` télécharge les fichiers dans un dossier
temporaire et affiche leurs empreintes ; vérifiez-les (par exemple avec celles publiées par le
projet : `digests.sha256` de `https://pypi.org/pypi/onnxruntime/1.23.2/json` pour les roues ONNX
Runtime, `gh release view <version> -R <dépôt> --json assets` pour GitHub), puis recopiez-les dans
le fichier. La version de YOLOX ne publie pas d'empreinte : celles de `yolox_s.onnx` et `yolox_nano.onnx` ont été relevées au téléchargement.

### Utilisateurs avancés : `<données>/models/`

Sans fichiers embarqués (serveur en ligne de commande, version de développement), le serveur
prend ceux de `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop/models/` (ou
`<dossier>/models/` avec `--data-dir`) : `detector.onnx`, `detector-cpu.onnx`, `detector.json`,
`libonnxruntime.dylib` (cherchée aussi à côté du programme, puis dans `ORT_DYLIB_PATH`). Ceux de
l'application **l'emportent**, et `models/detector.json` ne s'applique **que si le modèle utilisé vient
lui aussi de `<données>/models/`** : il ne change jamais les réglages d'un modèle embarqué (le journal dit
quel `detector.json` est pris, ou que celui de `models/` est ignoré).

- Un fichier qui n'appartient pas à votre compte (ou à root), ou modifiable par le groupe ou les
  autres comptes, est **refusé** (c'est du code que le serveur exécute) : `chmod 644` (`755` pour la
  bibliothèque).
- Une bibliothèque téléchargée par un navigateur porte l'attribut de quarantaine, et macOS peut
  refuser de la charger : `xattr -d com.apple.quarantine models/libonnxruntime.dylib`.
- Un échec de chargement (fichier absent, refusé) est **réessayé** toutes les 5 minutes et à chaque
  changement de mode ou de caméra : un fichier déposé ensuite est pris sans redémarrer.

Sans détecteur, la détection est **désactivée** : le journal le dit une fois
(`détection désactivée : …`), la fenêtre du serveur aussi, et une caméra en Surveillance se comporte
comme en Diffusion (aucune connexion de plus).

### Moteur choisi (banc d'essai sur le Mac serveur)

MacBook Pro 2017 (i7-7820HQ, Radeon Pro 560) : **YOLOX-s 640 sur CoreML** (unités de calcul « ALL »,
format MLProgram) : ~50 ms par image, ~0,4 cœur. Repli : **YOLOX-nano 416 sur le processeur**, 2 fils,
~23 ms. YOLOX-s sur le processeur prend ~330 ms : le serveur ne fait **jamais** tourner
`detector.onnx` sur le processeur (sauf `"engine": "cpu"` dans `detector.json`, pour un petit modèle
seul). Le modèle compilé par CoreML est gardé dans `models/coreml-cache/` (premier démarrage plus long).

Pourquoi `ort` en chargement dynamique : les binaires d'ONNX Runtime que la caisse `ort` sait
télécharger n'existent pas pour les Mac Intel ; en chargeant la bibliothèque au lancement, le
binaire universel reste du seul code Rust (rien à lier, rien à télécharger à la compilation).

### `detector.json` (facultatif)

| Clé | Par défaut | Rôle |
|---|---|---|
| `format` | `auto` | `yolox`, `yolov8` (YOLOv8 / YOLO11) ou `auto` (d'après la forme de la sortie) |
| `input_size` | celle du modèle | taille de l'entrée si le modèle ne la déclare pas |
| `bgr`, `normalize` | selon le format | YOLOX : BGR, valeurs 0 à 255 ; YOLOv8 : RGB divisé par 255 |
| `decode_grid` | auto | YOLOX : sortie brute de la grille (pas 8, 16, 32) à décoder |
| `engine` | `auto` | `coreml`, `cpu` ou `auto` (CoreML, puis le petit modèle sur le processeur) |
| `cpu_threads` | 2 | fils du moteur processeur |
| `floor`, `iou` | 0,25 / 0,45 | score plancher, chevauchement de la suppression des doublons |

Formats lus : YOLOX `1 × N × 85` (cx, cy, w, h, objectness, 80 classes COCO ; score = objectness ×
classe) et YOLOv8/YOLO11 `1 × 84 × N`. L'image est mise à la taille de l'entrée sans déformation,
collée en haut à gauche, bordure grise (114). Un autre modèle s'ajoute dans
`crates/server/src/detection/detector.rs` (trait `Detector`).

## Comment ça marche

1. **Une seule connexion par caméra** : la détection est un spectateur de la même session RTSP que
   le direct et les vignettes (flux réduit s'il existe). Une Aqara G100 échoue au-delà de 2 ou 3
   canaux : on n'en ouvre jamais un de plus. Pendant ce temps, « Tester » est refusé pour cette
   caméra (comme pendant un direct). Une caméra sans aucune classe cochée n'est pas analysée.
2. **Pré-enregistrement** : les ~10 dernières secondes de H.264 brut restent en mémoire (à partir
   d'une image clé, 8 Mo au plus par caméra, même si la caméra n'envoie pas d'image clé), sans
   décodage ni disque.
3. **Mouvement** : le décodeur matériel (VideoToolbox) décode toutes les images mais n'en convertit
   que **2 par seconde** ; chacune est réduite en petite image grise et comparée à la précédente.
4. **Objets** : le modèle ne tourne **que s'il y a du mouvement** (hors masques), et une boîte où
   rien n'a bougé ne compte pas. Un seul modèle sert à toutes les caméras. Chaque objet est suivi
   d'une image à l'autre : immobile depuis **10 s** (voiture garée, chat endormi), il ne compte
   plus, jusqu'à ce qu'il bouge.
5. **Passage** : le premier objet actif ouvre l'événement, **annoncé tout de suite « en cours »**
   (capture, automatisations) ; les suivants le prolongent ; **8 s** sans objet actif le ferment et
   l'événement est complété (meilleure capture, clip). Un passage dure **2 minutes au plus** : il
   est alors fermé et enregistré, et le suivant demande un nouvel objet en mouvement. Le clip =
   pré-enregistrement + passage + ces 8 s, en **MP4 sans réencodage** (les images de la caméra
   telles quelles ; les flux des caméras n'ont pas d'images B, l'ordre de décodage est celui de
   l'affichage).

### Réglages par caméra (fenêtre « Caméras… » du serveur, section « Détection »)

- **Classes** suivies, et **score minimal** par classe (0,5 par défaut).
- **Zones** : si vous en dessinez, seul un objet dont le **bas** (les pieds, les roues) est dans
  une zone compte.
- **Masques** : le mouvement et les objets dedans sont ignorés (arbre, rue, reflet).
- Écriture : un polygone par ligne, en points `x,y` de 0 à 1 (0,0 en haut à gauche) ; par exemple
  la moitié basse de l'image : `0,0.5 1,0.5 1,1 0,1`.

### Charge machine

- 2 images analysées par seconde et par caméra au plus (`fps` dans `detection.json`).
- La détection d'une caméra se **suspend 30 s** (`pause_s`), avec une ligne au journal, si :
  - la file d'attente de l'analyse est pleine (les images sont sautées jusqu'à la prochaine
    image clé) ;
  - l'analyse d'une image prend plus que son budget (500 ms à 2 images/s) 10 fois d'affilée ;
- elle se suspend aussi tant que macOS juge le Mac **trop chaud** (`ProcessInfo.thermalState`
  « sérieux » ou « critique »), relu toutes les 10 s.
- La fenêtre du serveur et les clients disent quelles caméras sont suspendues et pourquoi. Le
  pré-enregistrement, lui, continue. L'écriture des clips (MP4, disque) se fait dans un fil à part.

## Ce qui est enregistré

Dossier `<données>/events/` (droits 0600), trois fichiers par passage :

| Fichier | Contenu |
|---|---|
| `<id>.json` | caméra, classes, meilleur score, début, fin, durée du clip, et la description et son niveau si [elles existent](descriptions.md) |
| `<id>.jpg` | la meilleure image du passage |
| `<id>.mp4` | le clip (avant + passage + après) |

- **Rétention** : 7 jours, et un **plafond total** de 2 Go (`max_total_bytes` dans
  `<données>/detection.json`) au-delà duquel les plus anciens partent ; appliqués au démarrage,
  toutes les heures, et dès qu'un enregistrement dépasse le plafond. Un événement se supprime aussi à
  la main (app web).
- **Disque presque plein** : moins de 2 Go libres (ou 5 % d'un disque de moins de 40 Go) après
  écriture, le clip n'est **pas** enregistré (la fiche et la capture, si) et le journal le dit.
- Une fiche illisible est ignorée, jamais effacée ; un événement resté « en cours » (serveur arrêté
  brutalement pendant un passage) est clos au démarrage suivant, **quel que soit son âge**. À l'arrêt
  propre du serveur, les passages en cours sont clos et écrits (attente de 3 s au plus).
- Les événements **ne sont pas** dans les sauvegardes `.matterhome` (ni les modèles) ; à
  l'import d'une sauvegarde, ceux de ce Mac sont gardés (`events/`, `models/` et `detection.json`
  sont recopiés dans le nouveau dossier de données, comme `serveur/`).

### `detection.json` (facultatif, dans le dossier de données)

```json
{ "max_total_bytes": 2147483648, "fps": 2.0, "close_after_s": 8.0, "pause_s": 30 }
```

Ces réglages sont relus à chaud (sans redémarrer la surveillance).

## Où les voir

- **App web** : onglet Caméras → « Événements » : vignette, classe, caméra, heure ; un tap ouvre la
  capture et lit le clip.
- **Client de bureau** : onglet Caméras, colonne « Événements » avec la capture ; le clip est
  téléchargé dans un dossier temporaire privé et s'ouvre dans le lecteur du Mac (les clips de plus
  d'un jour sont effacés au lancement du client).
- **API** : `GET /api/events?since=&camera=&limit=`, `GET /api/events/{id}`,
  `GET /api/events/{id}/snapshot`, `GET /api/events/{id}/clip` (avec `Range`),
  `DELETE /api/events/{id}`, et l'événement `detection` du WebSocket (voir [Protocole](protocole.md)).
- **Automatisations** : « Quand une caméra détecte… » (voir [Automatisations](automatisations.md)),
  au **début** du passage, ou quand une classe suivie par la règle apparaît en cours de passage.
- **Notifications** sur l'iPhone ou l'iPad, même app fermée : un message par passage, à sa fin
  (avec sa description si elle est attendue) ; le toucher ouvre la fiche (voir
  [Notifications](notifications.md)).

## Vie privée

- Tout reste sur le Mac du serveur : l'analyse est locale (CoreML ou processeur) et **aucune image ne
  part vers un service extérieur**, sauf si vous activez les [descriptions](descriptions.md) (ticket
  n° 19, désactivées par défaut) : la capture d'un événement, et elle seule, est alors envoyée à l'API
  d'Anthropic avec votre clé.
- Seules les caméras en **Surveillance** dans le mode actuel sont analysées. Une caméra **Éteinte**
  n'a aucune connexion, et une caméra en Diffusion n'est pas analysée.
- Les clips et captures sont lisibles par les clients connectés au compte du serveur, comme le
  direct. Ils disparaissent au bout de 7 jours.
- Pas de reconnaissance de visages ni de plaques.
