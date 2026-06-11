# Yzendris — share one keyboard and mouse across Windows and Linux

**Yzendris is a lightweight, open-source software KVM**: control several
computers from a single keyboard and mouse, moving the cursor between their
screens over your LAN. The host runs on **Windows**; clients run on **Linux
(Wayland / Hyprland)** or **Windows**. Written in Rust, encrypted with TLS,
configured entirely from a graphical app — no terminal required.

It's an alternative to **Synergy, Barrier, Input Leap, Deskflow and Mouse
Without Borders** with one thing they get wrong on modern Linux: it correctly
delivers modifier keys (`Super`, `Ctrl`, `Alt`) to **Wayland compositors like
Hyprland**, so window-manager shortcuts (`Super+Q`, `Super+1`, `Ctrl+Alt+T`)
actually fire.

**Highlights**

- 🖱️ Move the cursor between machines by crossing a screen edge — and back.
- 🧩 Any monitor arrangement: a laptop **between two monitors** (side by side or
  stacked), or at an outer edge. Works in 2D.
- 👥 **Multiple clients at once** — share to several computers, each at its own
  spot.
- ⌨️ Full keyboard incl. modifiers + WM shortcuts, all mouse buttons, scroll.
- 📋 Two-way clipboard, 🔒 TLS with fingerprint pinning, ♻️ auto-reconnect.
- 🪟 **Graphical configurator** that installs and starts everything for you.

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
- **Any monitor arrangement**: place a client between any two adjacent
  monitors — **side by side** *or* **stacked** (one above the other, crossing by
  the top/bottom edge) — or at an outer edge. PC screen 1 → laptop → PC screen 2,
  seamlessly in both directions.
- **Multiple clients at once**: the host can share to several machines
  simultaneously, each at its own boundary/edge (`[[clients]]`).
- **Graphical configurator is the main path** (`yzendris-gui`): one app for both
  machines — on first run it asks whether the machine is the Host or a Client,
  detects your monitors, lets you add clients and place each one, manages TLS
  pairing, and with one click **installs everything** (copies the program, opens
  the firewall, enables autostart). No terminal or config files required.
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
- No packaged installer — binaries plus install scripts.
- No clipboard sync for binary/image clipboards — text only.
- The Host role runs on Windows. The Client runs on Linux (uinput/Hyprland) **or
  Windows (SendInput)** — handy for a dual-boot laptop that sits between the
  monitors regardless of which OS it's in. The GUI runs on both.

## Requirements

| Side    | Needs                                                                 |
| ------- | --------------------------------------------------------------------- |
| Windows | Rust toolchain (stable) to build, PowerShell 5+ for the install script |
| Linux   | Rust toolchain (stable), user in the `input` group (`usermod -aG input $USER`), Hyprland (or any wlroots compositor), `wl-clipboard` if you want clipboard sync |

Tested daily on Hyprland 0.55.x with Omarchy on CachyOS, talking to Windows 11.

## Install

### 1. Build

