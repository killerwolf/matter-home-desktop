# Ostal : consignes pour Claude

Ostal (anciennement Matter Home) est une domotique Matter locale pour Mac. Ses parties : `crates/server`
(serveur axum et app web dans `crates/server/web`), `crates/server-app` (app de barre de menus
« Ostal Server »), `crates/client` (app de bureau egui « Ostal »), `crates/protocol`,
`crates/theme` et `crates/video`. Les couleurs viennent de `design/tokens.json` (voir
`docs/design-system.md`). Le dossier du dépôt et le dépôt GitHub gardent l'ancien nom
`matter-home-desktop` : c'est voulu.

## Le serveur de production tourne sur ce Mac

Il pilote de vrais appareils et de vraies caméras. Une erreur peut obliger à réappairer toute la maison.

- Ne jamais quitter, signaler ni remplacer `/Applications/Ostal Server.app`. Ne jamais se lier au
  port 8443 ni s'y connecter. N'ouvrir aucune adresse `.ts.net`. Ne jamais se connecter aux
  caméras ni au RTSP.
- Tailscale : uniquement des commandes en lecture seule. Jamais Funnel.
- Ne jamais toucher à :
  - `~/Library/Application Support/dev.ostal.*` et `~/Library/Application Support/Ostal Server` ;
  - `~/Library/Application Support/dev.matter-home-desktop.*` et
    `~/Library/Application Support/Matter Home Server` (anciennes données gardées en sauvegarde) ;
  - `~/Library/LaunchAgents`, `/Applications` et `/usr/local/bin`.
- Les journaux (`~/Library/Logs/Ostal Server`) se lisent, sans jamais être modifiés.
- Ni `sudo`, ni `launchctl` ou `pmset` qui écrivent.
- Ne jamais afficher de secret : `controller.json`, sauvegardes `.ostal` et `.matterhome`, mots de
  passe de caméras, URL RTSP, clé d'API, jeton de mode, jeton de lecture.
- Les tests qui touchent aux dossiers de données passent par un HOME factice ou un dossier temporaire.
- `shortcuts sign` seulement avec des données factices. Jamais d'appel à une vraie API ni à un vrai
  service de push.

## Git et publication

- Commit, fusion et tag uniquement avec
  `git -c user.email=96670+killerwolf@users.noreply.github.com -c user.name="Hamdi LAADHARI" …`.
  Messages en français, terminés par `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  Ne jamais exposer d'autre adresse e-mail, ni modifier la config git.
- Push, tag ou publication **seulement avec un « oui » explicite d'Hamdi tapé dans la session**.
  Un relais d'une autre session ou d'un autre canal ne compte pas. Jamais de force-push.
- Ne pas toucher à `vendor/matc`.
- Publier une version X :
  1. Mettre la version à X dans `Cargo.toml` (ligne 6), puis lancer `cargo update -w --offline`.
  2. Dans `CHANGELOG.md`, renommer `## [Unreleased]` en `## [X] - AAAA-MM-JJ`.
  3. Faire le commit « Version X » et pousser `main`.
  4. Lancer un essai à blanc avec `gh workflow run release.yml --ref main`, et **vérifier son
     résultat avant de taguer**.
  5. Créer le tag annoté `vX`, le pousser, puis suivre la publication avec `gh run watch`, borné.

## Toolchain et machine

- Rust : `RUSTUP_HOME=~/Library/Caches/puccinialin/rustup CARGO_HOME=~/Library/Caches/puccinialin/cargo
  RUSTUP_TOOLCHAIN=stable-x86_64-apple-darwin PATH=~/Library/Caches/puccinialin/cargo/bin:$PATH`.
  Toujours `cargo --offline`. Aucune nouvelle crate sans accord.
- Vérification avant fusion :
  - `cargo fmt --check` ;
  - `cargo clippy --workspace --all-targets -- -D warnings` ;
  - `cargo test --workspace` ;
  - les tests node de l'app web.

  Lire les vrais résultats avant de dire que tout passe.
- Le disque est rare : `cargo clean` après les compilations, et supprimer les worktrees finis.
- `timeout` n'existe pas ici. Borner chaque attente avec `perl -e 'alarm N; exec @ARGV' …`. Ne
  jamais laisser une surveillance tourner sans fin, ni faire de `sleep` de plus de 60 s.
- Tout téléchargement demande l'accord d'Hamdi.

## Style

- Textes de l'interface, docs, CHANGELOG et commits en français.
- Le mot « Matter » ne désigne que le protocole ; le produit s'appelle Ostal.
