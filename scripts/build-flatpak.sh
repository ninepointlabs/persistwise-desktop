#!/usr/bin/env bash
# Builds dist/persistwise-desktop-<version>-x86_64.flatpak, a self-hosted
# single-file bundle (never Flathub).
#
# The bundle repackages the release binary (built on Ubuntu 22.04, so its
# glibc is older than the runtime's) on the GNOME 50 runtime, whose
# WebKitGTK 4.1 it uses. There is nothing to compile, so this uses flatpak's
# own build-init/build-finish/build-export instead of flatpak-builder: no
# appstream-compose, no sandboxed builder that cannot see the host's SDK.
#
# Needs: flatpak, and the release binary:
#   npx tauri build --no-bundle      (or: cargo build --release -p persistwise-desktop)
set -euo pipefail
cd "$(dirname "$0")/.."
APP_ID=com.ninepointlabs.Persistwise
RUNTIME_VERSION=50
VERSION="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json | head -1)"
BIN=target/release/persistwise-desktop
[ -x "$BIN" ] || { echo "build the release binary first"; exit 1; }

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y --noninteractive flathub \
  "org.gnome.Platform//$RUNTIME_VERSION" "org.gnome.Sdk//$RUNTIME_VERSION"

B=build/flatpak/build-dir
REPO=build/flatpak/repo
rm -rf "$B"
mkdir -p dist build/flatpak
flatpak build-init "$B" "$APP_ID" "org.gnome.Sdk//$RUNTIME_VERSION" "org.gnome.Platform//$RUNTIME_VERSION"

F="$B/files"
install -Dm755 "$BIN" "$F/bin/persistwise-desktop"
install -Dm644 packaging/linux/$APP_ID.desktop "$F/share/applications/$APP_ID.desktop"
install -Dm644 packaging/linux/$APP_ID.metainfo.xml "$F/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 assets/icons/$APP_ID.svg "$F/share/icons/hicolor/scalable/apps/$APP_ID.svg"
for s in 16 24 32 48 64 128 256 512; do
  install -Dm644 "assets/icons/hicolor/${s}x${s}/$APP_ID.png" "$F/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
done

# Permissions. No home-directory access: the archive lives in the app's own
# data dir (~/.var/app/com.ninepointlabs.Persistwise/data/persistwise) and
# imports are picked through the file-chooser portal.
flatpak build-finish "$B" \
  --command=persistwise-desktop \
  --share=network \
  --share=ipc \
  --socket=wayland \
  --socket=fallback-x11 \
  --device=dri

flatpak build-export "$REPO" "$B"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$REPO" "dist/persistwise-desktop-$VERSION-x86_64.flatpak" "$APP_ID"
( cd dist && sha256sum "persistwise-desktop-$VERSION-x86_64.flatpak" > "persistwise-desktop-$VERSION-x86_64.flatpak.sha256" )
echo "Built dist/persistwise-desktop-$VERSION-x86_64.flatpak"
