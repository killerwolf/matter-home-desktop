# Agents d'IA : le serveur MCP intégré (étapes E1a, E1b et E2)

Ostal Server peut recevoir un agent d'IA (Claude Code, Cursor…) qui lit l'état de la maison par le
protocole MCP. **C'est éteint tant que vous ne l'allumez pas.** Les appareils, les pièces,
l'historique, les automatisations et les caméras se lisent avec le droit de lecture ; **la présence
et le mode** demandent un droit à part, « occupation », éteint par défaut. Depuis l'étape E2, un
troisième droit, « commandes », permet d'**allumer ou d'éteindre des prises et des lampes** que vous
avez nommées, et rien d'autre : aucun volet, aucun changement de mode (voir
[Les commandes](#les-commandes-e2)).

Les étapes suivantes (confirmations par une personne ; volets et pièces entières ; mode ; écrans pour
créer un agent et lui donner des droits) sont décrites dans les spécifications du projet, pas encore
livrées. Tant que les confirmations n'existent pas, **tout ce qui en demanderait une est refusé**.

## Ce que ça ouvre, et ce que ça n'ouvre pas

| | |
|---|---|
| Ouvert | `POST /mcp` sur **le même écouteur HTTPS** que l'app web (même port, même certificat), avec un jeton d'agent |
| Fermé | Tout le reste : un jeton d'agent n'ouvre aucune route `/api/…`, et une session, un jeton de mode ou de lecture n'ouvrent pas `/mcp` |
| Éteint par défaut | Tant que l'interrupteur général est éteint, `/mcp` répond comme une route qui n'existe pas (404, sur toutes les méthodes) |
| Pas d'accès public | Aucun Funnel, aucun tunnel : local et Tailscale `serve` seulement (voir [Accès à distance](acces-distant.md)). L'app Claude et claude.ai ne sont pas visés |

**Limite à connaître** : un agent qui a un **terminal sur le Mac du serveur** peut lancer
`ostal-server agent enable`, lire `~/Library/Application Support/Ostal Server` ou se créer un jeton de
lecture. Rien dans Ostal ne l'en empêche. Prévoyez l'agent sur **un autre ordinateur**, ou refusez-lui
l'accès à `ostal-server` et au dossier de données dans les règles de l'agent. La ligne de commande ne
crée de toute façon que des jetons en lecture.

## Mettre en route (ligne de commande)

```sh
ostal-server agent add --username hamdi --label "Claude Code sur le portable" --days 90
ostal-server agent enable      # allume l'interrupteur général
ostal-server agent list        # jamais de jeton
ostal-server agent revoke --agent "Claude Code sur le portable"
ostal-server agent disable     # éteint tout : /mcp n'existe plus
```

- `add` crée un jeton **en lecture seule** pour ce compte et l'affiche **une seule fois**
  (`ostal_agent_` suivi de 64 caractères). Il n'existe aucune option pour donner d'autres droits :
  commander ou changer le mode se donnera depuis une session, jamais d'un terminal. Durée : 90 jours
  par défaut, 365 au plus. 16 agents au plus par personne, un libellé par agent.
- Ces commandes agissent aussi sur un serveur qui tourne, sans redémarrage : le serveur relit
  `serveur/agents.json` dès qu'il change. Une révocation prend effet à la requête suivante.
- Un compte désactivé coupe ses agents. Un jeton expiré est refusé.
- Rangez le jeton dans une variable d'environnement ou le Trousseau, **pas** dans un fichier de
  configuration (voir Claude Code : `headers` avec `${OSTAL_AGENT_TOKEN}`).

L'interrupteur et les jetons vivent dans `serveur/agents.json` (0600, jamais exporté) : seule
l'**empreinte** SHA-256 de chaque jeton y est gardée. Un fichier illisible coupe tout l'accès. Le
journal d'audit note la création et la révocation d'un agent et les changements d'interrupteur
(`ostal-server audit`), jamais le jeton.

## Le protocole

- **Streamable HTTP**, réponses toujours en `application/json` (jamais de flux SSE), **sans état** :
  aucune session MCP (`Mcp-Session-Id` n'existe pas), un redémarrage du serveur ne change rien.
- **Deux ères**, sur le même point d'entrée : la révision `2026-07-28` (sans `initialize`, avec
  `Mcp-Method` et `Mcp-Name`) et les révisions `2025-06-18` et `2025-11-25` (par `initialize`).
  L'aiguillage se fait **d'abord sur la méthode** : `initialize` est accepté même sans en-tête
  `MCP-Protocol-Version` (le client ne l'envoie que sur les requêtes suivantes). Toute autre requête
  sans cet en-tête reçoit **400 `-32020`** ; une version inconnue **400 `-32022`** avec la liste des
  versions prises en charge. En ère moderne, `Mcp-Method` doit égaler la méthode du corps et
  `Mcp-Name` le nom de l'outil appelé (en clair ou en `=?base64?…?=`), sinon 400 `-32020`. Les
  résultats modernes portent `resultType: "complete"` et l'identité du serveur ; les listes portent
  `ttlMs` et `cacheScope`. `server/discover` n'existe qu'en ère moderne, `ping` qu'en ère héritée.
- `notifications/…` et les réponses de client : 202, sans corps. Pas de lot JSON-RPC.
- `GET` et `DELETE` : 405 (`Allow: POST`). **Toute requête qui porte un en-tête `Origin` : 403**,
  même s'il égale `Host` (un DNS rebinding l'écrit ainsi) : aucun client visé n'est un navigateur.
- Corps : 64 Kio au plus (413 au-delà).
- Authentification : `Authorization: Bearer ostal_agent_…`. Un échec : 401 + `WWW-Authenticate:
  Bearer`, après une pause de 250 ms. **Jamais de blocage par adresse** : derrière `tailscale serve`,
  toutes les connexions viennent de 127.0.0.1, un bon jeton ne doit donc jamais être refusé à cause
  des essais d'un autre.
- Débit : **60 appels d'outil par minute et par jeton** (429 + `Retry-After`), tous outils
  confondus (l'historique, qui lit le disque, compte comme une lecture de plus : il est borné par ses
  500 points). Les autres méthodes ne sont pas comptées. Compteur en mémoire, remis à zéro au
  redémarrage. Un dépassement est écrit au journal (une ligne par minute et par agent).

Quatre points sont des **choix de cette implémentation**, faits sans la page du schéma sous les yeux,
et non des exigences relues dans les spécifications : `ttlMs` à 0 et `cacheScope` à `private` sur les
listes, la forme du résultat de `server/discover`, `_meta` d'une requête moderne non exigé, `Mcp-Name`
vérifié pour `tools/call` seulement. À confirmer à l'essai avec un vrai client (étape E5).

## Les outils (E1a et E1b)

Tous les outils de lecture sont en lecture seule (`readOnlyHint`), sans paramètre superflu (un argument inconnu est refusé par le
serveur, pas seulement par le schéma). Chaque résultat est un objet `structuredContent` et **le même
JSON** en bloc `text`. Une erreur d'exécution est un résultat `isError: true` avec un code de motif
(`invalid_argument`, `not_found`, `forbidden`) et une phrase en français. Un jeton ne voit dans
`tools/list` que les outils qu'il peut appeler, et **le compte du jeton doit avoir le droit de lire la
maison** (`View`, toujours accordé en V1) en plus du droit du jeton.

### Avec le droit « lecture »

| Outil | Rend |
|---|---|
| `ostal_home_summary` | nombre d'appareils, joignables, injoignables, en connexion ; la phrase d'état (la même que sur les autres écrans) ; nombre de caméras ; piles faibles. **Le mode seulement avec le droit « occupation »** ; sans lui, une caméra éteinte par le mode n'est ni comptée ni nommée |
| `ostal_list_devices` | 200 appareils au plus : `id` (texte décimal), `name`, `kind` (`cover`, `on_off`, `sensor`, `other`), `room` (`{id, name}` ou `null`), `status` (`online`, `connecting` ou `unreachable` avec une raison fermée), `last_seen`, `latest`. Filtres : `kind`, `room` (identifiant de pièce), `only_problems`, `limit` |
| `ostal_get_device` | un appareil par son `id` : mêmes champs, plus `capabilities`, `open_since`, `cover_motion` |
| `ostal_list_rooms` | les étages et leurs pièces : `id` (texte décimal) et `name` de chacun, `device_count` par pièce, `devices_without_room`. **Sans coordonnées.** L'identifiant de pièce se donne au filtre `room` de `ostal_list_devices` (deux étages peuvent avoir une pièce du même nom) |
| `ostal_get_history` | `id`, `metrics?` (liste de `temperature`, `humidity`, `power`, `energy`, `battery`, `position`, `open`, `on_off`, `battery_low`), `since?` et `until?` (secondes Unix ; 24 h avant maintenant par défaut, 7 jours au plus), `buckets?` (1 à 48, 24 par défaut) : des points `{at, metric, value}` lissés (minimum et maximum de chaque tranche), **500 au plus** (les plus récents, avec `truncated`). Jamais le CSV |
| `ostal_list_automations` | 100 au plus : `id`, `name`, `enabled`, `summary` (une phrase fermée : « Quand la porte s'ouvre, sous 1 condition : notifier. »). **Jamais** une adresse de webhook, un corps de requête, le titre ou le texte d'une notification ; un mode n'y est pas nommé |
| `ostal_list_automation_runs` | `limit?` (1 à 50, 20 par défaut) : `rule_id`, `name`, `at`, `ok`. **Pas de compte rendu** : il cite l'hôte d'un webhook, l'erreur de `curl` ou le mode demandé (« mode : Nuit »). Sans le droit « occupation », les exécutions des règles liées au mode ou à la détection (et celles d'une règle qui n'existe plus, qu'on ne sait pas classer) sont omises : leur date dit que le mode vient de changer |
| `ostal_list_cameras` | `id`, `name`, `placement` (`indoor`, `outdoor`), `reachable`. **Jamais une adresse, un flux ni une image.** `reachable` n'est faux que pour une caméra dite injoignable : une caméra éteinte par le mode ou pas encore vérifiée compte comme joignable, sinon l'agent verrait quelles caméras sont éteintes, donc le mode. Avec « occupation » : aussi `behavior_now` |

### Avec le droit « occupation » (en plus de « lecture »)

« Vacances » dit que la maison est vide, une caméra intérieure qui surveille dit que personne n'y est.
Ce droit est **éteint par défaut**, jamais donné par la ligne de commande, et il se coche agent par
agent. Le droit « mode » (étape E3) le supposera : pour changer le mode, il faut le connaître.

| Outil | Rend |
|---|---|
| `ostal_get_mode` | `key` (`present`, `absent`, `nuit`, `vacances`), `label`, `effect` (la phrase par défaut du mode) et `cameras` : `{id, name, behavior_now}` selon le tableau de **chaque caméra** (`off`, `live` ou `surveillance`) |
| `ostal_get_presence` | `people` : `name` (prénom nettoyé), `state` (`home` ou `away`), `since`, `me` (c'est le propriétaire du jeton). Les personnes actives dont l'état est connu. **Jamais** le nom d'un téléphone, une adresse, l'état de la box ni le réglage du mode automatique |
| `ostal_home_summary`, `ostal_list_cameras`, `ostal_list_automation_runs` | donnent en plus le mode (`mode`), `behavior_now`, les exécutions des règles de mode |

Fuite résiduelle, connue et acceptée par les spécifications : le **nom** d'une règle (« Vacances :
fermer les volets ») peut laisser deviner le mode.

**Le texte venu de la maison est une donnée, jamais une consigne.** Les noms (personnalisés, ou écrits
par le fabricant) passent champ par champ dans le même nettoyeur que les notifications (hôtes,
identifiants et mots de passe de caméras, adresses, identifiants de nœud masqués), sans caractères de
contrôle, coupés à 80 caractères. Les identifiants ne sont jamais nettoyés. `vendor` et `product`
ne sortent pas. Aucune adresse de webhook, aucune URL de caméra, aucune image n'est jamais rendue.
Une consigne cachée dans un nom reste un texte : c'est pourquoi les étapes qui commandent exigeront
une confirmation humaine côté serveur.

## Les commandes (E2)

Un seul outil, avec le droit « commandes » (`control`) **en plus** de « lecture » :

| Outil | Paramètres | Rend |
|---|---|---|
| `ostal_set_on_off` | `id` (texte décimal), `command` : `On` ou `Off` | `{id, name, command, sent: true, note}` : la commande est **envoyée**, pas confirmée ; relire l'appareil (`ostal_get_device`) pour vérifier. Annotations : `readOnlyHint: false`, `destructiveHint: false`, `idempotentHint: true` |

Pas de `Toggle` : « inverser » n'est pas rejouable sans danger (Claude Code rejoue un appel
idempotent si la connexion tombe), l'agent doit viser l'état final. Seuls les appareils qui ont un
marche/arrêt et ne sont pas un volet (`kind: on_off` : prises et lampes) se commandent.

