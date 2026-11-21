#!/usr/bin/env bash
#
# Essaie install.sh et uninstall.sh de l'archive du serveur sans toucher au Mac : tout se passe
# dans un dossier temporaire (préfixe factice et HOME factice), et AUCUNE commande système
# (sudo, launchctl, pmset, pare-feu) n'est lancée.
#
#   scripts/test-server-package.sh [dist/ostal-server-<version>-macos.tar.gz]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="${1:-}"
if [ -z "$ARCHIVE" ]; then
  set -- "$ROOT"/dist/ostal-server-*-macos.tar.gz
  ARCHIVE="$1"
fi
[ -f "$ARCHIVE" ] || { echo "archive introuvable : $ARCHIVE" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/mh-server-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
REAL_HOME="$HOME"

mkdir "$WORK/archive"
tar -xzf "$ARCHIVE" -C "$WORK/archive"
PKG="$(echo "$WORK"/archive/ostal-server-*-macos)"
BIN="$PKG/ostal-server"

PREFIX="$WORK/racine"
# Jamais le vrai pgrep : l'état réel du Mac ne doit pas décider du résultat de l'essai.
export MH_PGREP=/usr/bin/false
export HOME="$PREFIX/home"
mkdir -p "$HOME"
DATA="$HOME/Library/Application Support/dev.ostal.OstalServer"
PLIST="$PREFIX/Library/LaunchDaemons/dev.ostal.server-daemon.plist"
INSTALLED="$PREFIX/usr/local/bin/ostal-server"
LOGS="$HOME/Library/Logs/OstalServer"

fail() { echo "ÉCHEC : $*" >&2; exit 1; }
ok() { echo "  ok : $*"; }
contains() { grep -qF -- "$2" <<< "$1" || fail "« $2 » absent de la sortie : $1"; }
mode() { stat -f '%Lp' "$1"; }
count_files() { find "$1" -type f | wc -l | tr -d ' '; }

echo "Contenu de l'archive"
for f in ostal-server install.sh uninstall.sh dev.ostal.server-daemon.plist.template LISEZMOI.txt VERSION; do
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
contains "$OUT" "socketfilterfw --add /usr/local/bin/ostal-server"
contains "$OUT" "AUCUNE"
[ "$(count_files "$PREFIX")" = "0" ] || fail "l'essai à blanc a écrit des fichiers : $(find "$PREFIX" -type f)"
[ ! -e "$DATA" ] || fail "le dossier de données ne doit jamais être créé à vide"
ok "rien d'écrit, commandes affichées"

echo "install.sh avec un HOME hors du préfixe : refusé avant toute action"
if HOME="$REAL_HOME" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" >/dev/null 2>&1; then
  fail "un HOME hors du préfixe doit être refusé"
fi
ok "refusé"

echo "Données de Matter Home (ancien nom) sur ce Mac : proposées, jamais « init »"
OLD_DATA="$HOME/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop"
mkdir -p "$OLD_DATA"
echo '{"factice":true}' > "$OLD_DATA/controller.json"
# Sans --donnees : arrêt avant d'écrire quoi que ce soit, à blanc comme pour de vrai.
for MODE in --a-blanc --oui; do
  if OUT="$("$PKG/install.sh" "$MODE" --prefixe "$PREFIX" 2>&1)"; then
    fail "$MODE : l'installation sur un dossier vide doit être refusée"
  fi
  contains "$OUT" "vos données de « Matter Home »"
  contains "$OUT" "--donnees \"$OLD_DATA\""
  contains "$OUT" "Ne lancez PAS"
  case "$OUT" in *"installation neuve sans sauvegarde"*) fail "« init » ne doit pas être conseillé" ;; esac
  [ "$(count_files "$PREFIX")" = "1" ] || fail "$MODE : des fichiers ont été écrits : $(find "$PREFIX" -type f)"
done
# La commande conseillée : le service est installé sur l'ancien dossier, tel quel.
OUT="$("$PKG/install.sh" --a-blanc --prefixe "$PREFIX" --donnees "$OLD_DATA" 2>&1)"
contains "$OUT" "identité Matter : trouvée"
case "$OUT" in *"Ne lancez PAS"*) fail "pas d'avertissement quand c'est déjà ce dossier" ;; esac
"$PKG/install.sh" --oui --prefixe "$PREFIX" --donnees "$OLD_DATA" >/dev/null 2>&1 || fail "--donnees vers l'ancien dossier"
[ "$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:2' "$PLIST")" = "$OLD_DATA" ] || fail "dossier du plist"
[ "$(cat "$OLD_DATA/controller.json")" = '{"factice":true}' ] || fail "l'ancienne identité a changé"
# Un service déjà installé sur un dossier vide : il faut le désinstaller d'abord.
"$PKG/uninstall.sh" --prefixe "$PREFIX" --oui >/dev/null
"$PKG/install.sh" --oui --prefixe "$PREFIX" --donnees "$DATA" >/dev/null 2>&1 || fail "service sur le dossier vide"
if OUT="$("$PKG/install.sh" --oui --prefixe "$PREFIX" 2>&1)"; then
  fail "un service sur un dossier vide alors que Matter Home a des données : refus attendu"
