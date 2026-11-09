# Migrer la v0.2.1 vers le serveur

Reprendre les appareils, le plan, les automatisations et l'historique de l'application de bureau
v0.2.1 sur le serveur, **avec la même identité Matter : aucun appareil à réappairer**.

> Jamais deux instances avec la même identité (l'application de bureau et le serveur, ou deux
> serveurs) : leurs abonnements se chassent, les règles s'exécutent deux fois et `controller.json`
> peut être écrasé. **Quittez l'application de bureau sur l'ancien Mac avant d'importer, et ne la
> rouvrez plus avec ces appareils.**

## Procédure

1. **Exporter depuis la v0.2.1** : menu **Fichier → Exporter mes données…**, puis enregistrez le
   fichier `.matterhome`. Il contient les clés privées de votre maison : ne le partagez pas.
2. **Quitter l'application de bureau** (Cmd+Q) sur le Mac qui l'utilisait.
3. **Copier le fichier** sur le Mac du serveur (AirDrop, clé USB), dans un endroit que vous
   supprimerez ensuite.
4. **Créer le compte** sur le Mac du serveur :
   `matter-home-server create-account --username NOM`
5. **Importer** (le serveur ne doit pas tourner) :
   `matter-home-server import ~/chemin/matter-home-2026-11-09.matterhome`
   La commande vérifie le fichier, affiche le résumé (appareils, pièces, règles, date), rappelle
   d'arrêter l'application de bureau et demande confirmation (`--yes` pour ne pas la demander). Elle
   applique l'import puis s'arrête : elle ne démarre ni Matter ni le réseau.
6. **Démarrer le serveur** normalement (`matter-home-server`, ou le service launchd), puis
   connectez le client avec l'adresse, l'identifiant et le mot de passe du compte.

L'import fonctionne aussi sur un dossier de données vide (premier déploiement) : l'ordre ci-dessus
(compte, import, démarrage) garde le compte et le certificat du serveur, que l'import ne remplace
jamais. Avec `--data-dir DOSSIER`, mettez la même option aux trois commandes.

## À savoir

- **Règles incomplètes** : la v0.2.1 enregistrait des règles dont l'adresse « Appeler une URL »
  était vide ou à moitié saisie. L'import les **désactive** (contenu conservé) et les nomme dans le
  résumé ; elles ne s'exécutent jamais. Corrigez l'adresse dans le client, puis cochez « Activée ».
- **Si un serveur tourne déjà** sur ces données, la commande refuse (« Arrêtez-le »). Si des
  données Matter existent déjà dans le dossier, elles sont mises de côté, pas supprimées.
- **Anciennes données** : conservées dans le dossier voisin « …avant-import-<date> » ; supprimez-le
  quand tout fonctionne.
- **Coupure pendant l'import** (courant coupé, disque plein) : relancez la commande, ou démarrez le
  serveur, qui reprend l'échange interrompu. Aucune nouvelle identité n'est jamais créée ; un
  dossier recréé entre-temps sans identité est mis de côté (« …sans-identite-<date> »).
- **Serveur qui ne démarre plus** après un import demandé par l'API (« n'a pas pu être appliqué après
  3 essais ») : corrigez la cause (espace disque, droits) ; pour renoncer à l'import, supprimez le
  fichier `import-en-attente.matterhome` indiqué dans le message, puis relancez.
- **Mac sans la v0.2.1 sous la main** : un fichier exporté par un serveur (menu Fichier du client) a
  le même format et s'importe de la même façon.