### Qui peut commander quoi : tout se règle à la main dans `agents.json`

Rien ne se règle encore par un écran (étape E4), et la ligne de commande ne donne **jamais** ce
droit (`agent add` ne crée que des jetons en lecture). On modifie `serveur/agents.json` (0600),
serveur arrêté ou non : il est relu dès qu'il change.

```json
{
  "version": 1,
  "enabled": true,
  "never": ["13828100"],
  "agents": [
    {
      "id": "…", "user_id": "…", "label": "Claude Code sur le portable",
      "scopes": ["read", "control"],
      "devices": ["13828101", "13828102"],
      "hash": "…", "created": 0, "expires": 0
    }
  ]
}
```

- `scopes` : ajouter `"control"`.
- `devices` : les appareils que **ce jeton** peut commander, en identifiants décimaux (ceux de
  `ostal_list_devices`). **Vide ou absent : aucun.** 20 au plus : avec 21 entrées, ou une seule
  entrée illisible, le jeton ne commande **rien** (on ne coupe pas une liste au hasard).
- `never` : la liste « jamais par un agent », pour **tous** les agents : congélateur, box, **prise du
  Mac serveur** (la couper supprimerait aussi le journal), chauffe-eau, alarme. Elle l'emporte sur
  `devices`. **Une entrée illisible (faute de frappe, zéro initial) bloque toute commande de tout
  agent** : un identifiant mal recopié ne doit jamais laisser un congélateur sans protection.
  Choisissez-la activement avant de donner le droit : elle est vide tant qu'on ne l'écrit pas.
