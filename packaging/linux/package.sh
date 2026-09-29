#!/usr/bin/env bash
# Packages a Linux build of Malgel as a .tar.gz and, when linuxdeploy is
# available, an AppImage.
#
#   packaging/linux/package.sh <malgel binary> <version> <output dir>
#
# The tarball unpacks to a prefix (bin/, share/) so it can be installed with
#   tar -xzf malgel-<version>-linux-x86_64.tar.gz -C ~/.local --strip-components=1
#
# Set LINUXDEPLOY to the linuxdeploy executable (an AppImage works; run it
# with APPIMAGE_EXTRACT_AND_RUN=1 where FUSE is unavailable) to also build
# Malgel-<version>-x86_64.AppImage. linuxdeploy bundles the shared
# libraries that are not part of a base system.
set -euo pipefail

if (($# != 3)); then
  echo "usage: $0 <malgel binary> <version> <output dir>" >&2
  exit 2
fi
binary="$1"
version="$2"
mkdir -p "$3"
out="$(cd "$3" && pwd)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
arch="$(uname -m)"
app_id="dev.malgel.Malgel"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Installs Malgel under the prefix $1: the layout of both packages.
install_prefix() {
  install -Dm755 "$binary" "$1/bin/malgel"
  install -Dm644 "$here/$app_id.desktop" "$1/share/applications/$app_id.desktop"
  install -Dm644 "$here/$app_id.metainfo.xml" "$1/share/metainfo/$app_id.metainfo.xml"
  (cd "$here/icons" && find hicolor -type f) | while read -r icon; do
    install -Dm644 "$here/icons/$icon" "$1/share/icons/$icon"
  done
}

# Tarball.
name="malgel-$version-linux-$arch"
install_prefix "$work/$name"
for doc in README.md LICENSE LICENSE-MIT LICENSE-APACHE; do
  if [[ -f "$root/$doc" ]]; then
    install -Dm644 "$root/$doc" "$work/$name/share/doc/malgel/$doc"
  fi
done
tar -C "$work" -czf "$out/$name.tar.gz" "$name"
echo "Wrote $out/$name.tar.gz"

# AppImage.
if [[ -z "${LINUXDEPLOY:-}" ]]; then
  echo "LINUXDEPLOY is not set; skipping the AppImage"
  exit 0
fi
appdir="$work/AppDir"
install_prefix "$appdir/usr"
appimage="Malgel-$version-$arch.AppImage"
(
  cd "$work"
  ARCH="$arch" LDAI_OUTPUT="$appimage" LDAI_NO_APPSTREAM=1 \
    "$LINUXDEPLOY" --appdir "$appdir" \
    --executable "$appdir/usr/bin/malgel" \
    --desktop-file "$appdir/usr/share/applications/$app_id.desktop" \
    --icon-file "$here/icons/hicolor/256x256/apps/$app_id.png" \
    --output appimage
)
mv "$work/$appimage" "$out/$appimage"
echo "Wrote $out/$appimage"
