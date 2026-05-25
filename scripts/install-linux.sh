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
DEST="$HOME/.local/bin/yzendris-client"
WRAPPER="$HOME/.local/bin/yzendris-client-wrapper.sh"
UNIT="$HOME/.config/systemd/user/yzendris-client.service"
CFG_DIR="$HOME/.config/yzendris"
CFG="$CFG_DIR/client.toml"
PORT=7547
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── 1. Copy binary ────────────────────────────────────────────────────────────
mkdir -p "$(dirname "$DEST")"
install -m 755 "$BINARY" "$DEST"
echo "✓ installed $DEST"

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
    SIG=$(ls -1 "$HIS_DIR" 2>/dev/null | head -1)
    [ -n "$SIG" ] && export HYPRLAND_INSTANCE_SIGNATURE="$SIG"
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

# ── 5. systemd user unit ──────────────────────────────────────────────────────
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
