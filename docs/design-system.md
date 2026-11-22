# Design system

Ce document décrit le langage visuel du client Ostal, issu de la refonte du ticket n° 13.
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

## La palette « Nuit et Lueur »

Quatre couleurs de marque, celles du logo Seuil : **Nuit** `#10232d` (le texte en clair), **Lueur**
`#f5b33f` (la lumière allumée), **Craie** `#f3efe6` (le trait du logo) et **Ardoise** `#0b6a94`
(l'accent : boutons, onglets choisis, liens). Les neutres sont légèrement bleutés, vers Nuit.

- **Lueur veut dire « allumé, tout va bien », jamais « attention ».** Elle sert à une lampe allumée
  (tuile, plan) et au mode **Présent** (pastille du mode : fond `lueur`, texte `on_lueur`).
  `lueur_text` est la Lueur lisible en texte sur `surface` (plus sombre en clair).
- L'alerte (`warn` : porte ouverte, pile faible, connexion en cours) est un orange plus rouge
  (`#b4500e` en clair), pour ne pas se confondre avec Lueur. Un test vérifie l'écart de teinte.
- Contrastes (clair / sombre) : texte 16,1 / 14,2 ; `on_lueur` sur `lueur` 8,8 / 10,2 ;
  `lueur_text` 5,9 / 10,4 ; `warn` 5,1 / 6,6 ; texte sur tuile allumée 14,7 / 9,9. L'icône d'une
  lampe allumée en clair (Lueur sur fond très pâle) reste sous 3:1, comme l'ancien jaune : elle est
  portée par le fond teinté et le mot « Allumée ».

## Le logotype

`assets/logo/ostal-logotype-clair.svg` (mot en Nuit, pour fond clair) et
`ostal-logotype-sombre.svg` (mot en Craie, pour fond sombre) : le logo Seuil suivi du mot « Ostal »
en **Outfit SemiBold (600)**, converti en tracés (aucune police embarquée ni à installer). Outfit est
sous licence SIL Open Font License 1.1, qui ne s'étend pas aux dessins faits avec la police : le
crédit est en commentaire dans les fichiers. L'interface garde la police du système ; Outfit ne
sert qu'au nom. Le logotype est en haut du README et sur l'écran de connexion de l'app web.

## Les tokens (`design/tokens.json`)

| Groupe | Contenu |
|---|---|
| `meta` | version et commentaires ; ignoré par le client et par le générateur CSS |
| `colors.light`, `colors.dark` | `bg` (fond), `surface`, `surface_alt`, `line`, `text`, `text_secondary`, `text_faint` (⋯ et texte désactivé : `#9aa8ae` en clair, 2,4:1 sur `surface`, jamais pour du texte à lire), `accent`, `accent_hover` (accent survolé), `accent_soft`, `accent_text`, `on_accent`, `lueur` (allumé), `on_lueur`, `lueur_text`, `ok`, `warn` (alerte), `on_warn`, `error`, `banner_warn`, `banner_info` (bandeaux), `no_sensor` (gris « pas de capteur »), `series_*` (courbes) |
| (dans `colors`) | `video_bg` et `video_text` : le fond sombre d'une image vidéo et son texte, les mêmes dans les deux thèmes (voulu) ; `series_1` à `series_8` : les courbes |
| `temperature` | `stops` : cinq paliers de 2 °C, du bleu à 16 °C au rouge à 24 °C ; `step` ; `fill_alpha` (transparence de l'aplat d'une pièce) |
| `space` | `xs` 4, `sm` 8, `md` 12, `lg` 16, `xl` 24 |
| `radius` | `sm` 4, `md` 6, `lg` 8, `card` 10 (cartes et bandeaux) |
| `tile` | `width` (180, repli), `desktop_width` (192 : la tuile de l'app de bureau, six colonnes de 192 et 16 d'écart = 1232 px), `min_width`, `height`, `radius`, `icon` (taille de l'icône), `on_tint_alpha` (part de `lueur` dans le fond d'une lampe allumée) |
| `size` | `row` (ligne d'appareil, 30, pour la web app), `dot` (point d'état, 7), `control` (24), `item_gap_y`, `button_pad_x`, `button_pad_y`, `window_desktop_w` / `window_desktop_h` (1280 × 800 : app de bureau), `window_server_w` / `window_server_h` (640 × 700 : fenêtre du serveur) |
| `font` | `family`, `weight` (`normal`, `strong`), `line_height`, `size` (`caption` 11, `body` 13, `title` 15, `heading` 18, `display` 26) |
| `state` | `hover` et `selected` : le nom d'une couleur de `colors` |

L'échelle de température prend le **palier le plus proche** (16 ou moins = bleu, 24 ou plus =
rouge). Les teintes sont identiques en clair et en sombre : l'aplat est posé en transparence sous
le texte. Une pièce sans capteur est grise (`no_sensor`, propre à chaque thème). Les tests de
`theme.rs` vérifient que le JSON se lit, que les paliers se suivent de 2 en 2 sans trou et que les
textes restent lisibles (contraste d'au moins 4,5 environ) sur leurs fonds en clair comme en sombre.

Le client suit le thème du système (clair ou sombre). Pour les essais : `OSTAL_THEME=light`
ou `dark`.

## Les composants

Dans le client : `crates/theme/src/lib.rs`. Ils sont décrits ici par leurs tokens ; la web
app les redessinera elle-même avec les mêmes variables, il n'y a pas de feuille de style de
composants.

- **Tuile d'appareil** (`devices::card`, tokens `tile.*`) : une carte de `tile.min_width` à environ
  `tile.width` de large (les colonnes remplissent la ligne, `devices::columns_for`) et de
  `tile.height` de haut, rayon `tile.radius`, fond `surface`, trait fin `line`. En haut à gauche,
  une grande icône d'état de `tile.icon`, dessinée avec des formes (`tile_icons`, jamais un glyphe de
  police) ; en dessous le nom (deux lignes au plus, coupé par « … ») ; en bas la valeur en
  `font.size.heading` + 2, et à droite un petit texte (humidité, « Bouton », « injoignable », « pile
  faible »). Une lampe allumée prend `lueur` à `tile.on_tint_alpha` sur le fond et une icône
  `lueur` ; une porte ou une fenêtre ouverte est en `warn` ; un appareil injoignable est estompé ; une
  pile faible a un petit point `warn` sur l'icône. Un clic agit : bascule (lumière seulement), ouvre, ferme ou arrête
  (volet), ou ouvre le détail (prise, capteur, porte, télécommande, injoignable) ; un second clic
  pendant 400 ms ou pendant une commande est ignoré. Les pièces se rangent en blocs côte à côte
  (`devices::place_blocks`, 4 colonnes au plus par bloc). Les trois points en
  haut à droite (dessinés) et le clic droit ouvrent le détail de toutes les tuiles. La commande est
  montrée aussitôt (contour `accent_text` jusqu'à la réponse) ; un refus remet l'affichage.
  Les ponts sont à la fin, en tuiles plus basses et plus grises.
- **En-tête de pièce** (`room_heading`) : le nom en `font.size.title`, l'étage en `caption`
  discret et, si la pièce a un capteur joignable, une pastille de température moyenne teintée par
  l'échelle de température (la même que le plan).
- **Panneau de détail** (`MatterApp::detail_panel`, `egui::Panel::right`) : un inspecteur à droite de
  la grille (Appareils) ou du plan, de `size.detail_panel` px (360) à l'ouverture, redimensionnable entre
  `detail_panel_min` et `detail_panel_max` ; la grille se réorganise dans la place restante. La
  tuile choisie est en `state.selected` avec un contour `accent_text`. Un clic sur une autre tuile
  change l'appareil ; « Fermer » et Échap ferment (Échap annule d'abord un nom ou une confirmation en
  cours) ; si l'appareil disparaît, le panneau se ferme. Sous `size.detail_narrow` (800 px) de large, le
  panneau prend toute la largeur à la place de la grille (`detail_full`). Un bandeau signale un
  appareil injoignable ; sous l'en-tête tout défile. Quatre blocs : 1. l'en-tête (grande icône, nom, pièce, valeur en grand quand aucune courbe
  ne la montre, interrupteur d'une lumière ou d'une prise) ; 2. la courbe et les périodes,
  seulement si l'appareil en a une (température, ouverture, consommation, volet) : jamais de section
  vide pour une lumière ou une télécommande ; 3. les actions (Reconnecter, Nom et icône, Effacer
  l'historique, Oublier) ; 4. « Infos techniques », repliées (état, pile, type, nœud, fabricant,
  dernière communication, pont). Le plan ouvre le même panneau au clic sur un appareil.
