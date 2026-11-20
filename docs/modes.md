# Modes de la maison

[← Retour au README](../README.md) · [Automatisations](automatisations.md) · [Accès à distance](acces-distant.md)

Ticket n° 8. Un **mode global**, tenu par le serveur et le même pour tous les clients :
**Présent**, **Absent**, **Nuit** ou **Vacances** (Présent au premier lancement). Il se choisit :

- dans le **client de bureau** : « Mode : … » dans la barre du haut, visible depuis tous les onglets ;
- dans l'**app web** : la pastille « Mode : … » en haut de l'écran, un tap ouvre les quatre choix ;
- dans la **barre de menus du serveur** : sous-menu « Mode » (coche sur le mode actuel) ;
- **automatiquement**, depuis l'iPhone, avec Raccourcis (voir plus bas).

Chaque changement est enregistré (`mode.json`, voir [Données](donnees.md)), diffusé aux clients
(`mode_changed`), inscrit au journal, et peut déclencher des règles. Redemander le mode actuel ne
fait rien (ni écriture, ni diffusion, ni déclenchement) : une automatisation « Arriver » qui part
alors qu'on est déjà Présent est sans effet.

## Les caméras selon le mode

Chaque caméra a un **emplacement** (Intérieure ou Extérieure) et un **comportement par mode** :

| Comportement | Effet |
|---|---|
| **Éteinte** | le serveur **refuse** le direct et l'image fixe (erreur 503 « caméra éteinte en mode … ») et **ferme** une lecture en cours dans les 2 s qui suivent le changement de mode |
| **Diffusion** | on peut la regarder (direct et image fixe) |
| **Surveillance** | se regarde comme en Diffusion, **et** le serveur l'analyse : personnes, animaux et véhicules deviennent des événements avec capture et clip (voir [Détection](detection.md)). Sans modèle installé : comme Diffusion |

Choisir l'emplacement **pré-remplit** le tableau, qui reste modifiable ligne par ligne :

| Emplacement | Présent | Absent | Nuit | Vacances |
|---|---|---|---|---|
| Extérieure | Diffusion | Surveillance | Surveillance | Surveillance |
| Intérieure | Éteinte | Surveillance | Éteinte | Surveillance |

- **Où le régler** : fenêtre « Caméras… » de l'application du serveur (le seul endroit où les
  caméras se modifient : aucune route de l'API ne les écrit). Les clients, eux, voient
  l'emplacement et le tableau (`CameraView`, sans adresse ni mot de passe) et n'ouvrent rien pour une
  caméra éteinte (« Éteinte en mode Nuit »).
- **Caméras déjà enregistrées** : elles deviennent **Extérieures** (tableau extérieur : jamais
  éteintes), donc rien ne change tant qu'on n'a pas choisi autre chose.
- **Éteinte = aucune connexion** : le serveur ne se connecte plus du tout à la caméra, pas même pour
  vérifier qu'elle répond (le `DESCRIBE` d'une minute est sauté). Les clients affichent l'état neutre
  « Éteinte » à la place de la dernière joignabilité connue ; elle est revérifiée dès que le mode la
  rallume. Le bouton « Tester » de la fenêtre du serveur, lui, reste possible (action manuelle).

## Automatisations

- **Alors** : « Changer le mode de la maison » (`Action::SetMode`) : par exemple *à 23:00, passer en
  Nuit*. Le même mode n'a aucun effet. Une règle « quand le mode devient X » qui change le mode doit
  avoir un **délai entre deux exécutions d'au moins 10 s** (le serveur la refuse sinon) : deux règles
  qui se renvoient le mode (Nuit → Absent → Nuit…) sont arrêtées par ce délai (60 s par défaut), elles
  ne bouclent pas sans fin.
- **Quand** : « Le mode de la maison change » : la règle se lance quand le mode **devient** le mode
  choisi (`{valeur}` vaut le nom du mode). Le mode lu au démarrage du serveur n'est pas un changement.
