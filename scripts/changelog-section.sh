#!/usr/bin/env bash
#
# Affiche la section d'une version de CHANGELOG.md, pour les notes de version GitHub.
#
#   scripts/changelog-section.sh v0.1.0

set -euo pipefail

VERSION="${1:-}"
VERSION="${VERSION#v}"

if [ -z "$VERSION" ]; then
  echo "usage : changelog-section.sh <version>" >&2
  exit 1
fi

CHANGELOG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/CHANGELOG.md"

# La section va de son titre « ## [x.y.z] » jusqu'au titre « ## » suivant.
BODY="$(awk -v version="## [$VERSION]" '
  index($0, version) == 1 { inside = 1; next }
  inside && /^## / { exit }
  inside { print }
' "$CHANGELOG")"

# Retire les lignes vides au début et à la fin.
BODY="$(printf '%s' "$BODY" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;};/\n$/ba')"

if [ -z "$BODY" ]; then
  echo "Pas de section $VERSION dans CHANGELOG.md" >&2
  exit 1
fi

printf '%s\n' "$BODY"
