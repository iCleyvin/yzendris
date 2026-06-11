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
}

// ─── Windows (Win32 CF_UNICODETEXT) ──────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use windows::Win32::{
        Foundation::{HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
    };

    const CF_UNICODETEXT: u32 = 13;

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
}

// ─── Other platforms ─────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn read() -> Option<String> {
        None
    }
    pub fn write(_text: &str) {}
}

pub use imp::{read, write};
