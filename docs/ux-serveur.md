# Expérience utilisateur de Ostal Server

[← Retour au README](../README.md) · [Installer le serveur](installation-serveur.md) · [Design system](design-system.md)

Ticket n° 21. L'application serveur (« Ostal Server », dans la barre de menus du Mac) tourne
jour et nuit et on ne la regarde presque jamais. Ce document dit ce qui n'allait pas, comment elle
est organisée maintenant, et ce qui a changé. Le comportement du serveur (`crates/server`) n'a pas
bougé : seule l'application `crates/server-app` est concernée.

## Principes

1. **Une question d'abord : « est-ce que tout va bien ? »** L'icône, son infobulle et le haut du
   menu y répondent sans rien ouvrir : une phrase (« Tout va bien », « 1 point à regarder », « Le
   serveur est à l'arrêt »), puis une ligne par sujet (appareils, caméras et détection, accès à
   distance).
2. **Une seule action quand quelque chose ne va pas**, juste sous le résumé (« Voir l'appareil
   injoignable… », « Terminer l'accès à distance… »). Elle ouvre la fenêtre là où ça se règle.
3. **Ce qui n'est pas configuré n'est pas une panne.** Pas d'accès à distance, pas de caméra :
   c'est dit, en gris, et le résumé reste « Tout va bien ». De même pour ce qui passe tout seul :
   une pause de détection de trente secondes (Mac momentanément surchargé) est dite sans alerte,
   pour que l'icône ne clignote pas ; seule la pause pour cause de chaleur, qui dure, compte comme
   un point à regarder. Une caméra n'est « en service » que si elle répond : éteinte dans ce mode
   ou pas encore vérifiée, elle est comptée à part.
8. **Le menu ne jette jamais une saisie.** Un formulaire en cours dans la fenêtre (caméra, clé,
   mot de passe) reste tel quel quand on rouvre la fenêtre depuis le menu.
4. **Les tâches rares sont guidées pas à pas**, dans la fenêtre, où il y a la place d'expliquer :
   première installation, connecter un ordinateur, iPhone hors de la maison, Raccourcis, clé
   Anthropic, ajout d'un appareil. Chaque étape numérotée dit où on en est (faite, à faire
   maintenant, plus tard).
5. **Les mots de l'utilisateur.** Le terme technique ne reste, entre parenthèses, que s'il aide à
   se repérer ailleurs (dans le client ou la documentation).
6. **Aucun secret affiché**, sauf le jeton de mode à sa création, une seule fois, comme avant. La
   clé Anthropic et les mots de passe ne sont jamais réaffichés.
7. **Le même aspect que le client** : tokens de `design/tokens.json` par la crate
   `ostal-theme` (couleurs, espacements, rayons, tailles de texte), onglets `theme::tab`,
   bouton principal `theme::primary_button`, cartes en aplat à trait fin. Ni ombre, ni animation.

## Audit de l'existant (avant)

### Le menu de la barre de menus

```text
Ostal Server 1.2.1
────────────────
12 appareils en ligne, 1 injoignable
Appareils injoignables :
    Salon
────────────────
Connecter un client ▸  (Adresse, Copier l'adresse, Copier l'empreinte du certificat)
☐ Accès web (Tailscale)
────────────────
Mode : Nuit ▸
Caméras…
Réglages…
Journal
────────────────
☑ Ouvrir à la connexion
────────────────
Quitter Ostal Server…
```

| Constat | Détail |
|---|---|
| Le résumé ne parle que des appareils | Une caméra hors ligne, une détection suspendue ou un accès à distance à autoriser ne se voyaient qu'en ouvrant la fenêtre des caméras ou en lisant le libellé d'une case. |
| L'icône ne change pas quand un appareil tombe | Trois icônes (démarrage, en marche, problème) : « en marche » couvrait aussi « un appareil est injoignable ». |
| La première ligne est le numéro de version | L'information la moins utile occupait la meilleure place. |
| « Accès web (Tailscale) » est une case à cocher | Un clic, sans explication, publie le serveur sur le réseau Tailscale. Les cas d'erreur étaient des libellés grisés (« port 443 déjà utilisé par Tailscale », « Funnel actif (public) », « état de Tailscale illisible ») sans marche à suivre. |
| « Connecter un client » ne dit pas quoi faire | Une adresse et deux « Copier », dont « l'empreinte du certificat », sans dire où les coller ni pourquoi. |
| « Réglages… » ouvre en réalité toute la fenêtre | Et « Caméras… » la même fenêtre sur une autre page : deux entrées, une seule fenêtre, pas de repère. |
| « Journal », « Ouvrir à la connexion » | Vocabulaire du système (journal, session), au même niveau que les actions courantes. |

