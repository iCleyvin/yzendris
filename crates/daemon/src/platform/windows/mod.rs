//! Windows back-end: low-level hooks to capture, `SendInput` to synthesize.

mod capture;
mod inject;
mod screens;

use anyhow::Result;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

pub use capture::Capture;
pub use screens::screens;

use super::Inject;

/// The daemon has no console on Windows, so it keeps a log file.
pub const LOG_TO_FILE: bool = true;

/// Work in physical pixels. Without this, cursor coordinates reported by
/// hooks and the monitor rectangles disagree on any display scaled above
/// 100 %. The application manifest requests the same; this covers builds
/// where the manifest could not be embedded.
pub fn prepare() {
    // SAFETY: plain call; failing (already set by the manifest) is harmless.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn local_clock() -> (u32, u32, u32) {
    // SAFETY: SYSTEMTIME is plain data and fully written by the call.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    (
        u32::from(t.wHour),
        u32::from(t.wMinute),
        u32::from(t.wSecond),
    )
}

pub fn injector(_keyboard_layout: &str) -> Result<Box<dyn Inject>> {
    Ok(Box::new(inject::SendInputInjector))
}
