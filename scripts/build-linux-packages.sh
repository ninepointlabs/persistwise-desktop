#!/usr/bin/env bash
# Builds native Linux packages from the release binary:
#   dist/persistwise-desktop_<version>_amd64.deb        (+ .sha256)
#   dist/persistwise-desktop-<version>-1.x86_64.rpm     (+ .sha256)
#   dist/PKGBUILD   (Arch; packages the .deb above, checksum filled in)
# The Arch package itself is built from that PKGBUILD with makepkg (see the
# `arch` job in .github/workflows/linux.yml).
#
# All three install the same files, named after the app id so the desktop
# entry matches the Wayland app_id and the AppStream metainfo:
#   /usr/bin/persistwise-desktop
#   /usr/share/applications/com.ninepointlabs.Persistwise.desktop
#   /usr/share/icons/hicolor/*/apps/com.ninepointlabs.Persistwise.{png,svg}
#   /usr/share/metainfo/com.ninepointlabs.Persistwise.metainfo.xml
# (Tauri's deb/rpm bundler would name the desktop file Persistwise.desktop
# with Icon=persistwise-desktop, which breaks icon and window matching.)
#
# Uses target/release/persistwise-desktop as built by scripts/build-appimage.sh
# (or `npx tauri build`). Build releases on Ubuntu 22.04 so the binary needs
# only glibc 2.35. Needs dpkg-dev (dpkg-shlibdeps) and rpm (rpmbuild).
#
# Usage: scripts/build-linux-packages.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

APP_ID="com.ninepointlabs.Persistwise"
APP="persistwise-desktop"
VERSION="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json | head -1)"
[ -n "$VERSION" ] || { echo "could not read version from tauri.conf.json"; exit 1; }
HOMEPAGE="https://github.com/ninepointlabs/persistwise-desktop"
MAINTAINER="Nine Point Labs <179739321+ninepointlabs@users.noreply.github.com>"
SUMMARY="A personal web archive on your own machine"
BIN="$ROOT/target/release/$APP"
OUT_DIR="$ROOT/dist"
WORK="$ROOT/build/linux-packages"

[ -x "$BIN" ] || { echo "missing $BIN (build the release binary first)"; exit 1; }
rm -rf "$WORK" && mkdir -p "$WORK" "$OUT_DIR"

# --- common file tree ---
STAGE="$WORK/stage"
install -D -m 755 "$BIN" "$STAGE/usr/bin/$APP"
strip --strip-unneeded "$STAGE/usr/bin/$APP"
install -D -m 644 "packaging/linux/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
install -D -m 644 "packaging/linux/$APP_ID.metainfo.xml" "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
for s in 16 24 32 48 64 128 256 512; do
  install -D -m 644 "assets/icons/hicolor/${s}x${s}/$APP_ID.png" "$STAGE/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
done
install -D -m 644 "assets/icons/$APP_ID.svg" "$STAGE/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"

checksum() { ( cd "$OUT_DIR" && sha256sum "$1" > "$1.sha256" ); }

# --- .deb ---
DEB_NAME="${APP}_${VERSION}_amd64.deb"
DEB_ROOT="$WORK/deb"
cp -a "$STAGE" "$DEB_ROOT"
install -D -m 644 LICENSE "$DEB_ROOT/usr/share/doc/$APP/copyright"
# Library dependencies straight from the binary (lowest versions = Ubuntu 22.04).
SHLIBS_DIR="$WORK/shlibs"
mkdir -p "$SHLIBS_DIR/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$APP" "$APP" > "$SHLIBS_DIR/debian/control"
DEPENDS="$(cd "$SHLIBS_DIR" && dpkg-shlibdeps -O "$DEB_ROOT/usr/bin/$APP" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
[ -n "$DEPENDS" ] || { echo "dpkg-shlibdeps found no dependencies"; exit 1; }
mkdir -p "$DEB_ROOT/DEBIAN"
cat > "$DEB_ROOT/DEBIAN/control" <<EOF
Package: $APP
Version: $VERSION
Architecture: amd64
Maintainer: $MAINTAINER
Installed-Size: $(du -sk --exclude=DEBIAN "$DEB_ROOT" | cut -f1)
Depends: $DEPENDS, hicolor-icon-theme
Recommends: xdg-utils
Section: web
Priority: optional
Homepage: $HOMEPAGE
Description: $SUMMARY
 Persistwise saves full copies of web pages on your own computer and lets
 you browse and search them offline. Optional backups to the Wayback Machine
 and to your own IPFS node or pinning service.
EOF
( cd "$DEB_ROOT" && find usr -type f -print0 | sort -z | xargs -0 md5sum ) > "$DEB_ROOT/DEBIAN/md5sums"
rm -f "$OUT_DIR/$DEB_NAME"
dpkg-deb --root-owner-group -Zxz --build "$DEB_ROOT" "$OUT_DIR/$DEB_NAME"
dpkg-deb --info "$OUT_DIR/$DEB_NAME"
checksum "$DEB_NAME"

# --- .rpm ---
RPM_TOP="$WORK/rpm"
RPM_ROOT="$WORK/rpm-stage"
cp -a "$STAGE" "$RPM_ROOT"
install -D -m 644 LICENSE "$RPM_ROOT/usr/share/licenses/$APP/LICENSE"
mkdir -p "$RPM_TOP/SPECS"
cat > "$RPM_TOP/SPECS/$APP.spec" <<EOF
Name:           $APP
Version:        $VERSION
Release:        1
Summary:        $SUMMARY
License:        MIT
URL:            $HOMEPAGE
BuildArch:      x86_64
Requires:       hicolor-icon-theme
Recommends:     xdg-utils
# Prebuilt, already stripped binary: no debuginfo, no brp post-processing.
%global debug_package %{nil}
%global __os_install_post %{nil}

%description
Persistwise saves full copies of web pages on your own computer and lets
you browse and search them offline. Optional backups to the Wayback Machine
and to your own IPFS node or pinning service.

%install
mkdir -p %{buildroot}
cp -a $RPM_ROOT/. %{buildroot}/

%files
%license /usr/share/licenses/$APP/LICENSE
/usr/bin/$APP
/usr/share/applications/$APP_ID.desktop
/usr/share/metainfo/$APP_ID.metainfo.xml
/usr/share/icons/hicolor/*/apps/$APP_ID.*
EOF
rpmbuild -bb --define "_topdir $RPM_TOP" "$RPM_TOP/SPECS/$APP.spec"
RPM_NAME="$APP-$VERSION-1.x86_64.rpm"
cp "$RPM_TOP/RPMS/x86_64/$RPM_NAME" "$OUT_DIR/$RPM_NAME"
rpm -qp --info --requires "$OUT_DIR/$RPM_NAME"
checksum "$RPM_NAME"

# --- Arch PKGBUILD (packages the .deb from the GitHub release) ---
DEB_SHA="$(cut -d' ' -f1 "$OUT_DIR/$DEB_NAME.sha256")"
sed -e "s/@VERSION@/$VERSION/" -e "s/@DEB_SHA256@/$DEB_SHA/" packaging/arch/PKGBUILD.in > "$OUT_DIR/PKGBUILD"
grep -qE '@[A-Z0-9_]+@' "$OUT_DIR/PKGBUILD" && { echo "unfilled placeholder in PKGBUILD"; exit 1; }

echo
ls -la "$OUT_DIR"
