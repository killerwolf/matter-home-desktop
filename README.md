# Matter Demo — contrôleur Matter over Thread (preuve de concept)

Application de bureau en Rust (egui) qui s'ajoute comme **contrôleur supplémentaire**
(multi-admin) à des appareils Matter over Thread déjà gérés par Apple Maison, puis :

1. appaire un appareil à partir d'un code d'appairage (code manuel à 11 chiffres ou `MT:...`) ;
2. liste les appareils appairés avec leur état (en ligne / connexion / injoignable) et leur dernière communication ;
3. pour un thermomètre : température (et humidité) en direct, graphique de l'historique,
   fenêtre 15 min / 1 h / session, min / max / moyenne, export CSV ;
4. pour un détecteur d'ouverture : ouvert / fermé en direct, depuis quand, nombre d'ouvertures
   et chronologie des derniers changements ;
5. pour une prise : puissance instantanée (W) avec graphique et statistiques, compteur d'énergie
   (kWh), énergie estimée sur la fenêtre, état allumée / éteinte (lecture seule) ;
6. pour tout appareil sur pile : niveau de la pile et alerte de pile faible ;
7. un **plan de la maison** : étages, pièces nommées, appareils et objets placés, avec leur
   état en direct (porte ouverte en orange, température, puissance, pile faible…) ;
8. des **automatisations** « Quand… Si… Alors… » : notifications macOS, marche/arrêt des
   prises et lampes, appels HTTP (webhooks).

## Option retenue : **A — tout en Rust, avec la crate [`matc`](https://crates.io/crates/matc) 0.1.3**

Crates examinées (octobre 2026) :

| Crate | Verdict |
|---|---|
| **`matc` 0.1.3** (tom-code) | **Retenue.** Côté contrôleur : PASE, commissioning on-network (CSR → AddTrustedRootCert → AddNOC → CASE → CommissioningComplete), CASE, lecture, souscription, mDNS intégré. Ne vérifie pas l'attestation de l'appareil, ce qui permet d'appairer des produits IKEA certifiés sans télécharger les racines PAA de la CSA. |
| `rs-matter` 0.4.1 | Implémentation surtout **côté appareil** (et embarqué). Pas de contrôleur prêt à l'emploi. Écartée. |
| `matter-controller` 0.18.3 (phunapps) | API très complète (souscriptions avec resouscription automatique, client ICD). En revanche, **l'attestation y est obligatoire** : il faut fournir les racines PAA et CD de production de la CSA, sinon le commissioning d'un produit IKEA échoue. C'est trop de friction pour une preuve de concept. Bonne piste pour la suite. |
| `mat-controller` 1.38 | Inconnue, peu documentée. Non retenue. |

L'option B (compagnon Node.js matter.js) n'a donc pas été nécessaire : il n'y a pas de dossier `sidecar/`.

### Ce que l'application ajoute à matc

- **Décodeur TLV maison** (`src/matter/tlv.rs`) : le décodeur de matc 0.1.3 lit les entiers
  signés sur 1 octet comme non signés. −0,05 °C (int8 −5) deviendrait 2,51 °C. Les réponses
  sont donc décodées à partir des octets bruts.
- **Analyse robuste des codes** (`src/matter/setup_code.rs`) : chiffre de contrôle Verhoeff
  et messages d'erreur clairs. Le décodeur manuel de matc panique si le code est trop court.
- **Découverte mDNS adaptée à Thread** (`src/matter/discovery.rs`) : les routeurs de bordure
  (HomePod) publient les appareils Thread via leur proxy SRP, et la réponse à un parcours
  ne contient souvent que le PTR. On interroge alors l'instance elle-même pour obtenir
  SRV/AAAA. Sans ça, 7 des 17 instances du réseau de test restaient sans adresse.
- **Persistance dans un seul fichier JSON** via une implémentation du trait `CertManager` de matc.
  matc utilise sinon plusieurs fichiers PEM.