- **Pastille d'état anormal** : un point de `size.dot` et un mot en `caption`, couleur `warn`
  (pile faible) ou `error` (injoignable). Jamais de pastille verte « en ligne ».
- **Vignette** (`thumbnail`) : une image 16/9 de rayon `radius.md`, le nom dessous, la pièce en
  légende (`caption`, `text_secondary`). Seul l'« injoignable » est signalé.
- **Bouton principal** (`primary_button`) : fond `accent`, texte `on_accent`, rayon `radius.md`. Un
  seul par écran (« + Ajouter »).
- **Onglets** (`tab`) : texte seul ; l'onglet choisi a le fond `accent` et le texte `on_accent`.
- **Bandeau** (`banner_frame`) : fond `banner_warn` (non enregistré) ou `banner_info` (un choix
  attend), texte `text`.

Composants de la refonte (fonctions ajoutées au thème, sans changer l'existant ; `Severity` et
`Banner` n'ont pas de nouvelle variante) :

- **Logo Seuil** (`logo(ui, taille)`) : carré arrondi Nuit, arche Craie et point Lueur, dessinés au
  peintre d'après le viewBox 1024 du kit (aucune crate de rendu SVG). Les couleurs de la marque sont
  les mêmes en clair et en sombre. **`logotype(ui)`** : le logo de 28 px puis « Ostal » en gras 18, dans
  la police d'egui (Outfit n'est pas embarquée).
- **Boutons** : `secondary_button` (fond `surface_alt`, trait `line`) et `destructive_button` (trait et
  texte `error` sur `surface`), à côté de `primary_button`.
- **Cartes** : `card_frame` (fond `surface`, trait `line`, rayon `radius.card`) ; `error_banner_frame` et
  `error_banner` (bandeau d'erreur : `surface`, trait `line`, **bordure gauche de 4 px en `error`**).
- **Mot d'état** (`state_word(ui, StateWord, texte)`) : une icône de 14 px et le mot, la couleur ne
  portant jamais seule l'information. `Ok` (coche, `ok`), `Lit` (point `lueur`, texte `lueur_text`),
  `Neutral` (anneau, `text_secondary`), `Watch` (cercle barré, `warn` : « injoignable »), `Error` (croix,
  `error`). `status_pill` reste tel quel.
- **Pastille de mode** (`mode_pill(ui, libellé, présent)`) : Présent en `lueur` (texte `on_lueur`) ; les
  autres modes en `accent_soft`, bordure `accent`, texte `accent_text`.

## La web app consomme `design/tokens.css`

`design/tokens.css` est généré depuis `design/tokens.json` et commité :

```text
cargo run -p ostal --example tokens_css
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
