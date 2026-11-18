#!/usr/bin/env bash
# Install veil for the current user only. No root required.
set -euo pipefail

cd "$(dirname "$0")"

prefix="${PREFIX:-$HOME/.local}"
share="${XDG_DATA_HOME:-$HOME/.local/share}"

echo "==> building (release)"
cargo build --release

echo "==> installing binaries to $prefix/bin"
install -Dm755 target/release/veild "$prefix/bin/veild"
install -Dm755 target/release/veilctl "$prefix/bin/veilctl"

echo "==> installing systemd user unit"
install -Dm644 packaging/veil.service \
    "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/veil.service"

if [ -d /usr/share/plasma/plasmoids ] || [ -d /usr/share/plasma ]; then
    echo "==> installing the Plasma wallpaper plugin"
    rm -rf "$share/plasma/wallpapers/org.veil.video"
    install -d "$share/plasma/wallpapers/org.veil.video"
    cp -r plugins/kde/org.veil.video/. "$share/plasma/wallpapers/org.veil.video/"
fi

echo
echo "Installed."
echo
echo "Start now:      $prefix/bin/veild &"
echo "Set wallpaper:  $prefix/bin/veilctl set ~/Videos/your-video.mp4"
echo "Show status:    $prefix/bin/veilctl status"
echo
echo "Autostart with systemd (niri/Hyprland/sway via uwsm or dbus-update-activation-environment):"
echo "  systemctl --user daemon-reload && systemctl --user enable --now veil.service"
echo
echo "Autostart without systemd (niri config.kdl):"
echo "  spawn-at-startup \"veild\""
