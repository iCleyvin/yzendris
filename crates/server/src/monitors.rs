/// Physical monitor enumeration via EnumDisplayMonitors.
///
/// Returns the rect and device name ("DISPLAY1", "DISPLAY2", …) of every
/// attached monitor so the layout logic can place the laptop *between* two
/// of them.
use windows::Win32::{
    Foundation::{BOOL, LPARAM, RECT},
    Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    /// Device name without the `\\.\` prefix, e.g. "DISPLAY1".
    pub device: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
}

#[allow(dead_code)]
impl Monitor {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// Enumerate all monitors. Order is the OS enumeration order; callers sort
/// by coordinates when they need a spatial order.
pub fn enumerate() -> Vec<Monitor> {
    let mut monitors: Vec<Monitor> = Vec::new();

    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let monitors = &mut *(lparam.0 as *mut Vec<Monitor>);

        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        if GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut MONITORINFO).as_bool() {
            let device_raw = String::from_utf16_lossy(&info.szDevice);
            let device = device_raw
                .trim_end_matches('\0')
                .trim_start_matches(r"\\.\")
                .to_owned();
            let rc = info.monitorInfo.rcMonitor;
            // MONITORINFOF_PRIMARY = 1
            let primary = info.monitorInfo.dwFlags & 1 != 0;
            monitors.push(Monitor {
                device,
                left: rc.left,
                top: rc.top,
                right: rc.right,
                bottom: rc.bottom,
                primary,
            });
        }
        BOOL(1) // continue enumeration
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }

    monitors
}

/// Find a monitor by device name; accepts "DISPLAY1", "\\.\DISPLAY1" or "1".
pub fn find<'a>(monitors: &'a [Monitor], name: &str) -> Option<&'a Monitor> {
    let wanted = name.trim().trim_start_matches(r"\\.\").to_uppercase();
    monitors.iter().find(|m| {
        let dev = m.device.to_uppercase();
        dev == wanted || dev == format!("DISPLAY{wanted}")
    })
}
