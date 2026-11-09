#!/bin/bash
#
# Installe le serveur Matter Home comme service launchd (LaunchDaemon) : il démarre avec le Mac,
# sans qu'aucune session soit ouverte, et il est relancé s'il plante.
#
# À lancer depuis le dossier de l'archive, en tant que VOUS-MÊME (jamais avec « sudo ») :
#
#     ./install.sh
#
# Le script vous montre ce qu'il va faire, demande votre accord, puis ne demande le mot de passe
# de votre session (sudo) que pour les gestes qui en ont besoin : copier le binaire dans
# /usr/local/bin, écrire le fichier de service dans /Library/LaunchDaemons, charger le service.
#
# Il ne modifie JAMAIS tout seul la veille du Mac ni le pare-feu : il vérifie ce qu'il peut sans
# droits, puis AFFICHE les commandes à lancer vous-même.
#
# Options :
#   --a-blanc          n'écrit rien et n'exécute aucune commande système : affiche tout ce qui
#                      serait fait (pour essayer sans risque)
#   --prefixe DOSSIER  installe dans ce dossier au lieu de la racine du Mac (essais) : aucune
#                      commande système n'est lancée. Avec cette option, HOME doit lui aussi
#                      être dans ce dossier
#   --donnees DOSSIER  dossier de données du serveur (défaut : celui de l'utilisateur, le même
#                      que celui de l'import, ~/Library/Application Support/...)
#   --oui              ne demande aucune confirmation
#   --demarrer         démarre le service à la fin sans le demander (s'il peut démarrer)
#   --ne-pas-demarrer  ne démarre pas le service et ne pose pas la question
#   --aide             affiche cette aide
#
# Variables équivalentes : MH_DRY_RUN=1 (--a-blanc), MH_PREFIX=DOSSIER (--prefixe).

set -euo pipefail

LABEL="dev.matter-home-desktop.server"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_NAME="matter-home-server"
TEMPLATE="$HERE/$LABEL.plist.template"

DRY=0
[ "${MH_DRY_RUN:-0}" = "1" ] && DRY=1
PREFIX="${MH_PREFIX:-}"
DATA_DIR=""
ASSUME_YES=0
START="ask"

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
    --oui) ASSUME_YES=1 ;;
    --demarrer) START="yes" ;;
    --ne-pas-demarrer) START="no" ;;
    --aide|-h|--help) usage; exit 0 ;;
    *) die "option inconnue : $1 (voir --aide)" ;;
  esac
  shift
done

# ---- garde-fous ------------------------------------------------------------

[ "$(uname -s)" = "Darwin" ] || die "ce script installe un service macOS (launchd) ; ce système n'est pas macOS."
[ "$(id -u)" -ne 0 ] || die "ne lancez pas ce script avec sudo ni en root : le service doit tourner sous VOTRE compte (jamais root). Lancez simplement ./install.sh ; il demandera sudo lui-même pour les gestes qui en ont besoin."
[ -f "$HERE/$BIN_NAME" ] || die "le binaire $BIN_NAME est introuvable à côté de ce script ($HERE). Lancez install.sh depuis le dossier de l'archive décompressée."
[ -f "$TEMPLATE" ] || die "le gabarit $LABEL.plist.template est introuvable à côté de ce script."

# Le dossier du serveur doit être sur un chemin absolu, sans retour à la ligne.
RUN_USER="$(id -un)"
USER_HOME="${HOME:?HOME doit être défini}"