- **Seulement si** : « Mode de la maison » : l'un des modes cochés (au moins un), par exemple
  « Absent ou Vacances ». Exemple : *Quand la porte d'entrée s'ouvre, seulement si le mode est Absent
  ou Vacances, notifier.*

## Les jetons de l'iPhone

Pour que l'iPhone change le mode ou affiche l'état **sans** connaître le mot de passe du compte, le
serveur tient deux **jetons à part**, chacun pour une seule porte :

| Jeton | Fichier | Ouvre seulement | Sert à |
|---|---|---|---|
| **jeton de mode** | `serveur/jeton-mode.json` | `PUT /api/mode` | les raccourcis « Mode absent » / « Mode present », les automatisations Raccourcis |
| **jeton de lecture** | `serveur/jeton-lecture.json` | `GET /api/state`, `GET /api/events` | le widget Scriptable |

Toute autre route répond 401 : ni appareils, ni WebSocket (ni par l'en-tête, ni par le sous-protocole
du navigateur), ni caméras, ni clips, ni export ou import, ni les routes des jetons ; le jeton de
lecture ne change pas le mode et le jeton de mode ne lit rien. Des tests l'essaient route par route.

- **Le plus simple** : l'app web, Appareils › **Widgets et raccourcis** › « Installer les
  raccourcis » ou « Installer le widget » (voir [App web](web-app.md#widgets-et-raccourcis)). Le
  serveur crée le jeton et l'écrit, avec l'adresse, dans les fichiers à installer : rien à taper.
- Le jeton de mode se crée aussi dans la fenêtre du serveur (onglet Connexion, « Changer de mode
  automatiquement ») : « Créer le code secret » ; il s'affiche **une seule fois** (bouton « Copier »),
  pour les automatisations faites à la main (plus bas).
- **Un nouveau jeton révoque l'ancien.** Réinstaller les raccourcis depuis l'app web crée un nouveau
  jeton de mode : les raccourcis d'avant **et les automatisations où l'ancien jeton a été collé à la
  main** cessent de marcher (à corriger avec le nouveau, ou à refaire). De même, réinstaller le widget
  révoque le widget installé avant (sur cet iPhone ou un autre), qui affiche « Jeton révoqué ».
- **Révoquer** : l'app web (bouton « Révoquer » de chaque jeton) ou la fenêtre du serveur
  (« Supprimer le code » pour le jeton de mode, « Révoquer » pour le jeton de lecture).
- Le serveur n'en garde que l'**empreinte** (SHA-256, droits 0600, jamais exportée ni importée avec
  les sauvegardes). Un jeton n'est jamais écrit dans le journal.
- Il se place dans l'en-tête `Authorization: Bearer <jeton>` (jamais dans l'URL). Le jeton est
  vérifié **d'abord** : un bon jeton n'est jamais refusé. Un mauvais jeton reçoit 401 après 250 ms et
  une ligne au journal, **sans blocage par adresse** : derrière `tailscale serve`, toutes les
  connexions viennent de 127.0.0.1 (voir [Accès à distance](acces-distant.md)), un compteur partagé
  aurait pu bloquer le bon Raccourci. Un jeton fait 256 bits : l'essai en force est sans espoir.
- Si l'un des fichiers est illisible, le serveur le met de côté (`….json.illisible`) et repart sans
  ce jeton : il suffit d'en générer un nouveau.

## Changer de mode automatiquement avec Raccourcis sur iPhone

Les raccourcis « Mode absent » et « Mode present » installés depuis l'app web font déjà le travail
de l'étape 4 ci-dessous : une automatisation peut simplement **lancer l'un d'eux** (action « Exécuter
le raccourci »). La recette à la main reste utile pour les modes Nuit et Vacances.

**Avant de commencer**

