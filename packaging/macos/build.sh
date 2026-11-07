#!/bin/sh
# Builds Ramus.app and a drag-to-Applications disk image into ./dist.
#
#   packaging/macos/build.sh
#
# Signing: by default the app gets an ad-hoc signature, which is what lets an arm64 binary
# run at all but is not an identity, so macOS will warn on first launch (right-click → Open).
# With a Developer ID certificate, sign (and later notarize) for a warning-free install:
#
#   CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" packaging/macos/build.sh
set -eu
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || { echo "could not read the version from Cargo.toml" >&2; exit 1; }
arch=$(uname -m)
identity=${CODESIGN_IDENTITY:--}

echo "Building Ramus $version ($arch)…"
cargo build --release --locked -p ramus

rm -rf dist
app=dist/Ramus.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/ramus "$app/Contents/MacOS/Ramus"
cp assets/icon/Ramus.icns "$app/Contents/Resources/Ramus.icns"
sed "s/__VERSION__/$version/g" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

if [ "$identity" = "-" ]; then
  codesign --force --deep --sign - "$app"
else
  codesign --force --deep --options runtime --timestamp --sign "$identity" "$app"
fi
codesign --verify --deep --strict "$app"

# The disk image: the app next to a shortcut to /Applications, to drag it across.
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
dmg="dist/Ramus-$version-$arch.dmg"
hdiutil create -quiet -volname "Ramus $version" -srcfolder "$staging" -ov -format UDZO "$dmg"
rm -rf "$staging"
if [ "$identity" != "-" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
fi
hdiutil verify -quiet "$dmg"

echo
echo "Built:"
echo "  $app"
echo "  $dmg ($(du -h "$dmg" | cut -f1))"
shasum -a 256 "$dmg"
