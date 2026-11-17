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
| **Surveillance** | se regarde comme en Diffusion pour l'instant ; la détection (mouvement, personne) viendra dans un ticket ultérieur |

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
- La vérification de joignabilité des caméras (un `DESCRIBE` bref par minute) continue pour une
  caméra éteinte : « éteinte » ne concerne que le direct et l'image fixe.

## Automatisations

- **Quand** : « Le mode de la maison change » : la règle se lance quand le mode **devient** le mode
  choisi (`{valeur}` vaut le nom du mode). Le mode lu au démarrage du serveur n'est pas un changement.
- **Seulement si** : « Mode de la maison » : l'un des modes cochés (au moins un), par exemple
  « Absent ou Vacances ». Exemple : *Quand la porte d'entrée s'ouvre, seulement si le mode est Absent
  ou Vacances, notifier.*

## Le jeton de mode

Pour qu'une automatisation de l'iPhone change le mode **sans** connaître le mot de passe du compte,
le serveur peut créer un **jeton de mode** : un secret à part, qui ne sert **qu'à** `PUT /api/mode`
(il ne lit rien et ne commande rien d'autre : toute autre route répond 401).

- Dans la fenêtre du serveur (Réglages, « Mode de la maison ») : « Générer un jeton de mode ». Le jeton
  s'affiche **une seule fois** (bouton « Copier ») ; le serveur n'en garde que l'**empreinte**
  (`serveur/jeton-mode.json`, droits 0600, jamais exportée ni importée avec les sauvegardes). Il n'est
  jamais écrit dans le journal.
- « Générer un nouveau jeton » **révoque l'ancien** (les automatisations de l'iPhone sont alors à
  mettre à jour) ; « Révoquer le jeton » supprime tout accès par jeton.
- Il se place dans l'en-tête `Authorization: Bearer <jeton de mode>` (jamais dans l'URL). Les échecs
  sont limités par adresse, avec un compteur **à part** de celui de la connexion : un Raccourci resté
  sur un ancien jeton ne bloque pas la connexion par mot de passe. Derrière `tailscale serve`, toutes
  les connexions viennent de 127.0.0.1 : ce compteur est alors partagé, comme celui de la connexion
  (voir [Accès à distance](acces-distant.md)).

## Changer de mode automatiquement avec Raccourcis sur iPhone

**Avant de commencer**

1. L'iPhone et le Mac du serveur sont sur le même tailnet Tailscale ([Accès à distance](acces-distant.md)).
2. Sur le Mac, menu du serveur : **« Accès web (Tailscale) »** coché. L'adresse est alors
   `https://<nom-du-mac>.<tailnet>.ts.net/` (port 443, certificat public). **N'utilisez pas** l'adresse
   directe en `:8443` : son certificat est auto-signé et Raccourcis le refuse.
3. Générez le **jeton de mode** (fenêtre du serveur, Réglages) et copiez-le : vous le collerez dans
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
| rien ne se passe | l'iPhone n'est pas sur le tailnet, « Accès web » est décoché, ou l'automatisation est restée sur « Exécuter après confirmation » |

## Limites

- Pas encore d'**action** « changer le mode » dans une règle (seulement un déclencheur et une condition).
- Le mode est global : pas de modes par pièce ni par personne.
- Un client plus ancien (1.1.0) qui reçoit des règles contenant « Le mode devient… » ne sait pas les
  lire (les énumérations de règles n'ont pas de repli) : mettez à jour le client avant d'en créer.