### La fenêtre

Une seule page qui défile, « Ostal Server », dans cet ordre : état, adresse du serveur,
empreinte du certificat, appareils injoignables, boutons « Caméras… » et « Descriptions des
événements… », mode et jeton de mode, compte, ouverture à la connexion, veille, dossier des données.

| Constat | Détail |
|---|---|
| Pas de structure | État, tâches rares et réglages mélangés ; rien ne dit par où commencer. |
| Redondant avec le menu | Adresse, empreinte, appareils injoignables, ouverture à la connexion : deux fois. |
| Les pages secondaires remplacent la page | « Caméras… » et « Descriptions… » prenaient la place des réglages, avec un bouton « Retour aux réglages » ; « Descriptions » (clé Anthropic) n'était accessible que par un bouton dans la section Caméras. |
| Tâches rares non guidées | Le jeton de mode se résumait à une ligne technique (« méthode PUT, en-tête Authorization : Bearer <jeton>, corps JSON… ») et renvoyait à `docs/modes.md`, un fichier que l'utilisateur n'a pas. L'accès depuis l'iPhone n'était expliqué nulle part dans l'application. L'ajout d'un appareil non plus (il se fait dans le client). |
| Sauvegardes absentes | Rien ne disait où se fait la sauvegarde (dans le client) ni comment la reprendre. |
| Vocabulaire du code | « Empreinte du certificat », « jeton », « identité Matter », « score minimal », « classes décrites », « polygone », « Funnel », « port 443 ». |
| Écart avec le client | Une page de libellés en gras et de séparateurs, sans onglets ni cartes ; fenêtre étroite (500 px). |

### Première installation

Trois écrans (bienvenue, compte, « Tout est prêt ») sans indication d'étape ; le dernier donnait
l'adresse et l'empreinte en une phrase dense, sans dire ce qu'on fait ensuite.

## L'organisation (après)

### L'icône

| Icône | Sens |
|---|---|
| Arche épaisse avec un point | Tout va bien. |
| Arche épaisse avec un point et une pastille en haut à droite | **Nouveau.** Le serveur tourne, un point est à regarder (appareil injoignable, caméra hors ligne, détection suspendue, accès à distance à terminer). |
| Arche fine, sans point | Démarrage ou arrêt en cours. |
| Arche fine avec un point d'exclamation | Le serveur ne tourne pas (configuration à terminer, données utilisées ailleurs, erreur). |

L'infobulle donne le titre du résumé, puis une ligne par point à regarder.

### Le menu

```text
1 point à regarder
Appareils : 12 sur 13 joignables · injoignable : Salon
Caméras : 3 caméras en service · détection active sur 2
Accès à distance : activé (iPhone, hors de la maison)
Voir l'appareil injoignable…          ← seulement quand quelque chose ne va pas
────────────────
Mode de la maison : Nuit ▸            (Présent, Absent, Nuit, Vacances)
────────────────
Ouvrir Ostal Server…            → Vue d'ensemble
Connecter un Mac ou un iPhone…        → Connexion
Caméras…                              → Caméras
Réglages…                             → Réglages
────────────────
Quitter Ostal Server…
```

Serveur à l'arrêt : le titre le dit (« Le serveur est à l'arrêt », « Le serveur ne tourne pas »,
« Configuration à terminer »), une ligne donne la raison en quelques mots, et l'action devient
« Voir comment le relancer… », « Voir le problème… » ou « Terminer la configuration… ». Le mode et
les entrées Connexion et Caméras n'apparaissent que lorsque le serveur tourne.

Le calcul est dans `crates/server-app/src/overview.rs` (sans interface, testé) : le menu
(`menu.rs`), l'icône et l'infobulle (`status.rs`) et la vue d'ensemble de la fenêtre le partagent.

### La fenêtre : six onglets

