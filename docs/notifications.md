# Notifications sur iPhone et iPad (web push)

[← Retour au README](../README.md) · [App web](web-app.md) · [Automatisations](automatisations.md) · [Détection](detection.md) · [Données](donnees.md)

Ticket n° 17. L'app web installée sur l'écran d'accueil reçoit une alerte **même fermée** : une
porte qui s'ouvre en mode Absent, une personne devant la caméra, une pile faible… C'est le « web
push » standard (VAPID, RFC 8292 ; message chiffré de bout en bout, RFC 8291), sans application à
installer et sans compte chez qui que ce soit.

## Activer sur l'iPhone ou l'iPad

**Il faut** : iOS ou iPadOS **16.4 ou plus**, et l'app **ajoutée à l'écran d'accueil**. Safari, dans
un onglet ordinaire, ne reçoit pas de notifications : c'est une règle d'Apple.

1. Ouvrez l'app web par l'adresse **`https://<nom-du-mac>.<tailnet>.ts.net/app/`** (voir
   [Accès à distance](acces-distant.md)). L'adresse directe en `:8443` ne convient pas : son
   certificat est auto-signé, et iOS n'accepte ni service worker ni notifications sans un certificat
   reconnu.
2. Dans Safari : **Partager** → **Sur l'écran d'accueil** → **Ajouter**.
3. Ouvrez **Maison** depuis l'écran d'accueil (pas depuis Safari) et connectez-vous.
4. Onglet **Appareils**, tout en bas : **Notifications** → **Activer les notifications sur cet
   appareil** → **Autoriser**.
5. **Envoyer un essai** : la notification « Notifications activées » arrive en quelques secondes.

L'app dit elle-même ce qui manque (version d'iOS trop ancienne, app ouverte dans Safari au lieu de
l'écran d'accueil, notifications refusées dans Réglages). Sur un Mac ou un PC, Safari, Chrome, Firefox
et Edge marchent aussi (même bouton).

## Ce que l'on reçoit

Chaque appareil choisit ses catégories (toutes cochées au départ) :

| Catégorie | Quand | Texte |
|---|---|---|
| **Règles « Notifier »** | une règle d'automatisation exécute « Notifier » | le titre et le message de la règle (variables remplacées) |
| **Détections caméra** | un passage détecté sur une caméra en **Surveillance** ([Détection](detection.md)) : **un seul message par passage**, à sa fin ; si une [description](descriptions.md) est attendue, avec elle (45 s au plus après la fin) | « Personne · Entrée » ; la description et le niveau de menace, ou « Passage détecté à 14:05. » ; le toucher ouvre la fiche de l'événement |
| **Pile faible** | une pile passe sous **15 %** ; un seul message, jusqu'à ce qu'elle remonte à 20 % ou plus (retenu même après un redémarrage du serveur). Un appareil qui ne donne pas de pourcentage : son signal « pile faible » (un message, jusqu'à ce qu'il redevienne correct). Dès qu'un pourcentage a été lu, c'est lui qui fait foi | « Capteur salon : pile à 12 %. » |
| **Appareil injoignable** | un appareil ne répond plus depuis **10 minutes** ; un message par panne | « Prise du four ne répond plus depuis 10 minutes. » |

