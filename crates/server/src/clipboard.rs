/// Windows clipboard helpers using the Win32 API.
///
/// We read/write CF_UNICODETEXT (UTF-16LE null-terminated).

// CF_UNICODETEXT = 13 (winuser.h).  The windows crate does not expose this
// constant directly, so we define it ourselves.
#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;

#[cfg(windows)]
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
            SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
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
