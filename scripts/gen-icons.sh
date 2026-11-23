#!/usr/bin/env bash
# Renders the app icon SVG into every PNG size the Linux, Windows and Store
# packaging needs. Needs rsvg-convert (librsvg) and ImageMagick (`magick`)
# for the Windows .ico.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=assets/icons/com.ninepointlabs.Persistwise.svg
OUT=assets/icons
mkdir -p "$OUT/hicolor" src-tauri/icons packaging/windows/msix/Assets
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$OUT/hicolor/${s}x${s}"
  rsvg-convert -w "$s" -h "$s" "$SVG" -o "$OUT/hicolor/${s}x${s}/com.ninepointlabs.Persistwise.png"
done
# Tauri (window icon + Windows exe icon)
cp "$OUT/hicolor/32x32/com.ninepointlabs.Persistwise.png" src-tauri/icons/32x32.png
cp "$OUT/hicolor/128x128/com.ninepointlabs.Persistwise.png" src-tauri/icons/128x128.png
rsvg-convert -w 256 -h 256 "$SVG" -o src-tauri/icons/128x128@2x.png
cp "$OUT/hicolor/512x512/com.ninepointlabs.Persistwise.png" src-tauri/icons/icon.png
magick "$OUT"/hicolor/{16x16,24x24,32x32,48x48,64x64,256x256}/com.ninepointlabs.Persistwise.png src-tauri/icons/icon.ico
# MSIX / Microsoft Store logos
M=packaging/windows/msix/Assets
rsvg-convert -w 44 -h 44 "$SVG" -o "$M/Square44x44Logo.png"
rsvg-convert -w 150 -h 150 "$SVG" -o "$M/Square150x150Logo.png"
rsvg-convert -w 50 -h 50 "$SVG" -o "$M/StoreLogo.png"
rsvg-convert -w 310 -h 310 "$SVG" -o "$M/Square310x310Logo.png"
for s in 16 24 32 48 256; do
  rsvg-convert -w "$s" -h "$s" "$SVG" -o "$M/Square44x44Logo.targetsize-${s}_altform-unplated.png"
done
magick -size 310x150 xc:'#011321' \( "$OUT/hicolor/128x128/com.ninepointlabs.Persistwise.png" \) -gravity center -composite "$M/Wide310x150Logo.png"
echo "icons written"
