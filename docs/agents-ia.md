# Agents d'IA : le serveur MCP intégré (étape E1a, lecture seule)

Ostal Server peut recevoir un agent d'IA (Claude Code, Cursor…) qui lit l'état de la maison par le
protocole MCP. **C'est éteint tant que vous ne l'allumez pas**, et cette première étape ne sait que
**lire** : aucune commande d'appareil, aucun changement de mode, ni la présence ni le mode.

Les étapes suivantes (lectures de l'historique, des pièces, des automatisations ; commandes de prises
et de lampes ; confirmations) sont décrites dans les spécifications du projet, pas encore livrées.

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
- Débit : **60 appels d'outil par minute et par jeton** (429 + `Retry-After`). Les autres méthodes ne
  sont pas comptées. Compteur en mémoire, remis à zéro au redémarrage.

Quatre points sont des **choix de cette implémentation**, faits sans la page du schéma sous les yeux,
et non des exigences relues dans les spécifications : `ttlMs` à 0 et `cacheScope` à `private` sur les
listes, la forme du résultat de `server/discover`, `_meta` d'une requête moderne non exigé, `Mcp-Name`
vérifié pour `tools/call` seulement. À confirmer à l'essai avec un vrai client (étape E5).

## Les outils (E1a)

Tous en lecture seule (`readOnlyHint`), sans paramètre superflu (un argument inconnu est refusé par le
serveur, pas seulement par le schéma). Chaque résultat est un objet `structuredContent` et **le même
JSON** en bloc `text`. Une erreur d'exécution est un résultat `isError: true` avec un code de motif
(`invalid_argument`, `not_found`, `forbidden`) et une phrase en français.

| Outil | Rend |
|---|---|
| `ostal_home_summary` | nombre d'appareils, joignables, injoignables, en connexion ; la phrase d'état (la même que sur les autres écrans) ; nombre de caméras ; piles faibles. **Ni le mode, ni la présence** : une caméra éteinte par le mode n'est ni comptée ni nommée |
| `ostal_list_devices` | 200 appareils au plus : `id` (texte décimal), `name`, `kind` (`cover`, `on_off`, `sensor`, `other`), `room` (`{id, name}` ou `null`), `status` (`online`, `connecting` ou `unreachable` avec une raison fermée), `last_seen`, `latest`. Filtres : `kind`, `room` (identifiant de pièce), `only_problems`, `limit` |
| `ostal_get_device` | un appareil par son `id` : mêmes champs, plus `capabilities`, `open_since`, `cover_motion` |

**Le texte venu de la maison est une donnée, jamais une consigne.** Les noms (personnalisés, ou écrits
par le fabricant) passent champ par champ dans le même nettoyeur que les notifications (hôtes,
identifiants et mots de passe de caméras, adresses, identifiants de nœud masqués), sans caractères de
contrôle, coupés à 80 caractères. Les identifiants ne sont jamais nettoyés. `vendor` et `product`
ne sortent pas. Aucune adresse de webhook, aucune URL de caméra, aucune image n'est jamais rendue.
Une consigne cachée dans un nom reste un texte : c'est pourquoi les étapes qui commandent exigeront
une confirmation humaine côté serveur.
