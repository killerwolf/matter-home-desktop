#!/usr/bin/env bash
#
# Régénère assets/icon.png et assets/icon.icns à partir de scripts/make-icon.swift.
# Nécessite les outils en ligne de commande de Xcode (swift, iconutil, sips).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

swift "$ROOT/scripts/make-icon.swift" "$WORK/master.png"
# Le rendu se fait à l'échelle de l'écran (2x sur un Mac Retina) : on ramène à 1024 px.
sips -z 1024 1024 "$WORK/master.png" --out "$ROOT/assets/icon.png" >/dev/null

ICONSET="$WORK/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$ROOT/assets/icon.icns"
echo "assets/icon.png et assets/icon.icns régénérés."