- **Copie locale de matc** (`vendor/matc`, branchée par `[patch.crates-io]` dans `Cargo.toml`),
  avec trois modifications repérées par `[matter-demo]` dans le code :
  - délai de retransmission porté de 10 s à **45 s**, avec un renvoi toutes les 5 s au lieu de 3 s
    (`retransmit.rs` pour PASE/CASE, `active_connection.rs` pour les requêtes). Sans ça, les
    capteurs sur pile qui dorment plus de 10 s (MYGGBETT, TIMMERFLOTTE : SII = 15,8 s)
    devenaient injoignables après l'appairage ;
  - certificats valables **20 ans** au lieu de 100 jours (`cert_x509.rs`) ;
  - `Connection::im_subscribe_request_paths` : **une seule souscription** pour plusieurs
    attributs, avec les intervalles choisis (`messages.rs`, `controller.rs`). Un appareil
    Matter ne garantit que 3 souscriptions par fabric : une par appareil suffit.

## Prérequis

- macOS (testé sur macOS 26, Apple Silicon), Rust ≥ 1.87 (`rustup`).
- Le Mac sur le **même réseau local** que les routeurs de bordure Thread, avec IPv6 actif.
  Vérification rapide : `dns-sd -B _matter._tcp local.` liste des appareils.
- Au premier lancement, macOS peut demander l'autorisation d'accès au **réseau local** : acceptez.

## Installation et lancement

```bash
cargo build --release
```

```bash
./target/release/matter-demo --demo
```

```bash
./target/release/matter-demo
```

| Commande | Effet |
|---|---|
| `matter-demo` | Mode normal, réseau réel |
| `matter-demo --demo` | Appareils simulés (thermomètres, porte, prise du frigo), aucune communication réseau |
| `matter-demo --rules` | Ouvre directement l'onglet « Automatisations » (combinable avec `--demo`) |
| `matter-demo --plan` | Ouvre directement l'onglet « Plan de la maison » (combinable avec `--demo`) |
| `matter-demo --discover` | Diagnostic : liste les nœuds Matter vus en mDNS par la pile de l'application, puis quitte |
| `RUST_LOG=debug matter-demo` | Journaux détaillés (`RUST_LOG=debug,matc=debug` pour le protocole) |

Tests et qualité :

```bash
cargo test
```

```bash
cargo clippy --all-targets
```

## Générer le code d'appairage dans l'app Maison

1. App **Maison** (iPhone, iPad ou Mac) → appui long sur l'appareil → **Réglages** (roue dentée en bas).
2. Faites défiler, puis touchez **Activer le mode jumelage** (« mode d'appairage » selon la version d'iOS).
3. Maison affiche un **code à 11 chiffres** (ex. `1234-567-8901`). Il est valable quelques minutes.
4. Dans Matter Demo, collez le code dans « Appairer un appareil » puis cliquez **Appairer**.
   Tirets et espaces sont acceptés. Un code QR `MT:...` fonctionne aussi.

L'appareil reste dans Apple Maison : notre application devient un administrateur de plus,
sur sa propre fabric. Pas de Bluetooth : l'appareil est déjà sur le réseau Thread.

Pour un appareil « sleepy » (capteur sur pile), réveillez-le (appui sur son bouton) juste avant
d'activer le mode jumelage. L'app Maison affiche le code immédiatement, mais l'appareil n'ouvre
sa fenêtre qu'à son réveil, puis la republie via le HomePod : l'application attend jusqu'à 2 min.

## Automatisations

Onglet **« Automatisations »** : une liste de règles (case à cocher = activée), un éditeur
et le journal des dernières exécutions.

| | Possibilités |
|---|---|
| **Quand** | une porte / fenêtre s'ouvre ou se ferme ; une mesure (température, humidité, puissance, pile) dépasse ou descend sous un seuil, éventuellement pendant N minutes ; une prise / lampe s'allume ou s'éteint ; un appareil reste injoignable N minutes ; à une heure donnée, certains jours |
| **Si** (toutes) | plage horaire (peut passer minuit : 22:00 → 07:00) ; porte ouverte / fermée ; prise allumée / éteinte ; mesure au-dessus / en dessous d'une valeur |
| **Alors** (dans l'ordre) | notification macOS ; allumer / éteindre / inverser une prise ou lampe (cluster On/Off) ; appeler une URL (GET, ou POST si un corps est saisi ; JSON si le corps commence par `{`) ; attendre N secondes |

- **Variables** dans les textes des notifications et webhooks : `{appareil}`, `{valeur}`,
  `{heure}`, `{regle}`.
- **Anti-rafale** : « pas plus d'une exécution toutes les N minutes » par règle (1 min par
  défaut). Un seuil ne se déclenche qu'une fois tant qu'il reste franchi ; il se réarme quand
  la mesure repasse de l'autre côté.
