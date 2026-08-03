/// Windows clipboard helpers using the Win32 API.
///
/// We read/write CF_UNICODETEXT (UTF-16LE null-terminated).

// CF_UNICODETEXT = 13 (winuser.h).  The windows crate does not expose this
// constant directly, so we define it ourselves.
#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;
#[cfg(windows)]
const CF_HDROP: u32 = 15;

#[cfg(windows)]
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL, POINT},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
            SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::Shell::{DragQueryFileW, DROPFILES, HDROP},
};

/// Read the current clipboard as a UTF-8 String.
/// Returns `None` if clipboard is empty, non-text, or on error.
#[cfg(windows)]
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

#[cfg(not(windows))]
pub fn read() -> Option<String> { None }

#[cfg(windows)]
unsafe fn read_inner() -> Option<String> {
    let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
    // HANDLE and HGLOBAL both wrap *mut c_void — just reinterpret the pointer.
    let ptr = GlobalLock(HGLOBAL(handle.0)) as *const u16;
    if ptr.is_null() {
        return None;
    }

    // Find null terminator.
    let mut len = 0usize;
    while *ptr.add(len) != 0 { len += 1; }

    let slice = std::slice::from_raw_parts(ptr, len);
    let text = String::from_utf16_lossy(slice);
    GlobalUnlock(HGLOBAL(handle.0)).ok();
    Some(text)
}

/// Write `text` to the clipboard.  Silently ignores errors.
#[cfg(windows)]
pub fn write(text: &str) {
    if text.is_empty() { return; }
    unsafe { write_inner(text); }
}

#[cfg(not(windows))]
pub fn write(_text: &str) {}

#[cfg(windows)]
unsafe fn write_inner(text: &str) {
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_size = utf16.len() * std::mem::size_of::<u16>();

    if OpenClipboard(None).is_err() { return; }

    let hglobal = GlobalAlloc(GMEM_MOVEABLE, byte_size);
    let Ok(hglobal) = hglobal else {
        let _ = CloseClipboard();
        return;
    };

    let ptr = GlobalLock(hglobal) as *mut u16;
    if ptr.is_null() {
        let _ = CloseClipboard();
        return;
    }
    std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
    GlobalUnlock(hglobal).ok();

    let _ = EmptyClipboard();
    // SetClipboardData expects HANDLE; hglobal.0 is *mut c_void — same layout.
    let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(hglobal.0));
    let _ = CloseClipboard();
}

// ─── Files (CF_HDROP) ─────────────────────────────────────────────────────────

/// Every file on the clipboard (CF_HDROP). Empty when it holds no file list.
#[cfg(windows)]
pub fn read_file_paths() -> Vec<std::path::PathBuf> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return Vec::new();
        }
        let result = read_file_inner();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
pub fn read_file_paths() -> Vec<std::path::PathBuf> { Vec::new() }

#[cfg(windows)]
unsafe fn read_file_inner() -> Vec<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    let Ok(handle) = GetClipboardData(CF_HDROP) else {
        return Vec::new();
    };
    let hdrop = HDROP(handle.0);
    // 0xFFFF_FFFF asks DragQueryFileW for the file count.
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
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
        paths.push(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf)));
    }
    tracing::info!("read_file_paths (Windows): {} file(s) on clipboard (CF_HDROP)", paths.len());
    paths
}

/// Put references to `paths` on the clipboard (CF_HDROP) so Explorer can paste
/// the files. Silently ignores errors.
#[cfg(windows)]
pub fn write_file_paths(paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    tracing::info!("write_file_paths (Windows): setting {} file(s) (CF_HDROP)", paths.len());
    unsafe { write_file_inner(paths) }
}

#[cfg(not(windows))]
pub fn write_file_paths(_paths: &[std::path::PathBuf]) {}

#[cfg(windows)]
unsafe fn write_file_inner(paths: &[std::path::PathBuf]) {
    use std::os::windows::ffi::OsStrExt;

    // CF_HDROP = DROPFILES header + double-null-terminated wide path list.
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
    (*df).pFiles = header as u32; // byte offset to the path list
    (*df).pt = POINT { x: 0, y: 0 };
    (*df).fNC = false.into();
    (*df).fWide = true.into(); // UTF-16 paths
    let dst = base.add(header) as *mut u16;
    std::ptr::copy_nonoverlapping(list.as_ptr(), dst, list.len());
    GlobalUnlock(hglobal).ok();

    let _ = EmptyClipboard();
    let _ = SetClipboardData(CF_HDROP, HANDLE(hglobal.0));
    let _ = CloseClipboard();
}
