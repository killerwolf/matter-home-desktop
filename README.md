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
6. pour tout appareil sur pile : niveau de la pile et alerte de pile faible.

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
  et la liste des nœuds (nom, dernière adresse, endpoints des capteurs). L'historique des
  mesures reste **en mémoire uniquement** et est perdu à la fermeture.
- **Export CSV** : bouton « Exporter en CSV ». Le fichier part dans `~/Downloads`
  (`matter-<nom>-<date>.csv`) avec les colonnes `timestamp_unix, datetime_local,
  temperature_c, humidity_pct`.

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
  en kWh est celui de la prise. Pas de commande marche/arrêt (hors périmètre).
- **Historique des ouvertures** : 1000 derniers changements en mémoire, perdus à la fermeture.
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
  persistence.rs       fichier JSON de l'application (+ test)
  demo.rs              backend simulé pour --demo
  ui/mod.rs            fenêtre, appairage, liste des appareils, journal (+ test de bout en bout du mode démo)
  ui/thermometer.rs    vue thermomètre
  ui/sensors.rs        vues détecteur d'ouverture et prise (consommation)
  matter/mod.rs        démarrage du backend, appairage
  matter/node.rs       supervision d'un nœud (CASE, description, souscription, interrogation)
  matter/discovery.rs  cache mDNS commissionable / opérationnel
  matter/certs.rs      génération de l'identité et CertManager JSON
  matter/setup_code.rs codes d'appairage (+ tests)
  matter/tlv.rs        décodeur TLV (+ tests)
  matter/convert.rs    conversion température / humidité (+ tests)
vendor/matc/           copie locale de matc 0.1.3 (délais « sleepy », certificats 20 ans)
```
