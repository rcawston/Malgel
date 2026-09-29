#!/usr/bin/env bash
# Renders the committed app icons from malgel.svg and malgel-small.svg.
#
# Needs rsvg-convert (librsvg2-bin) and ImageMagick 6 or 7. Run it from any
# directory after editing either SVG, then commit the results:
#
#   png/                       malgel-<size>.png, 16…1024
#   ../macos/Malgel.iconset/   input for `iconutil -c icns` (done in CI)
#   ../windows/malgel.ico      embedded in malgel.exe by build.rs
#   ../linux/icons/hicolor/    installed as dev.malgel.Malgel
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
packaging="$(dirname "$here")"
if command -v magick >/dev/null 2>&1; then im=(magick); else im=(convert); fi

big="$here/malgel.svg"
small="$here/malgel-small.svg"

# Sizes up to 32 px use the simplified artwork.
source_for() { if (($1 <= 32)); then echo "$small"; else echo "$big"; fi; }

render() { # size svg output
  rsvg-convert --width "$1" --height "$1" --keep-aspect-ratio "$2" --output "$3"
}

# The small artwork fills its canvas; on macOS every size should sit on the
# same 824/1024 grid as the large artwork, so shrink it and pad it back out.
render_mac() { # size output
  if (($1 <= 32)); then
    local inner=$(( ($1 * 824 + 512) / 960 ))
    render "$inner" "$small" "$2.tmp.png"
    "${im[@]}" "$2.tmp.png" -background none -gravity center -extent "$1x$1" "PNG32:$2"
    rm "$2.tmp.png"
  else
    render "$1" "$big" "$2"
  fi
}

mkdir -p "$here/png"
for size in 16 24 32 48 64 128 256 512 1024; do
  render "$size" "$(source_for "$size")" "$here/png/malgel-$size.png"
done

iconset="$packaging/macos/Malgel.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  render_mac "$size" "$iconset/icon_${size}x${size}.png"
  render_mac "$((size * 2))" "$iconset/icon_${size}x${size}@2x.png"
done

mkdir -p "$packaging/windows"
ico_inputs=()
for size in 16 24 32 48 64 128 256; do ico_inputs+=("$here/png/malgel-$size.png"); done
"${im[@]}" "${ico_inputs[@]}" "$packaging/windows/malgel.ico"

hicolor="$packaging/linux/icons/hicolor"
for size in 16 24 32 48 64 128 256 512; do
  mkdir -p "$hicolor/${size}x${size}/apps"
  cp "$here/png/malgel-$size.png" "$hicolor/${size}x${size}/apps/dev.malgel.Malgel.png"
done
mkdir -p "$hicolor/scalable/apps"
cp "$big" "$hicolor/scalable/apps/dev.malgel.Malgel.svg"

echo "Icons written under $packaging"
