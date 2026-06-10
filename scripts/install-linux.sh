#!/usr/bin/env bash
# Install yzendris-client as a systemd user unit on any wlroots/Hyprland setup.
# Run once after building: ./scripts/install-linux.sh
#
# Tested on: Hyprland 0.55+ (Arch / CachyOS / Omarchy).
# Should work on Sway/Niri/river too, but the runtime kb_layout assignment uses
# `hyprctl` and won't apply there — if you're on another wlroots compositor,
# set kb_layout manually in client.toml (or rely on its global keyboard config).
#
# Usage:
#   ./scripts/install-linux.sh [path/to/yzendris-client]
#   Default binary path: ./target/release/yzendris-client

set -euo pipefail

BINARY="${1:-./target/release/yzendris-client}"
GUI_BINARY="${2:-./target/release/yzendris-gui}"
DEST="$HOME/.local/bin/yzendris-client"
GUI_DEST="$HOME/.local/bin/yzendris-gui"
WRAPPER="$HOME/.local/bin/yzendris-client-wrapper.sh"
UNIT="$HOME/.config/systemd/user/yzendris-client.service"
CFG_DIR="$HOME/.config/yzendris"
CFG="$CFG_DIR/client.toml"
PORT=7547
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── 1. Copy binaries ──────────────────────────────────────────────────────────
mkdir -p "$(dirname "$DEST")"
install -m 755 "$BINARY" "$DEST"
echo "✓ installed $DEST"

if [[ -f "$GUI_BINARY" ]]; then
    install -m 755 "$GUI_BINARY" "$GUI_DEST"
    echo "✓ installed $GUI_DEST"
else
    echo "  (GUI not found at $GUI_BINARY — build with: cargo build --release -p yzendris-gui)"
fi

# ── 2. Create wrapper script ──────────────────────────────────────────────────
# The wrapper injects Wayland / Hyprland environment variables that are absent
# in systemd sessions.  It auto-detects HYPRLAND_INSTANCE_SIGNATURE from the
# hypr socket directory so the service doesn't need manual configuration.
cat > "$WRAPPER" <<'WRAPEOF'
#!/bin/bash
export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export WAYLAND_DISPLAY="wayland-1"
export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR}/bus"
export XDG_CURRENT_DESKTOP="Hyprland"
export XDG_SESSION_TYPE="wayland"

HIS_DIR="${XDG_RUNTIME_DIR}/hypr"
if [ -d "$HIS_DIR" ]; then
    # Pick the newest instance that still has a live IPC socket — stale dirs
    # from previous sessions can coexist with the running one.
    for d in $(ls -1t "$HIS_DIR" 2>/dev/null); do
        if [ -S "$HIS_DIR/$d/.socket.sock" ]; then
            export HYPRLAND_INSTANCE_SIGNATURE="$d"
            break
        fi
    done
fi

exec "$HOME/.local/bin/yzendris-client" "$@"
WRAPEOF
chmod +x "$WRAPPER"
echo "✓ installed $WRAPPER"

# ── 3. Firewall ───────────────────────────────────────────────────────────────
if command -v ufw &>/dev/null; then
    sudo ufw allow "$PORT/tcp" comment "yzendris" 2>/dev/null || true
    echo "✓ ufw: allowed $PORT/tcp"
fi

# ── 4. Default config (if none exists) ───────────────────────────────────────
mkdir -p "$CFG_DIR"
if [[ ! -f "$CFG" ]]; then
    cp "$SCRIPT_DIR/../config/client.example.toml" "$CFG"
    echo "✓ wrote default config to $CFG"
    echo "  → Edit $CFG if you need to change port or kb_layout."
fi

# ── 5. Icon + desktop entry ───────────────────────────────────────────────────
ICON_SRC="$SCRIPT_DIR/../assets/icon.png"
ICON_DEST="$HOME/.local/share/icons/hicolor/512x512/apps/yzendris.png"
DESKTOP_FILE="$HOME/.local/share/applications/yzendris-client.desktop"

if [[ -f "$ICON_SRC" ]]; then
    mkdir -p "$(dirname "$ICON_DEST")"
    cp "$ICON_SRC" "$ICON_DEST"
    echo "✓ icon installed: $ICON_DEST"

    mkdir -p "$(dirname "$DESKTOP_FILE")"
    cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Yzendris KVM
Comment=Keyboard and mouse sharing (Hyprland/Wayland)
Exec=$HOME/.local/bin/yzendris-client-wrapper.sh
Icon=yzendris
Categories=Utility;
NoDisplay=true
EOF
    echo "✓ desktop entry installed: $DESKTOP_FILE"

    # Visible launcher for the GUI configurator.
    if [[ -f "$GUI_DEST" ]]; then
        GUI_DESKTOP_FILE="$HOME/.local/share/applications/yzendris-gui.desktop"
        cat > "$GUI_DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Yzendris KVM
Comment=Configure keyboard and mouse sharing
Exec=$GUI_DEST
Icon=yzendris
Categories=Utility;Settings;
EOF
        echo "✓ GUI desktop entry installed: $GUI_DESKTOP_FILE"
    fi

    command -v update-desktop-database &>/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache   &>/dev/null && gtk-update-icon-cache -f "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
fi

# ── 6. systemd user unit ──────────────────────────────────────────────────────
mkdir -p "$(dirname "$UNIT")"
cat > "$UNIT" <<'EOF'
[Unit]
Description=Yzendris KVM client (uinput keyboard/mouse sharing)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/yzendris-client-wrapper.sh
Restart=on-failure
RestartSec=3

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now yzendris-client.service
echo "✓ systemd unit enabled and started"
echo ""
echo "Check status:  systemctl --user status yzendris-client"
echo "Follow logs:   journalctl --user -u yzendris-client -f"
echo ""
echo "First run: check logs for the TLS fingerprint if tls=true in client.toml."
