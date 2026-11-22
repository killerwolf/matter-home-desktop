# Présence par le Wi-Fi de la box et mode automatique

Ostal Server se relie à la box de la maison (Bbox) avec son mot de passe d'administration, relève
toutes les 2 minutes les appareils actifs, et en tire qui est à la maison. Lecture seule : rien n'est
jamais écrit sur la box, et aucun balayage du réseau n'est fait.

## La box

- `POST /api/v1/login` (formulaire `password=…&remember=0`, cookie de session) puis
  `GET /api/v1/hosts`. Adresse par défaut `https://mabbox.bytel.fr`, certificat auto-signé : la
  vérification TLS n'est désactivée que pour cette adresse ou une adresse IP privée (RFC 1918,
  `fe80::/10`, `fd00::/8`) ; toute autre adresse https garde la vérification. Le `curl` du
  système, environnement vide, mot de passe par l'entrée standard (jamais en argument).
- Seuls le nom (`hostname`) et l'état (`active`) sont gardés : jamais l'adresse MAC ni l'IP. Un
  téléphone se reconnaît à son nom (insensible à la casse), pas à son adresse MAC (privée par réseau).
- Le mot de passe est dans `serveur/box.json` (0600) : jamais affiché, journalisé ni exporté.
- **Box injoignable, mot de passe refusé, ou liste sans aucun appareil actif : rien n'est conclu**, le
  mode est figé (jamais lu comme « personne à la maison »), et les absences en cours de comptage
  repartent de zéro. Un mot de passe refusé n'est plus rejoué (la box pourrait bloquer l'accès) avant
  qu'on en donne un autre.

## Qui a quel téléphone

À sa première connexion, l'app web demande « Lequel est votre téléphone ? » parmi les téléphones vus
sur le Wi-Fi et sans propriétaire : **noms seulement**, jamais d'adresse. « Plus tard » est possible
(redemandé au bout d'une semaine). Un membre ne voit que les noms qui ressemblent à un téléphone
(iPhone, Android, Galaxy, Pixel, Redmi…) ; l'administrateur corrige dans Ostal Server (Réglages ›
Présence, fiche d'un compte) et peut attribuer n'importe quel appareil libre. Fichier : `serveur/telephones.json`.

- `GET /api/wifi/phones`, `POST /api/wifi/refresh`, `PUT /api/wifi/phone {"phone": "nom" | null}`
  (404 : plus vu ; 409 : déjà pris), `GET /api/presence/people` (prénoms et états, `auto_mode`).
- Retirer ou changer un téléphone efface ce que le Wi-Fi avait dit de la personne.

## Présence de chacun

Le signal le plus récent l'emporte, entre le téléphone vu sur le Wi-Fi et le raccourci « Je pars » /
« Je rentre » :

- une arrivée vue sur le Wi-Fi compte tout de suite, sauf si un « Je pars » plus récent a été dit ;
- un départ vu seulement sur le Wi-Fi ne compte qu'après 10 minutes sans voir le téléphone (délai
  `away_delay_min`), et un « Je rentre » plus récent que le début de l'absence l'annule.

## Mode automatique

L'interrupteur « Changer le mode de la maison selon la présence » est **éteint par défaut**, y compris
après la mise à jour. Il peut être allumé **sans box reliée** : la règle suit alors les seuls raccourcis
« Je pars » et « Je rentre ». Il reste allumé quand on délie la box.

**Interrupteur éteint** : le comportement d'avant. « Je pars » met la maison en Absent et « Je rentre »
en Présent, directement, sans regarder qui d'autre est là ; Nuit et Vacances sont respectés comme dans le
tableau ci-dessous. La présence déclarée est quand même enregistrée. Le Wi-Fi, lui, ne change jamais le mode.

**Interrupteur allumé** :

| Événement | Présent | Absent | Nuit | Vacances |
|---|---|---|---|---|
| La dernière personne part | **Absent** | – | – | – |
| Quelqu'un arrive | – | **Présent** | – | **Présent** |

Un départ ne compte que si personne d'autre n'est déclaré à la maison (Wi-Fi, raccourci ou app), et cela
se vérifie **sous le verrou du mode**, au moment d'appliquer : une déclaration « à la maison » qui arrive
entre-temps empêche d'armer la maison. Une arrivée l'emporte sur un départ au même relevé. Un choix fait
à la main entre-temps n'est jamais écrasé. La règle « aucun téléphone relié à un compte actif : jamais »
ne vaut que pour la source Wi-Fi. Chaque arrivée ou départ vu sur le Wi-Fi, chaque changement automatique
et chaque changement du délai de départ sont écrits au journal. Box reliée mais injoignable : un « Je
pars » n'arme pas la maison (le raccourci le dit).

Dans tous les cas, la réponse du raccourci dit ce que la maison est devenue (« Maison en mode Absent »,
« Léa est encore à la maison : la maison reste en Présent », « Mode Nuit : la maison ne change pas »…).

## Affichage

App web (accueil) et app Mac (barre du haut) : « À la maison : … », qui n'y est pas depuis quand, et
« Mode Présent, automatique ». Aucune phrase ne suppose un genre. Un serveur plus ancien n'a pas la
route : rien n'est montré, sans erreur.
