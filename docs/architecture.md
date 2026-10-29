# Architecture

[← Retour au README](../README.md) · [Contribuer](../CONTRIBUTING.md)

## Pile Matter : la crate `matc`

Tout est en Rust. La partie protocole repose sur [`matc`](https://crates.io/crates/matc) 0.1.3,
dont une copie modifiée est dans `vendor/matc`. Crates examinées en octobre 2026 :

| Crate | Verdict |
|---|---|
| **`matc` 0.1.3** | **Retenue.** Côté contrôleur : PASE, commissioning on-network (CSR → AddTrustedRootCert → AddNOC → CASE → CommissioningComplete), CASE, lecture, invocation, souscription, mDNS. Ne vérifie pas l'attestation de l'appareil, ce qui permet d'appairer des produits certifiés sans les racines PAA de la CSA. |
| `rs-matter` 0.4 | Surtout côté appareil (et embarqué), pas de contrôleur prêt à l'emploi. |
| `matter-controller` 0.18 | API très complète, mais attestation obligatoire avec les racines PAA et CD de production : trop de friction ici. Piste pour la suite. |
| `mat-controller` 1.38 | Peu documentée. |

Un compagnon Node.js (matter.js) était prévu en repli ; il n'a pas été nécessaire.

### Ce que l'application ajoute à matc

- **Décodeur TLV** (`src/matter/tlv.rs`) : celui de matc lit les entiers signés sur 1 octet
  comme non signés (−0,05 °C deviendrait 2,51 °C). Les réponses sont décodées depuis les
  octets bruts (`Message::payload`).
- **Codes d'appairage** (`src/matter/setup_code.rs`) : chiffre de contrôle Verhoeff et
  messages clairs ; le décodeur manuel de matc panique sur un code trop court.
- **Découverte mDNS adaptée à Thread** (`src/matter/discovery.rs`) : les HomePod publient
  les appareils Thread via leur proxy SRP, et la réponse à un parcours ne contient souvent
  que le PTR. L'application interroge alors l'instance (SRV, TXT) puis l'hôte (AAAA).
- **Identité dans un seul JSON** (`src/matter/certs.rs`) : implémentation du trait
  `CertManager` de matc, au lieu de ses fichiers PEM.

### Modifications de `vendor/matc`

Branchée par `[patch.crates-io]` dans `Cargo.toml`, chaque modification est repérée par
`[matter-home-desktop]` dans le code :

| Fichier | Modification | Pourquoi |
|---|---|---|
| `retransmit.rs`, `active_connection.rs` | retransmission jusqu'à 45 s (au lieu de 10 s), renvoi toutes les 5 s | les capteurs sur pile dorment jusqu'à ~16 s entre deux réveils |
| `cert_x509.rs` | certificats valables 20 ans (au lieu de 100 jours) | éviter que les appareils refusent le contrôleur au bout de 3 mois |
| `messages.rs`, `controller.rs` | `im_subscribe_request_paths` : une souscription pour plusieurs attributs | un appareil ne garantit que 3 souscriptions par fabric |

## Threads et canaux

```
┌──────────── thread UI (egui) ────────────┐        ┌──────── thread « matter-backend » (tokio) ────────┐
│ ui/mod.rs     liste, appairage, journal  │ Command│ matter/mod.rs      commandes, appairage           │
│ ui/plan.rs    plan de la maison          │──────▶ │ matter/node.rs     1 tâche par nœud : CASE,       │
│ ui/rules.rs   automatisations            │        │                    description, souscription      │
│ history.rs    tampons circulaires        │ ◀──────│ matter/discovery.rs cache mDNS                    │
└──────────────────────────────────────────┘  Event │ automation.rs      moteur de règles               │
                                                     └──────────┬────────────────────────────────────────┘
                                                                │ mesures
                                                     ┌──────────▼──────────┐
                                                     │ thread history-writer│ history/<nœud>.jsonl
                                                     └─────────────────────┘
```

- L'interface ne fait **aucune** entrée/sortie réseau. Elle envoie des `Command` (canal tokio
  non bloquant) et applique les `Event` (`std::sync::mpsc`) dans `App::logic`, appelé même
  fenêtre masquée.
- Chaque attente réseau est bornée : 60 s par requête, 150 s par connexion, 30 s par
  commande. Un appareil endormi ne bloque ni l'interface ni les autres appareils.
