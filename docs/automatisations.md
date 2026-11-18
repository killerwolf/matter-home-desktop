# Automatisations

[← Retour au README](../README.md)

Onglet **« Automatisations »** : la liste des règles (case cochée = règle activée), l'éditeur
de la règle sélectionnée et le journal des dernières exécutions.

## Composer une règle

| | Possibilités |
|---|---|
| **Quand** | le mode de la maison devient Présent, Absent, Nuit ou Vacances (voir [Modes](modes.md)) ; une caméra (ou n'importe laquelle) détecte une personne, un chat, un chien ou un véhicule (voir [Détection](detection.md)) ; une porte / fenêtre s'ouvre ou se ferme ; une mesure (température, humidité, puissance, pile, ouverture d'un volet) dépasse ou descend sous un seuil, éventuellement pendant N minutes ; une prise / lampe s'allume ou s'éteint ; un appareil reste injoignable N minutes ; à une heure donnée, certains jours |
| **Si** (toutes les conditions) | plage horaire (elle peut passer minuit : 22:00 → 07:00) ; le mode de la maison est l'un des modes cochés (« Absent ou Vacances ») ; porte ouverte / fermée ; prise allumée / éteinte ; mesure au-dessus / en dessous d'une valeur |
| **Alors** (dans l'ordre) | notification macOS ; allumer / éteindre / inverser une prise ou lampe ; ouvrir / fermer / arrêter un volet ou l'amener à N % d'ouverture ; appeler une URL (webhook) ; attendre N secondes ; changer le mode de la maison (voir [Modes](modes.md)) |

**« ▶ Tester »** exécute les actions tout de suite, sans vérifier « Quand » ni « Si ».

## Exemples

- **Porte ouverte la nuit** — Quand : la porte d'entrée s'ouvre. Si : entre 23:00 et 06:00.
  Alors : notifier « {appareil} ouverte à {heure} ».
- **Veilleuse du soir** — Quand : à 19:30, du lundi au vendredi. Alors : allumer la prise de
  la lampe.
- **Frigo en surconsommation** — Quand : la puissance de la prise du frigo dépasse 150 W
  pendant 15 min. Alors : notifier.
- **Capteur muet** — Quand : le thermomètre reste injoignable 60 min. Alors : notifier.
- **Volets du soir** — Quand : à 21:30. Alors : fermer le volet du salon, ouvrir à 20 % celui
  de la cuisine. L'ouverture d'un volet ne sert de seuil que si le moteur connaît sa position.

- **Alerte de porte selon le mode** — Quand : la porte d'entrée s'ouvre. Si : le mode est Absent
  ou Vacances. Alors : notifier « {appareil} ouverte à {heure} ».
- **Accueil** — Quand : le mode devient Présent. Alors : allumer la lampe de l'entrée.
- **Quelqu'un dans l'allée** — Quand : la caméra « Allée » détecte une personne. Si : le mode est
  Absent ou Vacances. Alors : notifier « {valeur} détectée sur {appareil} à {heure} ».

## Textes et variables

Les textes des notifications et des webhooks acceptent :

| Variable | Remplacée par |
|---|---|
| `{appareil}` | nom de l'appareil qui a déclenché la règle (de la caméra pour une détection) |
| `{valeur}` | valeur ou état (« ouverte », « 26,40 °C », « allumée »…, le nom du mode pour « Quand le mode devient… », la classe pour une détection : « personne », « chat », « chien », « véhicule ») |
| `{heure}` | heure du déclenchement (HH:MM) |
| `{regle}` | nom de la règle |
| `{description}` | pour une détection : la phrase qui décrit l'événement (voir [Descriptions](descriptions.md)) ; vide sans description |
| `{niveau}` | pour une détection : `normal`, `suspect` ou `critique` ; vide sans description |

Une règle dont le texte emploie `{description}` ou `{niveau}` **attend la description** de l'événement
(à la fin du passage, ou dès la première description si « Décrire dès l'ouverture » est coché) avant de
partir ; sans description (fonction désactivée, plafond atteint, erreur), elle part avec ces variables
vides. Voir [Descriptions](descriptions.md).

## Webhooks

L'action « Appeler une URL » fait un GET, ou un POST si un corps est saisi (envoyé en JSON
s'il commence par `{`, en texte sinon). Seules les adresses `http://` et `https://` sont
acceptées ; délai maximal de 10 s. Pratique pour IFTTT, ntfy, Slack, Home Assistant…

Une règle **activée** doit avoir une adresse complète : tant qu'elle est vide ou à moitié saisie,
l'ensemble des règles reste un brouillon (bandeau orange, et « ⚠ Règles non enregistrées » dans la
barre du haut) et rien n'est envoyé au serveur. Le bouton « Revenir à la version du serveur »
abandonne ce brouillon. Une règle **désactivée** peut garder une adresse incomplète ; c'est aussi
ce que fait l'import d'une sauvegarde de la v0.2.1 pour ses brouillons (voir
[migration.md](migration.md)).

## Quand une règle se déclenche

- **Anti-rafale** : « pas plus d'une exécution toutes les N minutes » (1 min par défaut).
- **Seuils** : une règle ne se déclenche qu'une fois tant que le seuil reste franchi ; elle
  se réarme quand la mesure repasse de l'autre côté. Un seuil déjà franchi au démarrage se
  déclenche à la première mesure reçue.
- **Mode** : une règle « Quand le mode devient X » ne se lance que sur un **changement** : redemander
  le mode actuel, ou le mode lu au démarrage du serveur, ne déclenche rien. Dans « Seulement si », au
  moins un mode doit être coché (le serveur refuse une condition sans mode).
- **Détection** : une règle part **une fois par passage**, pas à chaque image : à son début (dès la
  première détection), ou, pour une règle qui suit une classe précise, quand cette classe **apparaît**
  en cours de passage (une personne sort de la voiture, le maître suit le chien). L'anti-rafale
  s'applique en plus. Sans case cochée, n'importe quelle classe compte (une fois, à l'ouverture).
- **Changements uniquement** : au démarrage ou à la reconnexion, la première valeur connue
  d'un appareil ne déclenche rien, pas plus que les mesures relues de l'historique.
- **Commandes** : une prise, une lampe ou un volet ne peut être commandé que si l'appareil
  (ou son pont) est connecté à ce moment-là ; sinon l'action échoue et le journal l'indique.

## Limites

- Les règles sont exécutées par **le serveur**, en continu, même quand aucun client n'est ouvert.
  Elles s'arrêtent si le serveur s'arrête (Mac en veille, service arrêté).
- Les notifications partent vers les clients connectés au serveur et s'affichent dans leur
  interface, **et** vers les iPhone et iPad abonnés en web push, même app fermée (catégorie
  « Règles », voir [Notifications](notifications.md)). Un client de bureau fermé ne reçoit rien.
- Pas encore de déclencheur « appui sur un bouton » (BILRESA…).

## Sauvegarde

Les règles sont dans `regles.json` (voir [Données](donnees.md)). Le mode démo utilise
`regles-demo.json`, avec trois règles d'exemple.