- **Front uniquement** : la première valeur connue d'un appareil (démarrage, reconnexion) et
  les mesures relues de l'historique ne déclenchent rien ; un seuil déjà franchi au démarrage
  se déclenche à la première mesure reçue.
- **« ▶ Tester »** exécute les actions tout de suite, sans vérifier « Quand » ni « Si ».
- **Sauvegarde** : `regles.json` dans les données locales (mode démo : `regles-demo.json`,
  avec trois règles d'exemple).
- **Fonctionnement** : le moteur reçoit une copie de chaque événement du backend, tient
  l'état courant des appareils et lance les actions dans des tâches séparées. Les commandes
  On/Off passent par la session Matter déjà ouverte avec l'appareil (partagée avec la
  souscription). Notifications : `osascript` ; webhooks : `curl` (fournis par macOS, textes
  passés en arguments, sans shell). Délai de 30 s par commande, 10 s par appel HTTP.
- **Limites** : les règles ne tournent que **tant que l'application est ouverte** (un mode
  service sans fenêtre serait l'étape suivante) ; les notifications apparaissent au nom de
  « Éditeur de script » / osascript ; pas encore de déclencheur « appui sur le bouton »
  (événements Matter à gérer).

## Nom et icône des appareils

Sur la fiche d'un appareil, **« ✏ Personnaliser le nom et l'icône »** (ou dans l'inspecteur du
plan en mode modification) :

- **Nom** : « Porte d'entrée », « Fenêtre de la cuisine »… Laissé vide, l'application reprend
  le nom annoncé par l'appareil, rappelé sous le titre de la fiche.
