#!/bin/sh
# Regenerates every icon file from the SVG sources. Needs `rsvg-convert` (brew install librsvg)
# and, on macOS, `iconutil` for the .icns.
set -eu
cd "$(dirname "$0")"

# The app icon: a rounded plate with the transparent margin macOS expects.
rsvg-convert -w 1024 -h 1024 ramus.svg -o ramus-1024.png
# The same artwork edge to edge, for avatars (the platform rounds or crops them itself).
rsvg-convert -w 1024 -h 1024 ramus-square.svg -o avatar-1024.png
# The card GitHub shows when the repository is linked (Settings → Social preview).
rsvg-convert -w 1280 -h 640 social-preview.svg -o social-preview.png

# macOS .icns: every size at 1x and 2x.
iconset=$(mktemp -d)/Ramus.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size" -h "$size" ramus.svg -o "$iconset/icon_${size}x${size}.png"
  rsvg-convert -w "$((size * 2))" -h "$((size * 2))" ramus.svg -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o Ramus.icns
rm -rf "$(dirname "$iconset")"