- `EventSink::with_tap` envoie une copie de chaque événement au moteur d'automatisations.
- Les commandes On/Off passent par la session CASE déjà ouverte avec l'appareil, partagée
  avec la souscription (`Ctx::set_connection`).
- Le mode démo (`src/demo.rs`) émet les mêmes événements et utilise le même moteur de règles,
  avec un actionneur simulé.

## Appareils et mesures

Après l'appairage, l'application lit la description de l'appareil (Basic Information,
Descriptor : endpoints, clusters, types) puis ouvre **une seule souscription** pour tous les
attributs qu'elle sait afficher :

| Cluster | Attribut | Conversion |
|---|---|---|
| Temperature Measurement `0x0402` | MeasuredValue `0x0000` | int16, centièmes de °C (2150 → 21,50 °C) |
| Relative Humidity `0x0405` | MeasuredValue `0x0000` | uint16, centièmes de % |
| Boolean State `0x0045` | StateValue `0x0000` | `true` = contact = fermé |
| Power Source `0x002F` (FeatureMap « BAT ») | BatPercentRemaining `0x000C`, BatChargeLevel `0x000E` | demi-pourcents (200 = 100 %) ; niveau 1-2 = pile faible |
| Electrical Power Measurement `0x0090` | ActivePower `0x0008` | int64 en mW → W |
| Electrical Energy Measurement `0x0091` | CumulativeEnergyImported `0x0001` | structure, champ 0 en mWh → kWh |
| On/Off `0x0006` | OnOff `0x0000` | booléen ; commandes Off `0x00`, On `0x01`, Toggle `0x02` |
| Window Covering `0x0102` | CurrentPositionLiftPercent100ths `0x000E`, OperationalStatus `0x000A` | centièmes de % fermés (0 = ouvert) → % d'ouverture ; bits 0-1 / 2-3 : arrêt, ouverture, fermeture. Commandes UpOrOpen `0x00`, DownOrClose `0x01`, StopMotion `0x02`, GoToLiftPercentage `0x05` (champ 0 : uint16). La position n'est suivie que si la FeatureMap annonce « PositionAwareLift » |
| Bridged Device Basic Information `0x0039` | Reachable `0x0011` | `false` → l'appareil ponté passe « Injoignable » sans nouvel essai de notre part |

- Intervalle demandé : 10 s minimum, 300 s maximum ; un appareil sur pile peut imposer plus.
  Si la souscription échoue, l'application interroge l'appareil toutes les 30 s.
- Sans rapport pendant MaxInterval + 30 s, ou après deux lectures ratées, le nœud passe
  « Injoignable » ; nouvel essai de 5 s à 120 s. À la reconnexion, l'adresse connue est
  essayée d'abord, puis une redécouverte mDNS.
- Quand l'application apprend à suivre de nouveaux clusters, la description des nœuds déjà
  appairés est relue (`INTERVIEW_VERSION` dans `persistence.rs`).

### Ponts (Aggregator)

Un nœud dont un endpoint a le type Aggregator `0x000E` est un pont : chaque endpoint portant
Bridged Device Basic Information `0x0039` est un appareil ponté, nommé par son NodeLabel
(sinon ProductName), dont les clusters sont cherchés sur lui-même et sur les sous-endpoints
de sa PartsList (appareil composé). Chaque appareil ponté reçoit un **identifiant propre**,
tiré au hasard dans `0xFFFF_FFF0_0000_0000..=0xFFFF_FFFF_FFFF_FFFF`, plage que Matter réserve
et que l'appairage ne tire jamais : interface, plan, règles et historique continuent de
manipuler un `u64` par appareil sans savoir s'il est ponté. L'identifiant, le nom et l'icône
choisis survivent aux relectures (même endpoint) ; un endpoint disparu retire l'appareil.

Une seule session CASE et une seule souscription par pont : `node.rs` route chaque rapport
vers l'appareil de son endpoint (`Routes`), et répercute statut, « vu » et mode du nœud sur
tous ses appareils, sauf « en ligne » pour ceux que le pont déclare injoignables. Les
commandes (`Command::SetCover`, actions des règles) passent par la session du pont.

## Choix techniques

