#!/usr/bin/env bash
#
# Construit « Matter Home Server.app » (l'application de la barre de menus) et, au besoin, le DMG.
#
#   scripts/bundle-server-app.sh          # dist/Matter Home Server.app
#   scripts/bundle-server-app.sh --dmg    # ...et dist/MatterHomeServer-<version>-<arch>.dmg
#
# Binaire universel (arm64 + x86_64) quand les deux cibles sont installées
# (`rustup target add aarch64-apple-darwin x86_64-apple-darwin`) ; sinon celui de ce Mac, et le
# nom du DMG le dit. Version minimale de macOS : 10.15 pour x86_64 (un ancien Mac), 11.0 pour
# arm64, comme l'archive du serveur (MH_MIN_MACOS_X86_64, MH_MIN_MACOS_ARM64 pour changer). Le
# script échoue si une dépendance exige plus récent. Signature « ad hoc ».

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
NAME="Matter Home Server"
PACKAGE="matter-home-server-app"
BIN="matter-home-server-app"
APP="$DIST/$NAME.app"

VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
BUNDLE_ID="dev.matter-home-desktop.server-app"
MIN_X86_64="${MH_MIN_MACOS_X86_64:-10.15}"
MIN_ARM64="${MH_MIN_MACOS_ARM64:-11.0}"

log() { printf '\033[1m▸\033[0m %s\n' "$*"; }
min_for() { case "$1" in aarch64-apple-darwin) echo "$MIN_ARM64" ;; *) echo "$MIN_X86_64" ;; esac; }
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
  FLAVOUR="$(uname -m)"
else
  FLAVOUR="universal"
fi

# ---- assemblage ------------------------------------------------------------

log "Assemblage de $NAME.app ($VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/$BIN" "${BINARIES[@]}"
cp "$ROOT/assets/icon.icns" "$APP/Contents/Resources/icon.icns"

# LSUIElement : pas d'icône dans le Dock, l'application vit dans la barre de menus.
# NSSupportsAutomaticGraphicsSwitching : sur un portable à deux GPU, macOS peut garder le GPU
# intégré (l'application tourne jour et nuit). NSAppSleepDisabled : pas d'App Nap, qui ralentirait
# les minuteries du serveur quand l'application n'a aucune fenêtre visible.
# NSLocalNetworkUsageDescription et NSBonjourServices : sans eux, macOS bloque la découverte
# mDNS des appareils Matter et l'accès au réseau local.
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>$BIN</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleDevelopmentRegion</key><string>fr</string>
  <key>LSMinimumSystemVersion</key><string>$MIN_X86_64</string>
  <key>LSUIElement</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSAppNapIsDisabled</key><true/>
  <key>NSAppSleepDisabled</key><true/>
  <key>NSHumanReadableCopyright</key><string>Licence MIT</string>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Matter Home Server communique avec vos appareils Matter sur le réseau local pour les appairer, lire leurs mesures et les commander, et il répond aux clients Matter Home de votre réseau.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_matter._tcp</string>
    <string>_matterc._udp</string>
  </array>
</dict>
</plist>
PLIST

log "Signature (ad hoc)"
codesign --force --sign - --timestamp=none "$APP"
codesign --verify --strict "$APP"

# ---- vérifications ---------------------------------------------------------

EXE="$APP/Contents/MacOS/$BIN"
ARCHS="$(lipo -archs "$EXE")"
for arch in $ARCHS; do
  want="$MIN_X86_64"
  [ "$arch" = "arm64" ] && want="$MIN_ARM64"
  got="$(otool -arch "$arch" -l "$EXE" | awk '
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
if ANNOUNCED="$("$EXE" --version 2>/dev/null)"; then
  echo "  $ANNOUNCED"
  [ "$ANNOUNCED" = "matter-home-server-app $VERSION" ] ||
    { echo "erreur : le binaire annonce « $ANNOUNCED » au lieu de la version $VERSION" >&2; exit 1; }
fi
plutil -lint "$APP/Contents/Info.plist" >/dev/null
echo "  $APP  ($(du -sh "$APP" | cut -f1), $ARCHS)"

# ---- DMG -------------------------------------------------------------------

if [ "${1:-}" = "--dmg" ]; then
  DMG="$DIST/MatterHomeServer-$VERSION-$FLAVOUR.dmg"
  log "Construction de $(basename "$DMG")"
  rm -f "$DMG"

  DMGBUILD_VERSION="1.6.7"
  VENV="$ROOT/target/dmgbuild-$DMGBUILD_VERSION"
  if [ ! -x "$VENV/bin/dmgbuild" ]; then
    log "Installation de dmgbuild $DMGBUILD_VERSION"
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install --quiet --disable-pip-version-check "dmgbuild==$DMGBUILD_VERSION"
  fi

  WORK="$(mktemp -d)"
  tiffutil -cathidpicheck "$ROOT/assets/dmg/background.png" "$ROOT/assets/dmg/background@2x.png" \
    -out "$WORK/background.tiff" >/dev/null 2>&1

  "$VENV/bin/dmgbuild" -s "$ROOT/scripts/dmg-settings.py" \
    -D app="$APP" -D background="$WORK/background.tiff" -D icon="$ROOT/assets/icon.icns" \
    "$NAME" "$DMG" >/dev/null
  rm -rf "$WORK"
  echo "  $DMG  ($(du -sh "$DMG" | cut -f1))"
fi
