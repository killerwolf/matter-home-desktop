# Accès à distance (Tailscale)

[← Retour au README](../README.md) · [App web](web-app.md) · [Dépannage](depannage.md)

Ticket n° 14. Rien n'est ouvert sur Internet : les appareils se joignent par un tailnet
Tailscale. Les exemples utilisent `mon-mac.tailnet-xxxx.ts.net`.

## Plusieurs adresses par serveur (client de bureau)

- L'écran de connexion a un champ facultatif **« Adresse à distance »** (une ou plusieurs
  adresses, séparées par un espace, une virgule ou un point-virgule). L'adresse du haut est la
  « maison » (la principale).
- Le fichier de connexion garde `url` (maison), `alt_urls` (les autres) et **une seule
  empreinte**, celle du certificat du serveur, utilisée par les adresses directes. Les adresses
  `https://nom/` (nom de domaine, port 443) ne s'y réfèrent pas (voir « Certificats »). `alt_urls` n'est
  écrit qu'après une connexion réussie, et vidé quand l'adresse principale change pour une
  adresse pas encore approuvée (autre serveur). Le champ `alt_urls` est absent quand il est
  vide : un ancien fichier se relit tel quel, et un ancien client ignore le champ.
- À chaque connexion (et à chaque reconnexion après une coupure), le client essaie les adresses
  dans l'ordre, la maison d'abord (3 s par adresse), et prend la **première qui répond**
  (empreinte du serveur pour une adresse directe, certificat public valable pour un nom de
  domaine sur le 443). Une adresse qui répond avec un autre certificat est ignorée. Si aucune ne
  répond, l'erreur est celle de l'adresse principale. Quitter le Wi-Fi de la maison bascule donc
  toute seule, et le retour à la maison aussi (au prochain rétablissement du lien).
- L'identité de la session (brouillons, etc.) est celle de l'adresse principale : changer
  d'adresse n'est pas changer de serveur.
- La barre d'état affiche « à distance » quand la session n'est pas sur l'adresse principale ou
  quand l'adresse est une adresse Tailscale (nom `.ts.net`, IPv4 `100.64.0.0/10`, IPv6
  `fd7a:115c:a1e0::/48`) ou un nom de domaine public sur le 443 (derrière un relais). L'infobulle donne l'adresse réellement utilisée.

## Caméras à distance

Le flux **réduit** (`sub`) est le flux par défaut partout : mosaïque et ouverture en grand
(« Fluide »). À distance, choisir « Haute » (flux principal) reste possible, avec un rappel que
le débit est lourd. L'app web fait de même (vignettes en `sub`, principal seulement en plein
écran).

## Certificats