- Socket UDP `[::]:0` : IPv6 double pile, port éphémère.
- Ordre des adresses : IPv6 globale, puis ULA (`fd..`), puis IPv4 ; les lien-local sont
  écartées (inutilisables sans identifiant d'interface).
- Identifiant de nœud tiré au hasard à l'appairage ; le contrôleur a l'ID 112233.
- Nom affiché : nom choisi, sinon NodeLabel, ProductName, nom mDNS, ou l'identifiant.
- Notifications par `osascript`, webhooks par `curl` : présents sur macOS, arguments passés
  sans shell, aucune dépendance HTTP.
- Graphique de puissance en escalier, avec `allow_band(false)` : sinon egui_plot remplace les
  marches verticales par une bande remplie.
- Historique : JSON Lines par appareil, rétention 7 jours, 5 Mo maximum
  (`src/history_store.rs`) ; l'interface garde 5000 points par mesure.

## Limites connues

- Pas de vérification d'attestation des appareils.
- Clés en clair dans `controller.json`, protégé par les droits 0600 (le trousseau macOS
  serait l'étape suivante).
- Délais fixes, non adaptés aux paramètres SII/SAI annoncés par les appareils.
- Rapports découpés en plusieurs messages (« chunked ») non gérés : sans effet pour les
  attributs lus ici.
- Pas d'événements Matter (appuis de bouton), pas de mode service.
- Volets : levée seulement (pas d'inclinaison des lames), pas de position cible affichée ;
  ponts et volets écrits d'après la spécification, pas encore vérifiés avec une TaHoma réelle.

## Organisation du code

```
src/
  main.rs              options de lancement, migration du dossier de données, eframe
  events.rs            Command / Event entre l'interface et le backend
  persistence.rs       fichiers JSON, dossier de données (+ tests)
  history.rs           tampon circulaire, fenêtres, statistiques, CSV (+ tests)
  history_store.rs     historique sur disque : ajout, relecture, rétention (+ tests)
  home.rs              modèle du plan : étages, pièces, objets (+ tests)
  automation.rs        règles, moteur, actions (+ tests)
  demo.rs              backend simulé (--demo) (+ test)
  ui/mod.rs            fenêtre, appairage, liste, fiche, journal (+ test de bout en bout)
  ui/thermometer.rs    vue thermomètre
  ui/sensors.rs        vues ouverture et prise (+ tests)
  ui/plan.rs           plan de la maison (+ tests)
  ui/rules.rs          onglet Automatisations (+ test)
  ui/customize.rs      éditeur nom et icône
  matter/mod.rs        démarrage du backend, appairage, commandes (+ tests)
  matter/node.rs       supervision d'un nœud : CASE, description, souscription
  matter/discovery.rs  cache mDNS (+ tests)
  matter/certs.rs      identité du contrôleur (+ test)
  matter/setup_code.rs codes d'appairage (+ tests)
  matter/tlv.rs        décodeur TLV (+ tests)
  matter/convert.rs    conversions des mesures (+ tests)
vendor/matc/           copie modifiée de matc 0.1.3
scripts/bundle.sh      construction du .app et du DMG
scripts/changelog-section.sh  notes de version tirées de CHANGELOG.md
scripts/make-icon.*    génération de l'icône (assets/icon.png, assets/icon.icns)
scripts/dmg-settings.py           mise en page de la fenêtre du DMG (dmgbuild)
scripts/make-dmg-background.swift fond du DMG (assets/dmg/background*.png)
.github/workflows/     CI : vérifications à chaque push, DMG publié sur un tag v*
docs/                  documentation et captures d'écran
```

## Paquet macOS

`scripts/bundle.sh` assemble `Matter Home Desktop.app` : binaire universel (`lipo`),
`Info.plist`, icône et signature ad hoc (pas de certificat Developer ID). L'`Info.plist`
déclare `NSLocalNetworkUsageDescription` et `NSBonjourServices` (`_matter._tcp`,
`_matterc._udp`) : sans eux, macOS bloque la découverte mDNS d'une application lancée depuis
un `.app`.

Le DMG contient l'application et un raccourci vers `/Applications`. Sa fenêtre (600 × 400,
fond 1x + 2x en TIFF, icônes de 128 px, barres masquées) est décrite par
`scripts/dmg-settings.py` et écrite par dmgbuild, qui produit lui-même le `.DS_Store` : pas
d'AppleScript ni de Finder, donc le même résultat en CI que sur un Mac.
