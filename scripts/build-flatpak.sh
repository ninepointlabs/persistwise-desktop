#!/usr/bin/env bash
# Builds dist/persistwise-desktop-<version>-x86_64.flatpak, a self-hosted
# single-file bundle (never Flathub). Needs flatpak and flatpak-builder, and
# the release binary (built on Ubuntu 22.04 for releases):
#   npx tauri build --no-bundle      (or: cargo build --release -p persistwise-desktop)
set -euo pipefail
cd "$(dirname "$0")/.."
APP_ID=com.ninepointlabs.Persistwise
VERSION="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json | head -1)"
[ -x target/release/persistwise-desktop ] || { echo "build the release binary first"; exit 1; }
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y --noninteractive flathub org.gnome.Platform//50 org.gnome.Sdk//50
mkdir -p dist build/flatpak
flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --repo=build/flatpak/repo build/flatpak/build-dir packaging/flatpak/$APP_ID.yml
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  build/flatpak/repo "dist/persistwise-desktop-$VERSION-x86_64.flatpak" $APP_ID
( cd dist && sha256sum "persistwise-desktop-$VERSION-x86_64.flatpak" > "persistwise-desktop-$VERSION-x86_64.flatpak.sha256" )
echo "Built dist/persistwise-desktop-$VERSION-x86_64.flatpak"