- **Icône** : porte, fenêtre, lumière, prise, thermomètre, bouton, ou « Automatique »
  (déduite de ce que mesure l'appareil). Utile pour une prise connectée qui alimente une
  lampe : choisissez « Lumière » et le plan affiche une lumière.

Le nom et l'icône s'appliquent partout (liste, fiche, plan) et sont sauvegardés dans
`controller.json` (`custom_name`, `icon`) ; la relecture de la description de l'appareil ne
les écrase pas. En mode démo, ils durent jusqu'à la fermeture.

## Fonctionnement

```
┌─────────────── thread UI (egui) ────────────────┐      ┌──── thread « matter-backend » (tokio) ────┐
│ ui/mod.rs        liste, appairage, journal      │ Cmd  │ matter/mod.rs   boucle de commandes,       │
│ ui/thermometer   valeur, stats, graphique, CSV  │────▶ │                 appairage                  │
│ history.rs       tampon circulaire (5000 pts)   │      │ matter/node.rs  1 tâche par nœud : CASE,   │
│                                                 │ ◀────│                 description, souscription, │
│                                                 │Event │                 reconnexion                │
└─────────────────────────────────────────────────┘      │ matter/discovery.rs  cache mDNS            │
                                                         └────────────────────────────────────────────┘
```

- L'interface ne fait **aucune** entrée/sortie réseau. Elle envoie des `Command`
  (canal tokio non bloquant) et lit les `Event` (`std::sync::mpsc`, `try_recv`) à chaque image.
  Le backend réveille l'interface à chaque événement.
- Chaque attente réseau est bornée par un délai : 60 s par requête, 150 s pour une connexion.
  Un appareil endormi ou disparu ne bloque donc ni l'interface ni les autres appareils.
- **Mesures** : après l'appairage, l'application lit la description de l'appareil
  (Descriptor : endpoints et clusters) puis souscrit, en une seule souscription, aux
  attributs qu'elle sait afficher :

  | Cluster | Attribut | Conversion |
  |---|---|---|
  | Temperature Measurement `0x0402` | MeasuredValue `0x0000` | int16, centièmes de °C (2150 → 21,50 °C) |
  | Relative Humidity `0x0405` | MeasuredValue `0x0000` | uint16, centièmes de % |
  | Boolean State `0x0045` | StateValue `0x0000` | `true` = contact = **fermé** |
  | Power Source `0x002F` (si FeatureMap « BAT ») | BatPercentRemaining `0x000C`, BatChargeLevel `0x000E` | demi-pourcents (200 = 100 %) ; niveau 1-2 = pile faible |
  | Electrical Power Measurement `0x0090` | ActivePower `0x0008` | int64 en mW → W |
  | Electrical Energy Measurement `0x0091` | CumulativeEnergyImported `0x0001` | structure, champ 0 en mWh → kWh |
  | On/Off `0x0006` | OnOff `0x0000` | booléen (lecture seule) |

  Intervalle demandé : 10 s mini, 300 s maxi ; un appareil sur pile peut imposer plus long
  (le TIMMERFLOTTE annonce 300 s). Si la souscription échoue, l'application interroge
  l'appareil toutes les 30 s. Quand l'application apprend à suivre de nouveaux clusters,
  la description des nœuds déjà appairés est relue automatiquement (`interview_version`).
- **Robustesse** : sans rapport pendant MaxInterval + 30 s, ou après deux lectures ratées,
  le nœud passe « Injoignable ». L'application réessaie alors automatiquement avec un délai
  croissant de 5 s à 120 s. Le bouton « Reconnecter » force un essai immédiat. À la
  reconnexion, l'adresse connue est essayée d'abord, puis une redécouverte mDNS opérationnelle.
- **Persistance** : `~/Library/Application Support/dev.matter-demo.MatterDemo/controller.json`
  (droits 0600). Il contient l'ID de fabric, les clés et certificats (racine et contrôleur)
  et la liste des nœuds (nom, dernière adresse, endpoints des capteurs).
- **Historique des mesures sur disque** : `history/<nœud>.jsonl` dans le même dossier, une
  ligne JSON par mesure ou changement d'état (`{"t":…,"k":"temperature","v":21.5}`),
  ajoutée au fil de l'eau par un thread dédié et **relue au démarrage**. Garde-fous contre
  le grossissement :
  - **rétention de 7 jours** : les mesures plus anciennes sont retirées ;
  - **5 Mo maximum par appareil** : au-delà, les plus anciennes lignes sont retirées ;
  - nettoyage au démarrage puis toutes les 6 h (le journal indique le bilan) ;
  - bouton **« Effacer l'historique »** sur la fiche d'un appareil (avec confirmation) ;
  - « Oublier » un appareil efface aussi son historique ; les fichiers d'appareils
    inconnus sont supprimés au démarrage.

  Les limites sont des constantes de `src/history_store.rs` (`RETENTION`,
  `MAX_BYTES_PER_NODE`). L'interface garde au plus 5000 points par grandeur : pour une
  mesure très fréquente (puissance), seules les plus récentes sont affichées. Le mode démo
  n'écrit rien sur disque. Fichiers en droits 0600 (les heures d'ouverture des portes
  renseignent sur la présence dans la maison).
- **Export CSV** : bouton « Exporter en CSV ». Le fichier part dans `~/Downloads`
  (`matter-<nom>-<date>.csv`) avec les colonnes `timestamp_unix, datetime_local,
  temperature_c, humidity_pct`.

## Plan de la maison

Onglet **« Plan de la maison »** en haut de la fenêtre.

- **Consultation** (par défaut) : les pièces affichent leur température moyenne et le nombre
  d'ouvertures ; chaque appareil placé montre son état en direct :

  | Couleur | Signification |
  |---|---|
  | vert / orange | porte ou fenêtre fermée / ouverte |
  | bleu, vert, orange | température < 18 °C, 18–24 °C, > 24 °C |
  | jaune / gris foncé | prise allumée / éteinte, avec sa puissance |
  | gris | appareil injoignable ou pas encore connu |
  | point rouge | pile faible |

  Survolez un appareil pour un résumé ; cliquez pour ouvrir sa fiche détaillée.
