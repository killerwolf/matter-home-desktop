#!/usr/bin/env bash
#
# Essaie install.sh et uninstall.sh de l'archive du serveur sans toucher au Mac : tout se passe
# dans un dossier temporaire (préfixe factice et HOME factice), et AUCUNE commande système
# (sudo, launchctl, pmset, pare-feu) n'est lancée.
#
#   scripts/test-server-package.sh [dist/matter-home-server-<version>-macos.tar.gz]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="${1:-}"
if [ -z "$ARCHIVE" ]; then
  set -- "$ROOT"/dist/matter-home-server-*-macos.tar.gz
  ARCHIVE="$1"
fi
[ -f "$ARCHIVE" ] || { echo "archive introuvable : $ARCHIVE" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/mh-server-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
REAL_HOME="$HOME"

mkdir "$WORK/archive"
tar -xzf "$ARCHIVE" -C "$WORK/archive"
PKG="$(echo "$WORK"/archive/matter-home-server-*-macos)"
BIN="$PKG/matter-home-server"

PREFIX="$WORK/racine"
export HOME="$PREFIX/home"
mkdir -p "$HOME"
DATA="$HOME/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop"
PLIST="$PREFIX/Library/LaunchDaemons/dev.matter-home-desktop.server.plist"
INSTALLED="$PREFIX/usr/local/bin/matter-home-server"
LOGS="$HOME/Library/Logs/MatterHomeServer"

fail() { echo "ÉCHEC : $*" >&2; exit 1; }
ok() { echo "  ok : $*"; }
contains() { grep -qF -- "$2" <<< "$1" || fail "« $2 » absent de la sortie : $1"; }
mode() { stat -f '%Lp' "$1"; }
count_files() { find "$1" -type f | wc -l | tr -d ' '; }

echo "Contenu de l'archive"
for f in matter-home-server install.sh uninstall.sh dev.matter-home-desktop.server.plist.template LISEZMOI.txt VERSION; do
  [ -f "$PKG/$f" ] || fail "$f manque dans l'archive"
done
ok "fichiers présents ($(cat "$PKG/VERSION"))"
[ "$(find "$WORK/archive" -name '._*' | wc -l | tr -d ' ')" = "0" ] || fail "fichiers ._* dans l'archive"
codesign --verify --strict "$BIN" || fail "signature du binaire"
ok "signature valide"
bash -n "$PKG/install.sh" && bash -n "$PKG/uninstall.sh" && ok "bash -n"

echo "install.sh --a-blanc : n'écrit rien"
OUT="$("$PKG/install.sh" --a-blanc --prefixe "$PREFIX" 2>&1)"
contains "$OUT" "[à blanc]"
contains "$OUT" "plutil -lint : OK"
contains "$OUT" "sudo pmset -a sleep 0"
contains "$OUT" "socketfilterfw --add /usr/local/bin/matter-home-server"
contains "$OUT" "AUCUNE"
[ "$(count_files "$PREFIX")" = "0" ] || fail "l'essai à blanc a écrit des fichiers : $(find "$PREFIX" -type f)"
[ ! -e "$DATA" ] || fail "le dossier de données ne doit jamais être créé à vide"
ok "rien d'écrit, commandes affichées"

echo "install.sh avec un HOME hors du préfixe : refusé avant toute action"
if HOME="$REAL_HOME" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" >/dev/null 2>&1; then
  fail "un HOME hors du préfixe doit être refusé"
fi
ok "refusé"

echo "install.sh --prefixe (sans identité)"
OUT="$("$PKG/install.sh" --prefixe "$PREFIX" --oui 2>&1)"
contains "$OUT" "Le service est installé mais pas démarré"
contains "$OUT" "pas démarré"
[ -x "$INSTALLED" ] || fail "binaire non installé"
[ "$(mode "$INSTALLED")" = "755" ] || fail "binaire installé en 755"
[ "$(mode "$PLIST")" = "644" ] || fail "plist en 644"
plutil -lint "$PLIST" >/dev/null || fail "plutil -lint"
PB=/usr/libexec/PlistBuddy
[ "$($PB -c 'Print :UserName' "$PLIST")" = "$(id -un)" ] || fail "UserName"
[ "$($PB -c 'Print :UserName' "$PLIST")" != "root" ] || fail "UserName ne doit pas être root"
[ "$($PB -c 'Print :KeepAlive:SuccessfulExit' "$PLIST")" = "false" ] || fail "KeepAlive SuccessfulExit"
[ "$($PB -c 'Print :ThrottleInterval' "$PLIST")" = "30" ] || fail "ThrottleInterval"
[ "$($PB -c 'Print :ProgramArguments:0' "$PLIST")" = "/usr/local/bin/matter-home-server" ] || fail "binaire"
[ "$($PB -c 'Print :ProgramArguments:1' "$PLIST")" = "--data-dir" ] || fail "--data-dir"
[ "$($PB -c 'Print :ProgramArguments:2' "$PLIST")" = "$DATA" ] || fail "dossier de données du plist"
[ "$($PB -c 'Print :ProgramArguments:3' "$PLIST")" = "--require-identity" ] || fail "--require-identity"
[ "$($PB -c 'Print :ProgramArguments:4' "$PLIST")" = "--log-file" ] || fail "--log-file"
[ "$(mode "$LOGS")" = "700" ] || fail "droits du dossier des journaux"
[ "$(mode "$LOGS/server.log")" = "600" ] || fail "droits de server.log"
[ "$(mode "$LOGS/launchd.log")" = "600" ] || fail "droits de launchd.log"
[ ! -e "$DATA" ] || fail "le dossier de données ne doit jamais être créé à vide"
ok "binaire, plist (UserName, KeepAlive, require-identity), journaux restreints ; données non créées"

echo "Le service avec ses vrais arguments, sur un dossier factice, refuse de démarrer sans identité"
set +e
"$INSTALLED" --data-dir "$DATA" --require-identity --listen 127.0.0.1:0 --log-file "$LOGS/server.log"
CODE=$?
set -e
[ "$CODE" = "78" ] || fail "code de sortie $CODE au lieu de 78"
grep -q "aucune identité Matter" "$LOGS/server.log" || fail "message absent du journal"
[ ! -e "$DATA/controller.json" ] || fail "une identité a été créée"
ok "code 78, rien créé"

echo "Réinstallation avec une identité (mise à jour)"
"$INSTALLED" init --data-dir "$DATA" >/dev/null
OUT="$("$PKG/install.sh" --prefixe "$PREFIX" --oui --demarrer 2>&1)"
contains "$OUT" "identité Matter : trouvée"
contains "$OUT" "[préfixe : non exécuté] sudo launchctl bootout system/dev.matter-home-desktop.server"
contains "$OUT" "[préfixe : non exécuté] sudo launchctl bootstrap system /Library/LaunchDaemons/dev.matter-home-desktop.server.plist"
ok "mise à jour : bootout puis bootstrap affichés, non exécutés"

echo "uninstall.sh --a-blanc : ne supprime rien"
BEFORE="$(count_files "$PREFIX")"
OUT="$("$PKG/uninstall.sh" --a-blanc --prefixe "$PREFIX" --supprimer-donnees 2>&1)"
contains "$OUT" "[à blanc]"
contains "$OUT" "rm -rf"
[ "$(count_files "$PREFIX")" = "$BEFORE" ] || fail "l'essai à blanc a supprimé des fichiers"
ok "rien supprimé"

echo "uninstall.sh : retire le service et le binaire, garde les données et les journaux"
"$PKG/uninstall.sh" --prefixe "$PREFIX" --oui >/dev/null
[ ! -e "$INSTALLED" ] || fail "binaire encore là"
[ ! -e "$PLIST" ] || fail "service encore là"
[ -f "$DATA/controller.json" ] || fail "les données ne doivent pas être supprimées"
[ -f "$LOGS/server.log" ] || fail "les journaux ne doivent pas être supprimés"
ok "données et journaux conservés"

echo "uninstall.sh --supprimer-donnees sans terminal : refusé, données conservées"
if "$PKG/uninstall.sh" --prefixe "$PREFIX" --oui --supprimer-donnees < /dev/null >/dev/null 2>&1; then
  fail "la suppression des données demande une confirmation tapée"
fi
[ -f "$DATA/controller.json" ] || fail "les données ont été supprimées"
ok "refusé"

echo
echo "Tout est bon (aucune commande système lancée)."
