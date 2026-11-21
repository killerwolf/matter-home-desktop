#!/usr/bin/env bash
#
# Télécharge et vérifie les fichiers de la détection (ticket n° 18) à embarquer dans
# « Ostal Server.app » : les modèles YOLOX (Apache-2.0) et ONNX Runtime (MIT, roues PyPI
# officielles de Microsoft, une par architecture, réunies en une bibliothèque universelle).
#
#   scripts/fetch-detection-assets.sh <dossier>        # télécharge, vérifie, prépare <dossier>
#   scripts/fetch-detection-assets.sh --pin            # affiche les empreintes à épingler
#
# Rien n'est dans le dépôt : chaque fichier est téléchargé depuis sa version officielle puis
# comparé à son empreinte SHA-256 **épinglée** dans scripts/detection-assets.sha256. Une empreinte
# différente, ou absente (« A_EPINGLER »), arrête tout : on n'embarque jamais un fichier non vérifié.
# `--pin` télécharge dans un dossier temporaire et affiche les empreintes, pour qu'une personne les
# vérifie (par exemple avec celles publiées par le projet) puis les recopie dans le fichier.
#
# <dossier> reçoit :
#   models/detector.onnx        (yolox_s.onnx, CoreML)
#   models/detector-cpu.onnx    (yolox_nano.onnx, processeur)
#   Frameworks/libonnxruntime.dylib
#   licences/YOLOX-LICENSE, licences/onnxruntime-LICENSE, licences/onnxruntime-ThirdPartyNotices.txt
#
# Un cache (target/detection-assets/) évite de retélécharger ; il est revérifié à chaque fois.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PINS="$ROOT/scripts/detection-assets.sha256"
CACHE="${MH_DETECTION_CACHE:-$ROOT/target/detection-assets}"
ORT_VERSION="1.23.2"

log() { printf '\033[1m▸\033[0m %s\n' "$*"; }
die() { echo "erreur : $*" >&2; exit 1; }
sha() { shasum -a 256 "$1" | awk '{print $1}'; }

PIN_MODE=0
OUT=""
case "${1:-}" in
  --pin) PIN_MODE=1 ;;
  "") die "usage : $0 <dossier> | --pin" ;;
  *) OUT="$1" ;;
esac

# Les lignes utiles du fichier d'empreintes : « empreinte nom adresse ».
entries() { grep -vE '^\s*(#|$)' "$PINS"; }

if [ "$PIN_MODE" = 1 ]; then
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  entries | while read -r _ name url; do
    log "Téléchargement de $name"
    curl -fsSL --retry 3 -o "$TMP/$name" "$url"
    printf '%s  %s  %s\n' "$(sha "$TMP/$name")" "$name" "$url"
  done
  echo "Vérifiez ces empreintes, puis recopiez-les dans $PINS." >&2
  exit 0
fi

mkdir -p "$CACHE"
entries | while read -r want name url; do
  [ "$want" != "A_EPINGLER" ] ||
    die "$name n'a pas d'empreinte épinglée dans scripts/detection-assets.sha256 (voir docs/detection.md)"
  file="$CACHE/$name"
  if [ ! -f "$file" ] || [ "$(sha "$file")" != "$want" ]; then
    log "Téléchargement de $name"
    rm -f "$file"
    curl -fsSL --retry 3 -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  got="$(sha "$file")"
  [ "$got" = "$want" ] || { rm -f "$file"; die "empreinte de $name : $got au lieu de $want"; }
  echo "  $name : empreinte vérifiée"
done

rm -rf "$OUT"
mkdir -p "$OUT/models" "$OUT/Frameworks" "$OUT/licences"
install -m 0644 "$CACHE/yolox_s.onnx" "$OUT/models/detector.onnx"
install -m 0644 "$CACHE/yolox_nano.onnx" "$OUT/models/detector-cpu.onnx"
install -m 0644 "$ROOT/assets/licences/YOLOX-LICENSE" "$OUT/licences/YOLOX-LICENSE"

# ONNX Runtime : les roues officielles de PyPI (une par architecture, macOS 13 minimum). Une roue
# est une archive zip ; on n'en prend que la bibliothèque C et les licences.
UNPACK="$(mktemp -d)"
trap 'rm -rf "$UNPACK"' EXIT
SLICES=()
for arch in x86_64 arm64; do
  wheel="$CACHE/onnxruntime-$ORT_VERSION-cp312-cp312-macosx_13_0_$arch.whl"
  [ -f "$wheel" ] || die "roue ONNX Runtime $arch absente ($wheel)"
  mkdir -p "$UNPACK/$arch"
  unzip -q "$wheel" 'onnxruntime/capi/libonnxruntime*.dylib' 'onnxruntime/LICENSE' \
    'onnxruntime/ThirdPartyNotices.txt' -d "$UNPACK/$arch" ||
    die "roue ONNX Runtime $arch : bibliothèque ou licences introuvables"
  found=("$UNPACK/$arch"/onnxruntime/capi/libonnxruntime*.dylib)
  if [ ${#found[@]} -ne 1 ] || [ ! -f "${found[0]}" ]; then
    die "roue ONNX Runtime $arch : une seule libonnxruntime*.dylib attendue (${#found[@]})"
  fi
  got="$(lipo -archs "${found[0]}")"
  [ "$got" = "$arch" ] || die "roue ONNX Runtime $arch : tranche $got"
  SLICES+=("${found[0]}")
done
lipo -create -output "$OUT/Frameworks/libonnxruntime.dylib" "${SLICES[@]}"
chmod 0755 "$OUT/Frameworks/libonnxruntime.dylib"
ARCHS="$(lipo -archs "$OUT/Frameworks/libonnxruntime.dylib")"
# La bibliothèque se désigne elle-même par @rpath/libonnxruntime.<version>.dylib : sans effet ici,
# le serveur la charge par son chemin (`dlopen`), mais on la renomme pour qu'elle dise vrai.
install_name_tool -id "@rpath/libonnxruntime.dylib" "$OUT/Frameworks/libonnxruntime.dylib"
# Licence MIT et avis de tiers d'ONNX Runtime, tels que la roue les livre.
install -m 0644 "$UNPACK/x86_64/onnxruntime/LICENSE" "$OUT/licences/onnxruntime-LICENSE"
install -m 0644 "$UNPACK/x86_64/onnxruntime/ThirdPartyNotices.txt" \
  "$OUT/licences/onnxruntime-ThirdPartyNotices.txt"
echo "  $OUT : modèles, ONNX Runtime ($ARCHS) et licences prêts"