1. L'iPhone et le Mac du serveur sont sur le même tailnet Tailscale ([Accès à distance](acces-distant.md)).
2. Sur le Mac, fenêtre du serveur, onglet Connexion : **accès à distance activé**. L'adresse est alors
   `https://<nom-du-mac>.<tailnet>.ts.net/` (port 443, certificat public). **N'utilisez pas** l'adresse
   directe en `:8443` : son certificat est auto-signé et Raccourcis le refuse.
3. Générez le **jeton de mode** (fenêtre du serveur, onglet Connexion) et copiez-le : vous le collerez dans
   chaque automatisation. Dans ce qui suit, `<jeton de mode>` est à remplacer par ce jeton.

**Une automatisation « Quitter la maison → Absent »**

1. App **Raccourcis** → onglet **Automatisation** → **+** → **Créer une automatisation personnelle**.
2. Choisissez **Quitter** : lieu **Maison**, à toute heure → **Suivant**.
3. **Ajouter une action** → cherchez **« Obtenir le contenu de l'URL »**.
4. Dans l'URL, écrivez `https://<nom-du-mac>.<tailnet>.ts.net/api/mode`, puis touchez la flèche
   **Afficher plus** :
   - **Méthode** : `PUT` ;
   - **En-têtes** → **Ajouter un en-tête** : nom `Authorization`, valeur `Bearer <jeton de mode>`
     (le mot « Bearer », une espace, puis le jeton) ;
   - **Corps de la requête** : `JSON` → **Ajouter un nouveau champ** → type **Texte**, clé `mode`,
     valeur `absent`. Le corps envoyé est `{"mode": "absent"}`.
5. **Suivant**, puis choisissez **Exécuter immédiatement** (et non « Exécuter après confirmation »,
   sinon rien ne part sans votre tap) ; désactivez **Notifier à l'exécution** si vous ne voulez pas
   de bannière. **OK**.

**Les quatre automatisations**

| Automatisation | Déclencheur | Valeur de `mode` |
|---|---|---|
| Arriver à la maison | **Arriver** (lieu : Maison) | `present` |
| Quitter la maison | **Quitter** (lieu : Maison) | `absent` |
| Se coucher | **Concentration** → Sommeil → **Quand il est activé** | `nuit` |
| Se lever | **Concentration** → Sommeil → **Quand il est désactivé** | `present` |

Refaites les étapes 2 à 5 pour les autres : seuls le déclencheur et la valeur du champ `mode` changent (`present`, `absent`,
`nuit` ou `vacances`). Le libellé exact des déclencheurs varie un peu selon la version d'iOS.

**Vérifier**

Depuis un terminal du Mac (le serveur doit tourner) :

```
curl -X PUT https://<nom-du-mac>.<tailnet>.ts.net/api/mode \
  -H "Authorization: Bearer <jeton de mode>" \
  -H "Content-Type: application/json" \
  -d '{"mode": "absent"}'
```

Réponse `200` : `{"mode":"absent","changed":true}` (`changed: false` si c'était déjà le mode). Le
sélecteur du client de bureau, la pastille de l'app web et le menu du serveur suivent aussitôt.

| Réponse | Cause |
|---|---|
| `401 unauthorized` | jeton faux, révoqué ou remplacé : générez-en un nouveau et corrigez l'en-tête |
| `429 too_many_attempts` | trop d'échecs de jeton : attendez le délai de `Retry-After` |
| `400 bad_request` | corps mal formé : le mode doit être `present`, `absent`, `nuit` ou `vacances` |
| rien ne se passe | l'iPhone n'est pas sur le tailnet, l'accès à distance est désactivé, ou l'automatisation est restée sur « Exécuter après confirmation » |

## Limites

- Le mode est global : pas de modes par pièce ni par personne.
- **Protocole 3** : un client 1.1.0 est refusé (« version incompatible ») : mettez à jour le client
  (et l'app web, rechargée, suit). Une **sauvegarde** faite avec cette version est refusée par un serveur
  1.1.0 (`mode.json` inconnu). Un serveur sans modes n'est plus joignable par ces clients ; si jamais
  l'état complet n'annonce pas de mode, le sélecteur reste caché.
