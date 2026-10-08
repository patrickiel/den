#!/bin/sh
# Build an extension and side-load it into den on macOS or Linux, which picks
# it up at its next start (scripts/sideload.ps1 does this on Windows):
#   ./scripts/sideload.sh examples/hello-extension
# It goes in as `<id>.pending`, as an install from the Extensions view does.
set -eu
[ $# -eq 1 ] || { echo "Usage: $0 <extension folder>" >&2; exit 1; }
DIR=$1
MANIFEST=$DIR/extension.json
[ -f "$MANIFEST" ] || { echo "No extension.json in $DIR" >&2; exit 1; }
ID=$(sed -n 's/^[[:space:]]*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)
ICON=$(sed -n 's/^[[:space:]]*"icon"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)

cargo build --release --manifest-path "$DIR/Cargo.toml"
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$DIR/Cargo.toml" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
LIB_NAME=$(printf '%s' "$ID" | tr '-' '_')
case "$(uname)" in
  Darwin) LIB="$TARGET/release/lib$LIB_NAME.dylib"; DATA="$HOME/Library/Application Support/den" ;;
  *) LIB="$TARGET/release/lib$LIB_NAME.so"; DATA="${XDG_CONFIG_HOME:-$HOME/.config}/den" ;;
esac

PENDING="$DATA/extensions/$ID.pending"
rm -rf "$PENDING"
mkdir -p "$PENDING"
cp "$MANIFEST" "$LIB" "$PENDING/"
[ -f "$DIR/README.md" ] && cp "$DIR/README.md" "$PENDING/"
if [ -n "$ICON" ]; then
  mkdir -p "$(dirname "$PENDING/$ICON")"
  cp "$DIR/$ICON" "$PENDING/$ICON"
fi
echo "Staged $ID. Restart den to load it; see $DATA/extensions.log"
