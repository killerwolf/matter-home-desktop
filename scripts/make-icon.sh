#!/usr/bin/env bash
#
# Régénère les icônes d'Ostal à partir de scripts/make-icon.swift (logo « Seuil »,
# assets/logo/e-seuil.svg) :
#   assets/icon.png, assets/icon.icns            applications Mac (carré arrondi, marge 10 %)
#   crates/server/web/favicon.png                onglet du navigateur (même dessin, 64 px)
#   crates/server/web/apple-touch-icon.png       écran d'accueil iOS (plein, sans marge, 180 px)
#   crates/server/web/icon-192.png, icon-512.png app web installée (plein : « any » et « maskable »)
# Nécessite les outils en ligne de commande de Xcode (swift, iconutil, sips).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WEB="$ROOT/crates/server/web"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

swift "$ROOT/scripts/make-icon.swift" "$WORK/arrondi.png" arrondi
swift "$ROOT/scripts/make-icon.swift" "$WORK/plein.png" plein
cp "$WORK/arrondi.png" "$ROOT/assets/icon.png"

ICONSET="$WORK/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$WORK/arrondi.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$WORK/arrondi.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$ROOT/assets/icon.icns"

sips -z 64 64 "$WORK/arrondi.png" --out "$WEB/favicon.png" >/dev/null
sips -z 180 180 "$WORK/plein.png" --out "$WEB/apple-touch-icon.png" >/dev/null
sips -z 192 192 "$WORK/plein.png" --out "$WEB/icon-192.png" >/dev/null
sips -z 512 512 "$WORK/plein.png" --out "$WEB/icon-512.png" >/dev/null
echo "Icônes régénérées : assets/icon.png, assets/icon.icns et celles de l'app web."
