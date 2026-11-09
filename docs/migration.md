# Migrer la v0.2.1 vers le serveur

Reprendre les appareils, le plan, les automatisations et l'historique de l'application de bureau
v0.2.1 sur le serveur, **avec la même identité Matter : aucun appareil à réappairer**.

Pour installer le serveur lui-même (service qui démarre avec le Mac), voir
[installation-serveur.md](installation-serveur.md). Cette page ne parle que des données.

> **Jamais deux instances avec la même identité** (la v0.2.1 et le serveur, ou deux serveurs) :
> leurs abonnements se chassent, les règles s'exécutent deux fois et `controller.json` peut être
> écrasé. **Arrêtez la v0.2.1 sur tous vos Mac, y compris celui-ci**, et ne la rouvrez plus avec
> ces appareils. La v0.2.1 ne prend pas le verrou du serveur : rien ne l'empêche de tourner en
> même temps, c'est à vous d'y veiller. Partez du Mac qui détient les **données les plus récentes**
> (celui où vous avez appairé ou modifié quelque chose en dernier) : un export ancien ferait
> perdre ce qui a été fait depuis.

## Procédure

1. **Exporter depuis la v0.2.1** : menu **Fichier → Exporter mes données…**, puis enregistrez le
   fichier `.matterhome`. Il contient les clés privées de votre maison : ne le partagez pas.
2. **Quitter la v0.2.1** (Cmd+Q) sur **tous** vos Mac qui l'ont utilisée, y compris celui où vous
   venez d'exporter.
3. **Copier le fichier** sur le Mac du serveur (AirDrop, clé USB), dans un endroit que vous
   supprimerez ensuite.
4. **Créer le compte** sur le Mac du serveur, dans le Terminal, depuis le dossier de l'archive
   décompressée (là où se trouve le programme, après `xattr -dr com.apple.quarantine .`, voir
   [installation-serveur.md](installation-serveur.md)) :
   `./matter-home-server create-account --username NOM`
   (une fois le service installé, le programme est aussi dans `/usr/local/bin` : la commande s'écrit
   alors sans `./`)
5. **Importer** (le serveur, ou le service, ne doit pas tourner) :
   `./matter-home-server import ~/chemin/matter-home-2026-11-09.matterhome`
   La commande vérifie le fichier, affiche le résumé (appareils, pièces, règles, date), **l'identité
   du fichier et celle du dossier** (numéro de fabric et liste des appareils), rappelle d'arrêter la
   v0.2.1 partout et demande confirmation (`--yes` pour ne pas la demander). Elle applique l'import
   puis s'arrête : elle ne démarre ni Matter ni le réseau.
6. **Démarrer le serveur** normalement (`matter-home-server`, ou le service launchd), puis
   connectez le client avec l'adresse, l'identifiant et le mot de passe du compte.

L'import fonctionne aussi sur un dossier de données vide (premier déploiement) : l'ordre ci-dessus
(compte, import, démarrage) garde le compte et le certificat du serveur, que l'import ne remplace
jamais. Avec `--data-dir DOSSIER`, mettez la même option aux trois commandes (un chemin relatif,
comme `donnees` ou `donnees/`, est accepté). Avec le **service**, le dossier de données est
celui de l'utilisateur qui l'a installé : lancez ces commandes avec ce même utilisateur, sans
`sudo`.

## Installation neuve, sans sauvegarde : `init`

Si vous partez de zéro (aucune v0.2.1, aucun appareil à reprendre), le serveur ne crée **jamais**
tout seul une identité Matter (hors `--demo`) : il refuse de
démarrer (code 78) tant que le dossier n'en a pas. Créez-la une fois, explicitement :

```bash
./matter-home-server init
./matter-home-server create-account --username NOM
```

`init` refuse de rien faire si une identité existe déjà, si des données (plan, règles, historique)
existent sans identité, ou si un import a laissé un dossier « …avant-import-… » qui en porte une :
dans ces cas, créer une nouvelle fabric ferait oublier tous vos appareils.

