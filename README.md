# Yzendris

A KVM (keyboard/mouse sharing) tool that actually handles `Super`, `Ctrl`, `Alt`
when the host is **Windows** and the client is a **wlroots Wayland compositor**
(Hyprland, Omarchy, etc.).

If you've been stuck on
[feschber/lan-mouse#446](https://github.com/feschber/lan-mouse/issues/446)
or fighting with Deskflow / Input Leap / Synergy because your `Super+Q` and
`Ctrl+T` never reach your Linux side — this is for you.

## Why it exists

I run a Windows desktop with the physical keyboard and mouse, and a laptop
sitting next to it running Hyprland. I wanted the laptop to be just "the
screen on the right" — move the cursor over and start typing.

Every existing tool failed the same way: cursor crossed fine, regular keys
worked, but **the modifiers never reached Hyprland's bind system**. Turns out
they all forward input through Wayland's `libei`, which goes through the
`org.freedesktop.portal.RemoteDesktop` portal, which `xdg-desktop-portal-hyprland`
does not implement at the time of writing.

Yzendris skips Wayland entirely on the client side:

```
Windows host                              Linux client
─────────────                             ─────────────
LowLevelKeyboardHook ─┐                   TCP listener
LowLevelMouseHook    ─┤── TCP + TLS ──→   /dev/uinput  (kernel)
edge detection         (bincode framed)     └─→ libinput → compositor
capture state                               hyprctl keyword (apply layout)
```

`/dev/uinput` is kernel-level. The compositor sees a virtual keyboard exactly
like a USB one plugged in. Modifiers work. Binds fire.

## What it currently does

- Mouse cursor crosses by hitting a configured screen edge (default: right),
  and **crosses back** when you push the matching edge of the client screen —
  no key combo needed (the combo `Ctrl+Shift+Alt` still works as an escape).
- **Laptop-in-the-middle layouts**: with `[layout] mode = "between"` the laptop
  sits between two of the PC's monitors — PC screen 1 → laptop → PC screen 2,
  seamlessly in both directions.
- **Graphical configurator** (`yzendris-gui`): one app for both machines — on
  first run it asks whether the machine is the Host or a Client, detects your
  monitors, lets you place the laptop visually, manages TLS pairing, and
  starts/stops the daemon.
- All keys including `Super` / `Ctrl` / `Alt` and combinations of them work.
- Mouse buttons (L/M/R/side1/side2), scroll (vertical and horizontal).
- Bidirectional clipboard sync on capture transitions.
- TLS with SHA-256 fingerprint pinning (enabled by default).
- Auto-reconnect with exponential backoff.
- Auto-detects the keyboard layout from `hyprctl devices` and applies it to the
  virtual device (without this step, Hyprland doesn't recognise modifiers — see
  the long comment in `crates/client/src/hyprland.rs`).
- systemd user unit on Linux, startup shortcut on Windows.

## What it doesn't do (yet)

- Hyprland-specific runtime layout assignment. On Sway/Niri/river the install
  works but you might need to set `kb_layout` manually in `client.toml` (or
  rely on your compositor's global keyboard config — global config DOES apply
  to the virtual device, so it usually just works).
- No packaged installer — binaries plus two install scripts.
- No clipboard sync for binary/image clipboards — text only.
- The Host role runs on Windows and the Client role on Linux (that's the
  combination the project exists to fix). The GUI runs on both.

## Requirements

| Side    | Needs                                                                 |
| ------- | --------------------------------------------------------------------- |
| Windows | Rust toolchain (stable) to build, PowerShell 5+ for the install script |
| Linux   | Rust toolchain (stable), user in the `input` group (`usermod -aG input $USER`), Hyprland (or any wlroots compositor), `wl-clipboard` if you want clipboard sync |

Tested daily on Hyprland 0.55.x with Omarchy on CachyOS, talking to Windows 11.

## Install

### Build

```bash
# Windows
cargo build --release -p yzendris-server -p yzendris-gui

# Linux
cargo build --release -p yzendris-client -p yzendris-gui
```

### Install (Linux, in the wlroots session)

```bash
./scripts/install-linux.sh
```

This copies the binary, writes a wrapper script that injects the Wayland
environment, writes a default config, opens `ufw` for TCP 7547, and enables
the systemd user unit so the client starts with the graphical session.

### Install (Windows, PowerShell as Administrator)

```powershell
.\scripts\install-windows.ps1
```

Copies the binary to `%APPDATA%\yzendris\`, writes a default config, adds an
outbound firewall rule, and creates a Startup folder shortcut so the server
launches at login.

### Configure

The easy way: run `yzendris-gui` on each machine. It asks whether the machine
is the **Host** (shares its keyboard/mouse) or a **Client** (receives them),
then shows the matching panel — connection settings, monitor arrangement with
laptop placement, TLS pairing, and start/stop buttons.

The manual way: edit `%APPDATA%\yzendris\server.toml` on Windows and set
`client_addr` to your Linux machine's LAN IP. Leave `tls = true`. If your
laptop sits between two PC monitors, add the `[layout]` section (see the
configuration reference below).

### Pair (one-time, takes ~30 seconds)

1. Start the Linux client. It prints the SHA-256 fingerprint of its self-signed
   cert to stderr / journal on first run:
   ```
   journalctl --user -u yzendris-client -e
   # → look for: TLS fingerprint: sha256:aa:bb:cc:...
   ```
2. Copy that whole line and paste it into `%APPDATA%\yzendris\trusted_peers.txt`
   on Windows (create the file if it doesn't exist — one fingerprint per line,
   `#` comments allowed).
3. Restart the Windows server.

(With the GUI: copy the fingerprint shown in the Client panel and paste it in
the Host panel's "Huellas TLS confiables" box.)

That's it. Cursor goes right → laptop takes over. Push the client screen's
edge facing the PC → cursor comes back. `Ctrl+Shift+Alt` also brings it back
from anywhere.

## Configuration reference

### `server.toml` (Windows)

| Field          | Default            | Notes |
| -------------- | ------------------ | ----- |
| `client_addr`  | `"192.168.1.42"`   | LAN IP of the Linux client (edit this!) |
| `port`         | `7547`             | TCP port the client listens on |
| `edge`         | `"right"`          | `right` / `left` / `top` / `bottom` (classic mode) |
| `heartbeat_ms` | `1000`             | Heartbeat interval. Client gives up at 5× this. |
| `clipboard`    | `true`             | Sync clipboard on capture/release |
| `tls`          | `true`             | Verify peer fingerprint. Keep on. |

Optional `[layout]` table — laptop between two monitors:

| Field           | Example      | Notes |
| --------------- | ------------ | ----- |
| `mode`          | `"between"`  | `"edge"` (default) or `"between"` |
| `monitor_left`  | `"DISPLAY1"` | Windows device name of the monitor LEFT of the laptop |
| `monitor_right` | `"DISPLAY2"` | Monitor RIGHT of the laptop |

In `between` mode, crossing the boundary between the two monitors routes the
cursor through the laptop in both directions. The GUI detects monitor names
and writes this section for you.

### `client.toml` (Linux)

| Field                  | Default        | Notes |
| ---------------------- | -------------- | ----- |
| `port`                 | `7547`         | Listen port |
| `bind_addr`            | `"0.0.0.0"`    | Bind interface; tighten if you want |
| `kb_layout`            | `""`           | Empty = auto-detect via `hyprctl` |
| `heartbeat_timeout_ms` | `5000`         | Release all keys if no heartbeat for this long |
| `clipboard`            | `true`         | Needs `wl-clipboard` |
| `tls`                  | `true`         | First run generates `cert.pem` + prints fingerprint |

## Security model — read this before exposing it on a hostile network

- All keystrokes go over the wire. **Always run with `tls = true`.** Without
  TLS, anyone on your LAN with `tcpdump` can keylog you.
- The TLS verifier is a custom one that pins the peer cert's SHA-256
  fingerprint. There's no PKI / CA chain. If your machine is compromised
  enough to swap `trusted_peers.txt`, an attacker can MITM the next
  connection — but at that point they already own you.
- The Linux side runs as your user, not as root. It uses `/dev/uinput` via
  the `input` group. No setuid binaries.
- Designed for trusted LANs. **Don't expose port 7547 to the internet.** If
  you really need cross-network operation, tunnel through Tailscale / WireGuard
  / ssh.

## Troubleshooting

**Mouse crosses but keys don't reach Hyprland binds**

Check the kb_layout was applied:
```bash
hyprctl devices | grep -A 5 yzendris-virtual-kb
```
You should see a non-empty `l "..."` (layout). If it's empty, your compositor
isn't Hyprland and runtime layout assignment didn't apply — set `kb_layout`
explicitly in `client.toml`.

**Connection refused / timeout**

UFW or another firewall blocking TCP 7547. The install script tries to handle
this on Ubuntu/Debian/Arch (`ufw`), but other distros vary:
```bash
sudo ufw allow 7547/tcp
# or: sudo firewall-cmd --permanent --add-port=7547/tcp && sudo firewall-cmd --reload
```

**"untrusted server fingerprint" on the Windows side**

You haven't pasted the Linux client's fingerprint into `trusted_peers.txt`
yet, or the cert was regenerated. Get the current one:
```bash
journalctl --user -u yzendris-client | grep fingerprint | tail -1
```

**Stuck modifier after a disconnect**

The client releases all keys on heartbeat timeout (default 5s) and on
`CaptureEnd`. If you somehow end up with a stuck Super:
```bash
systemctl --user restart yzendris-client
```

## Contributing

This is a personal project I maintain in my spare time. I'll respond to issues
and review PRs, but expect days, not hours.

If you're filing a bug:
- Include `journalctl --user -u yzendris-client -e` from the Linux side.
- Include the contents of `%LOCALAPPDATA%\yzendris\server.err.log` from Windows.
- Tell me your compositor and version (`hyprctl version`).

If you're sending a PR, please make it focused — one logical change at a time.

## Related

- [feschber/lan-mouse](https://github.com/feschber/lan-mouse) — the tool I tried
  first. Great on Linux-to-Linux. The portal limitation is upstream and the
  maintainer is open about it.
- [deskflow/deskflow](https://github.com/deskflow/deskflow) — fork of Synergy,
  same Wayland modifier limitation today.
- [htrefil/rkvm](https://github.com/htrefil/rkvm) — Linux-only, but its uinput
  approach inspired Yzendris's client side.

## License

MIT. See [LICENSE](LICENSE).
