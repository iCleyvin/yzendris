/// Linux Wayland clipboard helpers using wl-clipboard (wl-paste / wl-copy).
///
/// Both binaries are part of the `wl-clipboard` package available in all
/// major distros (CachyOS: `sudo pacman -S wl-clipboard`).
use std::io::Write;
use std::process::{Command, Stdio};

/// Read the current clipboard contents.  Returns `None` if empty or on error.
pub fn read() -> Option<String> {
    let out = Command::new("wl-paste")
        .arg("--no-newline") // don't add trailing \n
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

/// Write `text` to the clipboard.  Silently ignores errors.
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
