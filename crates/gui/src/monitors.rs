//! Monitor enumeration for the host panel.
//! Windows: real data via EnumDisplayMonitors.
//! Other OSes: empty list (the host role is Windows-only for now).

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// Device name without prefix, e.g. "DISPLAY1".
    pub device: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
}

impl MonitorInfo {
    pub fn width(&self) -> i32 { self.right - self.left }
    pub fn height(&self) -> i32 { self.bottom - self.top }
    /// Short label like "1" from "DISPLAY1".
    pub fn number(&self) -> String {
        self.device
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .to_owned()
    }
}

#[cfg(windows)]
pub fn enumerate() -> Vec<MonitorInfo> {
    use windows::Win32::{
        Foundation::{BOOL, LPARAM, RECT},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
    };

    let mut monitors: Vec<MonitorInfo> = Vec::new();

    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
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
            monitors.push(MonitorInfo {
                device,
                left: rc.left,
                top: rc.top,
                right: rc.right,
                bottom: rc.bottom,
                primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }

    monitors.sort_by_key(|m| m.left);
    monitors
}

#[cfg(not(windows))]
pub fn enumerate() -> Vec<MonitorInfo> {
    Vec::new()
}
