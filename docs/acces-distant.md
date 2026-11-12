# Accès à distance (Tailscale)

[← Retour au README](../README.md) · [App web](web-app.md) · [Dépannage](depannage.md)

Ticket n° 14. Rien n'est ouvert sur Internet : les appareils se joignent par un tailnet
Tailscale. Les exemples utilisent `mon-mac.tailnet-xxxx.ts.net`.

## Plusieurs adresses par serveur (client de bureau)

- L'écran de connexion a un champ facultatif **« Adresse à distance »** (une ou plusieurs
  adresses, séparées par un espace, une virgule ou un point-virgule). L'adresse du haut est la
  « maison » (la principale).
- Le fichier de connexion garde `url` (maison), `alt_urls` (les autres) et **une seule
  empreinte**, celle du certificat du serveur. Le champ `alt_urls` est absent quand il est
  vide : un ancien fichier se relit tel quel, et un ancien client ignore le champ.
- À chaque connexion (et à chaque reconnexion après une coupure), le client essaie les adresses
  dans l'ordre, la maison d'abord (3 s par adresse), et prend la **première qui répond avec le
  bon certificat**. Une adresse qui répond avec un autre certificat est ignorée. Si aucune ne
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

## Ce qui ne se vérifie qu'avec un vrai Tailscale

Les tests génèrent leurs certificats et n'ouvrent aucune connexion réelle. Restent à essayer
sur un vrai tailnet : la bascule maison / distance sur un vrai changement de réseau, le
certificat Let's Encrypt réel de Tailscale via le port 443, et le débit des caméras en 4G.
