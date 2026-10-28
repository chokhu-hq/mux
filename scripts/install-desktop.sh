#!/usr/bin/env bash
# puts mux in the app launcher with its icon, pointing at a built binary
# usage: scripts/install-desktop.sh [path/to/mux-gui]
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=$(realpath "${1:-$ROOT/target/release/mux-gui}")
[ -x "$BIN" ] || { echo "no mux-gui at $BIN, build it first or pass its path" >&2; exit 1; }
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
for size in 16 32 48 64 128 256 512; do
  install -Dm644 "$ROOT/assets/icons/mux-$size.png" "$DATA/icons/hicolor/${size}x${size}/apps/mux.png"
done
# the desktop entry's name matches the window's app id, which is how wayland finds the icon
sed "s|^Exec=.*|Exec=$BIN|" "$ROOT/assets/mux.desktop" > "$DATA/applications/mux.desktop"
update-desktop-database "$DATA/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true
echo "installed mux.desktop for $BIN"