- Pour une alerte plus fine (seulement en mode Absent, seulement la porte d'entrée…), décochez
  « Détections caméra » et faites une règle « Quand une détection… seulement si le mode est Absent →
  Notifier » : elle arrive par la catégorie « Règles ».
- Les alertes et détections partent en urgence **haute** (affichées tout de suite), valables 1 heure
  chez le service de push ; pile et pannes en urgence normale (24 h et 6 h).
- **Jamais** dans le texte : l'adresse d'une caméra, un identifiant ou un mot de passe de caméra, un
  identifiant d'appareil brut, une adresse `rtsp://` ou IP, un chemin du Mac. Le serveur masque ce
  qui y ressemble (« <adresse> », « <appareil> », « ••• ») avant l'envoi, y compris dans les textes
  des règles. Un appareil sans nom s'appelle « Un appareil ».

Le service de push renouvelle parfois un abonnement : le service worker se réabonne de lui-même,
et l'app web renvoie l'abonnement au serveur **à chaque démarrage** (sans rien demander). Le nom
d'un appareil abonné n'est donné qu'à la création : un renvoi ne l'écrase pas.

## Ce qui sort du serveur

C'est le seul passage par un service extérieur, et il est réduit au minimum :

- le serveur envoie au **service de push de l'appareil** — pour un iPhone ou un iPad,
  `web.push.apple.com` (Apple) ; pour un navigateur de bureau, celui de Google, Mozilla ou Microsoft —
  et **à aucune autre adresse** (une adresse d'abonnement d'un autre domaine est refusée) ;
- **le message est chiffré de bout en bout** (AES-128-GCM, clé propre à chaque message, RFC 8291) :
  seul l'appareil abonné peut le lire, pas Apple ;
- Apple voit l'adresse d'abonnement, l'heure, la taille du message chiffré et un jeton signé par la
  clé du serveur (VAPID), dont le contact est l'adresse du projet sur GitHub (pas la vôtre) ;
- l'envoi passe par le `curl` du système (environnement vide, sans proxy, https seulement) ; le
  message en clair n'est jamais écrit dans le journal.

Pour que rien ne parte : **couper** « Notifications activées sur le serveur » (feuille Notifications
de l'app web, ou fenêtre du serveur, onglet Connexion). Les abonnements restent, rien n'est envoyé. Tant
qu'aucun appareil n'est abonné, rien ne part non plus. En mode démonstration, rien n'est jamais
envoyé.

## Révoquer un appareil

- Depuis l'appareil : **Notifications** → **Désactiver sur cet appareil**.
- Depuis n'importe quel appareil connecté : **Appareils abonnés** → **Révoquer** (un iPhone perdu,
  par exemple).
- Depuis la fenêtre du serveur (onglet Connexion, « Notifications sur l'iPhone et l'iPad ») : **Révoquer** sur la ligne de l'appareil.
- Supprimer l'app de l'écran d'accueil, ou retirer l'autorisation dans Réglages › Notifications :
  le service de push répond alors « abonnement inconnu » (404 ou 410) au prochain envoi, et le
  serveur l'oublie de lui-même.

## Fichiers

Dans `serveur/` (dossier 0700, fichiers 0600), **jamais exportés** dans une sauvegarde
`.ostal` (voir [Données](donnees.md)) :

- `push-vapid.json` : la clé privée VAPID du serveur, créée au premier lancement. Si elle disparaît
  ou devient illisible, une nouvelle est créée et **les abonnements sont oubliés** (ils étaient liés
  à l'ancienne) : rouvrez la feuille Notifications sur chaque appareil et touchez « Activer ».
  Après l'import d'une sauvegarde sur un autre Mac, il faut aussi réabonner les appareils.
- `push.json` : le réglage général, les abonnements (adresse, clés de l'appareil, nom, date,
  catégories, dernier envoi) et les appareils déjà signalés pour leur pile.

## Limites

- **iOS 16.4 ou plus, app sur l'écran d'accueil, adresse `.ts.net`** : sinon pas de notifications.
- Le Mac du serveur doit être allumé et joindre Internet (le service d'Apple) ; l'iPhone, lui, n'a
  pas besoin d'être sur le tailnet pour **recevoir** une notification, seulement pour ouvrir la
  fiche ensuite.
- 20 appareils abonnés au plus. Un message trop long est coupé (80 caractères de titre, 240 de corps).
- Si le service de push est saturé (429, 5xx), un seul nouvel essai ; ensuite le message est perdu
  (l'erreur s'affiche sur la ligne de l'appareil).
- Les seuils (15 % de pile, 10 minutes d'absence) ne se règlent pas : pour d'autres valeurs, faites
  une règle (« Quand la pile passe sous… », « Quand l'appareil reste injoignable N minutes ») avec
  « Notifier ».
- Le client de bureau (macOS) ne reçoit pas ces notifications : il affiche celles des règles quand il
  est ouvert, comme avant.

## Routes

Sous le jeton de session (voir [Protocole](protocole.md#notifications-web-push)) : `GET` et `PUT
/api/push`, `POST /api/push/subscriptions`, `PUT` et `DELETE /api/push/subscriptions/{id}`,
`POST /api/push/subscriptions/{id}/test`.
