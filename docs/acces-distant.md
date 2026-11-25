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
  `https://nom.ts.net/` (port 443) ne s'y réfèrent pas (voir « Certificats »). `alt_urls` n'est
  écrit qu'après une connexion réussie, et vidé quand l'adresse principale change pour une
  adresse pas encore approuvée (autre serveur). Le champ `alt_urls` est absent quand il est
  vide : un ancien fichier se relit tel quel, et un ancien client ignore le champ.
- À chaque connexion (et à chaque reconnexion après une coupure), le client essaie les adresses
  dans l'ordre, la maison d'abord (3 s par adresse), et prend la **première qui répond**
  (empreinte du serveur pour une adresse directe, certificat public valable pour une adresse en
  `.ts.net` sur le 443). Une adresse qui répond avec un autre certificat est ignorée. Si aucune ne
  répond, l'erreur est celle de l'adresse principale. Quitter le Wi-Fi de la maison bascule donc
  toute seule, et le retour à la maison aussi (au prochain rétablissement du lien).
- L'identité de la session (brouillons, etc.) est celle de l'adresse principale : changer
  d'adresse n'est pas changer de serveur.
- La barre d'état affiche « à distance » quand la session n'est pas sur l'adresse principale ou
  quand l'adresse est une adresse Tailscale (nom `.ts.net`, IPv4 `100.64.0.0/10`, IPv6
  `fd7a:115c:a1e0::/48`). L'infobulle donne l'adresse réellement utilisée.

## Caméras à distance

Le flux **réduit** (`sub`) est le flux par défaut partout : mosaïque et ouverture en grand
(« Fluide »). À distance, choisir « Haute » (flux principal) reste possible, avec un rappel que
le débit est lourd. L'app web fait de même (vignettes en `sub`, principal seulement en plein
écran).

## Certificats

| Adresse | Certificat vu par le client | Validation |
|---|---|---|
| `mon-mac.tailnet-xxxx.ts.net:8443` (accès direct), IP `100.x`, IP locale | celui du serveur (auto-signé) | **empreinte SHA-256 épinglée** ; le nom n'est pas contrôlé (il ne figure pas dans le certificat), la signature de la poignée de main l'est |
| `https://mon-mac.tailnet-xxxx.ts.net/` (port 443, `tailscale serve`) | celui de Tailscale (Let's Encrypt), d'empreinte changeante | **chaîne de confiance publique** (autorités Mozilla embarquées), nom et dates contrôlés, **sans épinglage** |
| tout le reste | | empreinte épinglée ; un autre certificat est refusé |

La règle « chaîne publique » ne vaut que pour un nom se terminant par `.ts.net` **et** le port
443 (`https://nom.ts.net/` sans port, c'est 443 ; `nom.ts.net` sans schéma reste l'accès direct
sur 8443). Si le certificat n'est pas valable, le message dit de vérifier `tailscale serve` et
l'HTTPS du tailnet.

Conseil : enregistrez toujours comme adresse principale l'adresse directe (maison ou
`:8443`), c'est elle dont l'empreinte est approuvée au premier lancement ; l'adresse en 443
n'est qu'une alternative.

## Limites de sécurité derrière `tailscale serve`

- Une adresse en `.ts.net` sur le port 443 est vérifiée par le **nom** et une autorité publique,
  **pas par l'empreinte du serveur** : un autre nœud du tailnet qui porte le nom saisi serait
  accepté. Le formulaire le rappelle ; vérifiez le nom de votre Mac.
- **Limiteur de tentatives** : derrière `tailscale serve` **sans** relais qui transmet l'adresse du
  client, le serveur voit toutes les connexions venir de 127.0.0.1. Les essais de mot de passe
  partagent donc **un seul compteur pour tout le monde** : quelqu'un du tailnet qui multiplie les
  essais peut bloquer aussi les connexions légitimes pendant la durée du blocage. Avec un relais
  qui écrit `X-Forwarded-For` (Caddy, voir ci-dessous), le compteur est par client.

## Derrière un relais (Caddy) : proxys de confiance et adresse publique

