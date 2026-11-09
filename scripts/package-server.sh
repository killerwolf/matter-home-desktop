#!/usr/bin/env bash
#
# Construit l'archive du serveur : dist/matter-home-server-<version>-macos.tar.gz
#
#   scripts/package-server.sh
#
# Contenu : le binaire (universel arm64 + x86_64 quand les deux cibles sont installées,
# `rustup target add aarch64-apple-darwin x86_64-apple-darwin` ; sinon celui de ce Mac, ce que
# le script dit et écrit dans VERSION), install.sh, uninstall.sh, le gabarit du plist, une doc.
# Le binaire est signé « ad hoc ».
#
# Version minimale de macOS : 10.15 pour la tranche x86_64 (un ancien Mac), 11.0 pour arm64
# (le plus ancien macOS qui tourne sur Apple Silicon). Modifiables : MH_MIN_MACOS_X86_64,
# MH_MIN_MACOS_ARM64. Le script échoue si une dépendance exige plus récent que demandé.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
PACKAGE="matter-home-server"
BIN="matter-home-server"
SRC="$ROOT/scripts/server"

VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
MIN_X86_64="${MH_MIN_MACOS_X86_64:-10.15}"
MIN_ARM64="${MH_MIN_MACOS_ARM64:-11.0}"
NAME="matter-home-server-$VERSION-macos"
STAGE="$DIST/$NAME"
ARCHIVE="$DIST/$NAME.tar.gz"

log() { printf '\033[1m▸\033[0m %s\n' "$*"; }
min_for() { case "$1" in aarch64-apple-darwin) echo "$MIN_ARM64" ;; *) echo "$MIN_X86_64" ;; esac; }
# 10.15 <= 11.0 : compare deux versions « a.b » (ou « a.b.c »).
version_le() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | head -1)" = "$1" ]; }

# ---- compilation -----------------------------------------------------------

INSTALLED="$(rustup target list --installed)"
TARGETS=()
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  if grep -qx "$target" <<< "$INSTALLED"; then
    TARGETS+=("$target")
  fi
done
[ ${#TARGETS[@]} -gt 0 ] || { echo "aucune cible macOS installée (rustup target list --installed)" >&2; exit 1; }

BINARIES=()
for target in "${TARGETS[@]}"; do
  MIN="$(min_for "$target")"
  log "Compilation $target (macOS $MIN minimum)"
  (cd "$ROOT" && MACOSX_DEPLOYMENT_TARGET="$MIN" cargo build --release -p "$PACKAGE" --target "$target")
  BINARIES+=("$ROOT/target/$target/release/$BIN")
done
if [ ${#TARGETS[@]} -lt 2 ]; then
  echo "  note : une seule cible installée (${TARGETS[*]}) : le binaire n'est pas universel." >&2
  echo "         rustup target add aarch64-apple-darwin x86_64-apple-darwin  # pour livrer les deux" >&2
fi

# ---- assemblage ------------------------------------------------------------

log "Assemblage de $NAME"
rm -rf "$STAGE" "$ARCHIVE" "$ARCHIVE.sha256"
mkdir -p "$STAGE"
lipo -create -output "$STAGE/$BIN" "${BINARIES[@]}"
chmod 755 "$STAGE/$BIN"
cp "$SRC/install.sh" "$SRC/uninstall.sh" "$STAGE/"
chmod 755 "$STAGE/install.sh" "$STAGE/uninstall.sh"
cp "$SRC/dev.matter-home-desktop.server.plist.template" "$SRC/LISEZMOI.txt" "$STAGE/"
chmod 644 "$STAGE/dev.matter-home-desktop.server.plist.template" "$STAGE/LISEZMOI.txt"

# Signature ad hoc : pas un certificat Developer ID (payant), mais une signature valide et
# stable plutôt qu'aucune. Elle ne dispense pas de l'autorisation du pare-feu (install.sh).
log "Signature (ad hoc)"
codesign --force --sign - --timestamp=none "$STAGE/$BIN"
codesign --verify --strict "$STAGE/$BIN"

# ---- vérifications ---------------------------------------------------------

ARCHS="$(lipo -archs "$STAGE/$BIN")"
printf 'matter-home-server %s (%s)\n' "$VERSION" "$ARCHS" > "$STAGE/VERSION"

# La version minimale de chaque tranche, lue dans le binaire (LC_BUILD_VERSION ou
# LC_VERSION_MIN_MACOSX) : une dépendance qui exigerait plus récent doit faire échouer le build.
for arch in $ARCHS; do
  want="$MIN_X86_64"
  [ "$arch" = "arm64" ] && want="$MIN_ARM64"
  got="$(otool -arch "$arch" -l "$STAGE/$BIN" | awk '
    /LC_BUILD_VERSION/ { f = 1 }
    /LC_VERSION_MIN_MACOSX/ { g = 1 }
    f && $1 == "minos" { print $2; exit }
    g && $1 == "version" { print $2; exit }')"
  echo "  $arch : macOS $got minimum (visé : $want)"
  if [ -z "$got" ] || ! version_le "$got" "$want"; then
    echo "erreur : la tranche $arch exige macOS $got, plus récent que $want" >&2
    exit 1
  fi
done

# Le binaire annonce la version du workspace (il s'exécute ici si la tranche de ce Mac existe).
if ANNOUNCED="$("$STAGE/$BIN" --version 2>/dev/null)"; then
  echo "  $ANNOUNCED"
  [ "$ANNOUNCED" = "matter-home-server $VERSION" ] ||
    { echo "erreur : le binaire annonce « $ANNOUNCED » au lieu de la version $VERSION" >&2; exit 1; }
else
  echo "  note : ce Mac ne peut pas exécuter le binaire ; version non vérifiée." >&2
fi

# ---- archive ---------------------------------------------------------------

log "Archive $(basename "$ARCHIVE")"
# COPYFILE_DISABLE : pas de fichiers « ._* » (attributs étendus) dans l'archive.
COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" -C "$DIST" "$NAME"
(cd "$DIST" && shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256")
rm -rf "$STAGE"
echo "  $ARCHIVE  ($(du -sh "$ARCHIVE" | cut -f1), $ARCHS)"
