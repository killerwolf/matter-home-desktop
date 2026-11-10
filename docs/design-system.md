# Design system

Ce document décrit le langage visuel du client Matter Home, issu de la refonte du ticket n° 13.
La source de vérité est **`design/tokens.json`** : le client egui la lit, et la future app web
mobile (ticket n° 15) la lira aussi. On ne copie jamais une couleur ou une taille ailleurs.

## Principes

1. **Une valeur par appareil.** Une ligne = un nom et une seule information (19,8 °C, Fermé, 86 W).
2. **Le détail à la demande.** Type, nœud, pile, dernière communication et commandes sont dans un
   panneau qui s'ouvre au clic, jamais dans la liste.
3. **Rangé par pièce**, avec le même découpage sur tous les écrans.
4. **Signaler seulement l'anormal.** On n'écrit pas « En ligne » : seulement « pile faible » ou
   « hors ligne ».
5. **Une seule navigation.** Les onglets du haut (Plan, Appareils, Températures, Caméras,
   Automatisations) ; l'appairage, rare, est derrière « + Ajouter ».
6. **Léger.** Des aplats (parfois transparents) et des traits fins. Ni flou, ni ombre, ni texture,
   ni animation : tout doit rester simple à dessiner dans egui comme en CSS.

Aucune information ni commande existante n'est supprimée : elle est déplacée ou cachée derrière un
clic.

## Les tokens (`design/tokens.json`)

| Groupe | Contenu |
|---|---|
| `colors.light`, `colors.dark` | `bg` (fond), `surface`, `surface_alt`, `line`, `text`, `text_secondary`, `accent`, `accent_soft`, `accent_text`, `on_accent`, `ok`, `warn` (alerte), `error` |
| `temperature` | `stops` : cinq paliers de 2 °C, du bleu à 16 °C au rouge à 24 °C ; `step` ; `fill_alpha` (transparence de l'aplat d'une pièce) ; `no_sensor` (gris « pas de capteur ») |
| `space` | `xs` 4, `sm` 8, `md` 12, `lg` 16, `xl` 24 |
| `radius` | `sm` 4, `md` 6, `lg` 8 |
| `text` | `caption` 11, `body` 13, `title` 15, `display` 26 |

L'échelle de température prend le **palier le plus proche** (16 ou moins = bleu, 24 ou plus =
rouge). Elle est identique en clair et en sombre : l'aplat est posé en transparence sous le texte.
Une pièce sans capteur est grise. Un test (`theme::tests`) vérifie que le JSON se lit et que les
paliers se suivent de 2 en 2 sans trou.

Le thème suit celui du système (clair ou sombre).

## Les composants

Dans le client : `crates/client/src/ui/theme.rs`. Dans la web app : les mêmes noms de classes, les
mêmes variables.

- **Ligne d'appareil** (`device_row`) : 30 px de haut, rayon `md`. Nom à gauche (coupé par « … »),
  pastille d'anomalie éventuelle, puis la valeur à droite en `title`. Fond `accent_soft` quand la
  ligne est ouverte, `surface_alt` au survol. Un pont est en gris et dit « N appareils ».
- **En-tête de pièce** (`room_header`) : le nom en capitales `caption`, `text_secondary`, sans cadre,
  avec de l'espace au-dessus (`md`).
- **Panneau de détail** (`detail_frame`) : surface `surface`, trait fin `line`, marge `lg`. À droite
  de la liste, redimensionnable. En-tête : pièce (`caption`), nom (`title`), valeur en `display`.
  Puis une grille clé / valeur (État, Pile, Type, Nœud, Dernière communication…), puis les commandes.
- **Pastille d'état anormal** (`status_pill`) : un point de 7 px et un mot en `caption`, couleur
  `warn` (pile faible) ou `error` (hors ligne). Jamais de pastille verte « en ligne ».
- **Vignette** (`thumbnail`) : une image 16/9 de rayon `md`, le nom dessous, la pièce en légende
  (`caption`, `text_secondary`). Seul le hors ligne est signalé.
- **Bouton principal** (`primary_button`) : fond `accent`, texte `on_accent`, rayon `md`. Un seul par
  écran (« + Ajouter »).
- **Onglets** (`tab`) : texte seul ; l'onglet choisi a le fond `accent` et le texte `on_accent`.

## La web app consommera les tokens en variables CSS

La web app (ticket n° 15) lit `design/tokens.json` à son build et produit des variables CSS, une par
token. Le thème clair est la valeur par défaut, le sombre suit `prefers-color-scheme` :

```css
:root {
  --bg: #f6f5f2;  --surface: #ffffff;  --text: #1d1f23;  --text-secondary: #62676f;
  --accent: #0b6a94;  --ok: #1f8f4e;  --warn: #b3590a;  --error: #c4372c;
  --space-sm: 8px;  --radius-md: 6px;  --text-body: 13px;
  --temp-16: #3b7fc4;  --temp-18: #4f9fc0;  --temp-20: #c9a94a;  --temp-22: #e0703a;  --temp-24: #d0483a;
}
@media (prefers-color-scheme: dark) {
  :root { --bg: #1b1b1b; --surface: #242424; --text: #e6e6e6; /* … */ }
}
```

Règle de nommage : `colors.<thème>.<nom>` devient `--<nom>` (les `_` deviennent `-`),
`space.<x>` devient `--space-<x>`, `radius.<x>` devient `--radius-<x>`, `text.<x>` devient
`--text-<x>` et le palier `at: N` devient `--temp-N`. Un petit script de build suffit ; on ne
modifie jamais les valeurs à la main côté web.
