#!/usr/bin/env bash
# Install veil for the current user only. No root required.
#
#   ./install.sh              build and install into ~/.local
#   ./install.sh --autostart  also add a spawn-at-startup line to niri
#   PREFIX=/usr ./install.sh  install somewhere else (still user-owned)
set -euo pipefail

cd "$(dirname "$0")"

prefix="${PREFIX:-$HOME/.local}"
share="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
autostart=0

for arg in "$@"; do
    case "$arg" in
        --autostart) autostart=1 ;;
        -h|--help)
            sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            exit 2
            ;;
    esac
done

# --- what the build needs -----------------------------------------------------

missing=()
for tool in cargo pkg-config; do
    command -v "$tool" >/dev/null || missing+=("$tool")
done

pkgs_missing=()
for pkg in mpv gtk4 libadwaita-1 gtk4-layer-shell-0; do
    pkg-config --exists "$pkg" 2>/dev/null || pkgs_missing+=("$pkg")
done

if [ ${#missing[@]} -gt 0 ] || [ ${#pkgs_missing[@]} -gt 0 ]; then
    echo "==> missing build requirements" >&2
    [ ${#missing[@]} -gt 0 ] && echo "    tools: ${missing[*]}" >&2
    [ ${#pkgs_missing[@]} -gt 0 ] && echo "    libraries: ${pkgs_missing[*]}" >&2
    echo >&2
    echo "    Arch:   sudo pacman -S rust pkgconf mpv gtk4 libadwaita gtk4-layer-shell ffmpeg playerctl" >&2
    echo "    Debian: sudo apt install cargo pkg-config libmpv-dev libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev" >&2
    exit 1
fi

# --- build --------------------------------------------------------------------

echo "==> building (release)"
cargo build --release --workspace

echo "==> installing binaries to $prefix/bin"
install -Dm755 target/release/veild "$prefix/bin/veild"
install -Dm755 target/release/veilctl "$prefix/bin/veilctl"
install -Dm755 target/release/veil-gui "$prefix/bin/veil-gui"
install -Dm755 target/release/veil-widgets "$prefix/bin/veil-widgets"

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

# --- autostart ----------------------------------------------------------------

if [ "$autostart" = 1 ]; then
    niri_config="$config/niri/config.kdl"
    if [ -f "$niri_config" ]; then
        if grep -qF 'spawn-at-startup' "$niri_config" && \
           grep -qF 'veild' "$niri_config"; then
            echo "==> niri already starts veild"
        else
            backup="$niri_config.bak-veil-$(date +%Y%m%d-%H%M%S)"
            cp "$niri_config" "$backup"
            {
                echo
                echo "// veil: video wallpapers"
                echo "spawn-at-startup \"$prefix/bin/veild\""
                echo "spawn-at-startup \"$prefix/bin/veil-widgets\""
            } >> "$niri_config"
            echo "==> added veild to $niri_config (backup: $backup)"
            if command -v niri >/dev/null; then
                niri validate >/dev/null 2>&1 && echo "    niri validate: ok" \
                    || echo "    niri validate failed, restore $backup" >&2
            fi
        fi
    else
        echo "==> no niri config at $niri_config, skipping autostart"
    fi
fi

# --- report -------------------------------------------------------------------

echo
echo "Installed."
echo
echo "Open the picker:   $prefix/bin/veil-gui"
echo "Start the daemon:  $prefix/bin/veild &"
echo "Set wallpaper:     $prefix/bin/veilctl set ~/Videos/your-video.mp4"
echo "Show status:       $prefix/bin/veilctl status"
echo
echo "Scenes from Wallpaper Engine need the engine as well:"
echo "  yay -S linux-wallpaperengine-git"
echo "  packaging/ROPETRAIL.md explains the optional particle patch."
echo
if [ "$autostart" = 0 ]; then
    echo "Autostart with systemd (needs WAYLAND_DISPLAY in the user manager):"
    echo "  systemctl --user daemon-reload && systemctl --user enable --now veil.service"
    echo
    echo "Autostart without systemd (niri), or re-run with --autostart to add it:"
    echo "  spawn-at-startup \"$prefix/bin/veild\""
    echo
    echo "Remember WAYLAND_DISPLAY for niri: start veild from the session, not a plain ssh shell."
fi
