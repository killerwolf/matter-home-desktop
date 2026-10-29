# Plan de la maison

[← Retour au README](../README.md)

Onglet **« Plan de la maison »** en haut de la fenêtre.

## Consultation

Mode par défaut. Les pièces affichent leur température moyenne et le nombre d'ouvertures ;
chaque appareil placé montre son état en direct :

| Couleur | Signification |
|---|---|
| vert / orange | porte ou fenêtre fermée / ouverte |
| bleu, vert, orange | température < 18 °C, 18–24 °C, > 24 °C |
| jaune / gris foncé | prise allumée / éteinte, avec sa puissance |
| gris | appareil injoignable ou pas encore connu |
| point rouge | pile faible |

Survolez un appareil pour un résumé ; cliquez pour ouvrir sa fiche.

## Modification

Cliquez sur **« ✏ Modifier le plan »** :

- **« + Étage »** ajoute un étage ; le nom de l'étage courant se modifie dans la barre, et
  « Supprimer l'étage » demande une confirmation.
- **« + Pièce »** ajoute un rectangle de 4 × 3 m sur une grille magnétique de 50 cm.
  Déplacez-le en le faisant glisser, redimensionnez-le par son coin bas-droit, renommez-le
  dans la colonne de droite. Les objets posés dans une pièce se déplacent avec elle.
- **Colonne de droite** : glissez sur le plan les appareils pas encore placés, ou des objets
  non connectés (porte, fenêtre, lumière, prise, thermomètre, bouton) pour rendre le plan
  lisible.
- **Objet sélectionné** : un appareil peut être renommé et changer d'icône ; un objet non
  connecté a son propre libellé et pictogramme. « Retirer du plan » ou la touche Suppr
  retire la sélection.

Cliquez sur **« ✔ Terminer »** pour revenir en consultation.

## Vue

Molette ou pincement pour zoomer, glisser dans le vide pour se déplacer, **« Recentrer »**
pour tout voir.

## Sauvegarde

Automatique dans `maison.json` (voir [Données](donnees.md)), à la fin de chaque geste. Le
mode démo utilise `maison-demo.json`, qui part d'un plan d'exemple à deux étages.

## Pièces d'Apple Maison

Elles ne peuvent pas être récupérées automatiquement : Matter ne transporte pas les pièces,
et la base HomeKit n'est accessible qu'à une application native Swift signée avec
l'autorisation HomeKit. Il faut donc dessiner les pièces une fois dans l'application.
