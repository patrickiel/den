#!/bin/sh
# Make target/release/bundle/den from a release build, the folder the Linux
# release archive holds (and the updater unpacks):
#   cargo build --release; ./scripts/bundle-linux.sh
#   tar -czf den.tar.gz -C target/release/bundle den
# It has the program, the icon, the launcher entry and install.sh, which
# puts them in place for the user.
set -eu
cd "$(dirname "$0")/.."

OUT=target/release/bundle/den
EXE=target/release/den
[ -x "$EXE" ] || { echo "No $EXE: run cargo build --release first." >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$EXE" "$OUT/den"
cp assets/icons/app.png "$OUT/den.png"
cp packaging/den.desktop packaging/install.sh "$OUT/"
chmod +x "$OUT/den" "$OUT/install.sh"
echo "Built $OUT"
