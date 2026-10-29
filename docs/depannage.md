# Dépannage

[← Retour au README](../README.md)

## « Appareil introuvable » à l'appairage

L'application n'a vu aucun appareil en mode jumelage correspondant au code.

- Générez un **nouveau code** : chaque code ne sert qu'une fois et expire au bout de
  quelques minutes.
- **Capteur sur pile** : appuyez sur son bouton juste avant et juste après « Activer le mode
  jumelage », et rapprochez-le d'un HomePod.
- Si rien n'y fait, retirez la pile 10 s, remettez-la, attendez une minute et réessayez.
  Vérifiez aussi la mise à jour du firmware dans l'app Maison.

**Diagnostic** : pendant que le mode jumelage est actif, listez les appareils qui se
déclarent appairables, puis le détail de l'un d'eux :

```bash
dns-sd -B _matterc._udp local.
```

```bash
dns-sd -L <instance> _matterc._udp local.
```

Une fenêtre ouverte annonce `CM=2` (ou `CM=1`) et le discriminateur du code. `CM=0` signifie
que la fenêtre n'est pas ouverte : le blocage vient alors de l'appareil ou de l'app Maison.
Lors des essais, un capteur TIMMERFLOTTE est resté à `CM=0` pendant plusieurs tentatives
avant de finalement s'appairer.

## « Échec de l'appairage : le code a été refusé »

Le code est expiré ou déjà utilisé : générez-en un nouveau.

## Appareil « Injoignable »

L'application réessaie seule, avec un délai croissant de 5 s à 2 min ; « Reconnecter »
force un essai immédiat. Un capteur sur pile peut aussi répondre « occupé » (BUSY) quelques
secondes : l'application patiente puis se reconnecte.

Vérifiez que l'appareil apparaît sur le réseau :

```bash
./target/release/matter-home-desktop --discover
```

## Rien n'apparaît sur le réseau

- Le Mac doit être sur le même réseau local que les routeurs de bordure Thread (HomePod),
  avec IPv6 actif.
- Au premier lancement, macOS demande l'accès au réseau local : s'il a été refusé, activez-le
  dans Réglages Système → Confidentialité et sécurité → Réseau local.
- `dns-sd -B _matter._tcp local.` doit lister vos appareils.

## Les connexions échouent après début février 2027

Les appareils appairés avec les toutes premières versions ont reçu des certificats valables
100 jours. Beaucoup d'appareils l'ignorent ; sinon, supprimez `controller.json` (voir
[Données](donnees.md)) et réappairez : les nouveaux certificats sont valables 20 ans.

## Journaux détaillés

```bash
RUST_LOG=debug,matc=debug ./target/release/matter-home-desktop
```