- **« ✏ Modifier le plan »** :
  - « + Étage », renommer ou supprimer l'étage courant (avec confirmation) ;
  - « + Pièce » : rectangle de 4 × 3 m sur une grille magnétique de 50 cm ; déplacez-le en le
    faisant glisser, redimensionnez-le par son coin bas-droit, renommez-le dans la colonne de
    droite. Les objets posés dans une pièce se déplacent avec elle ;
  - colonne de droite : **glissez** les appareils pas encore placés, ou des objets non
    connectés (porte, fenêtre, lumière, prise, thermomètre, bouton), sur le plan ;
  - un appareil sélectionné peut être renommé et changer d'icône directement depuis la
    colonne de droite (voir « Nom et icône ») ; un objet non connecté a son propre libellé et
    pictogramme. « Retirer du plan » ou la touche Suppr retire la sélection.
- **Vue** : molette ou pincement pour zoomer, glisser dans le vide pour se déplacer,
  « Recentrer » pour tout voir.
- **Sauvegarde** : automatique dans `maison.json` (même dossier que `controller.json`),
  à la fin de chaque geste. Le mode démo utilise `maison-demo.json` et part d'un plan
  d'exemple à deux étages. Écart volontaire avec la règle de départ « seuls les
  identifiants sont sauvegardés » : un plan doit être conservé.
- **Pièces d'Apple Maison** : elles ne sont pas récupérables. Matter ne transporte pas les
  pièces, et la base HomeKit n'est accessible qu'à une application native Swift signée avec
  l'autorisation HomeKit. Piste future : un petit exportateur HomeKit qui associerait les
  appareils par numéro de série.

## Choix faits en autonomie (option la plus simple qui fonctionne)

- **matc** plutôt que matter-controller : pas d'attestation, donc pas de racines CSA à gérer.
- **Socket UDP `[::]:0`** : IPv6 double pile, port éphémère. Les appareils Thread ne sont joignables qu'en IPv6.
- **Ordre des adresses** : IPv6 globale, puis ULA (`fd..`), puis IPv4. Les adresses lien-local
  sont écartées : elles sont inutilisables sans identifiant d'interface.
- **Identifiant de nœud** tiré au hasard à l'appairage. Le contrôleur a l'ID 112233 sur une
  fabric créée au premier lancement.
