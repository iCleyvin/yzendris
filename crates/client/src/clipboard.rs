//! Clipboard helpers, cross-platform.
//!   Linux:   wl-clipboard (wl-paste / wl-copy).
//!   Windows: Win32 CF_UNICODETEXT.
//!   Other:   no-op stubs.

// ─── Linux (wl-clipboard) ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod imp {
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn read() -> Option<String> {
        let out = Command::new("wl-paste")
            .arg("--no-newline")
            .arg("--type")
            .arg("text/plain;charset=utf-8")
            .output()
            .ok()?;
        if out.status.success() && !out.stdout.is_empty() {
            String::from_utf8(out.stdout).ok()
        } else {
            None
        }
    }

    pub fn write(text: &str) {
        if text.is_empty() {
            return;
        }
        if let Ok(mut child) = Command::new("wl-copy")
            .arg("--type")
            .arg("text/plain;charset=utf-8")
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }

    // ── Files ────────────────────────────────────────────────────────────────

    /// Every file currently on the clipboard, read from `text/uri-list` (the
    /// standard target that both GTK and Qt file managers set on copy).
    pub fn read_file_paths() -> Vec<std::path::PathBuf> {
        let out = match Command::new("wl-paste")
            .arg("--type")
            .arg("text/uri-list")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("read_file_paths: wl-paste failed to run: {e}");
                return Vec::new();
            }
        };
        if !out.status.success() {
            // Non-zero is normal when the clipboard holds no uri-list at all.
            tracing::debug!("read_file_paths: wl-paste exit {} (no uri-list)", out.status);
            return Vec::new();
        }
        let Ok(text) = String::from_utf8(out.stdout) else {
            tracing::warn!("read_file_paths: uri-list is not valid UTF-8");
            return Vec::new();
        };
        let mut paths = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("file://") {
                // Drop an optional host component: file://host/path → /path.
                let path_part = match rest.find('/') {
                    Some(i) => &rest[i..],
                    None => rest,
                };
                paths.push(std::path::PathBuf::from(percent_decode(path_part)));
            }
        }
        tracing::info!("read_file_paths (Linux): {} file(s) on clipboard", paths.len());
        paths
    }

    /// Put file references on the clipboard so a file manager pastes copies.
    /// Uses `x-special/gnome-copied-files` (`copy\n<uri>…`), which GTK managers
    /// (Nautilus, Nemo, Caja, Thunar, PCManFM) honour as a real file copy.
    pub fn write_file_paths(paths: &[std::path::PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut payload = String::from("copy");
        for p in paths {
            payload.push('\n');
            payload.push_str(&path_to_uri(p));
        }
        tracing::info!(
            "write_file_paths (Linux): setting {} file(s) via x-special/gnome-copied-files",
            paths.len()
        );
        match Command::new("wl-copy")
            .arg("--type")
            .arg("x-special/gnome-copied-files")
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdin) = child.stdin.as_mut() {
                    if let Err(e) = stdin.write_all(payload.as_bytes()) {
                        tracing::warn!("write_file_paths: writing to wl-copy stdin failed: {e}");
                    }
                }
                match child.wait() {
                    Ok(status) => tracing::info!("write_file_paths: wl-copy exited {status}"),
                    Err(e) => tracing::warn!("write_file_paths: wl-copy wait failed: {e}"),
                }
            }
            Err(e) => tracing::warn!(
                "write_file_paths: wl-copy failed to run (is wl-clipboard installed?): {e}"
            ),
        }
    }

    fn path_to_uri(path: &std::path::Path) -> String {
        let s = path.to_string_lossy();
        let mut out = String::from("file://");
        for &b in s.as_bytes() {
            match b {
                b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
                _ if b.is_ascii_alphanumeric() => out.push(b as char),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
}

// ─── Windows (Win32 CF_UNICODETEXT) ──────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    use windows::Win32::{
        Foundation::{HANDLE, HGLOBAL, POINT},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::Shell::{DragQueryFileW, DROPFILES, HDROP},
    };

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

    pub fn read() -> Option<String> {
        unsafe {
            if OpenClipboard(None).is_err() {
                return None;
            }
            let result = read_inner();
            let _ = CloseClipboard();
            result
        }
    }

    unsafe fn read_inner() -> Option<String> {
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let ptr = GlobalLock(HGLOBAL(handle.0)) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(HGLOBAL(handle.0));
        Some(text)
    }

    pub fn write(text: &str) {
        if text.is_empty() {
            return;
        }
        unsafe { write_inner(text) }
    }

    unsafe fn write_inner(text: &str) {
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_size = utf16.len() * std::mem::size_of::<u16>();

        if OpenClipboard(None).is_err() {
            return;
        }
        let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, byte_size) else {
            let _ = CloseClipboard();
            return;
        };
        let ptr = GlobalLock(hglobal) as *mut u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return;
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
        let _ = GlobalUnlock(hglobal);
        let _ = EmptyClipboard();
        let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(hglobal.0));
        let _ = CloseClipboard();
    }

    // ── Files (CF_HDROP) ─────────────────────────────────────────────────────

    pub fn read_file_paths() -> Vec<PathBuf> {
        unsafe {
            if OpenClipboard(None).is_err() {
                return Vec::new();
            }
            let result = read_file_inner();
            let _ = CloseClipboard();
            result
        }
    }

    unsafe fn read_file_inner() -> Vec<PathBuf> {
        let Ok(handle) = GetClipboardData(CF_HDROP) else {
            return Vec::new();
        };
        let hdrop = HDROP(handle.0);
        // 0xFFFF_FFFF asks for the count of files on the clipboard.
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            // Length of path `i` in UTF-16 units, excluding the null.
            let len = DragQueryFileW(hdrop, i, None);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf));
            if got == 0 {
                continue;
            }
            buf.truncate(got as usize);
            paths.push(PathBuf::from(OsString::from_wide(&buf)));
        }
        tracing::info!("read_file_paths (Windows): {} file(s) on clipboard (CF_HDROP)", paths.len());
        paths
    }

    pub fn write_file_paths(paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        tracing::info!("write_file_paths (Windows): setting {} file(s) (CF_HDROP)", paths.len());
        unsafe { write_file_inner(paths) }
    }

    unsafe fn write_file_inner(paths: &[PathBuf]) {
        // A CF_HDROP payload is a DROPFILES header followed by a
        // double-null-terminated list of wide file paths.
        let mut list: Vec<u16> = Vec::new();
        for path in paths {
            list.extend(path.as_os_str().encode_wide());
            list.push(0); // terminate this path
        }
        list.push(0); // terminate the list

        let header = std::mem::size_of::<DROPFILES>();
        let bytes = header + list.len() * std::mem::size_of::<u16>();

        if OpenClipboard(None).is_err() {
            return;
        }
        let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            let _ = CloseClipboard();
            return;
        };
        let base = GlobalLock(hglobal) as *mut u8;
        if base.is_null() {
            let _ = CloseClipboard();
            return;
        }
        let df = base as *mut DROPFILES;
        (*df).pFiles = header as u32; // offset to the path list
        (*df).pt = POINT { x: 0, y: 0 };
        (*df).fNC = false.into();
        (*df).fWide = true.into(); // paths are UTF-16
        let dst = base.add(header) as *mut u16;
        std::ptr::copy_nonoverlapping(list.as_ptr(), dst, list.len());
        let _ = GlobalUnlock(hglobal);

        let _ = EmptyClipboard();
        let _ = SetClipboardData(CF_HDROP, HANDLE(hglobal.0));
        let _ = CloseClipboard();
    }
}

// ─── Other platforms ─────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn read() -> Option<String> {
        None
    }
    pub fn write(_text: &str) {}
    pub fn read_file_paths() -> Vec<std::path::PathBuf> {
        Vec::new()
    }
    pub fn write_file_paths(_paths: &[std::path::PathBuf]) {}
}

pub use imp::{read, read_file_paths, write, write_file_paths};
