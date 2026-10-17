//! End-to-end test on real kernel input devices.
//!
//! Starts a host and a client daemon on this machine, pairs them with a code,
//! then presses keys and moves a mouse on *fake physical devices* and checks
//! that the events come out of the client's virtual devices.
//!
//! Needs access to `/dev/uinput` and `/dev/input`, so it is ignored by
//! default:
//!
//! ```sh
//! cargo test -p yzendris --test e2e -- --ignored --nocapture
//! ```
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use evdev::raw_stream::RawDevice;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode};
use yzendris_core::config::{Config, Role};
use yzendris_core::ipc::{Client, Link, Request, Response, Status};

const FAKE: &str = "yz-e2e";
const KEY_F13: u16 = 183;
const TRIO: [u16; 3] = [29, 42, 56]; // LeftCtrl, LeftShift, LeftAlt
const KEY_RIGHT: u16 = 106;
const BTN_LEFT: u16 = 0x110;

struct Daemon {
    child: Child,
    home: PathBuf,
}

impl Daemon {
    fn start(home: &Path, config: &Config) -> Self {
        std::fs::create_dir_all(home).unwrap();
        config.save(&home.join("config.toml")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_yzendris"))
            .arg("run")
            .env("YZENDRIS_HOME", home)
            .env("YZENDRIS_CAPTURE_ONLY", FAKE)
            .env("YZENDRIS_LOG", "debug")
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let daemon = Self {
            child,
            home: home.to_owned(),
        };
        wait_for("daemon to answer", || {
            daemon.try_call(Request::Status).is_some()
        });
        daemon
    }

    fn try_call(&self, request: Request) -> Option<Response> {
        Client::connect_to(&self.home.join("control.sock"))
            .ok()?
            .call(&request)
            .ok()
    }

    fn call(&self, request: Request) -> Response {
        self.try_call(request).expect("daemon reachable")
    }

    fn status(&self) -> Status {
        match self.call(Request::Status) {
            Response::Status(s) => *s,
            other => panic!("unexpected {other:?}"),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.try_call(Request::Quit);
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn fake_device(name: &str, mouse: bool) -> VirtualDevice {
    let mut keys = AttributeSet::<KeyCode>::new();
    let builder = VirtualDevice::builder()
        .expect("access to /dev/uinput")
        .name(name)
        .input_id(InputId::new(BusType::BUS_USB, 0x1234, 0x5678, 1));
    if mouse {
        keys.insert(KeyCode(BTN_LEFT));
        let mut axes = AttributeSet::<RelativeAxisCode>::new();
        axes.insert(RelativeAxisCode::REL_X);
        axes.insert(RelativeAxisCode::REL_Y);
        axes.insert(RelativeAxisCode::REL_WHEEL);
        builder
            .with_keys(&keys)
            .unwrap()
            .with_relative_axes(&axes)
            .unwrap()
            .build()
            .unwrap()
    } else {
        (1..=248).for_each(|c| keys.insert(KeyCode(c)));
        builder.with_keys(&keys).unwrap().build().unwrap()
    }
}

/// Open the newest input device with this exact name, non-blocking.
fn open_named(name: &str) -> RawDevice {
    use std::os::unix::fs::OpenOptionsExt;
    let mut found = None;
    wait_for(name, || {
        found = std::fs::read_dir("/dev/input")
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
            .filter_map(|e| {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(e.path())
                    .ok()?;
                let dev = RawDevice::from_fd(file.into()).ok()?;
                (dev.name() == Some(name)).then_some(dev)
            })
            .last();
        found.is_some()
    });
    found.unwrap()
}

/// Everything the device reported within `window`, as (type, code, value).
fn collect(dev: &mut RawDevice, window: Duration) -> Vec<(u16, u16, i32)> {
    let deadline = Instant::now() + window;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        if let Ok(events) = dev.fetch_events() {
            out.extend(
                events
                    .filter(|e| e.event_type() != EventType::SYNCHRONIZATION)
                    .map(|e| (e.event_type().0, e.code(), e.value())),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    out
}

/// The real cursor position, when an X11 session is reachable.
fn x11_cursor() -> Option<(i32, i32)> {
    let out = Command::new("xdotool")
        .args(["getmouselocation", "--shell"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.parse::<i32>().ok())
    };
    Some((field("X=")?, field("Y=")?))
}

fn key(dev: &mut VirtualDevice, code: u16, down: bool) {
    dev.emit(&[InputEvent::new(EventType::KEY.0, code, i32::from(down))])
        .unwrap();
    std::thread::sleep(Duration::from_millis(15));
}

fn tap(dev: &mut VirtualDevice, code: u16) {
    key(dev, code, true);
    key(dev, code, false);
}

/// Hold Ctrl+Shift+Alt, optionally tap `chord`, release.
fn trio(dev: &mut VirtualDevice, chord: Option<u16>) {
    TRIO.iter().for_each(|k| key(dev, *k, true));
    if let Some(k) = chord {
        tap(dev, k);
    }
    TRIO.iter().rev().for_each(|k| key(dev, *k, false));
}

#[test]
#[ignore = "needs /dev/uinput and /dev/input access"]
fn keyboard_and_mouse_travel_from_host_to_client() {
    let tmp = tempfile::tempdir().unwrap();
    let port = 20000 + (std::process::id() % 20000) as u16;

    // Fake "physical" devices exist before the host starts capturing.
    let mut fake_kbd = fake_device(&format!("{FAKE}-keyboard"), false);
    let mut fake_mouse = fake_device(&format!("{FAKE}-mouse"), true);
    std::thread::sleep(Duration::from_millis(300));

    let mut host_cfg = Config {
        role: Some(Role::Host),
        name: "e2e-host".into(),
        ..Config::default()
    };
    host_cfg.host.port = port;
    host_cfg.host.bind = "127.0.0.1".into();
    host_cfg.clipboard = false;
    let client_cfg = Config {
        role: Some(Role::Client),
        name: "e2e-client".into(),
        clipboard: false,
        ..Config::default()
    };

    let host = Daemon::start(&tmp.path().join("host"), &host_cfg);
    let client = Daemon::start(&tmp.path().join("client"), &client_cfg);
    wait_for("host to listen", || host.status().port == Some(port));
    assert!(
        host.status()
            .notices
            .iter()
            .all(|n| format!("{:?}", n.code) != "CaptureDenied"),
        "{:?}",
        host.status().notices
    );

    // ── Pairing ────────────────────────────────────────────────────────────
    let addr = format!("127.0.0.1:{port}");
    // Without an open window, and with a wrong code, nothing is accepted.
    let refused = client.call(Request::PairJoin {
        addr: addr.clone(),
        code: "000000".into(),
    });
    assert!(matches!(refused, Response::Error { .. }), "{refused:?}");
    assert!(matches!(host.call(Request::PairOpen), Response::Ok));
    let code = host.status().pairing.expect("pairing window").code;
    let wrong = if code == "111111" { "222222" } else { "111111" };
    let refused = client.call(Request::PairJoin {
        addr: addr.clone(),
        code: wrong.into(),
    });
    assert!(matches!(refused, Response::Error { .. }), "{refused:?}");
    assert!(host.status().peers.is_empty());

    let joined = client.call(Request::PairJoin { addr, code });
    assert!(matches!(joined, Response::Ok), "{joined:?}");
    assert!(
        host.status().pairing.is_none(),
        "window closes after success"
    );
    wait_for("client to connect", || {
        host.status()
            .peers
            .first()
            .is_some_and(|p| p.link == Link::Online)
    });
    wait_for("client to see the host", || {
        client
            .status()
            .peers
            .first()
            .is_some_and(|p| p.link == Link::Online)
    });
    assert_eq!(host.status().peers[0].name, "e2e-client");
    assert_eq!(client.status().peers[0].name, "e2e-host");

    let mut out_kbd = open_named("Yzendris Keyboard");
    let mut out_mouse = open_named("Yzendris Mouse");
    let mut out_pointer = open_named("Yzendris Pointer");
    std::thread::sleep(Duration::from_millis(400)); // let the host pick up nothing new

    // ── Local input stays local ────────────────────────────────────────────
    tap(&mut fake_kbd, KEY_F13);
    assert!(
        collect(&mut out_kbd, Duration::from_millis(300)).is_empty(),
        "nothing is forwarded while local"
    );

    // ── Switch to the client with the shortcut ─────────────────────────────
    trio(&mut fake_kbd, Some(KEY_RIGHT));
    wait_for("focus on the client", || {
        host.status().active.as_deref() == Some("e2e-client")
    });
    wait_for("client to know it", || {
        client.status().active.as_deref() == Some("e2e-host")
    });
    let entered = collect(&mut out_pointer, Duration::from_millis(300));
    assert!(
        entered.iter().any(|e| e.0 == EventType::ABSOLUTE.0),
        "cursor placed on entry: {entered:?}"
    );
    // On a real X11 desktop, check the cursor landed exactly where the host
    // aimed: the centre of the client's first screen.
    let on_desktop = x11_cursor().filter(|_| std::env::var_os("YZ_E2E_DESKTOP").is_some());
    if let Some(at) = on_desktop {
        let screen = client.status().screens[0].rect;
        let centre = (screen.x + screen.w / 2, screen.y + screen.h / 2);
        assert!(
            (at.0 - centre.0).abs() <= 1 && (at.1 - centre.1).abs() <= 1,
            "cursor at {at:?}, expected {centre:?}"
        );
    }
    collect(&mut out_kbd, Duration::from_millis(100));

    tap(&mut fake_kbd, KEY_F13);
    let keys = collect(&mut out_kbd, Duration::from_millis(400));
    assert_eq!(
        keys,
        [
            (EventType::KEY.0, KEY_F13, 1),
            (EventType::KEY.0, KEY_F13, 0)
        ]
    );

    fake_mouse
        .emit(&[
            InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, 40),
            InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, -25),
        ])
        .unwrap();
    let moved = collect(&mut out_pointer, Duration::from_millis(400));
    assert!(
        moved.iter().any(|e| e.0 == EventType::ABSOLUTE.0),
        "pointer moved: {moved:?}"
    );
    if let (Some(before), Some(after)) = (on_desktop, x11_cursor()) {
        println!("real cursor moved {before:?} → {after:?}");
        assert!(
            after.0 > before.0 && after.1 < before.1,
            "cursor should move right and up"
        );
    }

    key(&mut fake_mouse, BTN_LEFT, true);
    key(&mut fake_mouse, BTN_LEFT, false);
    fake_mouse
        .emit(&[InputEvent::new(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_WHEEL.0,
            -1,
        )])
        .unwrap();
    let mouse = collect(&mut out_mouse, Duration::from_millis(400));
    assert!(
        mouse.contains(&(EventType::KEY.0, BTN_LEFT, 1))
            && mouse.contains(&(EventType::KEY.0, BTN_LEFT, 0)),
        "{mouse:?}"
    );
    assert!(
        mouse.contains(&(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, -1)),
        "{mouse:?}"
    );
    assert!(
        mouse.contains(&(
            EventType::RELATIVE.0,
            RelativeAxisCode::REL_WHEEL_HI_RES.0,
            -120
        )),
        "{mouse:?}"
    );

    // ── A key held when the host disappears is released on the client ──────
    key(&mut fake_kbd, KEY_F13, true);
    assert_eq!(
        collect(&mut out_kbd, Duration::from_millis(300)),
        [(EventType::KEY.0, KEY_F13, 1)]
    );
    key(&mut fake_kbd, KEY_F13, false);
    collect(&mut out_kbd, Duration::from_millis(200));

    // ── Tap Ctrl+Shift+Alt: input comes home ───────────────────────────────
    trio(&mut fake_kbd, None);
    wait_for("focus back on the host", || host.status().active.is_none());
    wait_for("client released", || client.status().active.is_none());
    collect(&mut out_kbd, Duration::from_millis(200));
    tap(&mut fake_kbd, KEY_F13);
    assert!(
        collect(&mut out_kbd, Duration::from_millis(300)).is_empty(),
        "local again"
    );

    // ── Crossing by screen edge, on a desktop where the cursor is visible ──
    if on_desktop.is_some() {
        let push = |mouse: &mut VirtualDevice, dx: i32| {
            mouse
                .emit(&[InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_X.0,
                    dx,
                )])
                .unwrap();
            std::thread::sleep(Duration::from_millis(12));
        };
        // The client sits on the right of the host's screen: push right.
        let mut crossed = false;
        for _ in 0..200 {
            push(&mut fake_mouse, 120);
            if host.status().active.is_some() {
                crossed = true;
                break;
            }
        }
        assert!(crossed, "pushing through the right edge enters the client");
        // …and pushing back through the client's left edge returns.
        let mut returned = false;
        for _ in 0..50 {
            push(&mut fake_mouse, -40);
            if host.status().active.is_none() {
                returned = true;
                break;
            }
        }
        assert!(returned, "pushing back through the facing edge comes home");
        std::thread::sleep(Duration::from_millis(300));
        let home = x11_cursor().unwrap();
        let screen = host.status().screens[0].rect;
        println!(
            "edge crossing ok; cursor returned to {home:?} on a {}×{} screen",
            screen.w, screen.h
        );
        assert!(
            home.0 > screen.x + screen.w - 60 && home.0 < screen.x + screen.w,
            "cursor reappears just inside the edge"
        );
    }

    // ── Losing the host releases whatever it was holding ───────────────────
    trio(&mut fake_kbd, Some(KEY_RIGHT));
    wait_for("focus on the client", || host.status().active.is_some());
    collect(&mut out_kbd, Duration::from_millis(200));
    key(&mut fake_kbd, KEY_F13, true);
    assert_eq!(
        collect(&mut out_kbd, Duration::from_millis(300)),
        [(EventType::KEY.0, KEY_F13, 1)]
    );
    drop(host);
    let released = collect(&mut out_kbd, Duration::from_millis(1500));
    assert!(
        released.contains(&(EventType::KEY.0, KEY_F13, 0)),
        "stuck key released: {released:?}"
    );
    wait_for("client to notice", || {
        client.status().peers[0].link != Link::Online
    });

    // ── Forgetting the host unpairs ─────────────────────────────────────────
    let fp = client.status().peers[0].fingerprint;
    assert!(matches!(
        client.call(Request::Forget { fingerprint: fp }),
        Response::Ok
    ));
    assert!(client.status().peers.is_empty());
}
