#!/bin/bash
#
# Désinstalle le service Matter Home : arrête le service, retire le fichier de service et le
# binaire. Vos DONNÉES (identité Matter, appareils, plan, règles, historique, compte) et vos
# journaux ne sont JAMAIS supprimés, sauf si vous le demandez avec l'option --supprimer-donnees,
# qui est un geste à part, avec sa propre confirmation.
#
# À lancer en tant que VOUS-MÊME (jamais avec « sudo ») : le script demande sudo lui-même pour
# les gestes qui en ont besoin.
#
#     ./uninstall.sh
#
# Options :
#   --a-blanc            n'écrit rien et n'exécute aucune commande système : affiche tout ce
#                        qui serait fait
#   --prefixe DOSSIER    désinstalle dans ce dossier (essais) : aucune commande système n'est
#                        lancée. HOME doit lui aussi être dans ce dossier
#   --donnees DOSSIER    dossier de données (défaut : celui lu dans le fichier de service, sinon
#                        celui de l'utilisateur)
#   --supprimer-donnees  EN PLUS : efface définitivement le dossier de données (et son verrou).
#                        Irréversible : exporter d'abord vos données (menu Fichier du client)
#   --oui                ne demande pas la confirmation de la désinstallation (JAMAIS celle de
#                        --supprimer-donnees)
#   --aide               affiche cette aide

set -euo pipefail

LABEL="dev.matter-home-desktop.server"
BIN_NAME="matter-home-server"

DRY=0
[ "${MH_DRY_RUN:-0}" = "1" ] && DRY=1
PREFIX="${MH_PREFIX:-}"
DATA_DIR=""
ASSUME_YES=0
WIPE=0

say() { printf '%s\n' "$*"; }
title() { printf '\n\033[1m%s\033[0m\n' "$*"; }
warn() { printf '\033[33m! %s\033[0m\n' "$*" >&2; }
die() { printf '\033[31mErreur : %s\033[0m\n' "$*" >&2; exit 1; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//' -e '1d'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --a-blanc) DRY=1 ;;
    --prefixe) [ $# -ge 2 ] || die "--prefixe attend un dossier"; PREFIX="$2"; shift ;;
    --donnees) [ $# -ge 2 ] || die "--donnees attend un dossier"; DATA_DIR="$2"; shift ;;
    --supprimer-donnees) WIPE=1 ;;
    --oui) ASSUME_YES=1 ;;
    --aide|-h|--help) usage; exit 0 ;;
    *) die "option inconnue : $1 (voir --aide)" ;;
  esac
  shift
done

[ "$(uname -s)" = "Darwin" ] || die "ce script désinstalle un service macOS (launchd) ; ce système n'est pas macOS."
[ "$(id -u)" -ne 0 ] || die "ne lancez pas ce script avec sudo ni en root : lancez simplement ./uninstall.sh ; il demandera sudo lui-même pour les gestes qui en ont besoin."

