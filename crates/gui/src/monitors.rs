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

/// Which sides of `monitors[idx]` are FREE (a desktop boundary the cursor can
/// reach), as `[right, left, bottom, top]` matching side codes 0,1,2,3. A side
/// is blocked when another monitor is adjacent there.
pub fn free_sides(monitors: &[MonitorInfo], idx: usize) -> [bool; 4] {
    let m = &monitors[idx];
    let mut free = [true; 4];
    for (k, o) in monitors.iter().enumerate() {
        if k == idx {
            continue;
        }
        let y_ov = m.top.max(o.top) < m.bottom.min(o.bottom);
        let x_ov = m.left.max(o.left) < m.right.min(o.right);
        if y_ov && (o.left - m.right).abs() <= 2 { free[0] = false; } // right blocked
        if y_ov && (m.left - o.right).abs() <= 2 { free[1] = false; } // left blocked
        if x_ov && (o.top - m.bottom).abs() <= 2 { free[2] = false; } // bottom blocked
        if x_ov && (m.top - o.bottom).abs() <= 2 { free[3] = false; } // top blocked
    }
    free
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

/// Reposition monitors in Windows (Display Settings). `changes` is a list of
/// (device name without prefix, new x, new y). The primary must end at (0,0);
/// callers should normalize before calling. Applies atomically.
#[cfg(windows)]
pub fn reposition(changes: &[(String, i32, i32)]) -> Result<(), String> {
    use std::iter::once;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_NORESET, CDS_TYPE, CDS_UPDATEREGISTRY,
        DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_POSITION, ENUM_CURRENT_SETTINGS,
    };

    unsafe {
        for (dev, x, y) in changes {
            let full = format!(r"\\.\{dev}");
            let devw: Vec<u16> = full.encode_utf16().chain(once(0)).collect();
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if !EnumDisplaySettingsW(PCWSTR(devw.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool()
            {
                return Err(format!("no pude leer los ajustes de {dev}"));
            }
            dm.Anonymous1.Anonymous2.dmPosition.x = *x;
            dm.Anonymous1.Anonymous2.dmPosition.y = *y;
            dm.dmFields |= DM_POSITION;
            // Queue the change in the registry without applying yet.
            let r = ChangeDisplaySettingsExW(
                PCWSTR(devw.as_ptr()),
                Some(&dm),
                None,
                CDS_UPDATEREGISTRY | CDS_NORESET,
                None,
            );
            if r != DISP_CHANGE_SUCCESSFUL {
                return Err(format!("ChangeDisplaySettings {dev}: código {}", r.0));
            }
        }
        // Apply all queued changes at once.
        let r = ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None);
        if r != DISP_CHANGE_SUCCESSFUL {
            return Err(format!("aplicar cambios: código {}", r.0));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn reposition(_changes: &[(String, i32, i32)]) -> Result<(), String> {
    Err("solo Windows".into())
}
