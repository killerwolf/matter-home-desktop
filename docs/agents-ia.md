# Agents d'IA : le serveur MCP intégré (étapes E1a à E4)

Ostal Server peut recevoir un agent d'IA (Claude Code, Cursor…) qui lit l'état de la maison par le
protocole MCP. **C'est éteint tant que vous ne l'allumez pas.** Les appareils, les pièces,
l'historique, les automatisations et les caméras se lisent avec le droit de lecture ; **la présence
et le mode** demandent un droit à part, « occupation », éteint par défaut. Un troisième droit,
« commandes », permet d'**allumer ou d'éteindre des prises et des lampes** et de **commander des
volets**, un par un ou **par pièce**, parmi les appareils que vous avez nommés. Un quatrième, « mode »,
permet de **changer le mode de la maison** (il suppose « occupation »). **Ce qui est sensible n'est
jamais exécuté sur la seule parole de l'agent** : une personne doit l'autoriser dans l'app web (voir
[Les confirmations](#les-confirmations-e3)).

**Les écrans (étape E4)** : chaque personne crée, voit et révoque **ses** agents dans l'app web
(« Mes agents »). L'interrupteur général, « Tout couper », la liste de tous les agents et les listes
« jamais » et « sensible » se règlent dans **Ostal Server › Réglages › Agents IA**, c'est-à-dire
devant le Mac du serveur. Voir [Créer un agent](#créer-un-agent-app-web),
[Réglages Agents IA dans Ostal Server](#réglages-agents-ia-dans-ostal-server) et
[Brancher Claude Code et Cursor](#brancher-claude-code-et-cursor).

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
  commander ou changer le mode se donne depuis une session (l'app web), jamais d'un terminal. Durée : 90 jours
  par défaut, 365 au plus. 16 agents au plus par personne, un libellé par agent.
- Ces commandes agissent aussi sur un serveur qui tourne, sans redémarrage : le serveur relit
  `serveur/agents.json` dès qu'il change. Une révocation prend effet à la requête suivante.
- Un compte désactivé coupe ses agents. Un jeton expiré est refusé.
- Rangez le jeton dans une variable d'environnement ou le Trousseau, **pas** dans un fichier de
  configuration (voir [Brancher Claude Code et Cursor](#brancher-claude-code-et-cursor)).

L'interrupteur, les listes et les jetons vivent dans `serveur/agents.json` (0600, jamais exporté) :
seule l'**empreinte** SHA-256 de chaque jeton y est gardée. Un fichier illisible coupe tout l'accès
(et n'est jamais réécrit). Le journal d'audit note la création et la révocation d'un agent, les
changements d'interrupteur et des listes (`ostal-server audit`), jamais le jeton.

**Qui écrit ce fichier.** Trois écrivains : la ligne de commande, les routes de l'app web (`/api/agents`,
sous une session) et la fenêtre d'Ostal Server. Chacun relit le fichier sur le disque avant de le
modifier, sous un verrou (un mutex du processus, et un verrou de fichier `agents.lock` pour la ligne
de commande lancée pendant que le serveur tourne), et ne réécrit rien quand rien ne change. Une
panne d'écriture se dit, elle ne perd rien.

## Créer un agent (app web)

App web › « Mes agents ». Une personne connectée voit **ses** agents (jamais ceux d'une autre) et en
crée un en choisissant :

- un **nom** (48 caractères au plus, unique pour elle : « Claude Code sur le portable ») ;
- une **durée** (7, 30, 90, 180 jours ou 1 an ; 90 par défaut) ;
- les **droits** : « lecture » est toujours donnée ; « voir qui est à la maison et le mode »
  (occupation), « commander des prises, des lampes et des volets » (avec les appareils permis, 20 au
  plus, cochés un par un) et « changer le mode » (qui suppose « occupation »).

**Le compte ne donne que ce qu'il a lui-même** : « commandes » demande le droit de commander, « mode »
celui de changer le mode. Les appareils proposés sont les prises, lampes et volets **hors de la liste
« jamais »** ; ceux de la liste « sensible » sont marqués (« chaque commande attend votre accord »).
Le serveur vérifie tout cela de son côté : un corps hors forme, un droit inconnu, un appareil qui
n'existe pas, qui n'est pas commandable ou qui est « jamais », une durée hors de 1 à 365 jours, un
17e agent ou un nom déjà pris sont refusés (400), sans rien écrire.

**Le jeton s'affiche une seule fois**, avec les lignes à copier pour brancher le client (voir plus bas).
Il n'est ni relu, ni gardé par l'app (il disparaît quand on ferme la feuille), ni écrit au journal ;
la réponse porte `Cache-Control: no-store`. Chaque agent montre ses droits, ses appareils permis,
son expiration et, **depuis le démarrage du serveur**, son dernier appel et ses appels du jour
(compteurs en mémoire). « Révoquer » coupe l'agent à la requête suivante.

Les routes (sous une **session** : un jeton d'agent, de mode ou de lecture y reçoit 401, un agent ne
se crée donc pas d'agent et ne se donne pas de droits) :

| Route | |
|---|---|
| `GET /api/agents` | `{enabled, default_days, max_days, max_devices, agents, never, sensitive, can}` : les agents de cette personne ; `never` et `sensitive` sont des identifiants, `can` ce que le compte peut donner |
| `POST /api/agents` | `{label, days?, scopes: ["read", "occupation", "control", "mode"], devices?: ["…"]}` → `201 {agent, token}`. Corps de 4 Kio au plus, aucun champ inconnu |
| `DELETE /api/agents/{id}` | `202`. Un agent inconnu **ou à une autre personne** : le même `404` ; un libellé ne désigne rien, seul l'identifiant |

**L'app web ne règle ni l'interrupteur ni les listes** : elle montre l'état (« éteint sur ce serveur »)
et renvoie vers le Mac du serveur. Un agent qu'on crée alors que l'interrupteur est éteint existe,
mais ne se connecte pas.

## Réglages Agents IA dans Ostal Server

La fenêtre tourne dans le même processus que le serveur : ses gestes écrivent `agents.json` et le
serveur les voit à la requête suivante. Chaque geste écrit une ligne au journal, au nom d'« Ostal Server ».

- **Avertissements** en tête : un agent d'IA envoie ce qu'il lit à son fournisseur ; et un agent qui
  a un terminal **sur ce Mac** peut contourner les protections (voir la limite plus haut), donc prévoir
  l'agent sur un autre ordinateur.
- **Interrupteur « Autoriser les agents IA »**, éteint par défaut. L'allumer pour la première fois, avec
  des listes encore vides, ouvre tout de suite le choix des listes.
- **« Tout couper »** (avec confirmation) : éteint l'interrupteur, **révoque tous les agents** (une
  seule écriture), oublie les demandes en attente et celles qui étaient autorisées sans avoir servi.
  Les listes restent. Ce n'est pas défaisable : il faut recréer chaque agent avec un nouveau jeton.
- **Tous les agents**, toutes personnes confondues : nom, personne, droits, appareils permis (par
  nom), dernier appel et appels du jour (depuis le démarrage), expiration, « Révoquer » (avec confirmation).
- **Les listes** « jamais par un agent » (aucun agent ne les commande, quel que soit son jeton) et
  « sensible » (chaque commande attend votre accord). **Propositions par défaut**, d'après le nom des
  appareils, ignorant la casse et les accents : « jamais » = congélateur, box, Mac ou serveur ;
  « sensible » = garage, baie. Elles sont **cochées d'avance, pas écrites** : rien ne l'est avant
  « Enregistrer ». Sans appareil de ce nom, les listes restent vides à remplir. Un appareil « jamais »
  n'est jamais aussi « sensible ». Le serveur n'écrit que des identifiants canoniques (une seule entrée
  illisible bloquerait toute commande, ou rendrait tout appareil sensible) ; si une entrée écrite à la
  main est illisible, la fenêtre le dit et « Enregistrer » la répare.
- Ce que font les agents se lit dans l'onglet **Activité**, filtre **Source › Agents d'IA** : leurs
  appels, leurs refus, leurs demandes à valider, leur création, leur révocation, l'interrupteur et les
  listes. La colonne « Depuis » y dit « Agent d'IA · Claude Code ». Le filtre agit **avant** la limite
  de 500 lignes (elles sont toutes des lignes d'agent). `ostal-server audit` écrit la même mention.

## Brancher Claude Code et Cursor

> **Ces configurations suivent la documentation de Claude Code et de Cursor ; elles n'ont pas été
> essayées avec les vrais clients** (ce serait appeler un vrai service d'IA). L'essai de l'étape E5
> les confirmera ou les corrigera. Si un client refuse, la ligne de commande
> `ostal-server agent list` et l'onglet Activité disent ce que le serveur a vu.

**Où est l'adresse.** `POST /mcp` est sur **le même écouteur que l'app web**, donc la même adresse que
celle que vous ouvrez dans le navigateur, suivie de `/mcp`.

| Où tourne l'agent | Adresse | Certificat |
|---|---|---|
| Un autre ordinateur de la maison ou à distance, par **Tailscale serve** | `https://<nom>.<tailnet>.ts.net/mcp` (voir [Accès à distance](acces-distant.md)) | un vrai certificat (Tailscale), rien à ajouter |
| Un autre ordinateur du réseau local | `https://<nom du Mac>.local:8443/mcp` ou l'adresse locale du Mac | celui d'Ostal, **auto-signé** : voir plus bas. Il annonce le nom de la machine (et `.local`), ses adresses et `localhost`, **pas** le nom `.ts.net` : sur `…ts.net:8443`, un client qui contrôle le nom le refusera, prenez le port 443 de `tailscale serve` |
| **Le Mac du serveur lui-même** | `https://localhost:8443/mcp` | idem. **À éviter** : un agent qui a un terminal sur ce Mac peut contourner les protections d'Ostal (voir la limite en tête de page) |

Aucun Funnel, aucun tunnel : l'accès public n'existe pas. L'app Claude (claude.ai) et l'iPhone ne sont
pas visés : leurs requêtes viendraient du cloud d'Anthropic, donc d'une adresse publique.

**Le jeton ne s'écrit jamais en clair dans une configuration.** Claude Code écrit la ligne
`--header "Authorization: Bearer …"` telle quelle dans `~/.claude.json` : ne la faites pas. Et ne
collez pas le jeton dans une commande de shell (il resterait dans l'historique et dans la liste des
processus). À la place :

1. **Le ranger dans le Trousseau** du Mac de l'agent. La commande demande le jeton au clavier :

   ```sh
   security add-generic-password -U -s ostal-agent -a "$USER" -w
   ```

   (`-w` en dernier : sans valeur, `security` la demande ; collez le jeton, ni lui ni la commande ne
   passent par l'historique.)
2. **Le lire dans une variable d'environnement**, dans votre profil de terminal (`~/.zshrc`), sans
   qu'il y figure :

   ```sh
   export OSTAL_AGENT_TOKEN="$(security find-generic-password -s ostal-agent -w)"
   ```

3. **Claude Code** : fichier `.mcp.json` du projet. Le jeton y est une variable (`${…}` est développée
   par Claude Code), jamais une valeur :

   ```json
   {
     "mcpServers": {
       "ostal": {
         "type": "http",
         "url": "https://<adresse>/mcp",
         "headers": { "Authorization": "Bearer ${OSTAL_AGENT_TOKEN}" }
       }
     }
   }
   ```

   Variante sans variable d'environnement : `headersHelper`, une commande que Claude Code lance à
   chaque connexion et qui écrit les en-têtes en JSON sur sa sortie (elle est coupée au bout de 10
   secondes). Par exemple un script `~/.config/ostal/agent-headers.sh`, `chmod 700`, qui lit le
   Trousseau :

   ```sh
   #!/bin/sh
   printf '{"Authorization":"Bearer %s"}\n' "$(security find-generic-password -s ostal-agent -w)"
   ```

   puis `"headersHelper": "/Users/vous/.config/ostal/agent-headers.sh"` à la place de `headers`.
4. **Cursor** : `.cursor/mcp.json`, avec l'interpolation `${env:…}` :

   ```json
   {
     "mcpServers": {
       "ostal": {
         "url": "https://<adresse>/mcp",
         "headers": { "Authorization": "Bearer ${env:OSTAL_AGENT_TOKEN}" }
       }
     }
   }
   ```

   Lancez Cursor depuis un terminal où la variable existe, ou définissez-la pour toute la session.

**Le certificat auto-signé** (adresse locale, pas Tailscale). Claude Code est un programme Node :
donnez-lui le certificat d'Ostal avec `NODE_EXTRA_CA_CERTS=/chemin/tls-cert.pem` (le fichier
`tls-cert.pem` se trouve dans `serveur/` du dossier de données ; il se copie depuis le Mac du serveur.
Il n'a rien de secret, mais **copiez seulement lui : `tls-key.pem`, la clé privée, ne sort jamais**). Ne le faites pas entrer dans le trousseau du système si
vous voulez éviter de changer un réglage de sécurité du Mac. Que Claude Code accepte une feuille
auto-signée comme ancre reste à confirmer à l'essai (E5) ; par Tailscale serve la question ne se pose pas.

**Dans le journal et dans les réglages**, l'agent apparaît sous le nom que vous lui avez donné.
Derrière `tailscale serve`, toutes ses requêtes viennent de 127.0.0.1 : seule compte son nom.

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
Une consigne cachée dans un nom reste un texte : c'est pourquoi ce qui est sensible exige une
confirmation humaine côté serveur (voir [Les confirmations](#les-confirmations-e3)).

## Les commandes (E2)

Le premier outil de commande, avec le droit « commandes » (`control`) **en plus** de « lecture » (les volets, les pièces et le mode viennent à l'étape E3, plus bas) :

| Outil | Paramètres | Rend |
|---|---|---|
| `ostal_set_on_off` | `id` (texte décimal), `command` : `On` ou `Off` | `{id, name, command, sent: true, note}` : la commande est **envoyée**, pas confirmée ; relire l'appareil (`ostal_get_device`) pour vérifier. Annotations : `readOnlyHint: false`, `destructiveHint: false`, `idempotentHint: true` |

Pas de `Toggle` : « inverser » n'est pas rejouable sans danger (Claude Code rejoue un appel
idempotent si la connexion tombe), l'agent doit viser l'état final. Seuls les appareils qui ont un
marche/arrêt et ne sont pas un volet (`kind: on_off` : prises et lampes) se commandent par cet outil ; les volets ont le leur, `ostal_set_cover`.

### Qui peut commander quoi

Les droits d'un agent se cochent à sa création dans l'app web, et les listes « jamais » et
« sensible » se règlent dans la fenêtre d'Ostal Server (voir plus haut). La ligne de commande ne
donne **jamais** ce droit (`agent add` ne crée que des jetons en lecture). On peut aussi modifier
`serveur/agents.json` (0600) à la main, serveur arrêté ou non : il est relu dès qu'il change. Le
fichier ressemble à ceci :

```json
{
  "version": 1,
  "enabled": true,
  "never": ["13828100"],
  "sensitive": ["13828103"],
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

- `scopes` : ajouter `"control"` (prises, lampes et volets) et/ou `"mode"` (changer le mode ; il suppose `"occupation"`).
- `devices` : les appareils (prises, lampes et volets) que **ce jeton** peut commander, en identifiants décimaux (ceux de
  `ostal_list_devices`). **Vide ou absent : aucun.** 20 au plus : avec 21 entrées, ou une seule
  entrée illisible, le jeton ne commande **rien** (on ne coupe pas une liste au hasard).
- `never` : la liste « jamais par un agent », pour **tous** les agents : congélateur, box, **prise du
  Mac serveur** (la couper supprimerait aussi le journal), chauffe-eau, alarme. Elle l'emporte sur
  `devices`. **Une entrée illisible (faute de frappe, zéro initial) bloque toute commande de tout
  agent** : un identifiant mal recopié ne doit jamais laisser un congélateur sans protection.
  Choisissez-la activement avant de donner le droit : elle est vide tant qu'on ne l'écrit pas (la
  fenêtre d'Ostal Server en propose une d'après le nom des appareils, voir plus haut).
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

Limite connue, dite en clair : trois sortes de déclencheurs ne sont pas suivies. Une règle « quand
la prise X reste injoignable » (`Unreachable`) : couper la prise d'un autre appareil peut en
déclencher une. Un seuil (`Threshold`) posé sur **un autre appareil** dont la mesure dépend de la
prise commandée (un thermomètre près d'un radiateur, par exemple) : le serveur ne sait pas que
l'un influe sur l'autre. Et le capteur de contact d'une porte de garage est un autre appareil que
son moteur : « quand la porte s'ouvre » ne se déduit pas de la commande du moteur.

### Dans le journal

- Une ligne `device_command` **par commande** (réussie ou en erreur) : `source: "agent"`, la
  connexion `agent` et son libellé, `target` le nom (nettoyé) de l'appareil, `detail` `on` ou `off`.
  Jamais regroupée avec les lectures, jamais le jeton.
- Un refus : `agent_denied` (`result: refused`), au plus une ligne par minute, par agent et par
  motif, avec un mot fermé : `device` (hors de `devices` ou dans `never`), `rule` (une automatisation
  réservée aux personnes), `control_rights` (le compte n'a plus le droit de commander), `scope`,
  `rate`. Jamais l'identifiant ni le nom de l'appareil visé.

## Les confirmations (E3)

Quand une action est sensible, le serveur **ne l'exécute pas**. Il met une demande en attente, prévient
la personne à qui appartient l'agent et répond à l'agent :

```json
{"status": "confirmation_required", "confirmation_id": "…32 chiffres hexadécimaux…", "state": "pending",
 "expires_at": 1795525023.9, "message": "En attente de l'accord de Hamdi dans l'app Ostal (5 minutes)…"}
```

1. **La personne** voit la carte « Demandes à valider » en tête de l'accueil de l'app web (et reçoit une
   notification, si elle en a activé la catégorie « Demandes d'un agent d'IA à valider ») : le nom de
   l'agent, **une phrase fabriquée par Ostal** (« Mettre la maison en mode Vacances (elle est en
   Absent). »), **pourquoi** une personne doit valider, les **automatisations qui partiraient** (leurs
   noms, jamais une adresse), un compte à rebours, **Autoriser** et **Refuser**. Rien de ce texte ne
   vient de l'agent.
2. **L'agent** rappelle le **même outil** avec les **mêmes arguments** et `confirmation_id`, ou lit
   `ostal_confirmation_status`. Le serveur exécute seulement si la demande est autorisée, qu'elle est
   **de ce jeton**, qu'elle porte la **même empreinte**, qu'elle a moins de 5 minutes et qu'elle n'a pas
   déjà servi. Elle passe à « utilisée » **avant** l'exécution : une commande qui échoue ne rend pas
   l'autorisation.
3. Toute autre issue : `isError` avec le code `confirmation_invalid` (refusée, expirée, déjà utilisée,
   inconnue, ou « les arguments ou la situation ne sont plus ceux de la demande »). Un `confirmation_id`
   donné est **toujours vérifié**, jamais ignoré.

| Garantie | Comment |
|---|---|
| Seule **la personne propriétaire de l'agent** autorise ou refuse | pour toute autre, ou un identifiant inconnu, la demande n'existe pas (404). Un agent révoqué ou expiré n'a plus de demande visible |
| **L'agent ne s'autorise pas lui-même** | les routes sont `GET /api/agent-requests`, `POST /api/agent-requests/{id}/approve` et `…/refuse`, **sous une session** : un jeton d'agent, de mode ou de lecture y reçoit 401 |
| Autoriser demande de **pouvoir faire la chose soi-même** | le compte doit avoir le droit de commander (appareils) ou de changer le mode ; refuser est toujours possible |
| **5 minutes, une seule fois** | en mémoire seulement : un redémarrage annule toutes les demandes |
| **Même empreinte** | l'outil, ses arguments, **ce que le serveur en a déduit** (appareils visés, mode de départ, règles qui partiraient, motifs). Si la situation change entre la demande et l'exécution, l'autorisation ne vaut plus et n'est pas brûlée |
| Bornes | 3 demandes vivantes et 10 nouvelles par heure et par jeton, 128 en tout : au-delà, `rate_limited`. Rappeler la même demande rend la même (pas de nouvelle place) |
| Contrôles **refaits** à chaque appel | une autorisation ne contourne ni le droit du jeton, ni `devices`, ni `never`, ni le droit du compte |

**La notification** part vers les appareils de **cette personne seulement** (`Push::notify_user`), avec
un texte **générique** (« « Claude Code » attend votre accord dans l'app Ostal ») : il s'affiche sur
l'écran verrouillé, où le mode demandé (« Vacances » dit que la maison est vide) n'a pas sa place. Un
appareil abonné avant les comptes (sans propriétaire) ne reçoit rien. Sans abonnement, la carte de
l'app web reste le seul signal.

### Ce qui demande l'accord d'une personne

| Outil | Confirmation |
|---|---|
| `ostal_set_on_off`, `ostal_set_cover`, `ostal_set_room` | **non**, sauf si un appareil visé est **sensible** (liste `sensitive`), ou si la commande fait partir, de proche en proche, une automatisation qui change le mode, appelle une adresse web, commande un appareil sensible, « jamais » ou hors des appareils permis du jeton, ou est inconnue |
| `ostal_set_mode` | **décidée par l'effet réel**, voir plus bas |

### Les outils de l'étape E3

| Outil | Droit | Paramètres | Rend |
|---|---|---|---|
| `ostal_set_cover` | « commandes » | `id`, puis **soit** `command` (`Open`, `Close`, `Stop`) **soit** `position` (0 à 100, 100 = tout ouvert) | comme `ostal_set_on_off` : `{id, name, command, sent: true, note}` |
| `ostal_set_room` | « commandes » | `room_id` (l'identifiant de `ostal_list_rooms`, **jamais un nom**), `kind` (`on_off` ou `cover`), `command` ou `position` | `{room, kind, sent: [{id, name}], failed: [{id, name, code}], skipped: [{id, name, reason: "not_permitted"}], note}` |
| `ostal_set_mode` | « mode » | `mode` (`present`, `absent`, `nuit`, `vacances`) | `{mode, changed, cameras: [{id, behavior_now}], rules_triggered}`, ou `confirmation_required` avec `reasons` et `rules` |
| `ostal_confirmation_status` | « commandes » ou « mode » | `confirmation_id` | `{confirmation_id, state, tool, expires_at}` ; `state` : `pending`, `approved`, `refused`, `expired`, `used`. **Seulement les demandes de ce jeton** : celle d'un autre est « inconnue » |

Tous les outils d'écriture portent `_meta["anthropic/requiresUserInteraction"]: true` (un second filet
côté Claude Code, **sans valeur de sécurité** : le serveur exige l'accord de toute façon) ; seul
`ostal_set_on_off` est `idempotentHint` (viser un état rejoue sans effet). `ostal_set_room` compte pour
**son nombre d'appareils** dans les 20 commandes par minute (tout ou rien) et écrit **une ligne
`device_command` par appareil**.

**`ostal_set_room`** : les appareils de la pièce du bon type, **20 au plus** (la liste d'un jeton n'en
permet pas plus). Ceux que ce jeton ne peut pas commander (hors de `devices`, ou dans `never`) sont
**sautés et listés avec un seul motif, `not_permitted`** : l'agent ne distingue pas la liste « jamais ».
Si aucun n'est permis, c'est le même refus qu'une commande seule. Une pièce ne se désigne que par son
identifiant, jamais un nom (deux étages peuvent avoir une pièce du même nom).

### Le mode, décidé par l'effet

Le serveur calcule le passage du mode actuel au mode demandé **avec le tableau réel de chaque caméra**
(pas seulement l'emplacement), et demande l'accord si :

1. une caméra qui **surveille** cesserait de surveiller (elle passerait à `off` ou `live`) : on se désarme ;
2. une caméra **intérieure** se mettrait à surveiller ou à diffuser alors que **quelqu'un est à la
   maison, ou que la présence est inconnue** (box injoignable, ou une personne active sans état
   déclaré : dans le doute, quelqu'un est là) ;
3. le mode demandé est **`vacances`**, toujours ;
4. le changement fait partir des règles « quand le mode devient… » qui appellent une adresse web,
   changent le mode, sont inconnues, ou commandent un appareil sensible ou « jamais ».

Avec les réglages d'origine : Présent → Nuit passe tout de suite (on s'arme) ; **Nuit → Présent en
demande une** (les caméras extérieures quittent la surveillance : c'est le prix de la règle par
l'effet, accepté pour chaque matin) ; Présent → Absent en demande une tant que quelqu'un est (ou
peut être) à la maison, pas quand tout le monde est parti ; Absent ou Vacances → Nuit éteint les
caméras intérieures, donc une confirmation. **Le même mode que l'actuel** : rien ne change, rien n'est
demandé (une ligne `agent_occupation` garde la trace de la lecture). Le changement se fait **sous le verrou du mode** : si le mode a bougé depuis le calcul, rien
n'est changé et la demande ne vaut plus.

La réponse et la carte **listent les automatisations qui se déclenchent** (noms nettoyés). Un
changement de mode écrit une ligne `mode_changed` d'origine `agent` avec le libellé de l'agent.

### La liste « sensible »

Comme `never`, elle se règle dans la fenêtre d'Ostal Server (ou à la main dans `agents.json` :
`"sensitive": ["13828101"]`), vaut pour tous les agents, survit à la ligne de commande, et se **durcit** : une entrée illisible (faute de frappe,
zéro initial) rend **tout** appareil sensible. Mettez-y la porte de garage et la baie vitrée. `never`
l'emporte toujours (un appareil « jamais » n'est commandé par aucun agent, sensible ou non).

## Le journal d'audit des agents

Chaque ligne porte le compte propriétaire du jeton, `connection.kind: "agent"` avec le libellé de
l'agent et l'identifiant du jeton (jamais le jeton), et `source: "agent"`. `ip` et `via` y sont, mais
derrière `tailscale serve` ils n'identifient rien : c'est le libellé de l'agent qui compte. L'onglet
Activité d'Ostal Server et `ostal-server audit` les lisent déjà (`audit::describe`).

| Ligne | Quand |
|---|---|
| `agent_read` | **les lectures ordinaires, regroupées** : une ligne par agent et par tranche de 10 minutes, écrite à l'échéance (un balayeur la passe chaque minute, et à l'arrêt du serveur). `detail` : `N:outil,outil` (le nombre de lectures et les outils lus, sans `ostal_`). Jamais une valeur lue ni un argument |
| `agent_occupation` | **une ligne par lecture** du droit « occupation » : `ostal_get_mode`, `ostal_get_presence`, et `ostal_home_summary`, `ostal_list_cameras`, `ostal_list_automation_runs` quand le jeton a le droit (leur réponse en dépend), ainsi que `ostal_set_mode` quand il ne change rien (même mode : la réponse dit le mode et ce que fait chaque caméra ; un vrai changement est la ligne `mode_changed`). Jamais regroupée. Écrite avant la réponse |
| `agent_denied` | un appel refusé, `result: refused`, `detail` : `unknown` (outil inconnu), `scope` (droit du jeton manquant), `rights` (le compte n'a plus le droit de lire la maison), `control_rights` (ni de commander), `mode_rights` (ni de changer le mode), `device` (appareil hors de `devices` ou dans `never`, y compris des appareils sautés dans une pièce) ou `rate` (débit dépassé). Au plus **une ligne par minute, par agent et par motif**. Le nom d'un outil inconnu n'y entre jamais. (`rule`, d'avant l'étape E3, ne s'écrit plus : une commande qui ferait partir une règle sensible attend une personne) |
| `confirmation_requested`, `confirmation_approved`, `confirmation_refused`, `confirmation_expired` | la vie d'une demande. `target` : le libellé de l'agent ; `detail` : l'outil (`set_on_off`, `set_cover`, `set_room`, `set_mode`), mot fermé. **Autorisée ou refusée : la ligne porte la personne** et sa session, pas l'agent. Jamais l'identifiant de la demande, l'appareil visé ni le mode |

Un changement de mode fait par un agent (étape E3) s'écrit `source: "agent"` (le libellé de l'agent
en `detail`) ; un ancien lecteur du journal qui ne connaît pas cette origine la lit « autre ».

## Choix de cette implémentation

Hors des spécifications ou à confirmer :

- **Comment un agent reçoit le droit « occupation »** : il se coche depuis une session (app web,
  « Mes agents »), jamais en ligne de commande : `ostal-server agent add` refuse toute option de
  droits. Depuis l'étape E4 le serveur écrit donc `agents.json` (voir « Qui écrit ce fichier »).
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
  ne permet aucun appareil. La liste `devices` se coche dans l'app web et les listes `never` et
  `sensitive` dans la fenêtre d'Ostal Server (étape E4) ; on peut toujours les écrire à la main. Une liste `devices` vide ou absente ne permet rien : le droit
  « commandes » seul ne commande rien. Les règles **désactivées** ne comptent pas dans le calcul
  des automatisations déclenchées (elles ne partiraient pas). Pour l'appareil commandé, le sens
  compte (« s'allume » ou « s'éteint ») ; pour les appareils commandés ensuite par des règles, on
  suppose les deux sens. Le texte de refus ne nomme jamais la règle, le mode ni l'adresse.
  Un appel à `ostal_set_on_off` (ou à un autre outil d'écriture) par un jeton sans le droit est un refus `scope` (comme pour
  `ostal_get_mode`), pas un outil inconnu.
- **Jetons refusés** : un échec d'authentification n'est ni compté ni écrit au journal d'audit
  (les spécifications techniques disaient « compté »). Il est retardé de 250 ms et une ligne du
  journal de fonctionnement (adresse, jamais le jeton) le signale ; aucun blocage par adresse.
- Les noms d'appareils rendus à l'agent perdent aussi les caractères invisibles ou bidi (largeur
  nulle, U+202A à U+202E, U+2066 à U+2069, séparateurs de ligne, étiquettes), pas seulement les
  caractères de contrôle.
- **E3 : durées et bornes** : 5 minutes fixes (les spécifications parlent d'un réglage de 1 à 15 minutes,
  que l'étape E4 **ne livre pas** : une confirmation qui dure plus longtemps laisse plus longtemps
  un ordre en attente, on ne l'allonge pas sans décision), 3 demandes vivantes et 10 nouvelles par heure par jeton,
  128 en tout, relues toutes les 5 s par l'app web tant qu'une attend. Les demandes expirées sont
  écrites au journal à la minute suivante (balayeur) ; une demande terminée reste lisible un quart
  d'heure pour `ostal_confirmation_status`.
- **E3 : autorisation et exécution séparées** : l'autorisation ne fait rien toute seule. C'est l'agent
  qui rappelle l'outil (comme le disent les spécifications), et tous les contrôles sont refaits.
- **E3 : « jamais » et confirmation** : une **règle** déclenchée qui commande un appareil de la liste
  `never` ou hors des appareils du jeton **attend une personne** (elle ne bloque plus), comme le
  disent les spécifications ; l'appareil visé directement, lui, reste refusé s'il est dans `never` ou
  hors de la liste du jeton.
- **E3 : `ostal_set_mode` et les appareils permis** : les règles de mode ne sont jugées que sur
  « sensible » et « jamais », pas sur les appareils permis du jeton (changer le mode allume
  normalement des lampes que l'agent n'a pas le droit de commander seul).
- **E3 : la liste « sensible »** vaut pour tous les agents ; les propositions par défaut à
  l'activation (porte de garage et baie vitrée, congélateur, box, prise du Mac serveur) sont celles de
  la fenêtre d'Ostal Server (étape E4), d'après le **nom** des appareils.
- **E4 : ce qu'on n'a pas fait** : pas d'assistant en plusieurs pages (l'interrupteur ouvre le choix
  des listes, c'est tout), pas de réglage de la durée de confirmation ni de la durée par défaut d'un
  jeton, pas de filtre « Source » à quatre valeurs (« personne, raccourci, automatisation, agent »)
  mais deux (toutes, agents d'IA), pas de création d'agent depuis la fenêtre du Mac (une personne le
  fait depuis l'app web, avec son compte). Les compteurs « dernier appel » et « appels du jour » sont
  en mémoire et repartent de zéro au redémarrage.
- **E4 : « Tout couper » révoque** tous les agents en plus d'éteindre l'interrupteur : éteindre seul
  se fait avec l'interrupteur, et se défait. Les demandes en mémoire sont oubliées, y compris les
  autorisées non utilisées ; de toute façon une autorisation ne vaut que pour le jeton qui l'a demandée.
- **E3 : le texte de la notification** est le même pour toutes les demandes et ne dit pas ce qui est
  demandé.
- **E3 : `Unreachable` et les capteurs de porte** ne sont pas suivis (voir plus haut).