USER_HOME="${HOME:?HOME doit être défini}"
if [ -n "$PREFIX" ]; then
  case "$PREFIX" in /*) ;; *) die "--prefixe doit être un chemin absolu" ;; esac
  PREFIX="${PREFIX%/}"
  case "$USER_HOME/" in
    "$PREFIX"/*) ;;
    *) die "avec --prefixe, HOME doit être dans ce dossier (essai : HOME=\"$PREFIX/home\" ./uninstall.sh --prefixe \"$PREFIX\")." ;;
  esac
fi

BIN_FINAL="/usr/local/bin/$BIN_NAME"
PLIST_FINAL="/Library/LaunchDaemons/$LABEL.plist"
BIN_DEST="$PREFIX$BIN_FINAL"
PLIST_DEST="$PREFIX$PLIST_FINAL"
LOG_DIR="$USER_HOME/Library/Logs/MatterHomeServer"

fmt() {
  local out="" a
  for a in "$@"; do out="$out $(printf '%q' "$a")"; done
  printf '%s' "${out# }"
}

fs_step() {
  if [ "$DRY" = "1" ]; then
    say "  [à blanc] sudo $(fmt "$@")"
  elif [ -n "$PREFIX" ]; then
    say "  \$ $(fmt "$@")"
    "$@"
  else
    say "  \$ sudo $(fmt "$@")"
    sudo "$@"
  fi
}

svc_step() {
  if [ "$DRY" = "1" ]; then
    say "  [à blanc] sudo $(fmt "$@")"
  elif [ -n "$PREFIX" ]; then
    say "  [préfixe : non exécuté] sudo $(fmt "$@")"
  else
    say "  \$ sudo $(fmt "$@")"
    sudo "$@"
  fi
}

user_step() {
  if [ "$DRY" = "1" ]; then
    say "  [à blanc] $(fmt "$@")"
  else
    say "  \$ $(fmt "$@")"
    "$@"
  fi
}

ask() {
  [ "$ASSUME_YES" = "1" ] && return 0
  [ "$DRY" = "1" ] && return 0
  [ -t 0 ] || die "pas de terminal pour poser la question : relancez avec --oui pour confirmer d'avance."
  local answer
  printf '%s [o/N] ' "$1"
  read -r answer
  case "$answer" in o|O|oui|Oui|OUI|y|Y|yes) return 0 ;; *) return 1 ;; esac
}

# Le dossier de données : celui du fichier de service installé (argument après --data-dir).
if [ -z "$DATA_DIR" ]; then
  if [ -f "$PLIST_DEST" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:1' "$PLIST_DEST" 2>/dev/null || true)" = "--data-dir" ]; then
    DATA_DIR="$(/usr/libexec/PlistBuddy -c 'Print :ProgramArguments:2' "$PLIST_DEST" 2>/dev/null || true)"
  fi
  [ -n "$DATA_DIR" ] || DATA_DIR="$USER_HOME/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop"
fi
case "$DATA_DIR" in /*) ;; *) die "le dossier de données doit être un chemin absolu : $DATA_DIR" ;; esac
DATA_DIR="${DATA_DIR%/}"

if [ -n "$PREFIX" ]; then
  case "$DATA_DIR/" in
    "$PREFIX"/*) ;;
    *) die "avec --prefixe, le dossier de données doit être dans ce dossier ($DATA_DIR)." ;;
  esac
fi

title "Matter Home — désinstallation du serveur"
[ "$DRY" = "1" ] && say "(mode à blanc : rien n'est supprimé, aucune commande système n'est lancée)"
[ -n "$PREFIX" ] && say "(préfixe d'essai : $PREFIX — aucune commande système n'est lancée)"
say "  service    : $PLIST_FINAL"
say "  binaire    : $BIN_FINAL"
say "  données    : $DATA_DIR — CONSERVÉES"
say "  journaux   : $LOG_DIR — conservés"
if [ "$WIPE" = "1" ]; then
  warn "--supprimer-donnees : le dossier de données sera AUSSI effacé, définitivement."
fi
ask "Désinstaller le service ?" || { say "Désinstallation annulée : rien n'a été modifié."; exit 1; }

title "1. Arrêt du service"
svc_step launchctl bootout "system/$LABEL" || true

title "2. Retrait du service et du binaire"
for f in "$PLIST_DEST" "$BIN_DEST"; do
  if [ -e "$f" ] || [ "$DRY" = "1" ]; then
    fs_step rm -f "$f"
  else
    say "  déjà absent : $f"
  fi
done

if [ "$WIPE" = "1" ]; then
  title "3. Suppression des données (demandée)"
  # Garde-fous : un vrai dossier de données de ce serveur, jamais « / » ni votre dossier personnel.
  case "$DATA_DIR" in
    "/"|"$USER_HOME"|"$USER_HOME/Library"|"$USER_HOME/Library/Application Support") die "refus de supprimer $DATA_DIR : ce n'est pas un dossier de données du serveur." ;;
  esac
  if [ "$DRY" = "0" ] && [ ! -e "$DATA_DIR" ]; then
    say "  $DATA_DIR n'existe pas : rien à supprimer."
  else
    if [ "$DRY" = "0" ] && [ ! -f "$DATA_DIR/controller.json" ] && [ ! -d "$DATA_DIR/serveur" ]; then
      die "$DATA_DIR ne ressemble pas à un dossier de données du serveur (ni controller.json ni serveur/) : rien n'est supprimé."
    fi
    say "  Ce dossier contient l'identité Matter de votre maison. Sans sauvegarde (.matterhome),"
    say "  vos appareils devront être réappairés un par un. Cette suppression est IRRÉVERSIBLE."
    if [ "$DRY" = "0" ]; then
      [ -t 0 ] || die "pas de terminal : --supprimer-donnees demande une confirmation tapée, même avec --oui."
      printf 'Pour confirmer, tapez SUPPRIMER : '
      read -r confirmation
      [ "$confirmation" = "SUPPRIMER" ] || { say "Suppression des données annulée : le dossier est conservé."; exit 1; }
    fi
    user_step rm -rf "$DATA_DIR"
    user_step rm -f "$DATA_DIR.lock"
    say "  Les dossiers voisins « …avant-import-… » et « …import-abandonne-… » ne sont pas touchés :"
    say "  supprimez-les vous-même si vous n'en avez plus besoin."
  fi
fi

title "Terminé"
if [ "$WIPE" = "0" ]; then
  say "  Vos données sont intactes : $DATA_DIR"
  say "  Pour les supprimer aussi (irréversible) : ./uninstall.sh --supprimer-donnees"
fi
say "  Les journaux restent dans $LOG_DIR (à supprimer à la main si vous le souhaitez)."
