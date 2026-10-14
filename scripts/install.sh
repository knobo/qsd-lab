#!/bin/sh
# Install qsd-lab for the current user: binary in ~/.cargo/bin, desktop entry
# and icon under ~/.local/share. No root needed. Remove with --uninstall.
set -eu

cd "$(dirname "$0")/.."
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
BIN="$HOME/.cargo/bin/qsd-lab"

if [ "${1:-}" = "--uninstall" ]; then
    cargo uninstall qsd-lab || true
    rm -f "$APPS/qsd-lab.desktop" "$ICONS/qsd-lab.svg"
    update-desktop-database "$APPS" 2>/dev/null || true
    echo "Removed qsd-lab. Lab data in ~/.local/share/qsd-lab is left alone."
    exit 0
fi

cargo install --locked --path .
mkdir -p "$APPS" "$ICONS"
install -m 644 packaging/qsd-lab.svg "$ICONS/qsd-lab.svg"
# Absolute Exec path: ~/.cargo/bin is often not on PATH for the desktop session.
sed "s|^Exec=qsd-lab|Exec=$BIN|" packaging/qsd-lab.desktop > "$APPS/qsd-lab.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
gtk-update-icon-cache -q "${ICONS%/scalable/apps}" 2>/dev/null || true
echo "Installed $BIN and qsd-lab.desktop. Find it as \"qsd-lab\" in the app launcher."
