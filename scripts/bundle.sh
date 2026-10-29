#!/usr/bin/env bash
#
# Construit « Matter Home Desktop.app » et, au besoin, le DMG qui la distribue.
#
#   scripts/bundle.sh          # dist/Matter Home Desktop.app
#   scripts/bundle.sh --dmg    # ...et dist/MatterHomeDesktop-<version>-<arch>.dmg
#
# Les deux architectures sont compilées et fusionnées en un binaire universel quand
# les deux cibles sont installées (`rustup target add x86_64-apple-darwin`) ; sinon
# le binaire ne vise que ce Mac, et le nom du DMG le dit.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
NAME="Matter Home Desktop"
BIN="matter-home-desktop"
APP="$DIST/$NAME.app"

VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
BUNDLE_ID="dev.matter-home-desktop.app"
MIN_MACOS="12.0"

log() { printf '\033[1m▸\033[0m %s\n' "$*"; }

# ---- compilation -----------------------------------------------------------

INSTALLED="$(rustup target list --installed)"
TARGETS=()
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  if grep -qx "$target" <<< "$INSTALLED"; then
    TARGETS+=("$target")
  fi
done

BINARIES=()
if [ ${#TARGETS[@]} -lt 2 ]; then
  echo "  note : x86_64-apple-darwin n'est pas installée, ce build n'est pas universel." >&2
  echo "         rustup target add x86_64-apple-darwin  # pour livrer les deux" >&2
  log "Compilation pour ce Mac"
  (cd "$ROOT" && cargo build --release)
  BINARIES+=("$ROOT/target/release/$BIN")
  FLAVOUR="$(uname -m)"
else
  for target in "${TARGETS[@]}"; do
    log "Compilation $target"
    (cd "$ROOT" && cargo build --release --target "$target")
    BINARIES+=("$ROOT/target/$target/release/$BIN")
  done
  FLAVOUR="universal"
fi

# ---- assemblage ------------------------------------------------------------

log "Assemblage de $NAME.app ($VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

lipo -create -output "$APP/Contents/MacOS/$BIN" "${BINARIES[@]}"
cp "$ROOT/assets/icon.icns" "$APP/Contents/Resources/icon.icns"

# NSLocalNetworkUsageDescription et NSBonjourServices : sans eux, macOS bloque la
# découverte mDNS des appareils Matter dès que l'application est lancée depuis un .app.
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
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Licence MIT</string>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Matter Home Desktop communique avec vos appareils Matter sur le réseau local pour les appairer, lire leurs mesures et les commander.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_matter._tcp</string>
    <string>_matterc._udp</string>
  </array>
</dict>
</plist>
PLIST

# Signature ad hoc : pas un certificat Developer ID (payant), mais une signature
# valide et stable plutôt qu'aucune.
log "Signature (ad hoc)"
codesign --force --sign - --timestamp=none "$APP" >/dev/null 2>&1 ||
  echo "  note : la signature ad hoc a échoué ; l'application n'est pas signée." >&2

echo "  $APP  ($(du -sh "$APP" | cut -f1))"

# ---- DMG -------------------------------------------------------------------

if [ "${1:-}" = "--dmg" ]; then
  # Le nom dit ce qu'il contient : un build local d'une seule architecture ne peut
  # pas être confondu avec le DMG universel publié par la CI.
  DMG="$DIST/MatterHomeDesktop-$VERSION-$FLAVOUR.dmg"
  log "Construction de $(basename "$DMG")"
  rm -f "$DMG"

  # dmgbuild met en page la fenêtre (fond, taille, position des icônes) sans piloter le
  # Finder. Installé à une version fixe dans un environnement Python isolé, sous target/.
  DMGBUILD_VERSION="1.6.7"
  VENV="$ROOT/target/dmgbuild-$DMGBUILD_VERSION"
  if [ ! -x "$VENV/bin/dmgbuild" ]; then
    log "Installation de dmgbuild $DMGBUILD_VERSION"
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install --quiet --disable-pip-version-check "dmgbuild==$DMGBUILD_VERSION"
  fi

  # Fond net en Retina : un TIFF qui contient l'image 1x et l'image 2x.
  WORK="$(mktemp -d)"
  tiffutil -cathidpicheck "$ROOT/assets/dmg/background.png" "$ROOT/assets/dmg/background@2x.png" \
    -out "$WORK/background.tiff" >/dev/null 2>&1

  "$VENV/bin/dmgbuild" -s "$ROOT/scripts/dmg-settings.py" \
    -D app="$APP" -D background="$WORK/background.tiff" -D icon="$ROOT/assets/icon.icns" \
    "$NAME" "$DMG" >/dev/null
  rm -rf "$WORK"

  echo "  $DMG  ($(du -sh "$DMG" | cut -f1))"
fi