- **Nom affiché** : NodeLabel, sinon ProductName (cluster Basic Information), sinon le nom mDNS, sinon l'ID.
- **Export CSV** vers `~/Downloads`, sans boîte de dialogue (moins de dépendances).
- **Graphique** : température seule. L'humidité n'a pas la même échelle ; elle est affichée en valeur et en statistiques.
- **Prise** : graphique de puissance en escalier (la valeur ne change qu'à chaque rapport).
  L'énergie « sur la fenêtre » est intégrée à partir des puissances reçues ; le compteur
  en kWh est celui de la prise. La marche/arrêt se commande par les automatisations.
- **Historique des ouvertures** : 1000 derniers changements affichés (relus du disque au démarrage).
- **« Oublier »** retire l'appareil de l'application seulement. Il garde notre fabric : on peut
  la supprimer dans Maison → appareil → Réglages, section des services/applications
  connectés (le libellé dépend de la version d'iOS).
- **Pas de rafraîchissement automatique de la description** : elle est lue une fois après l'appairage puis mémorisée.

## Limites et problèmes connus

- **Commissioning non testé sur un appareil réel** lors du développement : un code de
  l'app Maison est nécessaire. Ont été vérifiés sur le vrai réseau : la découverte mDNS (les
  17 instances, adresses IPv6 Thread comprises), l'ouverture du socket et la création de la
  fabric. Les échanges PASE/CASE/lecture/souscription reposent entièrement sur matc.
- **Pas de vérification d'attestation** : l'application ne vérifie pas que l'appareil est
  un produit certifié. Acceptable pour une preuve de concept sur un réseau domestique, pas pour un produit.
- **Clés en clair** dans `controller.json`, protégé uniquement par les droits 0600. Le
  trousseau macOS serait l'étape suivante.
- **Appareils « sleepy »** : les délais de matc sont fixes (45 s dans la copie locale) et ne
  s'adaptent pas aux paramètres SII/SAI annoncés. Un appareil peut aussi répondre « BUSY »
  pendant quelques secondes : l'application patiente puis se reconnecte seule.
- **Certificats de la fabric actuelle** : la fabric créée avant la copie locale de matc a des
  certificats valables 100 jours (jusqu'au ~5 février 2027), déjà installés sur les 4 appareils
  appairés. Beaucoup d'appareils sans horloge fiable l'ignorent ; si les connexions échouent
  après cette date, supprimez `controller.json` et réappairez (les nouveaux certificats valent 20 ans).
- **Souscriptions** : l'intervalle maximal demandé est fixé par matc (30 s). Il n'y a pas
  de resouscription transparente : après une perte de session, la tâche du nœud refait
  CASE puis souscrit à nouveau.
- **Rapports découpés en plusieurs messages** (« chunked ») non gérés : sans effet pour
  les attributs lus ici, qui sont petits.
- **Capteurs sur pile très endormis** : lors des essais, une prise GRILLPLATS et un détecteur
  d'ouverture MYGGBETT (sur pile) se sont appairés du premier coup. Pour un capteur sur pile (SII = 15,8 s), l'app Maison a affiché des codes, mais
  l'appareil n'a jamais annoncé de fenêtre ouverte (`CM=0` en mDNS) : l'appairage échoue alors
  avec « Appareil introuvable ». Diagnostic : `dns-sd -B _matterc._udp local.` puis
  `dns-sd -L <instance> _matterc._udp local.` : une fenêtre ouverte annonce `CM=2` et le
  discriminateur du code. Le capteur de température TIMMERFLOTTE a fini par s'appairer après
  plusieurs essais (température, humidité et souscription OK). En repli, l'application tente PASE directement sur les appareils
  qui s'annoncent appairables (essai qui avait échoué avec les délais d'origine de matc, 10 s).
- **Adresse pas encore connue** : un appareil Thread en mode appairage peut s'annoncer avant que
  le HomePod publie son adresse IPv6. L'application attend alors l'adresse (requêtes SRV/AAAA
  ciblées) au lieu d'échouer avec « aucune adresse IP utilisable ».
- **Un seul appairage à la fois.** Un code de l'app Maison ne sert qu'une fois.
- **Lancement depuis un terminal** : il n'y a pas de bundle `.app` signé.
- Pour **repartir de zéro**, supprimez le fichier `controller.json` ci-dessus. Une nouvelle
  fabric sera créée ; pensez à retirer l'ancienne des appareils dans Maison.

## Organisation du code

```
src/
  main.rs              arguments (--demo, --discover), lancement d'eframe
  events.rs            Command / Event échangés entre l'interface et le backend
  history.rs           tampon circulaire, fenêtres, statistiques, CSV (+ tests)
  persistence.rs       fichiers JSON de l'application (+ test)
  history_store.rs     historique sur disque : ajout, relecture, rétention, taille max (+ tests)
  home.rs              modèle du plan : étages, pièces, objets (+ tests)
  demo.rs              backend simulé pour --demo
  ui/mod.rs            fenêtre, appairage, liste des appareils, journal (+ test de bout en bout du mode démo)
  ui/thermometer.rs    vue thermomètre
  ui/sensors.rs        vues détecteur d'ouverture et prise (consommation)
  ui/plan.rs           plan de la maison : dessin, édition, état en direct (+ tests)
  ui/rules.rs          onglet Automatisations : liste, éditeur, journal (+ test)
  automation.rs        règles, moteur (déclencheurs, conditions), actions (+ tests)
  matter/mod.rs        démarrage du backend, appairage
  matter/node.rs       supervision d'un nœud (CASE, description, souscription, interrogation)
  matter/discovery.rs  cache mDNS commissionable / opérationnel
  matter/certs.rs      génération de l'identité et CertManager JSON
  matter/setup_code.rs codes d'appairage (+ tests)
  matter/tlv.rs        décodeur TLV (+ tests)
  matter/convert.rs    conversion température / humidité (+ tests)
vendor/matc/           copie locale de matc 0.1.3 (délais « sleepy », certificats 20 ans)
```
