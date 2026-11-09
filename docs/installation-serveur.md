# Installer le serveur sur un Mac

[← Retour au README](../README.md)

Le **serveur** est un petit programme sans fenêtre qui tourne en permanence sur un Mac de votre
réseau (un ancien Mac fait très bien l'affaire). Il parle à vos appareils Matter, exécute les
automatisations et garde les données. Le **client** (l'application « Matter Home Desktop »)
s'y connecte, depuis ce Mac ou depuis un autre.

Le plus simple : **l'application « Matter Home Server »**. Elle se glisse dans Applications,
vit dans la barre de menus en haut de l'écran et contient le serveur. Tout se règle dans une
petite fenêtre : **ni Terminal ni mot de passe administrateur pour l'installer et l'utiliser**. Deux
exceptions, une seule fois : si vous aviez installé l'ancien service (archive en ligne de commande),
le désinstaller demande le mot de passe administrateur du Mac (voir « Passer du service à
l'application ») ; et si le pare-feu de macOS est activé, il peut demander d'autoriser l'application
(voir « Pare-feu »). L'archive en ligne de commande (service launchd) reste disponible plus bas,
comme option avancée.

## Méthode simple : l'application Matter Home Server

### Installer

1. Sur la [page des versions](https://github.com/killerwolf/matter-home-desktop/releases/latest),
   téléchargez `MatterHomeServer-<version>-universal.dmg` (macOS 10.15 ou plus récent sur Mac
   Intel, macOS 11 ou plus récent sur Apple Silicon). Attention : cette adresse (« dernière
   version ») ne montre **que les versions définitives**. Une **pré-version** (un nom comme
   `v1.0.0-rc.1`) n'y apparaît pas, même publiée : ouvrez la
   [liste complète des versions](https://github.com/killerwolf/matter-home-desktop/releases) et
   choisissez-la là.
2. Ouvrez le DMG et glissez **Matter Home Server** dans **Applications**. Ouvrez-la depuis Applications, pas depuis le DMG :
   depuis l'image disque, l'ouverture à la connexion reste désactivée.
3. Ouvrez l'application. L'application n'est pas signée avec un certificat Apple payant : si macOS
   la refuse (« endommagée » ou « développeur non identifié »), ouvrez Réglages Système →
   Confidentialité et sécurité, descendez jusqu'au message sur Matter Home Server et cliquez sur
   **Ouvrir quand même** (sur les macOS plus anciens, clic droit sur l'application → **Ouvrir**) ;
   en dernier recours, une seule commande dans le Terminal :
   `xattr -cr "/Applications/Matter Home Server.app"`.
4. Une petite maison apparaît dans la barre de menus (pas d'icône dans le Dock). Au premier
   lancement, macOS demande l'accès au **réseau local** : acceptez.

### Première configuration (une seule fois)

Si le dossier de données n'a pas encore d'identité Matter ou de compte, une fenêtre s'ouvre :

1. **Reprendre une sauvegarde** (fichier `.matterhome`, exporté depuis la v0.2.1 ou depuis un
   autre serveur) : l'application affiche le contenu du fichier et la comparaison des identités.
   Cochez que l'ancienne application est arrêtée partout, puis confirmez. Aucun appareil à
   réappairer. L'application ne remplace **jamais** une identité Matter différente sans que vous
   l'ayez demandé explicitement, et ne crée **jamais** une identité sans que vous ayez choisi
   « Créer une nouvelle maison ».
   Ou **Créer une nouvelle maison**, pour une première installation sans aucun appareil.
2. **Créer le compte** : un identifiant et un mot de passe (10 caractères au minimum), saisis
   deux fois.
3. L'écran de fin donne **l'adresse du serveur** (`https://nom-du-mac.local:8443`) et
   **l'empreinte du certificat** : dans le client, saisissez l'adresse, comparez l'empreinte
   affichée avec celle-ci, puis connectez-vous.

Si l'identité et le compte existent déjà (par exemple parce que vous avez déjà utilisé le serveur
sur ce Mac), l'application démarre directement le serveur, sans rien demander ni importer.

### Le menu de la barre de menus

L'icône change de forme : maison pleine (en marche), maison creuse (démarrage), maison avec un
point d'exclamation (problème ou configuration à terminer). Le menu montre l'état (« En marche ·
12 appareils sur 13 en ligne »), les appareils injoignables, l'adresse du serveur, « Copier
l'adresse », « Copier l'empreinte du certificat », « Ouvrir le journal », « Ouvrir à la
connexion », « Compte et réglages… » et « Quitter ». Quitter demande confirmation : les
automatismes s'arrêtent tant que l'application n'est pas rouverte.

### Ouvrir à la connexion, ouverture de session automatique

L'application se relance toute seule à l'ouverture de votre session et après un plantage tant que
la case « Ouvrir à la connexion » est cochée. Elle l'est **par défaut** dès la première fois que le
serveur tourne dans l'application (fin de la configuration, ou premier lancement sur des données
déjà en place, par exemple après le passage depuis le service) ; si vous la décochez, l'application
respecte ce choix aux lancements suivants. Cela passe par un fichier dans `~/Library/LaunchAgents`,
sans droits administrateur. Attention : macOS ne lit ce fichier **qu'à votre prochaine ouverture de
session**. Tant que vous ne vous êtes pas déconnecté puis reconnecté (ou que le Mac n'a pas
redémarré), ni le lancement à la connexion ni la relance après plantage ne sont actifs. De plus,
l'application ne peut démarrer qu'**une fois votre session ouverte**. Pour un Mac qui redémarre sans
personne devant :

1. Réglages Système → Utilisateurs et groupes → **Ouvrir la session automatiquement** : choisissez
   votre compte (macOS demande votre mot de passe, c'est un réglage à faire vous-même ; il est
   indisponible si FileVault est activé).
2. Ce Mac doit rester **allumé et branché**, de préférence en **Ethernet**.

### Pare-feu

Si le pare-feu de macOS est activé (Réglages Système → Réseau → Pare-feu), macOS demande, la
première fois, s'il faut **autoriser les connexions réseau entrantes** pour « Matter Home Server » :
répondez **Autoriser**, sans quoi le client ne pourra pas se connecter. L'application a une
signature « ad hoc » qui change à chaque version : macOS peut reposer la question après une mise
à jour. Si vous avez cliqué sur **Refuser** : Réglages Système → Réseau → Pare-feu → **Options…**,
trouvez Matter Home Server dans la liste (ajoutez-le avec **+** s'il n'y est pas) et réglez-le sur
« Autoriser les connexions entrantes ».

### Veille, capot ouvert, secteur

Tant que l'application tourne, elle empêche le Mac de se mettre en veille tout seul (l'écran peut
s'éteindre). Elle ne peut rien contre un **portable dont on ferme le capot** : il s'endort alors
quoi que l'on fasse, sauf s'il est branché sur secteur avec un écran externe. Laissez le **capot
ouvert** et le Mac **sur secteur**. Réglages Système → Batterie (ou Économiseur d'énergie) →
« Empêcher la mise en veille automatique lorsque l'écran est éteint » aide sur un Mac de bureau,
mais c'est un réglage à faire vous-même.

### Quand l'application ne démarre pas le serveur

Elle le dit dans le menu (et dans « Compte et réglages… ») et ne démarre rien, plutôt que de
risquer deux serveurs sur les mêmes données :

- **Le service installé au Terminal tient les données** (fichier
  `/Library/LaunchDaemons/dev.matter-home-desktop.server.plist`) : voir « Passer du service à
  l'application » ci-dessous.
- **Un autre serveur lancé à la main** : arrêtez-le, l'application démarre alors toute seule.
- **L'ancienne application de bureau v0.2 est ouverte** : elle ne prend pas le verrou, donc
  l'application ne peut pas savoir si elle utilise les données. Fermez-la (sur tous vos Mac),
  le serveur démarre tout seul. L'application reconnaît la v0.2 au chemin réel de l'exécutable
  (pas à son nom) et à la version lue dans son `Info.plist`. Le client Matter Home V1, qui porte le
  même nom de processus mais ne touche pas aux données, ne bloque rien, pas plus qu'un exécutable
  lancé hors d'une application (`cargo run`, `target/debug`).
  **Au démarrage**, un doute suffit à refuser (application déplacée ou remplacée pendant qu'elle
  tourne, `Info.plist` illisible) : fermez-la puis rouvrez-la, nouvel essai toutes les 10 secondes.
  **Pendant que le serveur tourne**, le contrôle est refait toutes les **30 secondes** mais le
  serveur ne s'arrête que sur preuve : une version 0.2.x effectivement lue pour ce processus. Un
  processus nouveau et incertain est noté dans le journal et mentionné dans le menu, sans arrêt.
  Un processus jugé sain n'est plus réévalué tant qu'il tourne.
- **Matter Home Server est déjà ouvert** : une seconde ouverture se ferme sans bruit, cherchez
  l'icône (une seule, même quand le serveur est bloqué).

### Passer du service launchd à l'application

Si vous aviez installé le serveur avec l'archive (service launchd), il faut d'abord le
désinstaller, une seule fois :

1. Dans le Terminal (Applications → Utilitaires), allez dans le **dossier de l'archive** (celui où
   se trouve le fichier `uninstall.sh`) et lancez `./uninstall.sh`. Le script demande le **mot de
   passe administrateur** du Mac (il le demande lui-même ; ne le lancez pas avec `sudo`). Vos
   données et vos journaux restent en place.
2. **Si ce dossier n'existe plus** (archive supprimée, déplacée) : retéléchargez la même archive
   `matter-home-server-<version>-macos.tar.gz` sur la
   [page des versions](https://github.com/killerwolf/matter-home-desktop/releases), ouvrez-la, et
   reprenez au point 1 ; ou tapez ces deux commandes, qui font la même chose (le mot de passe
   administrateur est demandé) :

   ```
   sudo launchctl bootout system/dev.matter-home-desktop.server
   sudo rm -f /Library/LaunchDaemons/dev.matter-home-desktop.server.plist /usr/local/bin/matter-home-server
   ```
3. Ouvrez l'application **Matter Home Server**. Elle reconnaît l'identité et le compte existants
   (même dossier de données, `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop`)
   et démarre directement. Le certificat du serveur reste le même : le client n'a rien à refaire.
   L'ouverture à la connexion est activée par défaut (voir plus haut : elle ne prend effet qu'à
   votre prochaine ouverture de session, et l'application ne démarre qu'une fois la session
   ouverte).

Tant que le service est installé, l'application le dit et ne démarre rien.

### Où sont les choses

- Journal : menu « Ouvrir le journal » (`~/Library/Logs/Matter Home Server/serveur.log`).
- Données : `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop`.
- Ouverture à la connexion : `~/Library/LaunchAgents/dev.matter-home-desktop.server-app.plist`.

### Essayer sans appareil (démo)

`"/Applications/Matter Home Server.app/Contents/MacOS/matter-home-server-app" --demo --data-dir
~/essai-demo --listen 127.0.0.1:0` : appareils simulés, aucun trafic réseau, aucune ouverture à la
connexion (le compte « demo » et son mot de passe sont affichés dans le Terminal).

---

## Option avancée : l'archive en ligne de commande (service launchd)

Réservée à qui préfère le Terminal : le serveur sans interface, installé comme service qui démarre
avec le Mac, **sans session ouverte**. Ne l'utilisez pas en même temps que l'application sur le
même Mac. Cette partie explique tout pas à pas ; vous aurez besoin de l'application **Terminal**
(dans Applications → Utilitaires) : on y tape des commandes ; copiez-collez-les une par une.

> **Une seule instance à la fois.** Ne lancez jamais deux serveurs, ni la v0.2.1 de l'application
> en même temps que le serveur, avec la même identité Matter : leurs abonnements se chassent, les
> règles s'exécutent deux fois et les données peuvent être écrasées. Si vous reprenez les données
> de la v0.2.1, **arrêtez-la sur tous vos Mac**, y compris celui-ci.

## Ce qu'il faut

- Un Mac qui restera **allumé et branché** : macOS 10.15 (Catalina) ou plus récent pour un Mac
  Intel, macOS 11 ou plus récent pour un Mac Apple Silicon. De préférence en **Ethernet**.
- Sur le même réseau que vos HomePod / routeurs de bordure Thread et que le Mac du client.
- Un compte administrateur sur ce Mac (le mot de passe de la session est demandé pour quelques
  gestes).

## 1. Télécharger et ouvrir l'archive

1. Sur la [page des versions](https://github.com/killerwolf/matter-home-desktop/releases/latest),
   téléchargez `matter-home-server-<version>-macos.tar.gz`.
2. Double-cliquez dessus dans le Finder : un dossier `matter-home-server-<version>-macos`
   apparaît à côté (dans Téléchargements, en général).
3. Ouvrez le **Terminal** et placez-vous dans ce dossier :

   ```bash
   cd ~/Downloads/matter-home-server-*-macos
   ```

4. **Toujours, avant de lancer quoi que ce soit**, retirez le blocage de macOS sur les fichiers
   téléchargés :

   ```bash
   xattr -dr com.apple.quarantine .
   ```

Le programme n'est pas signé avec un certificat Apple payant : sans cette commande, macOS refuse
de l'ouvrir (« développeur non identifié » ou « endommagé »), y compris pour les commandes de
l'étape 2. Le dossier contient le programme **`matter-home-server`**, `install.sh`, `uninstall.sh`,
un gabarit de fichier de service et un court `LISEZMOI.txt`.

## 2. Les données : reprendre la v0.2.1, ou partir de zéro

Le serveur travaille sur **votre dossier de données**, le même que celui de la v0.2.1 :

```
~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop
```

(`~` est votre dossier personnel.) Ce dossier contient l'identité de votre maison Matter. Le
serveur ne le crée **jamais** à vide tout seul : sans lui, il refuse de démarrer, pour ne pas
créer une nouvelle maison par erreur (vos appareils ne seraient plus reconnus).

**Vous avez déjà des appareils dans la v0.2.1** (cas général) : suivez
[migration.md](migration.md), en bref : exporter depuis la v0.2.1, arrêter la v0.2.1 partout,
puis, dans le Terminal, dans le dossier de l'archive :

```bash
./matter-home-server import ~/chemin/vers/matter-home-2026-11-09.matterhome
```

**Installation neuve, sans appareil déjà appairé** :

```bash
./matter-home-server init
```

Dans les deux cas, créez ensuite **le compte** (un seul : celui que le client demandera) :

```bash
./matter-home-server create-account --username votre-nom
```

Le mot de passe est demandé sans s'afficher.

## 3. Installer le service

```bash
./install.sh
```

Le script :

1. retire la quarantaine du programme et vérifie qu'il se lance sur ce Mac ;
2. **vous montre** le dossier de données, l'identité trouvée, ce qu'il va installer, et demande
   votre accord ;
3. demande le **mot de passe de votre session** (`sudo`) uniquement pour : copier le programme
   dans `/usr/local/bin`, écrire le fichier de service dans `/Library/LaunchDaemons`, charger le
   service ;
4. vérifie le fichier de service (`plutil -lint`) et **affiche l'empreinte du certificat** du
   serveur (si le serveur a déjà tourné) ;
5. **affiche les commandes à lancer vous-même** pour la veille et le pare-feu (voir ci-dessous) ;
6. propose de démarrer le service.

Pour **essayer sans rien modifier** : `./install.sh --a-blanc` affiche tout ce qui serait fait.

Le service tourne sous **votre compte** (jamais en administrateur) et démarre avec le Mac, sans
qu'il faille ouvrir de session. S'il plante, macOS le relance au bout de 30 secondes.

## 4. Réglages à faire vous-même (le script ne les change jamais)

Le script **ne modifie jamais** la veille ni le pare-feu sans vous : il lit ce qu'il peut sans
droits et affiche les commandes. À lancer dans le Terminal, une fois :

- **Pas de veille** (sinon le serveur s'endort et le client affiche « déconnecté ») :

  ```bash
  sudo pmset -a sleep 0
  sudo pmset -a autorestart 1
  ```

  La seconde commande rallume le Mac tout seul après une coupure de courant. Pour vérifier :
  `pmset -g`. Un Mac portable doit rester branché, capot ouvert.
- **Pare-feu** (Réglages Système → Réseau → Pare-feu). S'il est activé, autorisez le serveur :

  ```bash
  sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add /usr/local/bin/matter-home-server
  sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp /usr/local/bin/matter-home-server
  ```

  Un programme sans service graphique ne peut pas afficher la fenêtre « Autoriser les connexions
  entrantes » : sans ces deux commandes, le client peut ne pas réussir à se connecter.
- **FileVault** (chiffrement du disque) : si le disque est chiffré, après un **redémarrage** du Mac
  le service ne démarre qu'une fois le Mac déverrouillé (mot de passe saisi à l'écran). Prévoyez de
  le faire après chaque redémarrage, ou laissez FileVault désactivé si le Mac est dans un endroit
  sûr.
- **Réseau local** (Réglages Système → Confidentialité et sécurité → Réseau local) : le **client**
  doit y être activé (macOS le demande au premier lancement : acceptez, sinon il ne voit rien).
  Pour le **serveur**, regardez si « matter-home-server » figure dans cette liste et laissez-le
  activé. Un programme lancé en service n'affiche pas de fenêtre de demande, et ce réglage n'a pas
  pu être vérifié sur tous les macOS : si le serveur ne voit aucun appareil, c'est avec le pare-feu
  la première chose à regarder.
- **Adresse** : réservez une adresse fixe à ce Mac dans votre box, ou utilisez son nom en
  `.local` (Réglages Système → Général → Partage → nom local), pour ne pas la changer dans le client.

## 5. Se connecter avec le client

Dans l'application « Matter Home Desktop », saisissez l'adresse du serveur
(`https://nom-du-mac.local:8443`, ou son adresse IP), l'identifiant et le mot de passe du compte.

À la première connexion, le client affiche **l'empreinte du certificat** du serveur. Comparez-la
avec celle affichée par `install.sh`, ou donnez-la vous-même :

```bash
matter-home-server fingerprint
```

Si elles sont identiques, acceptez : le client ne fera plus confiance qu'à ce certificat.

## Où sont les choses

| | |
|---|---|
| Le programme | `/usr/local/bin/matter-home-server` |
| Le fichier de service | `/Library/LaunchDaemons/dev.matter-home-desktop.server.plist` |
| Vos données | `~/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop` |
| Le journal | `~/Library/Logs/MatterHomeServer/server.log` (3 anciens fichiers gardés, droits restreints) |
| Journal de secours | `~/Library/Logs/MatterHomeServer/launchd.log` (presque vide : seulement ce qui s'affiche avant l'ouverture du journal normal ; `install.sh` le met de côté s'il dépasse 1 Mo) |

Dans le Terminal, `matter-home-server --version` donne la version. Pour suivre ce que fait le
serveur en direct : `tail -f ~/Library/Logs/MatterHomeServer/server.log` (Ctrl-C pour quitter).

## Arrêter, démarrer, vérifier

```bash
sudo launchctl bootout system/dev.matter-home-desktop.server
```
arrête le service (il redémarrera avec le Mac, sauf si vous le désactivez, voir plus bas) ;

```bash
sudo launchctl bootstrap system /Library/LaunchDaemons/dev.matter-home-desktop.server.plist
```
le démarre ;

```bash
sudo launchctl print system/dev.matter-home-desktop.server | head -20
```
dit s'il tourne ; et pour qu'il **ne démarre plus avec le Mac** :
`sudo launchctl disable system/dev.matter-home-desktop.server` (`enable` pour annuler).

**Avant `import` ou `init`**, arrêtez le service : ces commandes refusent de tourner tant qu'un
serveur tient les données. `create-account` fonctionne service lancé (le compte est pris en compte
tout de suite ; avec `--replace`, les sessions ouvertes sont fermées : redémarrez ensuite le
service). Lancez toutes ces commandes sous votre compte, **sans `sudo`** : elles refusent de
tourner en root, pour ne pas laisser dans vos données des fichiers que le service ne pourrait
plus lire.

## Ça ne démarre pas

Regardez la fin du journal : `tail -30 ~/Library/Logs/MatterHomeServer/server.log`.

- **« aucune identité Matter … »** (le service redémarre toutes les 30 secondes ; le serveur ne
  crée jamais d'identité tout seul) : le dossier de données n'a pas encore d'identité. Reprenez vos données (`import`) ou faites `init`, avec **le
  même compte utilisateur** que l'installation, puis relancez.
- **« un autre serveur Matter Home utilise déjà ces données »** : une autre instance tourne (un
  serveur lancé à la main dans un Terminal, ou la v0.2.1 ?). Arrêtez-la : jamais deux instances.
- **« en attente du réseau »** : le Mac n'a pas encore d'adresse réseau ; le serveur patiente puis
  réessaie. Vérifiez le câble Ethernet.
- Le **client dit « déconnecté »** alors que le service tourne : pare-feu (étape 4), mauvaise
  adresse, ou Mac en veille.

D'autres cas : [depannage.md](depannage.md).

## Mettre à jour

Téléchargez la nouvelle archive, ouvrez-la, retirez la quarantaine (`xattr -dr
com.apple.quarantine .`) et relancez `./install.sh` depuis son dossier : le service est arrêté,
remplacé puis relancé. Il reprend le dossier de données du service déjà installé (jamais un
autre) ; vos données ne sont pas touchées.

`install.sh` refuse de continuer si l'application « Matter Home Desktop » est ouverte sur ce Mac :
si c'est la v0.2.1, quittez-la (jamais deux instances avec la même identité) ; si c'est le client
de la version 1, ajoutez `--ignorer-application-ouverte`.

## Désinstaller

```bash
./uninstall.sh
```

Arrête le service et retire le programme et le fichier de service. **Vos données et vos journaux
ne sont jamais supprimés** par ce geste. Pour effacer aussi les données (irréversible, à ne faire
qu'après un export) : `./uninstall.sh --supprimer-donnees`, qui demande de taper `SUPPRIMER`.
Essai sans rien modifier : `./uninstall.sh --a-blanc`.