| Onglet | Contenu |
|---|---|
| **Vue d'ensemble** | Le bandeau « Tout va bien » (ou ce qui ne va pas), les mêmes lignes que le menu avec un bouton vers l'onglet concerné, le mode de la maison (quatre boutons), et « Que voulez-vous faire ? » : les tâches rares, chacune menant au bon onglet. Serveur à l'arrêt : l'explication complète, « Réessayer » et « Ouvrir le journal ». |
| **Appareils** | Combien répondent, lesquels sont injoignables et quoi faire ; « Ajouter un appareil » en trois étapes (cela se fait dans le client) ; où voir et piloter ses appareils. |
| **Caméras** | La liste et le formulaire (inchangés sur le fond), l'état de la détection, et « Décrire les événements par IA » : état, puis une page en deux étapes (créer et coller la clé Anthropic, activer et régler). |
| **Connexion** | Quatre blocs : *depuis un ordinateur de la maison* (adresse et code de vérification à copier), *depuis l'iPhone, même hors de la maison* (Tailscale sur ce Mac, activer l'accès, Tailscale sur l'iPhone, adresse à ouvrir dans Safari), *notifications sur l'iPhone et l'iPad* (ticket n° 17 : interrupteur général, appareils abonnés, « Révoquer »), *changer de mode automatiquement* (accès à distance, code secret, champs à remplir dans Raccourcis). |
| **Journal** | Qui a fait quoi, depuis quel téléphone ou ordinateur : une liste par jour (Heure, Qui, Depuis, Quoi), filtrée par personne (« Toutes les personnes » ou un compte) et par période (« Aujourd'hui et hier », « Aujourd'hui », « 7 derniers jours »). Gardé 6 mois ; ne se lit que dans Ostal Server et par `ostal-server audit`. Les phrases viennent de `ostal_server::audit::describe`, les mêmes dans la fenêtre et dans le Terminal. |
| **Réglages** | Compte (identifiant et mot de passe), démarrage et veille, sauvegarde (où elle se fait, comment la reprendre), dépannage (journal, dossier des données, version). |

Les onglets apparaissent une fois la première installation terminée. Un onglet qui a besoin du
serveur (Appareils, Caméras, Connexion) le dit quand il ne tourne pas, et renvoie à la vue
d'ensemble.

### Première installation

Trois étapes numérotées (« étape 1 sur 3 ») : **la maison** (reprendre une sauvegarde, ou partir de
zéro), **le compte**, **c'est prêt** (les quatre gestes pour connecter son ordinateur, le lancement
à l'ouverture de session, la veille, et où retrouver tout cela plus tard).

### Vocabulaire