- La ligne de commande (`agent add`, `revoke`, `enable`, `disable`) réécrit ce fichier **sans perdre**
  ces listes.

### L'ordre des contrôles

1. Le droit « lecture » du jeton et du compte, puis le droit « commandes » du jeton et celui du compte
   (`ControlDevices`) : sinon `forbidden` (`agent_denied` : `scope`, `rights` ou `control_rights`).
2. Les arguments : `id` (texte décimal canonique) et `command` (`On` ou `Off`, ni `Toggle` ni autre
   casse), rien d'autre (`invalid_argument`).
3. L'appareil est dans `devices`, **avant** de chercher s'il existe, puis il n'est pas dans `never` :
   dans les deux cas **la même phrase** (« Cet agent n'a pas le droit de commander cet appareil. »)
   sans rien dire de la maison : un agent ne sonde pas les appareils ni la liste « jamais ».
4. L'appareil existe (`not_found`) et c'est une prise ou une lampe (`invalid_argument`).
5. **Les automatisations.** Une prise qui s'allume peut faire partir « quand la prise s'allume, change
   le mode » ou « …appelle cette adresse » : l'agent ferait alors, sans confirmation, ce qu'on lui
   interdit de faire directement. Le serveur regarde donc les automatisations **actives** dont le
   déclencheur est « la prise s'allume » ou « s'éteint » (dans le sens de la commande) ou une mesure
   franchissant un seuil (dans les deux sens), **de proche en proche** (une règle qui commande un
   autre appareil, dont les règles comptent aussi). La commande est **refusée** si l'une de leurs
   actions change le mode, appelle une adresse web, commande un volet, commande un appareil de la
   liste `never`, commande un appareil hors des appareils permis de ce jeton (`devices`) ou est
   inconnue de ce serveur. Les conditions sont ignorées (elles peuvent être
   vraies à ce moment-là), les règles désactivées ne comptent pas (le calcul est refait à chaque
   commande). La réponse dit seulement « ferait partir une automatisation que seule une personne
   peut déclencher », **sans nom de règle, de mode ni d'adresse**.
6. Le débit : **20 commandes par minute et par jeton**, en plus des 60 appels (429 +
   `Retry-After`), compté avant tout le reste pour un jeton qui a le droit de commander ; une
   commande refusée compte aussi.
7. `Hub::set_on_off`, puis la ligne du journal.

Limite connue, dite en clair : deux sortes de déclencheurs ne sont pas suivies. Une règle « quand
la prise X reste injoignable » (`Unreachable`) : couper la prise d'un autre appareil peut en
déclencher une. Un seuil (`Threshold`) posé sur **un autre appareil** dont la mesure dépend de la
prise commandée (un thermomètre près d'un radiateur, par exemple) : le serveur ne sait pas que
l'un influe sur l'autre.

### Dans le journal

- Une ligne `device_command` **par commande** (réussie ou en erreur) : `source: "agent"`, la
  connexion `agent` et son libellé, `target` le nom (nettoyé) de l'appareil, `detail` `on` ou `off`.
  Jamais regroupée avec les lectures, jamais le jeton.
- Un refus : `agent_denied` (`result: refused`), au plus une ligne par minute, par agent et par
  motif, avec un mot fermé : `device` (hors de `devices` ou dans `never`), `rule` (une automatisation
  réservée aux personnes), `control_rights` (le compte n'a plus le droit de commander), `scope`,
  `rate`. Jamais l'identifiant ni le nom de l'appareil visé.

## Le journal d'audit des agents

Chaque ligne porte le compte propriétaire du jeton, `connection.kind: "agent"` avec le libellé de
l'agent et l'identifiant du jeton (jamais le jeton), et `source: "agent"`. `ip` et `via` y sont, mais
derrière `tailscale serve` ils n'identifient rien : c'est le libellé de l'agent qui compte. L'onglet
Activité d'Ostal Server et `ostal-server audit` les lisent déjà (`audit::describe`).

| Ligne | Quand |
|---|---|
| `agent_read` | **les lectures ordinaires, regroupées** : une ligne par agent et par tranche de 10 minutes, écrite à l'échéance (un balayeur la passe chaque minute, et à l'arrêt du serveur). `detail` : `N:outil,outil` (le nombre de lectures et les outils lus, sans `ostal_`). Jamais une valeur lue ni un argument |
| `agent_occupation` | **une ligne par lecture** du droit « occupation » : `ostal_get_mode`, `ostal_get_presence`, et `ostal_home_summary`, `ostal_list_cameras`, `ostal_list_automation_runs` quand le jeton a le droit (leur réponse en dépend). Jamais regroupée. Écrite avant la réponse |
| `agent_denied` | un appel refusé, `result: refused`, `detail` : `unknown` (outil inconnu), `scope` (droit du jeton manquant), `rights` (le compte n'a plus le droit de lire la maison), `control_rights` (ni de commander), `device` ou `rule` (une commande refusée, voir [Les commandes](#les-commandes-e2)) ou `rate` (débit dépassé). Au plus **une ligne par minute, par agent et par motif**. Le nom d'un outil inconnu n'y entre jamais |

Un changement de mode fait par un agent (étape E3) s'écrira `source: "agent"` ; un ancien lecteur du
journal qui ne connaît pas cette origine la lit « autre ».

## Choix de cette implémentation

Hors des spécifications ou à confirmer :

- **Comment un agent reçoit le droit « occupation »** : les spécifications disent qu'il se coche
  depuis une session (étape E4), jamais en ligne de commande. Cette étape ne livre donc **pas** le
  moyen de le donner : `ostal-server agent add` continue de refuser toute option de droits, et le
  serveur n'écrit jamais `agents.json`. Les tests le donnent en réécrivant le fichier.
- Un jeton doit avoir « lecture » **pour tout outil**, y compris ceux de l'occupation (« occupation »
  s'ajoute à « lecture », il ne la remplace pas).
- `ostal_list_automation_runs` ne rend **pas** le texte des exécutions (les spécifications en
  parlaient, assaini) : il peut citer un hôte de webhook, une erreur de `curl` ou le mode demandé.
- `ostal_list_cameras` : `reachable` est vrai pour une caméra éteinte ou non vérifiée (voir plus haut).
- Les mots `indoor`, `outdoor`, `off`, `live`, `surveillance`, `home`, `away` et la forme du détail
  d'une ligne `agent_read` sont de notre fait.
- `ostal_get_history` : `since` et `until` en secondes Unix (pas de date écrite), 500 points au plus
  au total.
- Pas de ligne `agent_expired` : un jeton expiré est simplement refusé (401, comme avant).
- **E2** : « 20 appareils au plus » est lu comme **20 appareils permis par jeton** (`devices`) ; les
  spécifications parlent aussi de 20 commandes par minute, appliquées de même. Une liste plus longue
  ne permet aucun appareil. Les listes `devices` et `never` sont **écrites à la main** (l'écran
  d'activation est l'étape E4). Une liste `devices` vide ou absente ne permet rien : le droit
  « commandes » seul ne commande rien. Les règles **désactivées** ne comptent pas dans le calcul
  des automatisations déclenchées (elles ne partiraient pas). Pour l'appareil commandé, le sens
  compte (« s'allume » ou « s'éteint ») ; pour les appareils commandés ensuite par des règles, on
  suppose les deux sens. Le texte de refus ne nomme jamais la règle, le mode ni l'adresse.
  Un appel à `ostal_set_on_off` par un jeton sans le droit est un refus `scope` (comme pour
  `ostal_get_mode`), pas un outil inconnu.
- **Jetons refusés** : un échec d'authentification n'est ni compté ni écrit au journal d'audit
  (les spécifications techniques disaient « compté »). Il est retardé de 250 ms et une ligne du
  journal de fonctionnement (adresse, jamais le jeton) le signale ; aucun blocage par adresse.
- Les noms d'appareils rendus à l'agent perdent aussi les caractères invisibles ou bidi (largeur
  nulle, U+202A à U+202E, U+2066 à U+2069, séparateurs de ligne, étiquettes), pas seulement les
  caractères de contrôle.
