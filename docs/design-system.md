# Design system

Ce document décrit le langage visuel du client Matter Home, issu de la refonte du ticket n° 13.
La source de vérité est **`design/tokens.json`** : le client egui la lit au build, et la future app
web mobile (ticket n° 15) lira `design/tokens.css`, généré depuis ce JSON. On ne copie jamais une
couleur ou une taille ailleurs.

## Principes

1. **Une valeur par appareil.** Une tuile = une icône, un nom et une seule information (19,8 °C,
   Fermée, 86 W).
2. **Le détail à la demande.** Type, nœud, pile, dernière communication et commandes sont dans une
   fenêtre qui s'ouvre au clic (ou aux trois points d'une tuile), jamais sur la tuile. Elle ne
   s'ouvre jamais toute seule.
3. **Rangé par pièce**, avec le même découpage sur tous les écrans (règle unique :
   `HomePlan::room_of`, la première pose d'un appareil qui tombe dans une pièce).
4. **Signaler seulement l'anormal.** On n'écrit pas « En ligne » : seulement « pile faible » ou
   « injoignable » (le même mot sur la liste, le plan et le détail). Un appareil injoignable montre
   « — » à la place de sa dernière mesure.
5. **Une seule navigation.** Les onglets du haut (Plan, Appareils, Températures, Caméras,
   Automatisations) ; l'appairage, rare, est derrière « + Ajouter ».
6. **Léger.** Des aplats (parfois transparents) et des traits fins. Ni flou, ni ombre, ni texture,
   ni animation : tout doit rester simple à dessiner dans egui comme en CSS.

Aucune information ni commande existante n'est supprimée : elle est déplacée ou cachée derrière un
clic.

## La pièce d'un appareil

Une seule règle, `HomePlan::room_of` (crate `protocol`), pour tous les écrans (liste Appareils,
noms des Températures, teinte des pièces du plan) : on parcourt les étages dans l'ordre, puis les
poses de l'appareil dans l'ordre, et la **première pose qui tombe dans une pièce** donne la pièce.
Une pose hors de toute pièce est ignorée, une seconde pose ne change rien, et un appareil posé
seulement hors des pièces est « Sans pièce ». La température d'une pièce est la moyenne de ses
capteurs joignables, arrondie à la décimale affichée ; la teinte se calcule depuis cette valeur
arrondie.

## Les tokens (`design/tokens.json`)

| Groupe | Contenu |
|---|---|
| `meta` | version et commentaires ; ignoré par le client et par le générateur CSS |
| `colors.light`, `colors.dark` | `bg` (fond), `surface`, `surface_alt`, `line`, `text`, `text_secondary`, `accent`, `accent_soft`, `accent_text`, `on_accent`, `ok`, `warn` (alerte), `on_warn`, `error`, `banner_warn`, `banner_info` (bandeaux), `no_sensor` (gris « pas de capteur »), `series_*` (courbes) |
| (dans `colors`) | `video_bg` et `video_text` : le fond sombre d'une image vidéo et son texte, les mêmes dans les deux thèmes (voulu) ; `series_1` à `series_8` : les courbes |
| `temperature` | `stops` : cinq paliers de 2 °C, du bleu à 16 °C au rouge à 24 °C ; `step` ; `fill_alpha` (transparence de l'aplat d'une pièce) |
| `space` | `xs` 4, `sm` 8, `md` 12, `lg` 16, `xl` 24 |
| `radius` | `sm` 4, `md` 6, `lg` 8 |
| `tile` | `width`, `min_width`, `height`, `radius`, `icon` (taille de l'icône), `on_tint` et `on_tint_alpha` (lampe allumée) |
| `size` | `row` (ligne d'appareil, 30, pour la web app), `dot` (point d'état, 7), `control` (24), `item_gap_y`, `button_pad_x`, `button_pad_y` |
| `font` | `family`, `weight` (`normal`, `strong`), `line_height`, `size` (`caption` 11, `body` 13, `title` 15, `heading` 18, `display` 26) |
| `state` | `hover` et `selected` : le nom d'une couleur de `colors` |

L'échelle de température prend le **palier le plus proche** (16 ou moins = bleu, 24 ou plus =
rouge). Les teintes sont identiques en clair et en sombre : l'aplat est posé en transparence sous
le texte. Une pièce sans capteur est grise (`no_sensor`, propre à chaque thème). Les tests de
`theme.rs` vérifient que le JSON se lit, que les paliers se suivent de 2 en 2 sans trou et que les
textes restent lisibles (contraste d'au moins 4,5 environ) sur leurs fonds en clair comme en sombre.

Le client suit le thème du système (clair ou sombre). Pour les essais : `MATTER_HOME_THEME=light`
ou `dark`.

## Les composants

Dans le client : `crates/client/src/ui/theme.rs`. Ils sont décrits ici par leurs tokens ; la web
app les redessinera elle-même avec les mêmes variables, il n'y a pas de feuille de style de
composants.

- **Tuile d'appareil** (`devices::card`, tokens `tile.*`) : une carte de `tile.min_width` à environ
  `tile.width` de large (les colonnes remplissent la ligne, `devices::columns_for`) et de
  `tile.height` de haut, rayon `tile.radius`, fond `surface`, trait fin `line`. En haut à gauche,
  une grande icône d'état de `tile.icon`, dessinée avec des formes (`tile_icons`, jamais un glyphe de
  police) ; en dessous le nom (deux lignes au plus, coupé par « … ») ; en bas la valeur en
  `font.size.heading` + 2, et à droite un petit texte (humidité, « Bouton », « injoignable », « pile
  faible »). Une lampe allumée prend `tile.on_tint` à `tile.on_tint_alpha` sur le fond et une icône
  jaune ; une porte ou une fenêtre ouverte est en `warn` ; un appareil injoignable est estompé ; une
  pile faible a un petit point `warn` sur l'icône. Un clic agit : bascule (lumière, prise), ouvre ou
  ferme (volet), ou ouvre le détail (capteur, porte, télécommande, injoignable). Les trois points en
  haut à droite (dessinés) et le clic droit ouvrent le détail de toutes les tuiles. La commande est
  montrée aussitôt (contour `accent_text` jusqu'à la réponse) ; un refus remet l'affichage.
  Les ponts sont à la fin, en tuiles plus basses et plus grises.
- **En-tête de pièce** (`room_heading`) : le nom en `font.size.title`, l'étage en `caption`
  discret et, si la pièce a un capteur joignable, une pastille de température moyenne teintée par
  l'échelle de température (la même que le plan).
- **Fenêtre de détail** (`MatterApp::detail_window`, `egui::Modal`) : centrée, de `580` px de large et
  de la taille de son contenu, sur un fond assombri ; « Fermer », Échap et un clic à l'extérieur la
  ferment. Quatre blocs : 1. l'en-tête (grande icône, nom, pièce, valeur en grand quand aucune courbe
  ne la montre, interrupteur d'une lumière ou d'une prise) ; 2. la courbe et les périodes,
  seulement si l'appareil en a une (température, ouverture, consommation, volet) : jamais de section
  vide pour une lumière ou une télécommande ; 3. les actions (Reconnecter, Nom et icône, Effacer
  l'historique, Oublier) ; 4. « Infos techniques », repliées (état, pile, type, nœud, fabricant,
  dernière communication, pont). Le plan ouvre la même fenêtre au clic sur un appareil.
- **Pastille d'état anormal** : un point de `size.dot` et un mot en `caption`, couleur `warn`
  (pile faible) ou `error` (injoignable). Jamais de pastille verte « en ligne ».
- **Vignette** (`thumbnail`) : une image 16/9 de rayon `radius.md`, le nom dessous, la pièce en
  légende (`caption`, `text_secondary`). Seul le hors ligne est signalé.
- **Bouton principal** (`primary_button`) : fond `accent`, texte `on_accent`, rayon `radius.md`. Un
  seul par écran (« + Ajouter »).
- **Onglets** (`tab`) : texte seul ; l'onglet choisi a le fond `accent` et le texte `on_accent`.
- **Bandeau** (`banner_frame`) : fond `banner_warn` (non enregistré) ou `banner_info` (un choix
  attend), texte `text`.

## La web app consomme `design/tokens.css`

`design/tokens.css` est généré depuis `design/tokens.json` et commité :

```text
cargo run -p matter-home-desktop --example tokens_css
```

Un test (`tokens_css::tests::committed_css_is_up_to_date`) échoue si le CSS n'est plus à jour. Le
fichier déclare des variables CSS sur `:root`, avec ces préfixes :

| Préfixe | Origine | Exemple |
|---|---|---|
| `--color-*` | `colors.<thème>.<nom>` (`_` devient `-`) ; thème clair par défaut, sombre dans `@media (prefers-color-scheme: dark)` | `--color-text-secondary` |
| `--color-state-*` | `state`, en `var(--color-…)` | `--color-state-hover` |
| `--temp-*` | un palier `at: N` devient `--temp-N` ; `--temp-fill-alpha` | `--temp-18` |
| `--space-*`, `--radius-*`, `--size-*` | groupes du même nom, en `px` | `--space-md` |
| `--font-*` | `--font-family`, `--font-line-height`, `--font-weight-*`, `--font-size-*` | `--font-size-body` |

La web app les utilise telles quelles (`background: var(--color-surface)`) ; on ne modifie jamais
une valeur à la main de ce côté.
