# Détection (personnes, animaux, véhicules)

[← Retour au README](../README.md) · [Modes](modes.md) · [Automatisations](automatisations.md) · [Données](donnees.md)

Ticket n° 18. Une caméra en **Surveillance** dans le mode actuel de la maison (voir [Modes](modes.md))
est analysée en continu par le serveur : quand une **personne**, un **chat**, un **chien** ou un
**véhicule** (voiture, camion, bus, moto, vélo) passe devant, le serveur en fait **un événement** avec
une capture et un clip vidéo (quelques secondes avant, le passage, quelques secondes après). Comme
Frigate, mais **sans enregistrement continu** : rien n'est écrit sur le disque tant que rien ne passe.

## Ce qu'il faut installer

Le dépôt ne contient **ni le modèle ni le moteur** : ils se déposent dans le dossier `models/` du
dossier de données du serveur (voir [Données](donnees.md)) :
`~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop/models/`, ou
`<dossier>/models/` avec `--data-dir`.

| Fichier | Rôle |
|---|---|
| `detector.onnx` | le modèle principal, exécuté par **CoreML** (GPU) : **YOLOX-s 640** conseillé |
| `detector-cpu.onnx` | facultatif : un modèle léger pour le **processeur**, si CoreML ne démarre pas : **YOLOX-nano 416** |
| `libonnxruntime.dylib` | ONNX Runtime **1.20 ou plus récent**, version macOS universelle de Microsoft (elle contient CoreML) |
| `detector.json` | facultatif : réglages du modèle (voir plus bas) |

1. **ONNX Runtime** : sur la page des versions de `microsoft/onnxruntime` (GitHub), prendre l'archive
   `onnxruntime-osx-universal2-<version>.tgz`, et copier son `lib/libonnxruntime.<version>.dylib` en
   `models/libonnxruntime.dylib`. Le serveur la cherche aussi à côté de son programme, dans
   `Contents/Frameworks` de l'application, puis dans la variable `ORT_DYLIB_PATH`.
2. **Modèle** : YOLOX (licence **Apache-2.0**, version officielle 0.1.1rc0 de
   `Megvii-BaseDetection/YOLOX`) : `yolox_s.onnx` → `models/detector.onnx`, et `yolox_nano.onnx` →
   `models/detector-cpu.onnx`.
3. Redémarrer le serveur, ou changer de mode : il réessaie aussi toutes les 5 minutes. Le journal
   dit `détection prête : CoreML, entrée 640×640` ; la fenêtre du serveur (Réglages, « Détection »)
   affiche le moteur et les caméras surveillées.

Sans ces fichiers, la détection est **désactivée** : le journal le dit une fois
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
   caméra (comme pendant un direct).
2. **Pré-enregistrement** : les ~10 dernières secondes de H.264 brut restent en mémoire (à partir
   d'une image clé, 8 Mo au plus par caméra), sans décodage ni disque.
3. **Mouvement** : le décodeur matériel (VideoToolbox) décode toutes les images mais n'en convertit
   que **2 par seconde** ; chacune est réduite en petite image grise et comparée à la précédente.
4. **Objets** : seulement s'il y a du mouvement (hors masques), ou pendant un passage, le modèle
   cherche les objets. Un seul modèle sert à toutes les caméras.
5. **Passage** : le premier objet sûr ouvre l'événement, les suivants le prolongent, **8 s** sans rien
   le ferment. Le clip = pré-enregistrement + passage + ces 8 s, en **MP4 sans réencodage** (les
   images de la caméra telles quelles), 2 minutes au plus.

### Réglages par caméra (fenêtre « Caméras… » du serveur, section « Détection »)

- **Classes** suivies, et **score minimal** par classe (0,5 par défaut).
- **Zones** : si vous en dessinez, seul un objet dont le **bas** (les pieds, les roues) est dans
  une zone compte.
- **Masques** : le mouvement et les objets dedans sont ignorés (arbre, rue, reflet).
- Écriture : un polygone par ligne, en points `x,y` de 0 à 1 (0,0 en haut à gauche) ; par exemple
  la moitié basse de l'image : `0,0.5 1,0.5 1,1 0,1`.

### Charge machine

- 2 images analysées par seconde et par caméra au plus (`fps` dans `detection.json`).
- Si l'analyse prend du retard (file d'attente pleine), les images sont sautées jusqu'à la prochaine
  image clé et la détection se **suspend 30 s** ; le journal dit `détection suspendue …`, la fenêtre
  du serveur aussi. Le pré-enregistrement, lui, continue.

## Ce qui est enregistré

Dossier `<données>/events/` (droits 0600), trois fichiers par passage :

| Fichier | Contenu |
|---|---|
| `<id>.json` | caméra, classes, meilleur score, début, fin, durée du clip |
| `<id>.jpg` | la meilleure image du passage |
| `<id>.mp4` | le clip (avant + passage + après) |

- **Rétention** : 7 jours, et un **plafond total** de 2 Go (`max_total_bytes` dans
  `<données>/detection.json`) au-delà duquel les plus anciens partent ; appliqués au démarrage puis
  toutes les heures. Un événement se supprime aussi à la main (app web).
- Les événements **ne sont pas** dans les sauvegardes `.matterhome` (ni les modèles) ; à
  l'import d'une sauvegarde, ceux de ce Mac sont gardés (`events/`, `models/` et `detection.json`
  sont recopiés dans le nouveau dossier de données, comme `serveur/`).

### `detection.json` (facultatif, dans le dossier de données)

```json
{ "max_total_bytes": 2147483648, "fps": 2.0, "close_after_s": 8.0, "pause_s": 30 }
```

## Où les voir

- **App web** : onglet Caméras → « Événements » : vignette, classe, caméra, heure ; un tap ouvre la
  capture et lit le clip.
- **Client de bureau** : onglet Caméras, liste des événements avec la capture ; le clip s'ouvre
  dans le navigateur ou le lecteur du Mac.
- **API** : `GET /api/events?since=&camera=&limit=`, `GET /api/events/{id}`,
  `GET /api/events/{id}/snapshot`, `GET /api/events/{id}/clip` (avec `Range`),
  `DELETE /api/events/{id}`, et l'événement `detection` du WebSocket (voir [Protocole](protocole.md)).
- **Automatisations** : « Quand une caméra détecte… » (voir [Automatisations](automatisations.md)).

## Vie privée

- Tout reste sur le Mac du serveur : aucune image ne part vers un service extérieur, l'analyse est
  locale (CoreML ou processeur).
- Seules les caméras en **Surveillance** dans le mode actuel sont analysées. Une caméra **Éteinte**
  n'a aucune connexion, et une caméra en Diffusion n'est pas analysée.
- Les clips et captures sont lisibles par les clients connectés au compte du serveur, comme le
  direct. Ils disparaissent au bout de 7 jours.
- Pas de reconnaissance de visages ni de plaques.
