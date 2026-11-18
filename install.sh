#!/usr/bin/env bash
# Install veil for the current user only. No root required.
set -euo pipefail

cd "$(dirname "$0")"

prefix="${PREFIX:-$HOME/.local}"
share="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"

echo "==> building (release)"
cargo build --release --workspace

echo "==> installing binaries to $prefix/bin"
install -Dm755 target/release/veild "$prefix/bin/veild"
install -Dm755 target/release/veilctl "$prefix/bin/veilctl"
install -Dm755 target/release/veil-gui "$prefix/bin/veil-gui"

echo "==> installing desktop entry and icon"
install -Dm644 packaging/org.veil.Veil.desktop \
    "$share/applications/org.veil.Veil.desktop"
install -Dm644 packaging/icons/org.veil.Veil.svg \
    "$share/icons/hicolor/scalable/apps/org.veil.Veil.svg"
command -v update-desktop-database >/dev/null && \
    update-desktop-database "$share/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && \
    gtk-update-icon-cache -f -t "$share/icons/hicolor" 2>/dev/null || true

echo "==> creating the wallpaper library"
mkdir -p "$share/veil/wallpapers"

echo "==> installing systemd user unit"
install -Dm644 packaging/veil.service "$config/systemd/user/veil.service"

if [ -d /usr/share/plasma/plasmoids ] || [ -d /usr/share/plasma ]; then
    echo "==> installing the Plasma wallpaper plugin"
    rm -rf "$share/plasma/wallpapers/org.veil.video"
    install -d "$share/plasma/wallpapers/org.veil.video"
    cp -r plugins/kde/org.veil.video/. "$share/plasma/wallpapers/org.veil.video/"
fi

echo
echo "Installed."
echo
echo "Open the picker:   $prefix/bin/veil-gui"
echo "Start the daemon:  $prefix/bin/veild &"
echo "Set wallpaper:     $prefix/bin/veilctl set ~/Videos/your-video.mp4"
echo "Show status:       $prefix/bin/veilctl status"
echo
echo "Autostart with systemd (needs WAYLAND_DISPLAY in the user manager):"
echo "  systemctl --user daemon-reload && systemctl --user enable --now veil.service"
echo
echo "Autostart without systemd (niri config.kdl):"
echo "  spawn-at-startup \"$prefix/bin/veild\""