## À savoir

- **Identité différente** : si la fabric du fichier n'est pas celle déjà présente dans le dossier
  de données, `import` affiche les deux (numéros de fabric et appareils) et **refuse**, même avec
  `--yes`. Il ne remplace une identité qu'avec l'option dédiée `--replace-identity` : les appareils
  appairés avec l'ancienne identité ne seraient plus reconnus (l'ancien dossier reste conservé).
  Rien n'est écrasé sans cette option.
- **Règles incomplètes** : la v0.2.1 enregistrait des règles dont l'adresse « Appeler une URL »
  était vide ou à moitié saisie. L'import les **désactive** (contenu conservé) et les nomme dans le
  résumé : une règle désactivée ne se déclenche jamais toute seule. Mais le bouton **▶ Tester**
  exécute une règle même désactivée : il lance d'abord les actions sur les appareils, puis échoue
  sur le webhook incomplet. Corrigez l'adresse dans le client, puis cochez « Activée ».
- **Si un serveur tourne déjà** sur ces données, la commande refuse (« Arrêtez-le »). Si des
  données Matter existent déjà dans le dossier, elles sont mises de côté, pas supprimées.
- **Anciennes données** : conservées dans le dossier voisin « …avant-import-<date> » ; supprimez-le
  quand tout fonctionne.
- **Coupure pendant l'import** (courant coupé, disque plein) : **relancez la même commande
  `import` avec le même fichier** : elle reprend là où l'import s'était arrêté (message « Import
  interrompu… repris »). Démarrer le serveur reprend aussi l'échange interrompu. Aucune nouvelle
  identité n'est jamais créée ; un dossier recréé entre-temps sans identité est mis de côté
  (« …sans-identite-<date> »). `fingerprint`, pendant ce temps, dit qu'un import attend d'être repris.
  Si l'import en attente est celui d'un **autre** fichier (ou demandé par l'API), `import` le dit
  et n'y touche pas.
- **Renoncer à un import** (en attente ou interrompu) : arrêtez le service, puis
  `matter-home-server import --abandon` (avec le même `--data-dir` si vous en utilisez un). Cela
  retire la sauvegarde en attente (`import-en-attente.matterhome`) **et** met de côté le dossier
  déjà préparé « …import-nouveau » sous le nom « …import-abandonne-<date> » (il contient une copie
  des clés de la sauvegarde : supprimez-le quand vous n'en avez plus besoin). Les données actuelles
  ne sont pas touchées. **Si l'import avait été coupé en plein échange** (le dossier de données
  absent, ou recréé sans identité, alors que « …import-nouveau » existe), `--abandon` remet les
  **anciennes** données en place depuis le dossier « …avant-import-<date> » le plus récent et le
  dit (« Import coupé en plein échange : les anciennes données … sont remises en place ») ; s'il n'y
  a aucun dossier « …avant-import-… », il refuse et explique pourquoi plutôt que de prétendre avoir
  abandonné. Supprimer à la main le fichier en attente a le même effet au lancement suivant (le
  dossier préparé est alors aussi mis de côté, jamais effacé).
- **Pas en root** : `import`, `init`, `create-account` et le serveur refusent de tourner avec
  `sudo` ou en root (ils laisseraient dans vos données des fichiers que le service ne pourrait
  plus lire). Lancez-les sous votre compte.
- **Serveur qui ne démarre plus** après un import demandé par l'API (« n'a pas pu être appliqué après
  3 essais ») : corrigez la cause (espace disque, droits) ; pour renoncer à l'import, lancez
  `matter-home-server import --abandon` comme ci-dessus, puis relancez.
- **Mac sans la v0.2.1 sous la main** : un fichier exporté par un serveur (menu Fichier du client) a
  le même format et s'importe de la même façon.
