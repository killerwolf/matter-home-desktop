# Dépannage

[← Retour au README](../README.md)

## L'application affiche « DÉCONNECTÉ »

Le client ne contacte que votre serveur et ne montre rien sans lui.

- Le serveur tourne-t-il (`matter-home-server`) et l'adresse saisie est-elle la bonne ? Le client
  réessaie seul, de plus en plus lentement (jusqu'à 30 s) ; « Réessayer maintenant » force un essai.
- **« Le certificat du serveur a changé »** : le serveur ne présente plus le certificat approuvé. Si
  vous l'avez réinstallé ou remplacé son dossier de données, comparez la nouvelle empreinte
  (`matter-home-server fingerprint`) avec celle affichée, puis « Oublier l'ancien certificat ». Sinon,
  ne vous connectez pas : un autre appareil se fait peut-être passer pour votre serveur.
- **« Votre session a expiré ou a été révoquée »** : saisissez à nouveau le mot de passe.
- **« Trop de tentatives de connexion »** : le serveur met en attente l'adresse concernée, patientez
  le délai indiqué.
- **« Versions incompatibles »** : mettez à jour l'application ou le serveur.
- Le journal du client (`RUST_LOG=debug`) n'écrit jamais le mot de passe ni le jeton.

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

Vérifiez que l'appareil apparaît sur le réseau (depuis le Mac du serveur) :

```bash
matter-home-server --discover
```

## Rien n'apparaît sur le réseau

- Le Mac doit être sur le même réseau local que les routeurs de bordure Thread (HomePod),
  avec IPv6 actif.
- Au premier lancement, macOS demande l'accès au réseau local : s'il a été refusé, activez
  « Matter Home Desktop » dans Réglages Système → Confidentialité et sécurité → Réseau local.
- `dns-sd -B _matter._tcp local.` doit lister vos appareils.

## Les connexions échouent après début février 2027

Les appareils appairés avec les toutes premières versions ont reçu des certificats valables
100 jours. Beaucoup d'appareils l'ignorent ; sinon, supprimez `controller.json` (voir
[Données](donnees.md)) et réappairez : les nouveaux certificats sont valables 20 ans.

## Journaux détaillés

```bash
RUST_LOG=debug,matc=debug "/Applications/Matter Home Desktop.app/Contents/MacOS/matter-home-desktop"
```

## « Matter Home Desktop est endommagé » au premier lancement

L'application n'est pas signée avec un certificat Apple Developer : voir
la section « Premier lancement » du [README](../README.md) : une commande `xattr -cr` à lancer une fois.
