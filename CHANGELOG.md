# Changelog

## 0.2.0

A ground-up rewrite. Not compatible with 0.1 — install on every machine and
pair again.

### New
- **Any Linux desktop as a client.** Three standard virtual devices through
  `uinput` (keyboard, mouse, absolute pointer) replace the Hyprland-only code
  path. The host sends absolute cursor positions, so no compositor has to be
  asked where the cursor is.
- **Linux as a host**, capturing through evdev. Screen-edge switching on
  Hyprland and X11; keyboard shortcuts everywhere.
- **Pairing with a six-digit code** (SPAKE2 bound to both TLS identities)
  instead of copying fingerprints by hand, with LAN discovery.
- **Clients connect to the host**, so only the host listens on the network and
  clients keep working when their address changes.
- **Shortcuts:** `Ctrl+Shift+Alt` + arrows / digits / `L`; tapping the three
  alone still brings input home.
- **Pointer lock** for games (raw relative motion, edges disabled).
- **New app:** drag-to-arrange screens, guided setup, light/dark, English and
  Spanish.
- **One service, one config file, one CLI** (`yzendris`), with `install`,
  `doctor`, `status`, `pair`, `join`.
- Installers for Linux (any systemd distribution, including immutable ones
  such as Bazzite) and Windows that need no manual steps.

### Security
- TLS 1.3 only; the client pins the host inside the handshake, before sending
  its own certificate.
- Received files go to a private per-transfer directory, with sanitized names,
  exclusive creation (no symlink following, no overwrite), enforced sizes and
  counts. Previously they went to a predictable path in `/tmp`.
- Linux no longer needs the `input` group; a `uaccess` udev rule grants device
  access to the seated user only.
- Windows: no longer changes the system-wide mouse acceleration setting; no
  PowerShell built from interpolated paths.
- Nothing sensitive in logs (no key codes, clipboard contents or file names),
  and the Windows log is size-capped.

### Fixed
- Display scaling on Windows (now per-monitor DPI aware).
- Keys stuck down on the client after the host disappears.
- Cursor never leaves a machine mid-drag or mid-keystroke.
- Media keys, Pause/NumLock and Print Screen are mapped correctly.

### Performance
- Single-threaded service: about 6 MB of memory and no measurable CPU while
  connected and idle. No polling, no helper processes.
- Pointer positions are coalesced when the link is congested.
