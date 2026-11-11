#!/bin/sh
# Rebuild every raster icon from the SVG sources in app/src-tauri/icons/source.
# Needs rsvg-convert (librsvg), ImageMagick (`magick`) and, for .icns, macOS `iconutil`.
# The full mark is used from 64 px up; 16/32 px and the tray use the pixel-grid glyph.
set -eu
icons=$(cd "$(dirname "$0")/../src-tauri/icons" && pwd)
src="$icons/source"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

full() { rsvg-convert -w "$1" -h "$1" "$src/agendar-icon.svg" -o "$2"; }
small() { rsvg-convert -w "$1" -h "$1" "$src/agendar-icon-small.svg" -o "$2"; }

small 32 "$icons/32x32.png"
full 64 "$icons/64x64.png"
full 128 "$icons/128x128.png"
full 256 "$icons/128x128@2x.png"
full 512 "$icons/icon.png"

# Tray: macOS template glyphs (black + alpha, 28x18 pt @2x) and colour tiles elsewhere.
rsvg-convert -w 44 -h 36 "$src/tray-idle.svg" -o "$icons/tray-idle.png"
rsvg-convert -w 44 -h 36 "$src/tray-needs.svg" -o "$icons/tray-needs.png"
rsvg-convert -w 44 -h 36 "$src/tray-empty.svg" -o "$icons/tray-empty.png"
rsvg-convert -w 32 -h 32 "$src/tray-idle-color.svg" -o "$icons/tray-idle-color.png"
rsvg-convert -w 32 -h 32 "$src/tray-needs-color.svg" -o "$icons/tray-needs-color.png"
rsvg-convert -w 32 -h 32 "$src/tray-empty-color.svg" -o "$icons/tray-empty-color.png"

# Windows .ico: small glyph at 16/24/32, full mark above.
small 16 "$work/16.png"; small 24 "$work/24.png"; small 32 "$work/32.png"
full 48 "$work/48.png"; full 64 "$work/64.png"; full 128 "$work/128.png"; full 256 "$work/256.png"
magick "$work/16.png" "$work/24.png" "$work/32.png" "$work/48.png" "$work/64.png" "$work/128.png" "$work/256.png" "$icons/icon.ico"

# macOS .icns
if command -v iconutil >/dev/null 2>&1; then
  set_dir="$work/agendar.iconset"; mkdir "$set_dir"
  small 16 "$set_dir/icon_16x16.png"
  small 32 "$set_dir/icon_16x16@2x.png"
  small 32 "$set_dir/icon_32x32.png"
  full 64 "$set_dir/icon_32x32@2x.png"
  full 128 "$set_dir/icon_128x128.png"
  full 256 "$set_dir/icon_128x128@2x.png"
  full 256 "$set_dir/icon_256x256.png"
  full 512 "$set_dir/icon_256x256@2x.png"
  full 512 "$set_dir/icon_512x512.png"
  full 1024 "$set_dir/icon_512x512@2x.png"
  iconutil -c icns "$set_dir" -o "$icons/icon.icns"
else
  echo "iconutil not found: icon.icns left unchanged" >&2
fi
echo "icons rebuilt in $icons"