| Avant | Après |
|---|---|
| Connecter un client | Connecter un Mac ou un iPhone ; « depuis un ordinateur de la maison » |
| Empreinte du certificat | Code de vérification (« empreinte du certificat », le mot que montre le client) |
| Accès web (Tailscale) | Accès à distance ; « depuis l'iPhone, même hors de la maison » |
| Serve à autoriser, Funnel actif, port 443 déjà utilisé | « Tailscale demande une autorisation, une seule fois » ; « Tailscale expose ce Mac à tout Internet » ; « Tailscale publie déjà autre chose depuis ce Mac » |
| Jeton de mode | Code secret (le « jeton de mode ») |
| Révoquer le jeton | Supprimer le code |
| Ouvrir à la connexion | Lancer Ostal Server à l'ouverture de session de ce Mac |
| Journal | Journal, avec sa définition (« le fichier où le serveur note ce qu'il fait ») |
| Identité Matter (première installation) | Votre maison ; « une nouvelle maison (une nouvelle identité Matter) » |
| Appairer (côté serveur) | Ajouter un appareil (le bouton du client s'appelle toujours « Appairer ») |
| Descriptions des événements | Décrire les événements par IA |
| Classes décrites ; score minimal | Ce qui est décrit ; certitude minimale |
| Zones et masques (un polygone par ligne) | « Limiter à une partie de l'image (avancé) », replié : zones à surveiller, zones à ignorer |

## Ce qui a changé de place

Rien n'est supprimé. Ce qui a quitté le menu est dans la fenêtre :

| Fonction | Avant | Maintenant |
|---|---|---|
| Copier l'adresse du serveur, copier l'empreinte | Menu, « Connecter un client » ; fenêtre | Onglet Connexion, étapes 2 et 3 (et écran de fin de la première installation) |
| Activer ou couper l'accès web | Menu, case « Accès web (Tailscale) » | Onglet Connexion, étape 2 : bouton « Activer l'accès à distance » / « Désactiver l'accès à distance » |
| Ouvrir l'app web, copier son adresse | Menu, sous la case | Onglet Connexion, étape 4 : « Copier », « Essayer sur ce Mac » |
| Page d'autorisation de Tailscale | Menu | Onglet Connexion, étape 2 ; le menu la signale (« Terminer l'accès à distance… ») |
| Journal | Menu ; fenêtre en cas de problème | Onglet Réglages, Dépannage ; vue d'ensemble en cas de problème |
| Ouvrir à la connexion | Menu (case) ; fenêtre | Onglet Réglages, Démarrage et veille (et écran de fin) |
| Version | Première ligne du menu | Onglet Réglages, Dépannage |
| Liste des appareils injoignables | Une ligne par appareil dans le menu | Une ligne dans le menu (deux noms, puis « et N autres ») ; liste complète dans l'onglet Appareils |
| Mode de la maison | Sous-menu | Sous-menu, et vue d'ensemble |
| Caméras, Descriptions | Menu « Caméras… » ; boutons dans la fenêtre | Onglet Caméras (le menu y mène) ; Descriptions depuis l'onglet Caméras |
| Jeton de mode (créer, renouveler, révoquer) | Fenêtre, section « Mode de la maison » | Onglet Connexion, « Changer de mode automatiquement » |
| Compte, veille, dossier des données | Fenêtre | Onglet Réglages |
| Notifications push (interrupteur général, appareils abonnés, Révoquer ; ticket n° 17) | Fenêtre, section « Notifications (iPhone, iPad) » | Onglet Connexion, « Notifications sur l'iPhone et l'iPad », juste après l'accès depuis l'iPhone (l'abonnement se fait depuis l'app web ouverte par cet accès). Même comportement. Pas de ligne dans le résumé : un abonnement en erreur n'empêche rien de tourner, et le résumé ne lit pas un état de plus à chaque passe. |

Ajouts, tous à l'initiative de l'utilisateur :

- l'icône « à regarder » ;
- « Montrer le dossier des données » (ouvre le dossier dans le Finder) ;
- trois boutons qui ouvrent une page d'aide dans le navigateur, aux adresses fixes écrites dans
  `system::HelpPage` (télécharger Tailscale, console Anthropic, guide des Raccourcis) ; les autres
  adresses passent toujours par `system::open_url`, qui n'accepte que Tailscale ;
- « C'est fait, vérifier » : relit l'état de Tailscale (lecture seule) ;
- l'option de lancement `--open ONGLET` (essais et captures).

L'état de Tailscale est relu comme avant à l'ouverture et à la fermeture du menu, et en plus à
l'ouverture de l'onglet Connexion. Rien ne le relit en boucle.

## Essayer sans risque

```text
ostal-server-app --demo --data-dir /tmp/essai --open connexion
```

En démonstration : appareils et caméras simulés, écoute sur `127.0.0.1` (port libre), aucun
LaunchAgent, jamais le vrai Tailscale, verrous et journal dans le dossier donné. `--demo-setup`
passe par la première installation. `OSTAL_THEME=light` ou `dark` force le thème.

## Refonte « Nuit et Lueur » (préversion)

- Fenêtre fixe de 640 × 700 (barre de titre de 28 px comprise, onglets de 42 px, corps de 630 px), non redimensionnable.
- Titre d'état : `protocol::status::headline`, en comptant une chose à regarder par catégorie (appareils, caméras, accès à distance). Le détail vient de `protocol::status::detail` ; le menu le met à la place de la ligne du sujet concerné. Un seul mot pour « ne répond pas » : « injoignable ».
- Vue d'ensemble : cartes État, Mode (avec `HomeMode::effect()`) et « Que voulez-vous faire ? » (repliée quand une chose est à regarder). Serveur à l'arrêt : « Pourquoi, et quoi faire », commandes dans des blocs copiables, « Réessayer maintenant ».
- Appareils : phrase « N appareils sur M sont joignables. », carte des injoignables avec « Réessayer » (`Hub::retry`), liste par pièce, ajout replié. Connexion : quatre cartes repliables, une ouverte à la fois.
- Six gestes se confirment dans une alerte native (`confirm.rs`, Annuler par défaut) : supprimer le code secret, révoquer le jeton de lecture, révoquer un appareil abonné, renoncer à l'import, supprimer une caméra, retirer la clé d'API.
- Les erreurs disent une cause et proposent « Réessayer » ; le détail va au journal. La version n'apparaît que dans Réglages.
- Menu : ⌘, (Réglages) et ⌘Q (Quitter) sont posés comme raccourcis des éléments (`muda`). Dans une icône de barre de menus ils ne répondent qu'une fois le menu ouvert. Le titre en gras n'est pas possible avec `tray-icon` : écart assumé.
- Un bouton grisé dit toujours pourquoi, dès l'ouverture (création du compte, caméra, clé d'API des descriptions).
- Captures : `OSTAL_DEMO_HEALTHY=1 ostal-server-app --demo` rend la caméra Garage joignable (« La maison va bien. »).
