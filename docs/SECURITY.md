# Security model

Yzendris moves keystrokes between computers and can type on them. This page
says exactly what protects that, and what does not.

## What an attacker on your network can and cannot do

**Cannot read what you type.** Everything travels inside TLS 1.3 (rustls).

**Cannot type on your machines.** Each machine has its own self-signed
certificate, generated on first run and never transmitted anywhere but in the
TLS handshake. Machines recognise each other by the SHA-256 of that
certificate, both ways:

- A client only talks to the one host it paired with. A different certificate
  fails the handshake before the client has sent anything.
- A host only opens a session for clients in its paired list. Anyone else is
  told "not paired" and disconnected.

**Cannot pair without the code.** Pairing uses SPAKE2 (a password-authenticated
key exchange) with the six-digit code, and both sides then prove they saw the
same pair of certificates. Someone relaying or intercepting the connection
gets exactly one guess per attempt, learns nothing from a wrong one, and the
host closes the pairing window after three failures or two minutes. A host
only answers network discovery while that window is open, or to a machine
that already knows its fingerprint (so a paired client can find it again).

**Can** see that two machines are talking, and can block the connection.

## What a paired machine can do

Pairing is trust. A paired **host** can type and click on its clients — that
is the feature. A paired **client** can do much less: report its screen size,
and send clipboard text and files to the host when the cursor leaves it. It
can never send input to the host.

Turn off *Clipboard* in Settings if you do not want either.

## Files received through the clipboard

- Written to a private per-transfer folder (`~/.cache/yzendris/incoming`,
  mode 0700; `%LOCALAPPDATA%\yzendris\incoming` on Windows), deleted after 24
  hours.
- Names are reduced to a safe bare file name; a file can never be written
  outside that folder, through a symlink, or over an existing file.
- Size is checked against your limit before anything is written, and a sender
  cannot exceed what it announced.
- Never opened or executed. They are only placed on the clipboard so *you* can
  paste them.

## What runs with which rights

Yzendris runs as your user. There is no root daemon and no setuid binary.

**Linux.** `yzendris install` asks for administrator rights once to write:

- `/etc/udev/rules.d/70-yzendris.rules` — tags `/dev/uinput` with `uaccess`,
  which gives the user *physically logged in at the seat* permission to create
  virtual input devices. This is narrower than the common advice of adding
  yourself to the `input` group.
- For the **host** role only, the same tag on input event devices, so the
  service can read your keyboard and mouse. Be aware of what this means: any
  program running as you at that seat can then read keystrokes too. On X11
  that was already true; on Wayland it is a real reduction in isolation, and
  it is the price of capturing input without a compositor-specific protocol.
  Clients do not need or get this.
- A firewall rule for the listening port, on hosts with firewalld or an active
  ufw (limited to private address ranges with ufw).

The exact script is in [`install.rs`](../crates/daemon/src/install.rs)
(`ROOT_SCRIPT`); it takes no input other than the role and port number.

**Windows.** Installed per user under `%LOCALAPPDATA%\Programs\Yzendris` and
started from the user's `Run` key. The only elevated action is an optional
firewall rule.

## Local control interface

The app and CLI talk to the service over a Unix socket in `$XDG_RUNTIME_DIR`
(mode 0600, inside a directory only you can enter, and the peer's uid is
verified).

On Windows it is a named pipe that rejects remote clients and that other
standard users cannot write to. One limitation remains there: pipe names are a
machine-wide namespace, so on a PC *shared with other people you do not
trust*, another signed-in user could create the pipe before your service
starts. Your service would then refuse to start, and the app would be talking
to theirs. It cannot make your machine accept input — pairing still needs the
code exchange with the real host — but treat Yzendris on Windows as intended
for single-user PCs.

A paired client may hand the host its clipboard only in the seconds after the
cursor leaves it; clipboard data sent at any other moment is ignored.

## Privacy

No telemetry, no accounts, no connection to anything but your own paired
machines. Logs never contain keys, clipboard contents or file names.

## Not covered

- A machine that is already compromised. Anything running as you can read
  your config, including the private key.
- The network beyond your LAN. Do not forward the port to the internet; use a
  VPN (WireGuard, Tailscale) if the machines are on different networks.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/iCleyvin/yzendris/security/advisories/new).
