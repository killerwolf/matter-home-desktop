# Automatisations

[← Retour au README](../README.md)

Onglet **« Automatisations »** : la liste des règles (case cochée = règle activée), l'éditeur
de la règle sélectionnée et le journal des dernières exécutions.

## Composer une règle

| | Possibilités |
|---|---|
| **Quand** | une porte / fenêtre s'ouvre ou se ferme ; une mesure (température, humidité, puissance, pile) dépasse ou descend sous un seuil, éventuellement pendant N minutes ; une prise / lampe s'allume ou s'éteint ; un appareil reste injoignable N minutes ; à une heure donnée, certains jours |
| **Si** (toutes les conditions) | plage horaire (elle peut passer minuit : 22:00 → 07:00) ; porte ouverte / fermée ; prise allumée / éteinte ; mesure au-dessus / en dessous d'une valeur |
| **Alors** (dans l'ordre) | notification macOS ; allumer / éteindre / inverser une prise ou lampe ; appeler une URL (webhook) ; attendre N secondes |

**« ▶ Tester »** exécute les actions tout de suite, sans vérifier « Quand » ni « Si ».

## Exemples

- **Porte ouverte la nuit** — Quand : la porte d'entrée s'ouvre. Si : entre 23:00 et 06:00.
  Alors : notifier « {appareil} ouverte à {heure} ».
- **Veilleuse du soir** — Quand : à 19:30, du lundi au vendredi. Alors : allumer la prise de
  la lampe.
- **Frigo en surconsommation** — Quand : la puissance de la prise du frigo dépasse 150 W
  pendant 15 min. Alors : notifier.
- **Capteur muet** — Quand : le thermomètre reste injoignable 60 min. Alors : notifier.

## Textes et variables

Les textes des notifications et des webhooks acceptent :

| Variable | Remplacée par |
|---|---|
| `{appareil}` | nom de l'appareil qui a déclenché la règle |
| `{valeur}` | valeur ou état (« ouverte », « 26,40 °C », « allumée »…) |
| `{heure}` | heure du déclenchement (HH:MM) |
| `{regle}` | nom de la règle |

## Webhooks

L'action « Appeler une URL » fait un GET, ou un POST si un corps est saisi (envoyé en JSON
s'il commence par `{`, en texte sinon). Seules les adresses `http://` et `https://` sont
acceptées ; délai maximal de 10 s. Pratique pour IFTTT, ntfy, Slack, Home Assistant…

## Quand une règle se déclenche

- **Anti-rafale** : « pas plus d'une exécution toutes les N minutes » (1 min par défaut).
- **Seuils** : une règle ne se déclenche qu'une fois tant que le seuil reste franchi ; elle
  se réarme quand la mesure repasse de l'autre côté. Un seuil déjà franchi au démarrage se
  déclenche à la première mesure reçue.
- **Changements uniquement** : au démarrage ou à la reconnexion, la première valeur connue
  d'un appareil ne déclenche rien, pas plus que les mesures relues de l'historique.
- **Commandes** : une prise ou lampe ne peut être commandée que si elle est connectée à ce
  moment-là ; sinon l'action échoue et le journal l'indique.

## Limites

- Les règles ne s'exécutent que **tant que l'application est ouverte**.
- Les notifications apparaissent au nom de « Éditeur de script » (osascript).
- Pas encore de déclencheur « appui sur un bouton » (BILRESA…).

## Sauvegarde

Les règles sont dans `regles.json` (voir [Données](donnees.md)). Le mode démo utilise
`regles-demo.json`, avec trois règles d'exemple.
