# Descriptions des événements (API Anthropic)

[← Retour au README](../README.md) · [Détection](detection.md) · [Automatisations](automatisations.md) · [Données](donnees.md)

Ticket n° 19. Comme la revue GenAI de Frigate, le serveur peut faire décrire chaque événement de
[détection](detection.md) par un modèle Claude : **une phrase en français** (200 caractères au plus) et
un **niveau de menace** : 0 normal, 1 suspect, 2 critique. La phrase est affichée avec l'événement
(app web, client de bureau) et peut entrer dans le texte d'une notification.

> « Une personne en veste claire dépose un colis devant la porte. » — niveau 0
> « Quelqu'un essaie d'ouvrir la porte du garage. » — niveau 2

**C'est désactivé par défaut** : sans votre clé d'API et sans avoir coché la case, rien ne part.

## Ce qui est envoyé

À chaque description, **un seul appel HTTPS** à `https://api.anthropic.com/v1/messages` contient :

- **la capture JPEG** de l'événement (une seule image, côté long **1 024 pixels au plus**) ;
- une consigne en français, avec **le nom d'affichage de la caméra** (« Allée ») et **la liste des classes
  détectées** (« personne, chien »).

Jamais : la vidéo ou le flux, les clips, l'adresse ou l'identifiant d'une caméra, ses identifiants de
connexion, l'adresse du serveur, le plan ou les appareils de la maison. La capture peut montrer des
personnes et le lieu : c'est ce que vous confiez à Anthropic en activant la fonction (voir leurs
conditions et leur politique de conservation des données pour l'API).

L'appel est fait par `curl` (celui de macOS, comme pour les webhooks) : la clé passe par l'entrée standard de
`curl`, jamais en argument (un `ps` la montrerait) ; le corps de la requête est un fichier temporaire en
0600, effacé aussitôt. Aucun journal ne contient la clé, ni l'image, ni la phrase (seulement
« description de l'événement … : niveau 1 »).

## Activer

Fenêtre du serveur → **« Descriptions des événements… »** :

1. **Clé d'API** : créez-la dans la [console Anthropic](https://console.anthropic.com) et collez-la. Elle
   est enregistrée une fois et **n'est plus jamais réaffichée** (la fenêtre dit seulement qu'une clé est
   enregistrée). « Retirer la clé » l'efface.
2. **Cochez « Décrire les événements de détection »**, choisissez le modèle, les caméras et les classes,
   le plafond quotidien, puis « Enregistrer les réglages ».

| Réglage | Par défaut | Rôle |
|---|---|---|
| Décrire les événements | non | interrupteur général |
| Décrire dès l'ouverture | non | une description de la première capture dès le début du passage, **en plus** de celle de la fin (deux fois le coût) ; sinon une seule, à la fin, avec la meilleure capture |
| Modèle | Opus 5.5 | voir le tableau des coûts |
| Caméras | toutes | toutes, les **extérieures** seulement (emplacement choisi dans « Caméras… »), ou une liste |
| Classes | toutes | personne, chat, chien, véhicule : seuls les événements qui en contiennent une sont décrits |
| Plafond par jour | 200 | au-delà, les événements du jour **restent sans description** (une ligne au journal par jour) |

## Modèle et coût

Estimation par événement (~1 500 jetons en entrée, ~150 en sortie) :

| Modèle | Identifiant API | Par événement | 100 événements par jour |
|---|---|---|---|
| Haiku 5.5 | `claude-haiku-5-5` | ≈ 0,02 centime | ≈ 0,70 $/mois |
| Sonnet 5.5 | `claude-sonnet-5-5` | ≈ 0,45 centime | ≈ 13 $/mois |
| **Opus 5.5** (défaut) | `claude-opus-5-5` | ≈ 0,9 centime | ≈ 27 $/mois |

Ce sont des ordres de grandeur : le tarif et la taille d'image comptée peuvent changer, la facture fait foi.
Avec « Décrire dès l'ouverture », doublez la ligne.

### Fixer une limite de dépense (à faire)