fi
contains "$OUT" "./uninstall.sh"
"$PKG/uninstall.sh" --prefixe "$PREFIX" --oui >/dev/null
rm -rf "$OLD_DATA" "$HOME/Library/Logs"
[ "$(count_files "$PREFIX")" = "0" ] || fail "restes : $(find "$PREFIX" -type f)"
ok "ancien dossier proposé, rien écrit sans lui, « init » déconseillé"

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
[ "$($PB -c 'Print :ProgramArguments:0' "$PLIST")" = "/usr/local/bin/ostal-server" ] || fail "binaire"
[ "$($PB -c 'Print :ProgramArguments:1' "$PLIST")" = "--data-dir" ] || fail "--data-dir"
[ "$($PB -c 'Print :ProgramArguments:2' "$PLIST")" = "$DATA" ] || fail "dossier de données du plist"
[ "$($PB -c 'Print :ProgramArguments:3' "$PLIST")" = "--log-file" ] || fail "--log-file"
[ "$(mode "$LOGS")" = "700" ] || fail "droits du dossier des journaux"
[ "$(mode "$LOGS/server.log")" = "600" ] || fail "droits de server.log"
[ "$(mode "$LOGS/launchd.log")" = "600" ] || fail "droits de launchd.log"
[ ! -e "$DATA" ] || fail "le dossier de données ne doit jamais être créé à vide"
ok "binaire, plist (UserName, KeepAlive), journaux restreints ; données non créées"

echo "Le service avec ses vrais arguments, sur un dossier factice, refuse de démarrer sans identité"
set +e
"$INSTALLED" --data-dir "$DATA" --listen 127.0.0.1:0 --log-file "$LOGS/server.log"
CODE=$?
set -e
[ "$CODE" = "78" ] || fail "code de sortie $CODE au lieu de 78"
grep -q "aucune identité Matter" "$LOGS/server.log" || fail "message absent du journal"
[ ! -e "$DATA/controller.json" ] || fail "une identité a été créée"
ok "code 78, rien créé"

echo "Application ouverte : refusée (faux pgrep)"
FAKE_PGREP="$WORK/pgrep-ouvert"
printf '#!/bin/sh\necho 4242\nexit 0\n' > "$FAKE_PGREP"
chmod +x "$FAKE_PGREP"
if OUT="$(MH_PGREP="$FAKE_PGREP" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" 2>&1)"; then
  fail "l'application ouverte doit faire refuser l'installation"
fi
contains "$OUT" "v0.2.1"
contains "$OUT" "QUITTÉE"
MH_PGREP="$FAKE_PGREP" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" --ignorer-application-ouverte >/dev/null 2>&1 \
  || fail "--ignorer-application-ouverte"
ok "refus clair, contournement explicite"

echo "Seul l'ancien programme (matter-home-desktop) fait refuser, jamais le client « ostal »"
FAKE_PGREP_OLD="$WORK/pgrep-ancien"
printf '#!/bin/sh\n[ "$1" = "-x" ] && [ "$2" = "matter-home-desktop" ]\n' > "$FAKE_PGREP_OLD"
FAKE_PGREP_CLIENT="$WORK/pgrep-client"
printf '#!/bin/sh\n[ "$1" = "-x" ] && [ "$2" = "ostal" ]\n' > "$FAKE_PGREP_CLIENT"
chmod +x "$FAKE_PGREP_OLD" "$FAKE_PGREP_CLIENT"
if OUT="$(MH_PGREP="$FAKE_PGREP_OLD" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" 2>&1)"; then
  fail "l'ancienne application ouverte doit faire refuser l'installation"
fi
contains "$OUT" "« Matter Home Desktop » est ouverte"
MH_PGREP="$FAKE_PGREP_CLIENT" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" >/dev/null 2>&1 \
  || fail "le client Ostal ouvert ne doit pas bloquer l'installation"
ok "garde v0.2.1 sur l'ancien nom de programme"

echo "Mise à jour : le dossier du service installé est repris, jamais un autre"
OTHER_HOME="$PREFIX/autre-home"
mkdir -p "$OTHER_HOME"
OUT="$(HOME="$OTHER_HOME" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" 2>&1)"
contains "$OUT" "$DATA"
contains "$OUT" "repris du service déjà installé"
case "$OUT" in *"$OTHER_HOME/Library/Application Support"*) fail "le service aurait changé de dossier" ;; esac
if HOME="$OTHER_HOME" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" --donnees "$OTHER_HOME/ailleurs" >/dev/null 2>&1; then
  fail "--donnees vers un autre dossier doit être refusé pour un service installé"
fi
HOME="$OTHER_HOME" "$PKG/install.sh" --a-blanc --prefixe "$PREFIX" --donnees "$DATA" >/dev/null 2>&1 \
  || fail "--donnees identique à celui du service doit passer"
ok "dossier du service conservé, changement refusé"

echo "Réinstallation avec une identité (mise à jour)"
"$INSTALLED" init --data-dir "$DATA" >/dev/null
OUT="$("$PKG/install.sh" --prefixe "$PREFIX" --oui --demarrer 2>&1)"
contains "$OUT" "identité Matter : trouvée"
contains "$OUT" "[préfixe : non exécuté] sudo launchctl bootout system/dev.ostal.server-daemon"
contains "$OUT" "[préfixe : non exécuté] sudo launchctl bootstrap system /Library/LaunchDaemons/dev.ostal.server-daemon.plist"
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
