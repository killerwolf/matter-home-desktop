# Contribuer

Merci de votre intérêt ! Ce projet est personnel et évolue au fil des besoins : les
rapports de bugs, idées et propositions de modification sont les bienvenus.

## Signaler un problème ou proposer une idée

Ouvrez une [issue](https://github.com/killerwolf/matter-home-desktop/issues) en précisant :
la version de macOS, les appareils concernés (marque, modèle, Thread ou Wi-Fi), ce que vous
avez fait, ce qui s'est passé, et les journaux utiles
(`RUST_LOG=debug ./target/release/matter-home-desktop`).

**Avant de coller des journaux, retirez vos données personnelles** : codes d'appairage,
adresses IP, identifiants de vos appareils et de votre fabric, chemins contenant votre nom.

## Préparer l'environnement

- macOS et [Rust](https://rustup.rs) (stable, 1.87 ou plus), outils en ligne de commande
  de Xcode (`xcode-select --install`) pour construire le `.app` et l'icône.
- Pas besoin d'appareil Matter pour la plupart des modifications : le **mode démo** simule
  thermomètres, porte, prise et automatisations.

```bash
cargo build
```

```bash
cargo run -- --demo
```

`--plan`, `--rules` et `--device <nom>` ouvrent directement un onglet ou une fiche.

## Avant de proposer une modification

Ces trois commandes doivent passer sans erreur ni avertissement :

```bash
cargo test
```

```bash
cargo clippy --all-targets
```

```bash
cargo build --release
```

Puis vérifiez votre modification à l'écran en mode démo et, si elle touche au protocole,
avec de vrais appareils.

## Conventions

- **Langue** : interface, commentaires, documentation et messages de commit en français.
- **Style** : `rustfmt` ; code dans le style des fichiers voisins, commentaires qui
  expliquent le *pourquoi*.
- **Interface jamais bloquée** : aucune entrée/sortie dans le thread egui. Le réseau passe par
  le backend tokio, les écritures disque par des threads ou tâches dédiés, et chaque attente
  réseau est bornée par un délai.
- **Tests** : toute logique pure (conversions, décodage, règles, plan, historique) a ses tests
  unitaires ; un comportement visible en mode démo passe par le test de bout en bout de
  `src/ui/mod.rs`.
- **Données personnelles** : jamais de vraies données dans le dépôt, y compris dans les tests
  (identifiants d'appareils, adresses, codes) ; utilisez des valeurs inventées. Les données de
  l'application vivent dans le dossier de données local, ignoré par Git.
- **`vendor/matc`** : modifiez-le le moins possible, repérez chaque changement par un
  commentaire `[matter-home-desktop]` et documentez-le dans
  [docs/architecture.md](docs/architecture.md).
- **Captures d'écran** : uniquement en mode démo, rangées dans `docs/screenshots/`.

## Construire l'application et le DMG

```bash
scripts/bundle.sh --dmg
```

Le script compile un binaire universel (Apple Silicon + Intel) si les deux cibles Rust sont
installées (`rustup target add x86_64-apple-darwin aarch64-apple-darwin`), sinon pour ce Mac
seulement, puis assemble `dist/Matter Home Desktop.app` (Info.plist, icône, signature ad hoc)
et `dist/MatterHomeDesktop-<version>-<arch>.dmg`. L'icône se régénère avec
`scripts/make-icon.sh`.

## Publier une version

La CI (`.github/workflows/release.yml`) vérifie chaque push et chaque pull request
(formatage, clippy, tests, construction du `.app`). Un tag `v*` publie en plus une version
GitHub avec le DMG universel :

1. Décrivez les changements dans `CHANGELOG.md`, sous un titre `## [x.y.z] - AAAA-MM-JJ` :
   c'est ce texte qui devient la note de version.
2. Commitez, puis créez et poussez le tag :

   ```bash
   git tag v0.2.0 && git push origin main v0.2.0
   ```

La version du tag est reportée dans `Cargo.toml` au moment du build. Un tag avec un tiret
(`v0.2.0-rc.1`) est publié comme pré-version. On peut aussi lancer le workflow à la main
(« Run workflow ») pour obtenir un DMG de test, sans publier de version.

## Commits et pull requests

- Un commit par modification qui fonctionne (compile, tests et clippy au vert), avec un
  message en français qui dit ce qui change et pourquoi.
- Utilisez votre adresse GitHub « noreply » si vous ne voulez pas publier votre e-mail.
- Ouvrez une pull request vers `main` en décrivant la modification et comment vous l'avez
  vérifiée (mode démo, appareils réels).

Pour comprendre le fonctionnement interne, commencez par
[docs/architecture.md](docs/architecture.md).

En contribuant, vous acceptez que votre code soit publié sous la licence MIT du projet.