Le plafond quotidien du serveur borne une rafale (une caméra qui s'affole), pas un oubli. **Fixez aussi
une limite côté Anthropic** : [console Anthropic](https://console.anthropic.com) → **Settings** →
**Limits** → limite de dépense mensuelle (et, si vous voulez, une alerte à un seuil). Une clé réservée à
cet usage (« Workspace » dédié) permet de la révoquer sans toucher au reste.

## Dans les règles

Les textes des notifications et des webhooks acceptent deux variables de plus pour une règle « Quand une
caméra détecte… » :

| Variable | Remplacée par |
|---|---|
| `{description}` | la phrase (vide si l'événement n'a pas de description) |
| `{niveau}` | `normal`, `suspect` ou `critique` (vide sans description) |

Exemple : notifier « {niveau} : {description} ({appareil}, {heure}) ».

**Une règle qui emploie une de ces variables attend la description pour partir** : sinon la notification
dirait « {description} » à vide. Par défaut (une description, à la fin du passage), elle part donc **à la
fin du passage**, quelques secondes après la dernière détection, au lieu de partir à l'ouverture ; avec
« Décrire dès l'ouverture », dès que la première description arrive. Si la description ne vient pas
(plafond atteint, erreur, refus), la règle part quand même, avec des variables vides. Les règles qui
n'emploient pas ces variables partent comme avant, à l'ouverture. Le délai anti-rafale s'applique au
départ réel de la règle.

La condition « niveau ≥ X » n'existe pas encore : pour ne prévenir que sur un niveau élevé, il n'y a pas
d'autre moyen pour l'instant que de lire `{niveau}` dans le texte.

## Fonctionnement

- **Un fil à part** : ni l'analyse des caméras ni l'écriture des événements ne font d'appel réseau. Les
  descriptions passent par une file de 8 tâches et un fil dédié, un appel à la fois ; si la file est
  pleine (l'API est lente), la **plus ancienne** est abandonnée, avec une ligne au journal.
- **Délais** : 30 s par appel ; un seul nouvel essai sur un 429, une erreur 5xx ou une erreur réseau,
  après le délai `retry-after` de la réponse s'il y en a un (30 s au plus). Les autres erreurs (clé
  refusée, 400) ne sont pas réessayées : le journal dit le code et le type d'erreur de l'API.
- **Refus du modèle** (`stop_reason: refusal`) : la fiche porte « description indisponible », sans nouvel
  essai. Réponse tronquée (`max_tokens`) : échec, sans description. Pour Opus 5.5 et Sonnet 5.5, la
  reprise côté Anthropic en cas de refus (`server-side-fallback`) est demandée.
- **Format** : sortie structurée `{"description": …, "niveau": 0 | 1 | 2}` ; la phrase est coupée à
  200 caractères si besoin.
- **Plafond** : compté par jour (heure locale) depuis le démarrage du serveur : un redémarrage remet le
  compte à zéro.
- La description est écrite dans la fiche de l'événement (`<données>/events/<id>.json`, champs
  `description` et `niveau`), **diffusée** aux clients (même événement, nouvel envoi), et s'efface avec lui
  au bout de 7 jours. Le protocole reste le 3 (voir [Protocole](protocole.md)).

## Où est la clé

`<données>/serveur/descriptions.json` (dossier 0700, fichier **0600**) garde les réglages **et la clé**, en
clair, comme `cameras.json` garde les mots de passe des caméras. Mais, contrairement à `cameras.json`, il est
dans `serveur/` : **il n'est jamais exporté** dans une sauvegarde `.matterhome` (une clé d'API dépense de
l'argent : on ne la laisse pas dans un fichier que l'on transfère). L'import d'une sauvegarde garde celui
du Mac. Sur un nouveau Mac, ressaisissez la clé (les réglages sont à refaire aussi). En mode démonstration,
rien n'est lu, écrit ni envoyé.

## Désactiver

- Décochez « Décrire les événements de détection » (rien ne part, la clé reste enregistrée), ou
- **Retirez la clé** (rien ne peut partir), ou
- révoquez la clé dans la console Anthropic.

Les descriptions déjà enregistrées restent sur les événements jusqu'à leur suppression.

## Essais sans l'API

Les tests du serveur n'appellent jamais l'API : ils parlent à un faux serveur HTTP local (127.0.0.1, port
éphémère). L'adresse de l'API ne se change pas par l'environnement ni par un fichier : seul le code de test
peut la remplacer, pour qu'une clé ne puisse pas partir ailleurs.
