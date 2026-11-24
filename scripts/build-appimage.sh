#!/usr/bin/env bash
# Builds dist/persistwise-desktop-<version>-x86_64.AppImage (+ .zsync + .sha256).
#
# How: Tauri's bundler makes a self-contained AppImage (WebKitGTK, GTK and
# their helper processes bundled via linuxdeploy + linuxdeploy-plugin-gtk).
# We then unpack it and make it follow the Nine Point Labs AppImage
# standard: proper desktop file and app id, full hicolor icon set, AppStream
# metainfo, our AppRun (opt-in --install-desktop-entry/--remove-desktop-entry,
# clean host env for child processes), and update information for
# AppImageUpdate / Gear Lever. Repacked with a pinned appimagetool.
#
# Build releases on Ubuntu 22.04 (see .github/workflows/linux.yml) so the
# glibc requirement stays low. Building on a rolling distro works for local
# testing but the result needs that distro's newer glibc.
#
# Usage: scripts/build-appimage.sh [--skip-tauri-build]
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

APP_ID="com.ninepointlabs.Persistwise"
APP="persistwise-desktop"
VERSION="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json | head -1)"
[ -n "$VERSION" ] || { echo "could not read version from tauri.conf.json"; exit 1; }
ARCH="x86_64"
OUT_DIR="$ROOT/dist"
WORK="$ROOT/build/appimage"
TOOLS="$ROOT/build/tools"

APPIMAGETOOL_VERSION="1.9.1"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
APPIMAGETOOL="$TOOLS/appimagetool-$APPIMAGETOOL_VERSION-x86_64.AppImage"
UPDATE_INFO="gh-releases-zsync|ninepointlabs|persistwise-desktop|latest|$APP-*$ARCH.AppImage.zsync"

if [ "${1:-}" != "--skip-tauri-build" ]; then
  [ -d node_modules/@tauri-apps/cli ] || npm ci --no-audit --no-fund
  NO_STRIP=true npx tauri build --bundles appimage
fi
TAURI_AI="$ROOT/target/release/bundle/appimage/Persistwise_${VERSION}_amd64.AppImage"
[ -f "$TAURI_AI" ] || { echo "missing $TAURI_AI"; exit 1; }

mkdir -p "$TOOLS" "$OUT_DIR"
if [ ! -f "$APPIMAGETOOL" ]; then
  curl -fsSL -o "$APPIMAGETOOL.part" "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage"
  mv "$APPIMAGETOOL.part" "$APPIMAGETOOL"
fi
echo "$APPIMAGETOOL_SHA256  $APPIMAGETOOL" | sha256sum -c - >/dev/null || { echo "appimagetool checksum mismatch"; exit 1; }
chmod +x "$APPIMAGETOOL"

rm -rf "$WORK" && mkdir -p "$WORK"
( cd "$WORK" && "$TAURI_AI" --appimage-extract >/dev/null )
AD="$WORK/squashfs-root"

# --- AppRun: ours in front of Tauri's (linuxdeploy) launcher ---
mv "$AD/AppRun" "$AD/AppRun.tauri"
install -m 755 packaging/linux/AppRun "$AD/AppRun"

# --- desktop file, icons, metainfo: replace what the bundler generated ---
rm -f "$AD"/*.desktop "$AD"/*.png "$AD"/*.svg "$AD/.DirIcon"
rm -f "$AD/usr/share/applications/"*.desktop
find "$AD/usr/share/icons" -name "persistwise-desktop*" -delete 2>/dev/null || true
find "$AD/usr/share/icons/hicolor" -type d -empty -delete 2>/dev/null || true

mkdir -p "$AD/usr/share/applications" "$AD/usr/share/metainfo"
{ cat packaging/linux/$APP_ID.desktop; echo "X-AppImage-Version=$VERSION"; } > "$AD/usr/share/applications/$APP_ID.desktop"
cp "$AD/usr/share/applications/$APP_ID.desktop" "$AD/$APP_ID.desktop"
cp packaging/linux/$APP_ID.metainfo.xml "$AD/usr/share/metainfo/$APP_ID.metainfo.xml"
# appimagetool looks for <id>.appdata.xml; keep both names.
cp packaging/linux/$APP_ID.metainfo.xml "$AD/usr/share/metainfo/$APP_ID.appdata.xml"

for s in 16 24 32 48 64 128 256 512; do
  install -D -m 644 "assets/icons/hicolor/${s}x${s}/$APP_ID.png" "$AD/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
done
install -D -m 644 "assets/icons/$APP_ID.svg" "$AD/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
cp "assets/icons/hicolor/256x256/$APP_ID.png" "$AD/$APP_ID.png"
ln -sf "$APP_ID.png" "$AD/.DirIcon"

# --- checks before packing ---
desktop-file-validate "$AD/$APP_ID.desktop"
if command -v appstreamcli >/dev/null 2>&1; then
  appstreamcli validate --no-net "$AD/usr/share/metainfo/$APP_ID.metainfo.xml"
fi

# --- pack ---
OUT="$OUT_DIR/$APP-$VERSION-$ARCH.AppImage"
rm -f "$OUT" "$OUT.zsync" "$OUT.sha256"
( cd "$OUT_DIR" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGETOOL" --no-appstream -u "$UPDATE_INFO" "$AD" "$OUT" )
[ -f "$(basename "$OUT").zsync" ] && mv "$(basename "$OUT").zsync" "$OUT.zsync" || true
( cd "$OUT_DIR" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256" )
echo
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
ls -la "$OUT_DIR"
