#!/usr/bin/env bash
# Assembles Malgel.app around a built binary (macOS only: needs iconutil).
#
#   packaging/macos/bundle.sh <malgel binary> <version> <output dir>
#
# LSMinimumSystemVersion is MACOSX_DEPLOYMENT_TARGET (default 11.0); build
# the binary with the same value. Signing is left to the caller.
set -euo pipefail

if (($# != 3)); then
  echo "usage: $0 <malgel binary> <version> <output dir>" >&2
  exit 2
fi
binary="$1"
version="$2"
out="$3"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
min_macos="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

app="$out/Malgel.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

install -m755 "$binary" "$app/Contents/MacOS/malgel"
# Bundle versions must be plain numbers: drop any pre-release part ("-beta.1").
sed -e "s/@VERSION@/${version%%[-+]*}/g" -e "s/@MIN_MACOS@/$min_macos/g" \
  "$here/Info.plist" >"$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"
install -m644 "$here/../../LICENSE" "$here/../../NOTICE" "$app/Contents/Resources/"
iconutil --convert icns --output "$app/Contents/Resources/Malgel.icns" \
  "$here/Malgel.iconset"

plutil -lint "$app/Contents/Info.plist"
echo "Wrote $app"