| Adresse | Certificat vu par le client | Validation |
|---|---|---|
| `mon-mac.tailnet-xxxx.ts.net:8443` (accès direct), IP `100.x`, IP locale | celui du serveur (auto-signé) | **empreinte SHA-256 épinglée** ; le nom n'est pas contrôlé (il ne figure pas dans le certificat), la signature de la poignée de main l'est |
| `https://mon-mac.tailnet-xxxx.ts.net/` ou `https://maison.exemple.fr/` (nom de domaine, port 443) | celui de Tailscale ou du relais (Let's Encrypt), d'empreinte changeante | **chaîne de confiance publique** (autorités Mozilla embarquées), nom et dates contrôlés, **sans épinglage** |
| IP (même sur 443), nom `.local`, nom sans point, tout autre port | celui du serveur (auto-signé) | empreinte épinglée ; un autre certificat est refusé |

**Qui décide, et sur quoi.** Le mode se décide sur l'**adresse seule** (`net::tls::mode_for`), pas
sur ce que le serveur présente : un **nom de domaine public** (au moins deux étiquettes, pas une IP,
pas `.local`, `.lan`, `.internal`, `.home`, `.localhost`, `.test`, `.arpa`…) **sur le port 443** est
toujours jugé par la chaîne publique ; tout le reste reste épinglé sur l'empreinte. Il n'y a **aucun
repli** : une chaîne invalide, un nom qui ne correspond pas au certificat, un certificat expiré ou
auto-signé sur un tel nom est refusé, jamais « accepté quand même » ni rattrapé par une empreinte.
Si le certificat n'est pas valable, le message dit de vérifier le nom, le relais et l'HTTPS.

**Port par défaut.** `https://nom/` sans port veut dire **443 pour tout nom de domaine public**
(avant, seuls les noms `.ts.net` l'avaient). Une IP ou un nom `.local` écrits `https://…` sans port
gardent 8443 ; sans le schéma (`maison.exemple.fr`), c'est toujours 8443, l'accès direct ; un port
écrit est toujours respecté. Conséquence à connaître : un serveur joint par un nom de domaine
**sur le 443 avec son certificat auto-signé** (redirection de port d'une box, par exemple) n'est plus
accepté ; utilisez un autre port que 443 (il reste épinglé) ou mettez-le derrière un relais avec un
vrai certificat.

**Première connexion à un nom public.** Si vous saisissez `https://maison.exemple.fr` comme
adresse principale, l'app vérifie la chaîne publique puis affiche « Approuver ce serveur ? » avec le
**nom vérifié** en gros : il n'y a aucune empreinte à comparer, la phrase dit seulement que le
certificat est délivré par une autorité publique pour ce nom, et qu'il faut approuver si c'est bien
votre serveur. Une chaîne invalide arrête tout avant l'écran d'approbation. Le champ « Adresse à
distance » rappelle de la même façon le nom qui sera vérifié.

**Adresse annoncée par le serveur.** Quand une adresse publique est réglée sur le serveur (voir
plus bas), il l'annonce après la connexion (`public_url`), et le client la garde dans `alt_urls`
**sous quatre conditions** : l'annonce est arrivée par une connexion **épinglée** (jamais par une
chaîne publique : un serveur joint ainsi ne peut pas envoyer l'app vers d'autres noms) ; elle est
écrite `https://nom` ; c'est un nom de domaine public sur le 443 (jamais une IP, un `.local`, un
autre port, un chemin) ; elle n'y est pas déjà et la liste ne porte pas plus de 3 adresses
publiques. Elle passe **après** les adresses déjà connues : l'adresse principale reste essayée la
première, l'adresse publique ne sert que quand les précédentes ne répondent pas.

Conseil : enregistrez toujours comme adresse principale l'adresse directe (maison ou
`:8443`), c'est elle dont l'empreinte est approuvée au premier lancement ; l'adresse en 443
n'est qu'une alternative.

## Limites de sécurité derrière `tailscale serve` ou un relais

- Un nom de domaine sur le port 443 (`….ts.net`, `maison.exemple.fr`) est vérifié par le **nom**
  et une autorité publique, **pas par l'empreinte du serveur** : quiconque obtient un certificat
  valable pour ce nom, ou un autre nœud du tailnet qui porte un nom `.ts.net` saisi, serait
  accepté. Le formulaire le rappelle ; vérifiez le nom. Une faute de frappe dans un nom de domaine
  qui existe envoie le mot de passe à son propriétaire : relisez le nom affiché à l'approbation.
- **Limiteur de tentatives** : derrière `tailscale serve` **sans** relais qui transmet l'adresse du
  client, le serveur voit toutes les connexions venir de 127.0.0.1. Les essais de mot de passe
  partagent donc **un seul compteur pour tout le monde** : quelqu'un du tailnet qui multiplie les
  essais peut bloquer aussi les connexions légitimes pendant la durée du blocage. Avec un relais
  qui écrit `X-Forwarded-For` (reverse proxy déclaré de confiance, voir ci-dessous), le compteur est par client.

## Derrière un reverse proxy

Facultatif : **sans rien régler, Ostal se comporte comme avant** (aucun proxy de confiance, aucune
adresse publique). Ce chapitre ne concerne que ceux qui placent devant Ostal un reverse proxy (nginx, Traefik, un autre) qui présente un certificat officiel pour un nom à eux. Le proxy joint Ostal
depuis une adresse à lui : sans réglage, Ostal croit que tous les clients sont ce proxy (un seul
compteur d'essais de mot de passe pour tout le monde, un journal qui ne dit que l'adresse du proxy).
Deux réglages (dans `serveur/reseau.json`, 0600, relus sans redémarrage) :

