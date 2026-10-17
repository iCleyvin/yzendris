//! Monitor enumeration.

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use yzendris_core::geom::Rect;
use yzendris_core::layout::Screen;

const MONITORINFOF_PRIMARY: u32 = 1;

unsafe extern "system" fn on_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<Screen>` passed by `screens` below, which
    // outlives the enumeration; MONITORINFOEXW is plain data.
    unsafe {
        let out = &mut *(data as *mut Vec<Screen>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) != 0 {
            let len = info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len());
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            let rc = info.monitorInfo.rcMonitor;
            out.push(Screen {
                // "\\.\DISPLAY1" → "DISPLAY1"
                name: device.trim_start_matches(r"\\.\").to_owned(),
                rect: Rect::new(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }
    1 // keep enumerating
}

pub fn screens() -> Vec<Screen> {
    let mut found: Vec<Screen> = Vec::new();
    // SAFETY: the callback only runs during this call and receives a pointer
    // to `found`, which is alive throughout.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(on_monitor),
            (&raw mut found) as LPARAM,
        );
    }
    found.sort_by_key(|s| (s.rect.x, s.rect.y));
    found
}
