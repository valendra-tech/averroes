#!/bin/bash
set -euo pipefail

# Regenerate local UI/bundle assets from the user-supplied QDivZero vector mark.
# Headless only: no application, browser, Quick Look or desktop interaction.
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="${PROJECT_ROOT}/assets"
for tool in rsvg-convert magick iconutil; do
    command -v "${tool}" >/dev/null || { printf 'Missing asset tool: %s\n' "${tool}" >&2; exit 1; }
done

WORK_DIR="$(mktemp -d "${ICON_WORK_ROOT:-${TMPDIR:-/tmp}}/qdivzero-harness-icons.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT
ICONSET="${WORK_DIR}/QDivZeroHarness.iconset"
mkdir -p "${ICONSET}"

rsvg-convert --width 1536 "${ASSETS}/qdivzero-harness.svg" \
    --output "${ASSETS}/qdivzero-harness-logo.png"
rsvg-convert --width 1536 --stylesheet "${ASSETS}/qdivzero-harness-dark.css" \
    "${ASSETS}/qdivzero-harness.svg" --output "${ASSETS}/qdivzero-harness-logo-dark.png"
magick -size 1024x1024 xc:none -fill '#FBFBFD' \
    -draw 'roundrectangle 48,48 976,976 206,206' \
    \( "${ASSETS}/qdivzero-harness-logo.png" -resize 760x400 \) \
    -gravity center -composite "${ASSETS}/qdivzero-harness-icon.png"

for size in 16 32 128 256 512; do
    magick "${ASSETS}/qdivzero-harness-icon.png" -resize "${size}x${size}" \
        "${ICONSET}/icon_${size}x${size}.png"
    magick "${ASSETS}/qdivzero-harness-icon.png" -resize "$((size * 2))x$((size * 2))" \
        "${ICONSET}/icon_${size}x${size}@2x.png"
done
iconutil --convert icns "${ICONSET}" --output "${ASSETS}/QDivZeroHarness.icns"
