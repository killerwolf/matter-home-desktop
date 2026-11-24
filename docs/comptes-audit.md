# Journal d'audit

Qui a fait quoi, depuis quel téléphone ou ordinateur. Il se lit dans l'onglet **Activité** d'Ostal
Server et avec `ostal-server audit`. Il n'est lisible nulle part ailleurs (ni app web, ni app Mac)
dans cette version.

## Ce qui y entre

| Action (`action`) | Quand |
|---|---|
| `login` | connexion réussie, ou refusée (`result: refused`) ; « trop d'essais » porte `detail: lockout` |
| `new_connection` | première connexion depuis un téléphone ou un ordinateur |
| `connection_cut` | connexion coupée (par son propriétaire, par Ostal Server) ou déconnexion (`detail: logout`) |
| `password_changed`, `user_created`, `user_disabled`, `user_enabled` | comptes, depuis la ligne de commande ou Ostal Server |
| `mode_changed` | seulement quand le mode change vraiment ; `target` est le mode, `source` dit d'où il vient |
| `device_command` | marche/arrêt d'une prise, volet ; `target` est le nom de l'équipement, `detail` la commande. Un agent d'IA écrit la sienne avec `source: "agent"` et son libellé |
| `automation_changed`, `automation_tested` | automatisations |
| `device_paired`, `device_removed` | appairage (le code n'est jamais écrit) et suppression d'un équipement |
| `export`, `import` | sauvegarde (un `import` n'est écrit que s'il est vraiment lancé, pas pour sa vérification) |
| `presence` | « Je pars » et « Je rentre » |
| `agent_created`, `agent_revoked`, `agent_switch` | un agent d'IA créé (`target` : son libellé, `detail` : la personne) ou révoqué ; l'interrupteur de l'accès des agents allumé ou éteint (`detail: on` ou `off`), depuis la ligne de commande. Jamais le jeton. Voir [Agents d'IA](agents-ia.md) |
| `confirmation_requested`, `confirmation_approved`, `confirmation_refused`, `confirmation_expired` | une action sensible d'un agent d'IA qui attend l'accord d'une personne : demandée par l'agent, autorisée ou refusée par la personne (la ligne porte **cette personne** et sa session), expirée sans réponse (5 minutes). `target` : le libellé de l'agent ; `detail` : l'outil, mot fermé. Voir [Agents d'IA](agents-ia.md#les-confirmations-e3) |
| `agent_read`, `agent_occupation`, `agent_denied` | ce que fait un agent d'IA : ses lectures **regroupées** (une ligne par 10 minutes), ses lectures de la présence et du mode **une par une**, ses appels refusés. Jamais une valeur lue. Voir [Agents d'IA](agents-ia.md#le-journal-daudit-des-agents) |

La source d'un changement de mode : `person` (une personne, dans une app), `shortcut` (un
raccourci de l'iPhone), `automation` (une automatisation, son nom est dans `detail`) ou `automatic`
(la maison toute seule, par la règle de présence : le journal l'attribue à celui qui a dit partir
ou rentrer) ; `agent` (un agent d'IA, son libellé est dans `detail`) ; une origine d'une version plus récente se lit `autre`.

Les actions des automatisations (une règle qui allume une prise) ne sont pas écrites une à une :
elles ont déjà leur historique.

## Où et combien de temps

`serveur/audit/AAAA-MM.jsonl`, un fichier par mois, une ligne JSON par événement, ajoutée à la fin
(dossier 0700, fichiers 0600). Chaque écriture ouvre le fichier, ajoute sa ligne d'un seul coup et le
referme : le serveur, la ligne de commande et la fenêtre écrivent et lisent le même dossier sans se
coordonner. Les fichiers dont tous les jours ont plus de 6 mois sont supprimés au démarrage, puis
chaque mois ; les lignes de plus de 6 mois ne sont jamais montrées. Le dossier est dans `serveur/` :
l'export ne l'emporte pas et un import le laisse en place. **Il faut donc le sauvegarder à part** si on
veut le garder. Il révèle les habitudes du foyer (qui est là, quand) : chacun doit savoir qu'il existe.

## Ce qui n'y entre jamais

Mot de passe, jeton (session, raccourci, widget), code d'appairage, mesure, message d'erreur. Un
identifiant tapé lors d'une connexion refusée n'y entre que s'il désigne un compte existant (on tape
parfois son mot de passe dans cette case). Une connexion « inconnue » (un client d'avant la version 4,
sans identifiant) n'a pas d'identifiant de connexion. Les refus en rafale sont limités : une ligne
par 5 secondes et par adresse et compte, une ligne de blocage par minute et par adresse.

## Les champs

`at` (secondes Unix), `user` (identifiant du compte) et `user_name` (son nom à ce moment-là),
`connection` (`id`, `name`, `kind` : `web`, `mac`, `shortcut`, `server` pour Ostal Server, `cli` pour
la ligne de commande, `agent` pour un agent d'IA : son `name` est le libellé de l'agent et son `id` celui du jeton, jamais le jeton), `ip` (derrière `tailscale serve`, c'est 127.0.0.1) et `via` (`local` ou
`tailscale`), `action`, `target`, `result` (`ok`, `refused` ou `error`), `source` et `detail`.
Un champ absent est omis. Une ligne illisible (arrêt brutal) est ignorée à la lecture.

## Lire

```sh
ostal-server audit                         # 7 derniers jours, tout le monde
ostal-server audit --user sam --days 30    # un compte (identifiant ou nom affiché)
```

Une ligne par événement, la plus récente d'abord : date, qui, depuis, quoi (séparés par des
tabulations). Lecture seule : rien n'est démarré et aucun verrou n'est pris.
