#!/bin/sh
# Make target/release/den.app from a release build, for running from the
# Finder and for the release:
#   cargo build --release; ./scripts/bundle-macos.sh [version]
# The version is Cargo.toml's unless given. The icon comes from
# assets/icons/app.png through sips and iconutil (both ship with macOS), and
# the bundle is signed ad hoc, which is what a Mac needs to run it at all.
set -eu
cd "$(dirname "$0")/.."

VERSION=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)}
OUT=target/release
APP=$OUT/den.app
EXE=$OUT/den
[ -x "$EXE" ] || { echo "No $EXE: run cargo build --release first." >&2; exit 1; }

rm -rf "$APP" "$OUT/den.iconset"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT/den.iconset"
cp "$EXE" "$APP/Contents/MacOS/den"
sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

for size in 16 32 128 256 512; do
  double=$((size * 2))
  sips -z "$size" "$size" assets/icons/app.png --out "$OUT/den.iconset/icon_${size}x${size}.png" > /dev/null
  sips -z "$double" "$double" assets/icons/app.png --out "$OUT/den.iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$OUT/den.iconset" -o "$APP/Contents/Resources/den.icns"
rm -rf "$OUT/den.iconset"

codesign --force --deep --sign - "$APP"
echo "Built $APP ($VERSION)"