**1. Proxys de confiance.** La liste des réseaux (CIDR) dont l'en-tête `X-Forwarded-For` est cru.
**Vide par défaut, loopback compris** (`127.0.0.1` et `::1` ne sont pas de confiance d'office : un
programme local qui écrirait un faux en-tête serait cru, et une installation existante verrait son
comportement changer sans l'avoir demandé).

```
ostal-server network trusted-proxies 10.0.0.5/32   # l'adresse du proxy, vue par Ostal
ostal-server network trusted-proxies --none         # n'en croire aucun (le défaut)
```

- **N'ajoutez que l'adresse de votre relais**, la plus étroite possible (`/32` pour une machine). Tout
  appareil qui joint Ostal depuis un réseau de cette liste peut écrire un faux `X-Forwarded-For`,
  échapper au limiteur d'essais de connexion et brouiller le journal d'audit : n'y mettez jamais
  le réseau de la maison.
- **Règle de lecture** (une seule fonction, `security::network::client_ip`, pour le classement
  « sur place / à distance », le limiteur d'essais, le journal d'audit et le serveur MCP) :
  si le pair TCP est de confiance et qu'il y a un `X-Forwarded-For`, le client est l'adresse **la plus à
  droite qui n'est pas elle-même un proxy de confiance** (toutes les lignes de l'en-tête sont lues) ;
  ce qui est à gauche vient du client et n'est jamais lu. Une valeur illisible arrête la lecture : on
  garde le pair. Un pair qui n'est pas de confiance : l'en-tête est ignoré.
- Les connexions d'un proxy de confiance ont un plafond plus large (64 au lieu de 16) : il porte
  celles de tous ses clients.

**2. Adresse publique.** Le nom en https, sans chemin ni port, sous lequel la maison se joint de loin.
**Vide par défaut.** Un nom de domaine seulement : ni IP, ni `.local`, `.lan`, `.internal`, `.home`,
`.test`, `.arpa`… (les mêmes fins de nom que l'app refuse).

```
ostal-server network public-address https://maison.exemple.fr
ostal-server network public-address --clear
```

(ou Ostal Server › Réglages › Adresse publique). Quand elle est réglée :

- une requête dont `Host` est cette adresse (ou un sous-nom) compte « à distance » dans le
  journal et dans « Mes connexions » ; `evilmaison.exemple.fr` n'en est pas un ;
- le serveur l'annonce au client après la connexion (`public_url` dans la réponse de
  `POST /api/login`, version 4 et plus), **en plus** des adresses `100.x` (`tailnet_urls`), qui sont
  annoncées comme sans adresse publique : le client range l'adresse publique **après** elles ;
- l'app web, Raccourcis et le widget de l'iPhone l'acceptent comme le nom en `.ts.net` (ils ont
  besoin d'un certificat officiel, ce qu'elle a) : `GET /api/tokens` donne `public_origin`, et la
  préparation des raccourcis n'accepte que **exactement** cette adresse ;
- les pages d'Ostal Server (Connexion, raccourcis) montrent cette adresse au lieu de celle de
  `tailscale serve`.

**Exemple générique.** Un reverse proxy sur la même machine, qui présente le certificat de
`maison.exemple.fr`, garde l'en-tête `Host` d'origine, ajoute `X-Forwarded-For` avec l'adresse du
client et relaie vers `https://127.0.0.1:8443` d'Ostal :

```
ostal-server network trusted-proxies 127.0.0.1/32
ostal-server network public-address https://maison.exemple.fr
ostal-server network show
```

Si le proxy tourne dans un conteneur ou une machine virtuelle, Ostal le voit venir d'une autre
adresse (celle de la passerelle du conteneur, par exemple) : mettez **cette** adresse, pas `127.0.0.1`.

