//! Reading the host's physical keyboards and mice through evdev.
//!
//! While input belongs to the host the devices are only *observed*, which is
//! enough to notice shortcuts and edge crossings and costs nothing when idle.
//! When input moves to a client the devices are grabbed (`EVIOCGRAB`), so the
//! local desktop stops seeing them until control comes back. The kernel drops
//! the grab by itself if this process ever dies.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use evdev::raw_stream::RawDevice;
use evdev::{EventType, KeyCode, RelativeAxisCode};
use yzendris_core::proto::SCROLL_NOTCH;
use yzendris_core::Point;

use super::probe::CursorProbe;
use super::{PRODUCT, VENDOR};
use crate::platform::Sink;

const INPUT_DIR: &str = "/dev/input";
/// Shortest interval between two cursor-position queries.
const PROBE_SPACING: Duration = Duration::from_millis(6);
/// Wait after a hot-plug notification, so udev can finish setting permissions.
const RESCAN_DELAY: Duration = Duration::from_millis(400);

enum Cmd {
    Grab(bool),
    Warp(Point),
    Stop,
}

pub struct Capture {
    cmds: mpsc::Sender<Cmd>,
    wake: Arc<OwnedFd>,
    sees_cursor: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    pub fn start(sink: Arc<dyn Sink>) -> Result<Self> {
        let devices = scan(&HashMap::new());
        if devices.is_empty() {
            match first_open_error() {
                Some(e) => bail!("cannot read input devices in {INPUT_DIR}: {e}"),
                None => tracing::warn!("no keyboard or mouse found yet; waiting for one"),
            }
        }
        let probe = CursorProbe::detect();
        let sees_cursor = probe.is_some();
        // SAFETY: plain syscall; the result is checked before use.
        let wake_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if wake_fd < 0 {
            return Err(std::io::Error::last_os_error()).context("eventfd");
        }
        // SAFETY: `wake_fd` is a fresh descriptor owned by nobody else.
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(wake_fd) });
        let (cmds, rx) = mpsc::channel();
        let mut worker = Worker {
            sink,
            probe,
            devices,
            rx,
            wake: wake.clone(),
            grabbed: false,
            last_probe: Instant::now(),
            last_motion: Instant::now(),
            rescan_at: None,
        };
        let thread = std::thread::Builder::new()
            .name("input-capture".into())
            .spawn(move || worker.run())
            .context("starting the capture thread")?;
        Ok(Self {
            cmds,
            wake,
            sees_cursor,
            thread: Some(thread),
        })
    }

    fn send(&self, cmd: Cmd) {
        if self.cmds.send(cmd).is_ok() {
            let one = 1u64.to_ne_bytes();
            // SAFETY: writes 8 bytes from a valid buffer to our own eventfd.
            unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), one.len()) };
        }
    }

    pub fn set_capture(&self, on: bool) {
        self.send(Cmd::Grab(on));
    }

    pub fn warp(&self, to: Point) {
        self.send(Cmd::Warp(to));
    }

    /// Whether the cursor position is observable, i.e. edge switching works.
    pub fn sees_cursor(&self) -> bool {
        self.sees_cursor
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.send(Cmd::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Device {
    raw: RawDevice,
    /// The device reports high-resolution wheel events.
    hires_wheel: bool,
    motion: (i32, i32),
    wheel: (i32, i32),
}

struct Worker {
    sink: Arc<dyn Sink>,
    probe: Option<CursorProbe>,
    devices: HashMap<PathBuf, Device>,
    rx: mpsc::Receiver<Cmd>,
    wake: Arc<OwnedFd>,
    grabbed: bool,
    last_probe: Instant,
    last_motion: Instant,
    rescan_at: Option<Instant>,
}

/// Does `path` look like a keyboard or mouse we should handle?
fn open_device(path: &Path) -> std::io::Result<Option<Device>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let raw = RawDevice::from_fd(OwnedFd::from(file))?;
    let id = raw.input_id();
    if id.vendor() == VENDOR && (PRODUCT..PRODUCT + 3).contains(&id.product()) {
        return Ok(None); // one of our own virtual devices
    }
    if let Ok(only) = std::env::var("YZENDRIS_CAPTURE_ONLY") {
        if !raw.name().unwrap_or("").contains(&only) {
            return Ok(None);
        }
    }
    let has_key = |code: u16| {
        raw.supported_keys()
            .is_some_and(|k| k.contains(KeyCode(code)))
    };
    let rel = raw.supported_relative_axes();
    let keyboard = [28, 30, 115, 164].into_iter().any(has_key); // Enter, A, Vol+, Play
    let mouse = has_key(0x110) && rel.is_some_and(|r| r.contains(RelativeAxisCode::REL_X));
    if !keyboard && !mouse {
        return Ok(None);
    }
    let hires_wheel = rel.is_some_and(|r| r.contains(RelativeAxisCode::REL_WHEEL_HI_RES));
    Ok(Some(Device {
        raw,
        hires_wheel,
        motion: (0, 0),
        wheel: (0, 0),
    }))
}

fn event_nodes() -> Vec<PathBuf> {
    std::fs::read_dir(INPUT_DIR)
        .map(|dir| {
            dir.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("event"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Open every usable device that is not already in `known`.
fn scan(known: &HashMap<PathBuf, Device>) -> HashMap<PathBuf, Device> {
    event_nodes()
        .into_iter()
        .filter(|path| !known.contains_key(path))
        .filter_map(|path| match open_device(&path) {
            Ok(Some(dev)) => {
                tracing::debug!("capturing {}", dev.raw.name().unwrap_or("unnamed device"));
                Some((path, dev))
            }
            _ => None,
        })
        .collect()
}

fn first_open_error() -> Option<std::io::Error> {
    let nodes = event_nodes();
    if nodes.is_empty() {
        return Some(std::io::Error::other("no devices present"));
    }
    nodes.iter().find_map(|p| open_device(p).err())
}

/// Gentle speed-dependent gain for raw mouse counts, so slow movement stays
/// precise and fast movement covers distance — close to what desktops do.
fn accelerate(dx: i32, dy: i32, elapsed: Duration) -> (f64, f64) {
    let ms = (elapsed.as_secs_f64() * 1000.0).clamp(1.0, 40.0);
    let speed = f64::from(dx).hypot(f64::from(dy)) / ms;
    let gain = (0.75 + 0.22 * speed).min(2.6);
    (f64::from(dx) * gain, f64::from(dy) * gain)
}

impl Worker {
    fn run(&mut self) {
        let inotify = watch_input_dir();
        loop {
            let mut fds = vec![pollfd(self.wake.as_raw_fd())];
            if let Some(fd) = &inotify {
                fds.push(pollfd(fd.as_raw_fd()));
            }
            let first_device = fds.len();
            let paths: Vec<PathBuf> = self.devices.keys().cloned().collect();
            fds.extend(
                paths
                    .iter()
                    .map(|p| pollfd(self.devices[p].raw.as_raw_fd())),
            );

            let timeout = match self.rescan_at {
                Some(at) => at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1,
                None => -1,
            };
            // SAFETY: `fds` is a valid slice for the duration of the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                tracing::error!("input polling failed; capture stopped");
                return;
            }

            if fds[0].revents != 0 {
                drain(fds[0].fd);
                if !self.commands() {
                    return;
                }
            }
            if inotify.is_some() && fds[1].revents != 0 {
                drain(fds[1].fd);
                self.rescan_at = Some(Instant::now() + RESCAN_DELAY);
            }
            if self.rescan_at.is_some_and(|at| at <= Instant::now()) {
                self.rescan_at = None;
                self.rescan();
            }
            for (path, fd) in paths.iter().zip(&fds[first_device..]) {
                if fd.revents == 0 {
                    continue;
                }
                if !self.read_device(path) {
                    self.devices.remove(path);
                    // Its keys can no longer be released; forget them.
                    self.sink.reset();
                }
                if !self.commands() {
                    return;
                }
            }
        }
    }

    /// Apply queued commands. Returns `false` when asked to stop.
    fn commands(&mut self) -> bool {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Grab(on) => self.grab(on),
                Cmd::Warp(to) => {
                    if let Some(probe) = &self.probe {
                        probe.warp(to);
                    }
                }
                Cmd::Stop => {
                    self.grab(false);
                    return false;
                }
            }
        }
        true
    }

    fn grab(&mut self, on: bool) {
        if self.grabbed == on {
            return;
        }
        self.grabbed = on;
        for dev in self.devices.values_mut() {
            let result = if on { dev.raw.grab() } else { dev.raw.ungrab() };
            if let Err(e) = result {
                tracing::debug!("grab({on}) failed on a device: {e}");
            }
            dev.motion = (0, 0);
        }
    }

    fn rescan(&mut self) {
        self.devices.retain(|path, _| path.exists());
        let mut fresh = scan(&self.devices);
        if self.grabbed {
            for dev in fresh.values_mut() {
                let _ = dev.raw.grab();
            }
        }
        self.devices.extend(fresh);
    }

    /// Process pending events of one device. Returns `false` if it is gone.
    fn read_device(&mut self, path: &Path) -> bool {
        let Some(dev) = self.devices.get_mut(path) else {
            return true;
        };
        let events: Vec<_> = match dev.raw.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        };
        for ev in events {
            let (code, value) = (ev.code(), ev.value());
            match ev.event_type() {
                EventType::KEY if value != 2 => {
                    // value 2 is auto-repeat; the receiving machine repeats on its own.
                    if (0x110..=0x117).contains(&code) {
                        self.sink.button(code, value == 1);
                    } else if !(0x100..0x160).contains(&code) {
                        self.sink.key(code, value == 1);
                    }
                }
                EventType::RELATIVE => {
                    let Some(dev) = self.devices.get_mut(path) else {
                        return true;
                    };
                    match RelativeAxisCode(code) {
                        RelativeAxisCode::REL_X => dev.motion.0 += value,
                        RelativeAxisCode::REL_Y => dev.motion.1 += value,
                        RelativeAxisCode::REL_WHEEL_HI_RES => dev.wheel.1 += value,
                        RelativeAxisCode::REL_HWHEEL_HI_RES => dev.wheel.0 += value,
                        RelativeAxisCode::REL_WHEEL if !dev.hires_wheel => {
                            dev.wheel.1 += value * SCROLL_NOTCH
                        }
                        RelativeAxisCode::REL_HWHEEL if !dev.hires_wheel => {
                            dev.wheel.0 += value * SCROLL_NOTCH
                        }
                        _ => {}
                    }
                }
                EventType::SYNCHRONIZATION => {
                    let Some(dev) = self.devices.get_mut(path) else {
                        return true;
                    };
                    let (motion, wheel) = (
                        std::mem::take(&mut dev.motion),
                        std::mem::take(&mut dev.wheel),
                    );
                    if motion != (0, 0) {
                        self.motion(motion.0, motion.1);
                    }
                    if wheel != (0, 0) {
                        self.sink.scroll(wheel.0, wheel.1);
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn motion(&mut self, dx: i32, dy: i32) {
        let now = Instant::now();
        if self.grabbed {
            let (ax, ay) = accelerate(dx, dy, now - self.last_motion);
            self.last_motion = now;
            self.sink.motion(ax, ay, None);
            return;
        }
        self.last_motion = now;
        // Local motion only matters for edge detection, which needs the real
        // cursor position; without a way to read it there is nothing to do.
        let Some(probe) = &self.probe else { return };
        if now - self.last_probe < PROBE_SPACING {
            return;
        }
        self.last_probe = now;
        if let Some(at) = probe.position() {
            self.sink.motion(f64::from(dx), f64::from(dy), Some(at));
        }
    }
}

fn pollfd(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

/// Empty a non-blocking notification descriptor.
fn drain(fd: RawFd) {
    let mut buf = [0u8; 4096];
    // SAFETY: reads into a local buffer of the stated size.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// Watch for keyboards and mice being plugged in.
fn watch_input_dir() -> Option<OwnedFd> {
    // SAFETY: plain syscalls with a valid, NUL-terminated path.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let owned = OwnedFd::from_raw_fd(fd);
        let dir = std::ffi::CString::new(INPUT_DIR).ok()?;
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB;
        (libc::inotify_add_watch(fd, dir.as_ptr(), mask) >= 0).then_some(owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceleration_is_gentle_and_bounded() {
        let slow = accelerate(1, 0, Duration::from_millis(8));
        let fast = accelerate(60, 0, Duration::from_millis(8));
        assert!(slow.0 < 1.0, "slow motion is slightly damped for precision");
        assert!(fast.0 > 60.0 && fast.0 <= 60.0 * 2.6);
        assert_eq!(accelerate(0, 0, Duration::ZERO), (0.0, 0.0));
    }
}
