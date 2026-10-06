# Architecture

```
crates/
  core/     yzendris-core   protocol, config, layout + focus logic. No I/O. Heavily tested.
  daemon/   yzendris        the background service and CLI
  gui/      yzendris-gui    the control panel (egui)
```

## The moving parts

```
        HOST                                         CLIENT
 physical keyboard/mouse
          │
   platform::Capture ──► Switch ──► Outbox ══ TLS 1.3 ══► session ──► platform::Inject
   (evdev / WH_*_LL)    (core)      (wire)                (client)     (uinput / SendInput)
          ▲                │
          └── Capture(on/off), Warp
```

**`core::switch::Switch`** is the brain, and it is pure: back-ends feed it
"the mouse moved by (dx, dy), the cursor is now at P" or "key 30 went down",
and it returns whether to hide that event locally plus a list of *effects*
(send this message to that client, start capturing, move the local cursor).
Edge crossing, shortcuts, "never switch mid-drag", pointer lock — all of it is
decided there and unit-tested without any OS.

**The host owns every cursor.** While a client has focus, the host tracks that
client's cursor position itself (`core::layout::RemoteCursor`) and sends
*absolute* coordinates. The client just places the cursor there. This is what
makes clients work on every Wayland compositor: nobody ever has to ask the
compositor where the cursor is, and local pointer-acceleration settings cannot
distort the motion. Raw relative motion is only used while the pointer is
locked (games).

**Layout** (`core::layout`): each client is attached to one side of one host
monitor. If another host monitor touches that side, the client is *between*
the two and crossing their boundary passes through it.

## Wire protocol

`core::proto`: length-prefixed [postcard] frames inside mutually authenticated
TLS 1.3. The first frame each way is `Hello`. Frames are capped at ~1 MiB;
input frames are 3–10 bytes. The writer coalesces consecutive pointer
positions, so a congested link drops stale motion instead of lagging.

Clients connect to the host. Discovery is a tiny UDP question/answer on port
7547 and is only a hint — trust always comes from the TLS identity.

## Service ⇄ app

The GUI and CLI are thin clients of the service over a local socket
(`core::ipc`): one JSON request per line, one JSON response. The service is
the only writer of `config.toml`.

## Adding a platform

Everything OS-specific lives in `crates/daemon/src/platform/<os>/` and must
provide the items listed at the top of `platform/mod.rs`:

| Item | Purpose |
|---|---|
| `prepare()` | one-time process setup |
| `screens()` | list monitors as `core::layout::Screen` |
| `injector(layout)` | a `platform::Inject`: place cursor, move, click, scroll, key |
| `Capture` | observe physical input, forward it to a `platform::Sink`, grab on demand |
| `local_clock()` | wall-clock time for log lines |

`platform/unsupported.rs` is the template. Keys cross the wire as Linux
`KEY_*` codes; `core::keymap` shows how Windows maps them, and a macOS
back-end would add its own table there.

To give a Linux desktop edge switching as a host, add a variant to
`platform/linux/probe.rs` that can report (and ideally set) the cursor
position.

## Testing

```sh
cargo test --workspace                                        # unit tests
cargo test -p yzendris --test e2e -- --ignored --nocapture    # real kernel devices (Linux)
YZ_E2E_DESKTOP=1 cargo test -p yzendris --test e2e -- --ignored   # also checks the real X11 cursor
yzendris-gui --demo host|pairing|active|empty|client|join|welcome [--theme light] [--lang es]
```

`YZENDRIS_HOME=<dir>` relocates config, cache and the control socket, which
lets a host and a client run side by side on one machine.
`YZENDRIS_CAPTURE_ONLY=<text>` limits a Linux host to input devices whose name
contains that text. `YZENDRIS_LOG=debug` raises verbosity.

[postcard]: https://docs.rs/postcard