**Risques.** Une liste trop large laisse n'importe quel appareil de ces réseaux se faire passer pour
un autre client ; un proxy qui ne remplace pas ou n'ajoute pas `X-Forwarded-For` (et le laisse passer
tel que le client l'a écrit) est cru à tort sur sa partie gauche, d'où la lecture de droite à gauche.
Vérifiez le résultat avec `ostal-server network show` puis dans le journal d'audit
(`ostal-server audit`) : l'adresse d'une connexion doit être celle du client, pas celle du proxy.
**Chaque changement de ces réglages** (depuis Ostal Server ou la ligne de commande) est inscrit au
journal d'audit, avec qui l'a fait ; jamais les adresses ni le nom, seulement leur nombre ou « réglée /
effacée ». Un fichier `reseau.json` illisible vaut « aucun proxy de confiance, aucune adresse
publique » et n'est jamais écrasé.

## Passer de l'adresse `.ts.net` à `https://maison.exemple.fr`

Les deux adresses restent servies pendant la transition : rien ne casse, on peut passer appareil par
appareil. Le nouveau nom n'est joignable que depuis le tailnet (Tailscale actif sur l'appareil).

**App Mac (Ostal).**

1. Dans Ostal Server, réglez l'adresse publique (Réglages › Adresse publique, ou
   `ostal-server network public-address https://maison.exemple.fr`). Sans elle, le client ne
   l'apprend pas tout seul.
2. Si l'app Mac est déjà connectée par son adresse directe (maison, `:8443`, `100.x`), **il n'y a
   rien à faire** : à la prochaine ouverture de session avec le mot de passe (le serveur l'annonce
   dans la réponse du login, pas à une simple reconnexion avec le jeton déjà gardé : déconnectez-vous
   puis reconnectez-vous), l'app la garde dans « Adresse à distance », même empreinte, rien à
   approuver. Vous la voyez apparaître dans ce champ.
3. Pour **remplacer** l'ancienne adresse `.ts.net` : ouvrez l'écran de connexion (menu › Changer de
   serveur), dans « Adresse à distance » effacez `mon-mac.tailnet-xxxx.ts.net…` et écrivez
   `https://maison.exemple.fr` (sans port : 443), puis « Se connecter ». Gardez l'adresse
   directe en haut : c'est elle dont l'empreinte est approuvée.
4. Pour **n'avoir que ce nom** (pas d'adresse directe) : saisissez `https://maison.exemple.fr`
   en haut. L'app vérifie la chaîne publique et affiche le nom vérifié, sans empreinte à comparer ;
   approuvez si le nom est bien le vôtre. Vous perdez le repli sur l'adresse de la maison hors
   tailnet : préférez le point 3.

**iPhone (app web installée).** L'app web est liée à son adresse : le nouveau nom est une
**nouvelle** app pour iOS.

1. Tailscale actif sur l'iPhone. Dans Safari, ouvrez `https://maison.exemple.fr/app/` et
   connectez-vous.
2. Partager › « Sur l'écran d'accueil ». Supprimez ensuite l'ancienne icône (`.ts.net`) quand tout
   marche.
3. Réactivez les notifications dans la nouvelle app (l'invite d'activation de l'app) : l'abonnement push
   d'un site ne se transfère pas à un autre site.
4. Refaites la préparation des **raccourcis** et du **widget** depuis la nouvelle adresse (ils
   s'appuient sur l'adresse d'où ils sont préparés ; le serveur n'accepte que l'adresse publique
   réglée, exactement). Les anciens, liés à `.ts.net`, continuent de marcher tant que cette adresse
   est servie.

Quand tout est passé, l'ancienne adresse `.ts.net` peut être retirée côté serveur (c'est une
affaire d'infrastructure, hors de l'app) ; les adresses `.ts.net` déjà saisies dans l'app restent
valables tant qu'elles répondent.

## Ce qui ne se vérifie qu'avec un vrai Tailscale

Les tests génèrent leurs certificats et n'ouvrent aucune connexion réelle. Restent à essayer
sur un vrai tailnet : la bascule maison / distance sur un vrai changement de réseau, le
certificat Let's Encrypt réel de Tailscale ou du relais via le port 443, et le débit des caméras en 4G.