if [ -n "$PREFIX" ]; then
  case "$PREFIX" in /*) ;; *) die "--prefixe doit être un chemin absolu" ;; esac
  PREFIX="${PREFIX%/}"
  case "$USER_HOME/" in
    "$PREFIX"/*) ;;
    *) die "avec --prefixe, HOME doit être dans ce dossier (essai : HOME=\"$PREFIX/home\" ./install.sh --prefixe \"$PREFIX\"), pour qu'aucun fichier ne soit écrit dans votre vrai dossier utilisateur." ;;
  esac
fi

BIN_FINAL="/usr/local/bin/$BIN_NAME"
PLIST_FINAL="/Library/LaunchDaemons/$LABEL.plist"
BIN_DEST="$PREFIX$BIN_FINAL"
PLIST_DEST="$PREFIX$PLIST_FINAL"
[ -n "$DATA_DIR" ] || DATA_DIR="$USER_HOME/Library/Application Support/dev.matter-home-desktop.MatterHomeDesktop"
case "$DATA_DIR" in /*) ;; *) die "--donnees doit être un chemin absolu" ;; esac
DATA_DIR="${DATA_DIR%/}"
LOG_DIR="$USER_HOME/Library/Logs/MatterHomeServer"
LOG_FILE="$LOG_DIR/server.log"
FALLBACK_LOG="$LOG_DIR/launchd.log"

REAL=0
[ "$DRY" = "0" ] && [ -z "$PREFIX" ] && REAL=1

# ---- exécution des gestes --------------------------------------------------

fmt() { # une commande avec ses arguments protégés, telle qu'on pourrait la retaper
  local out="" a
  for a in "$@"; do out="$out $(printf '%q' "$a")"; done
  printf '%s' "${out# }"
}

# Geste sur des fichiers du système, qui demande sudo sur un vrai Mac.
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

# Geste sur le service (launchctl…) : jamais lancé à blanc ni dans un préfixe.
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

# Geste sur vos propres fichiers (aucun sudo).
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

xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }
sedrep() { printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'; }

# ---- 1. le binaire ---------------------------------------------------------

title "Matter Home — installation du serveur"
[ "$DRY" = "1" ] && say "(mode à blanc : rien n'est écrit, aucune commande système n'est lancée)"
[ -n "$PREFIX" ] && say "(préfixe d'essai : $PREFIX — aucune commande système n'est lancée)"

title "1. Le binaire"
# Un fichier téléchargé porte une « quarantaine » : sans l'enlever, macOS refuse de le lancer.
user_step xattr -dr com.apple.quarantine "$HERE" 2>/dev/null || true
if ! VERSION_LINE="$("$HERE/$BIN_NAME" --version 2>&1)"; then
  die "le binaire ne se lance pas sur ce Mac : $VERSION_LINE
  Cause possible : archive d'une autre architecture, ou macOS trop ancien. Vérifiez avec : lipo -archs \"$HERE/$BIN_NAME\" ; uname -m"
fi
say "  $VERSION_LINE ($(lipo -archs "$HERE/$BIN_NAME" 2>/dev/null || echo '?'))"
if codesign --verify "$HERE/$BIN_NAME" 2>/dev/null; then
  say "  signature (ad hoc) valide"
else
  warn "la signature du binaire n'est pas valide : il est peut-être abîmé. Retéléchargez l'archive."
fi

# ---- 2. les données --------------------------------------------------------

title "2. Le dossier de données"
say "  $DATA_DIR"
HAS_IDENTITY=0
HAS_ACCOUNT=0
if [ -f "$DATA_DIR/controller.json" ]; then
  HAS_IDENTITY=1
  say "  identité Matter : trouvée (vos appareils appairés seront reconnus)"
else
  say "  identité Matter : AUCUNE dans ce dossier."
  if [ -d "$DATA_DIR" ]; then
    say "  Le dossier existe mais n'a pas d'identité."
  else
    say "  Le dossier n'existe pas encore : ce script ne le crée pas à vide."
  fi
  say "  Le service est lancé avec --require-identity : il refusera de démarrer (code 78) tant"
  say "  que vous n'aurez pas, avec ce même compte utilisateur :"
  say "    - repris vos données de la v0.2.1 :  $BIN_NAME import FICHIER.matterhome"
  say "    - ou, installation neuve sans sauvegarde :  $BIN_NAME init"
fi
[ -f "$DATA_DIR/serveur/compte.json" ] && HAS_ACCOUNT=1
if [ "$HAS_ACCOUNT" = "1" ]; then
  say "  compte du serveur : existe"
else
  say "  compte du serveur : à créer avec  $BIN_NAME create-account --username NOM"
fi
case "$DATA_DIR" in
  "$USER_HOME"/*) ;;
  *) warn "ce dossier n'est pas dans votre dossier utilisateur ($USER_HOME) : vérifiez que c'est voulu." ;;
esac

# ---- 3. ce qui va être fait ------------------------------------------------

title "3. Ce que fait l'installation"
say "  utilisateur du service : $RUN_USER (jamais root)"
say "  binaire    : $BIN_FINAL"
say "  service    : $PLIST_FINAL"
say "  journaux   : $LOG_FILE (droits 0600, rotation à 5 Mo) et $FALLBACK_LOG"
say "  écoute     : port 8443 (HTTPS), sur toutes les interfaces"
if [ -f "$PLIST_DEST" ]; then
  say "  Un service est déjà installé : il sera arrêté, remplacé, puis rechargé si vous le souhaitez."
fi
if [ "$REAL" = "1" ]; then
  say "  sudo sera demandé pour : copier le binaire, écrire le service, charger le service."
fi
ask "Continuer ?" || { say "Installation annulée : rien n'a été modifié."; exit 1; }

# ---- 4. le fichier de service ----------------------------------------------

TMP_PLIST="$(mktemp "${TMPDIR:-/tmp}/matter-home-server.plist.XXXXXX")"
trap 'rm -f "$TMP_PLIST"' EXIT
sed \
  -e "s|__LABEL__|$(sedrep "$(xml "$LABEL")")|g" \
  -e "s|__BINARY__|$(sedrep "$(xml "$BIN_FINAL")")|g" \
  -e "s|__DATA_DIR__|$(sedrep "$(xml "$DATA_DIR")")|g" \
  -e "s|__LOG_FILE__|$(sedrep "$(xml "$LOG_FILE")")|g" \
  -e "s|__FALLBACK_LOG__|$(sedrep "$(xml "$FALLBACK_LOG")")|g" \
  -e "s|__USER__|$(sedrep "$(xml "$RUN_USER")")|g" \
  "$TEMPLATE" > "$TMP_PLIST"
if grep -q '__[A-Z_]*__' "$TMP_PLIST"; then
  die "le fichier de service contient encore des champs non remplis : $(grep -o '__[A-Z_]*__' "$TMP_PLIST" | sort -u | tr '\n' ' ')"
fi
title "4. Vérification du fichier de service"
plutil -lint "$TMP_PLIST" >/dev/null || die "le fichier de service est invalide (plutil -lint)."
say "  plutil -lint : OK"

# ---- 5. journaux (votre compte, sans sudo) ---------------------------------

title "5. Journaux"
user_step mkdir -p "$LOG_DIR"
user_step chmod 700 "$LOG_DIR"
for f in "$LOG_FILE" "$FALLBACK_LOG"; do
  user_step touch "$f"
  user_step chmod 600 "$f"
done

# ---- 6. installation (sudo) ------------------------------------------------

title "6. Installation"
if [ -f "$PLIST_DEST" ]; then
  svc_step launchctl bootout "system/$LABEL" || true
fi
fs_step mkdir -p "$(dirname "$BIN_DEST")"
if [ -n "$PREFIX" ]; then
  fs_step install -m 755 "$HERE/$BIN_NAME" "$BIN_DEST"
  fs_step mkdir -p "$(dirname "$PLIST_DEST")"
  fs_step install -m 644 "$TMP_PLIST" "$PLIST_DEST"
else
  fs_step install -m 755 -o root -g wheel "$HERE/$BIN_NAME" "$BIN_DEST"
  fs_step install -m 644 -o root -g wheel "$TMP_PLIST" "$PLIST_DEST"
fi
if [ "$DRY" = "0" ] && [ -f "$PLIST_DEST" ]; then
  plutil -lint "$PLIST_DEST" >/dev/null || die "le fichier de service installé est invalide."
  say "  fichier installé : plutil -lint OK"
fi

# ---- 7. empreinte du certificat --------------------------------------------

title "7. Empreinte du certificat TLS"
SHOW_BIN="$BIN_DEST"
[ -x "$SHOW_BIN" ] || SHOW_BIN="$HERE/$BIN_NAME"
if [ -f "$DATA_DIR/serveur/tls-cert.pem" ]; then
  say "  À comparer avec celle que le client affiche à la première connexion :"
  FP="$("$SHOW_BIN" fingerprint --data-dir "$DATA_DIR" 2>&1 || true)"
  say "  $FP"
else
  say "  Pas encore de certificat : il est créé au premier démarrage du service."
  say "  Affichez ensuite l'empreinte avec :  $BIN_NAME fingerprint"
  say "  (ou lisez-la dans le journal : $LOG_FILE)"
fi

# ---- 8. veille, pare-feu : jamais modifiés ici ------------------------------

title "8. À vérifier vous-même : veille, pare-feu, réseau"
say "  Ce script ne change JAMAIS ces réglages. Voici l'état (quand il se lit sans droits) et les"
say "  commandes à lancer vous-même dans le Terminal."
echo
say "  Veille — le Mac ne doit jamais se mettre en veille :"
if [ "$REAL" = "1" ]; then
  SLEEP_NOW="$(pmset -g 2>/dev/null | awk '$1=="sleep"{print $2; exit}' || true)"
  case "$SLEEP_NOW" in
    0) say "    état : OK (veille système désactivée)" ;;
    "") say "    état : indéterminé (vérifiez avec : pmset -g)" ;;
    *) warn "état : le Mac se met en veille après $SLEEP_NOW min ; réglez-le avec la commande ci-dessous." ;;
  esac
else
  say "    état : non vérifié (à blanc ou préfixe d'essai)"
fi
say "    sudo pmset -a sleep 0          # jamais de veille système"
say "    sudo pmset -a autorestart 1    # redémarre tout seul après une coupure de courant"
say "    sudo pmset -a womp 1           # réveil par le réseau, si votre Mac le permet"
say "    (Mac portable : gardez-le branché et le capot ouvert, ou : sudo pmset -a disablesleep 1)"
echo
say "  Pare-feu macOS — autoriser le serveur à recevoir des connexions :"
if [ "$REAL" = "1" ]; then
  FW="$(/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate 2>/dev/null || true)"
  say "    état : ${FW:-indéterminé}"
else
  say "    état : non vérifié (à blanc ou préfixe d'essai)"
fi
say "    sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add $BIN_FINAL"
say "    sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp $BIN_FINAL"
say "    (un binaire signé « ad hoc » n'affiche pas la fenêtre d'autorisation : sans ces deux"
say "     commandes, un pare-feu activé peut bloquer les connexions du client)"
echo
say "  FileVault :"
if [ "$REAL" = "1" ]; then
  say "    état : $(fdesetup status 2>/dev/null | head -1 || echo indéterminé)"
else
  say "    état : non vérifié (à blanc ou préfixe d'essai)"
fi
say "    Si FileVault est activé, après un redémarrage le service ne démarre qu'une fois le Mac"
say "    déverrouillé (mot de passe à saisir à l'écran) : prévoyez-le après chaque redémarrage."
echo
say "  Réseau : préférez l'Ethernet au Wi-Fi ; au premier démarrage, le service attend que le réseau"
say "  soit prêt. Une adresse fixe (réservation sur la box) ou un nom en .local évite de"
say "  ressaisir l'adresse dans le client."

# ---- 9. démarrage -----------------------------------------------------------

title "9. Démarrage"
CAN_START=0
[ "$HAS_IDENTITY" = "1" ] && CAN_START=1
if [ "$CAN_START" = "0" ]; then
  say "  Le service est installé mais pas démarré : il n'y a pas encore d'identité Matter."
  say "  Importez vos données ou lancez « $BIN_NAME init » (voir ci-dessus), créez le compte, puis :"
  say "    sudo launchctl bootstrap system $PLIST_FINAL"
else
  DO_START=0
  case "$START" in
    yes) DO_START=1 ;;
    no) DO_START=0 ;;
    *) ask "Démarrer le service maintenant ?" && DO_START=1 || DO_START=0 ;;
  esac
  if [ "$DO_START" = "1" ]; then
    svc_step launchctl bootstrap system "$PLIST_FINAL"
    [ "$HAS_ACCOUNT" = "1" ] || warn "aucun compte : personne ne pourra se connecter. Créez-le : $BIN_NAME create-account --username NOM"
  else
    say "  Pour le démarrer plus tard :  sudo launchctl bootstrap system $PLIST_FINAL"
  fi
fi

title "Aide-mémoire"
say "  Statut     : sudo launchctl print system/$LABEL | head -20"
say "  Arrêter    : sudo launchctl bootout system/$LABEL"
say "  Démarrer   : sudo launchctl bootstrap system $PLIST_FINAL"
say "  Ne plus démarrer au lancement du Mac : sudo launchctl disable system/$LABEL"
say "  Journal     : tail -f \"$LOG_FILE\""
say "  Désinstaller : ./uninstall.sh   (vos données ne sont jamais supprimées sans le demander)"
say "  Ne lancez jamais deux serveurs (ni la v0.2.1) avec la même identité Matter."