Montage visé : `tailscale serve --tcp=443 --proxy-protocol=2` envoie vers Caddy (dans Docker, sur
ce Mac), qui présente un certificat officiel pour un nom à vous (`https://ostal.exemple.fr`,
joignable seulement depuis le tailnet), garde l'en-tête `Host`, ajoute `X-Forwarded-For` avec
l'adresse réelle du client (`100.x` pour un appareil du tailnet) et joint Ostal. Ostal voit alors
la connexion venir de la passerelle de Docker, pas du client. Deux réglages (dans
`serveur/reseau.json`, relus sans redémarrage) :

**Proxys de confiance.** La liste des réseaux (CIDR) dont `X-Forwarded-For` est cru. Par défaut :
`192.168.5.0/24`, `127.0.0.1/32` et `::1/128`.

- `192.168.5.0/24` est le réseau de la machine virtuelle Colima/Lima : c'est par sa passerelle
  qu'un conteneur Docker de ce Mac joint Ostal. Ce réseau n'existe que dans le Mac.
- La boucle locale est celle d'un relais lancé sur le Mac lui-même (`tailscale serve` en mode
  HTTP) : un programme du Mac qui y écrit un faux en-tête ne gagne rien qu'il ne puisse déjà faire.
- **Règle de lecture** (une seule fonction, `security::network::client_ip`, pour le classement
  « sur place / à distance », le limiteur d'essais, le journal d'audit et le serveur MCP) :
  si le pair TCP est de confiance et qu'il y a un `X-Forwarded-For`, le client est l'adresse **la plus à
  droite qui n'est pas elle-même un proxy de confiance** (toutes les lignes de l'en-tête sont lues) ;
  ce qui est à gauche vient du client et n'est jamais lu. Une valeur illisible arrête la lecture : on
  garde le pair. Un pair qui n'est pas de confiance : l'en-tête est ignoré.
- **Attention** : si le réseau de votre maison est lui-même en `192.168.5.x`, n'importe quel appareil
  de la maison pourrait forger l'en-tête (échapper au limiteur d'essais, brouiller le journal).
  Changez alors la liste : `ostal-server network trusted-proxies 192.168.7.0/24 127.0.0.1/32`
  (`--default` revient au défaut, `--none` n'en fait confiance à aucun).
- Les connexions d'un proxy de confiance ont un plafond plus large (64 au lieu de 16) : il porte
  celles de tous ses clients.

**Adresse publique.** Le nom en https, sans chemin ni port : `ostal-server network public-address
https://ostal.exemple.fr` (ou Ostal Server › Réglages › Adresse publique ; `--clear` l'efface).
Quand elle est réglée :

- une requête dont `Host` est cette adresse (ou un sous-nom) compte « à distance » dans le
  journal et dans « Mes connexions » ; `evilostal.exemple.fr` n'en est pas un ;
- le serveur l'annonce au client après la connexion (`public_url` dans la réponse de
  `POST /api/login`, version 4 et plus) **à la place** des adresses `100.x` (`tailnet_urls`) ;
- l'app web, Raccourcis et le widget de l'iPhone l'acceptent comme le nom en `.ts.net` (ils ont
  besoin d'un certificat officiel, ce qu'elle a) : `GET /api/tokens` donne `public_origin`, et la
  préparation des raccourcis n'accepte que **exactement** cette adresse ;
- les pages d'Ostal Server (Connexion, raccourcis) montrent cette adresse au lieu de celle de
  `tailscale serve`.

Sans adresse publique réglée, rien ne change. `ostal-server network show` affiche les deux
réglages. Un fichier `reseau.json` illisible vaut « aucun proxy de confiance, aucune adresse
publique » et n'est jamais écrasé.

## Ce qui ne se vérifie qu'avec un vrai Tailscale

Les tests génèrent leurs certificats et n'ouvrent aucune connexion réelle. Restent à essayer
sur un vrai tailnet : la bascule maison / distance sur un vrai changement de réseau, le
certificat Let's Encrypt réel de Tailscale via le port 443, et le débit des caméras en 4G.
