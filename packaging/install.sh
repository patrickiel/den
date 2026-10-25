#!/bin/sh
# Install den for this user from the release archive: run it from the
# unpacked folder (./install.sh). The program goes to ~/.local/bin, the
# launcher entry and icon to ~/.local/share; no root needed. Running it again
# reinstalls; den updates itself from then on.
set -eu
cd "$(dirname "$0")"

BIN=${XDG_BIN_HOME:-$HOME/.local/bin}
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
ICONS=$DATA/icons/hicolor/512x512/apps

mkdir -p "$BIN" "$DATA/applications" "$ICONS"
# Over a running den too: a new file, not a write into the old one.
cp den "$BIN/.den.new"
chmod +x "$BIN/.den.new"
mv -f "$BIN/.den.new" "$BIN/den"
cp den.png "$ICONS/den.png"
sed "s|__EXEC__|$BIN/den|" den.desktop > "$DATA/applications/den.desktop"

command -v update-desktop-database > /dev/null && update-desktop-database -q "$DATA/applications" || true
command -v gtk-update-icon-cache > /dev/null && gtk-update-icon-cache -q -t "$DATA/icons/hicolor" || true

echo "Installed den to $BIN/den; it is in your applications."
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "To start it from a terminal as den, add $BIN to your PATH." ;;
esac