You need a stable [Rust toolchain](https://rustup.rs). Build the three binaries
(`yzendris-server`, `yzendris-client`, `yzendris-gui`) on each machine for its OS:

```bash
# On the Windows host
cargo build --release -p yzendris-server -p yzendris-gui

# On a client (Linux or Windows)
cargo build --release -p yzendris-client -p yzendris-gui
```

The compiled programs land in `target/release/`.

### 2. Set up — the easy way (GUI, recommended)

Run **`yzendris-gui`** on each machine and follow it:

1. **Pick a role** — *Host* (the PC with the physical keyboard/mouse) or
   *Cliente* (a machine that receives them).
2. **Host panel:** add each client (name + LAN IP), choose **where it sits**
   from the dropdown (between two monitors, or an outer edge), and tick *TLS*.
   **Client panel:** set the listen port (default 7547); leave *TLS* on.
3. Click **⚙ Instalar y habilitar inicio automático** on each machine. With one
   administrator prompt it copies the program, opens the firewall and enables
   autostart — nothing else to do by hand.
4. **Pair TLS once:** on each client the panel shows a fingerprint
   (`sha256:…`) — copy it and paste it into the host's *Huellas TLS confiables*
   box. (Skip if you set `tls = false`.)
5. Press **▶ Iniciar** on both. The top of the host panel shows
   *“Servidor en ejecución”* and a green dot per connected client.

That's it — move the cursor across the edge and you're on the other machine.
Push the client screen's edge facing the PC to come back; `Ctrl+Shift+Alt`
always brings control home.

### 2-alt. Set up — scripts (no GUI)

```bash
# Linux client (run inside the Wayland session)
./scripts/install-linux.sh
```
```powershell
# Windows host (PowerShell as Administrator)
.\scripts\install-windows.ps1
# Windows client (a laptop booted into Windows)
.\scripts\install-windows-client.ps1
```

These copy the binary to the per-user dir, write a default config, open the
firewall (outbound on the host, inbound on a client) and set up autostart
(systemd user unit on Linux, Startup shortcut / scheduled task on Windows).
Then edit the config (see the reference below) and pair TLS as in step 4.

### Manual pairing reference

Each client generates a self-signed certificate on first run with `tls = true`
and prints its SHA-256 fingerprint:

```bash
# Linux
journalctl --user -u yzendris-client -e   # → "TLS fingerprint: sha256:aa:bb:…"
# Windows: see %APPDATA%\yzendris\client.log, or the GUI Cliente panel
```

Paste each fingerprint (one per line, `#` comments allowed) into the host's
`%APPDATA%\yzendris\trusted_peers.txt`, then restart the host.

## Configuration reference

### `server.toml` (host) — the GUI writes this for you

```toml
heartbeat_ms = 1000   # heartbeat interval (ms); a client drops at ~5× this
clipboard    = true   # sync clipboard text

[[clients]]           # one block per client machine
name = "laptop"
addr = "192.168.1.42" # the client's LAN IP
port = 7547
tls  = true
between = ["DISPLAY1", "DISPLAY2"]   # placement: between these two monitors
# edge = "right"                     # …or an outer edge: right/left/top/bottom
```

| Field (per `[[clients]]`) | Notes |
| ------------------------- | ----- |
| `name`                    | Friendly label shown in logs/GUI |
| `addr` / `port`           | The client's LAN address and listen port |
| `tls`                     | Verify the client's cert fingerprint (keep on) |
| `between = ["A", "B"]`    | Laptop sits between monitors A and B (device names like `DISPLAY1`). Side-by-side or stacked is auto-detected. |
| `edge`                    | Or place it past an outer edge: `right`/`left`/`top`/`bottom` |

Add more `[[clients]]` blocks to share to several machines at once. The legacy
single-client form (top-level `client_addr`/`port`/`edge`/`tls` + `[layout]`) is
still read for backward compatibility.

### `client.toml` (Linux or Windows)

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

## Related projects & alternatives

Yzendris is a software KVM in the same space as these — it focuses on getting
Wayland modifier keys right and on a no-terminal graphical setup:

- [feschber/lan-mouse](https://github.com/feschber/lan-mouse) — tried first;
  great Linux-to-Linux. The Wayland portal limitation is upstream.
- [deskflow/deskflow](https://github.com/deskflow/deskflow) — the Synergy fork;
  same Wayland modifier limitation today.
- [Synergy](https://symless.com/synergy) /
  [Input Leap](https://github.com/input-leap/input-leap) — the classics.
- [Microsoft Mouse Without Borders](https://github.com/microsoft/PowerToys) —
  Windows-to-Windows only.
- [htrefil/rkvm](https://github.com/htrefil/rkvm) — Linux-only; its uinput
  approach inspired Yzendris's client side.

## License

MIT. See [LICENSE](LICENSE).

---

<sub>**Keywords:** software KVM · share keyboard and mouse between computers ·
mouse and keyboard sharing over LAN · Windows ↔ Linux KVM · Wayland / Hyprland
keyboard sharing · multi-monitor cursor sharing · open-source Synergy / Barrier
/ Input Leap / Deskflow / Mouse Without Borders alternative · KVM software for
Windows and Linux · move mouse between screens · Rust.</sub>
