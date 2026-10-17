//! Key codes.
//!
//! Keys travel as Linux `KEY_*` codes. Linux reads and writes them natively;
//! Windows converts to and from PS/2 set-1 scan codes here, which keeps the
//! mapping about *physical keys* — the receiving machine applies its own
//! keyboard layout.

pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_0: u16 = 11;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_L: u16 = 38;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_NUMLOCK: u16 = 69;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_PAUSE: u16 = 119;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;

/// Highest key code we forward (`KEY_MAX` in the kernel).
pub const KEY_MAX: u16 = 0x2ff;

/// How to synthesize a key on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinKey {
    /// A scan code, optionally E0-prefixed.
    Scan { code: u16, extended: bool },
    /// A virtual-key code, for keys without a reliable scan code.
    Vk(u16),
}

/// `(evdev, scan code, extended)` for keys that are not the 1:1 block.
const SCAN_TABLE: &[(u16, u16, bool)] = &[
    // Non-extended keys past F12.
    (183, 0x64, false), // F13
    (184, 0x65, false),
    (185, 0x66, false),
    (186, 0x67, false),
    (187, 0x68, false),
    (188, 0x69, false),
    (189, 0x6a, false),
    (190, 0x6b, false),
    (191, 0x6c, false),
    (192, 0x6d, false),
    (193, 0x6e, false),
    (194, 0x76, false), // F24
    (93, 0x70, false),  // KATAKANAHIRAGANA
    (89, 0x73, false),  // RO
    (92, 0x79, false),  // HENKAN
    (94, 0x7b, false),  // MUHENKAN
    (124, 0x7d, false), // YEN
    (121, 0x7e, false), // KPCOMMA
    // E0-prefixed keys.
    (96, 0x1c, true), // KPENTER
    (KEY_RIGHTCTRL, 0x1d, true),
    (98, 0x35, true),        // KPSLASH
    (KEY_SYSRQ, 0x37, true), // Print Screen
    (KEY_RIGHTALT, 0x38, true),
    (KEY_NUMLOCK, 0x45, true),
    (102, 0x47, true), // HOME
    (103, 0x48, true), // UP
    (104, 0x49, true), // PAGEUP
    (KEY_LEFT, 0x4b, true),
    (KEY_RIGHT, 0x4d, true),
    (107, 0x4f, true), // END
    (108, 0x50, true), // DOWN
    (109, 0x51, true), // PAGEDOWN
    (110, 0x52, true), // INSERT
    (111, 0x53, true), // DELETE
    (KEY_LEFTMETA, 0x5b, true),
    (KEY_RIGHTMETA, 0x5c, true),
    (127, 0x5d, true), // COMPOSE (menu)
    (116, 0x5e, true), // POWER
    (142, 0x5f, true), // SLEEP
    (143, 0x63, true), // WAKEUP
];

/// `(evdev, virtual key)` for media/browser keys. Keyboards often report these
/// with no scan code at all, so they are matched and injected by VK.
const VK_TABLE: &[(u16, u16)] = &[
    (KEY_PAUSE, 0x13),
    (158, 0xa6), // BACK
    (159, 0xa7), // FORWARD
    (173, 0xa8), // REFRESH
    (128, 0xa9), // STOP
    (217, 0xaa), // SEARCH
    (156, 0xab), // BOOKMARKS
    (172, 0xac), // HOMEPAGE
    (113, 0xad), // MUTE
    (114, 0xae), // VOLUMEDOWN
    (115, 0xaf), // VOLUMEUP
    (163, 0xb0), // NEXTSONG
    (165, 0xb1), // PREVIOUSSONG
    (166, 0xb2), // STOPCD
    (164, 0xb3), // PLAYPAUSE
    (155, 0xb4), // MAIL
    (226, 0xb5), // MEDIA
    (157, 0xb6), // COMPUTER
    (140, 0xb7), // CALC
];

/// Translate what a Windows low-level keyboard hook reports into an evdev
/// code. `vk` disambiguates the keys whose scan codes Windows reports oddly.
pub fn from_windows(scan: u32, extended: bool, vk: u32) -> Option<u16> {
    if let Some(&(code, _)) = VK_TABLE.iter().find(|(_, v)| u32::from(*v) == vk) {
        return Some(code);
    }
    match (scan, extended) {
        // Windows flags Right Shift as extended although it has no E0 prefix.
        (0x36, _) => return Some(KEY_RIGHTSHIFT),
        // Alt+PrintScreen arrives as the dedicated SysRq scan code.
        (0x54, false) => return Some(KEY_SYSRQ),
        _ => {}
    }
    if let Some(&(code, _, _)) = SCAN_TABLE
        .iter()
        .find(|(_, s, e)| u32::from(*s) == scan && *e == extended)
    {
        return Some(code);
    }
    // Scan codes 0x01–0x58 are numerically identical to evdev codes.
    (!extended && (1..=0x58).contains(&scan)).then_some(scan as u16)
}

/// Translate an evdev code into the key to synthesize on Windows.
pub fn to_windows(code: u16) -> Option<WinKey> {
    if let Some(&(_, vk)) = VK_TABLE.iter().find(|(c, _)| *c == code) {
        return Some(WinKey::Vk(vk));
    }
    if let Some(&(_, scan, extended)) = SCAN_TABLE.iter().find(|(c, _, _)| *c == code) {
        return Some(WinKey::Scan {
            code: scan,
            extended,
        });
    }
    (1..=0x58).contains(&code).then_some(WinKey::Scan {
        code,
        extended: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_block_is_identity() {
        for scan in 1..=0x58u32 {
            if scan == 0x36 || scan == 0x54 || scan == 0x45 {
                continue;
            }
            assert_eq!(from_windows(scan, false, 0), Some(scan as u16));
        }
    }

    #[test]
    fn every_table_entry_round_trips() {
        for &(code, scan, extended) in SCAN_TABLE {
            assert_eq!(
                from_windows(u32::from(scan), extended, 0),
                Some(code),
                "evdev {code}"
            );
            assert_eq!(
                to_windows(code),
                Some(WinKey::Scan {
                    code: scan,
                    extended
                })
            );
        }
        for &(code, vk) in VK_TABLE {
            assert_eq!(from_windows(0, false, u32::from(vk)), Some(code));
            assert_eq!(to_windows(code), Some(WinKey::Vk(vk)));
        }
    }

    #[test]
    fn tables_have_no_conflicting_entries() {
        let mut codes: Vec<u16> = SCAN_TABLE
            .iter()
            .map(|e| e.0)
            .chain(VK_TABLE.iter().map(|e| e.0))
            .collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(before, codes.len(), "an evdev code is mapped twice");
        assert!(
            SCAN_TABLE.iter().all(|e| e.0 > 0x58 || e.0 == KEY_NUMLOCK),
            "table overlaps the identity block"
        );
    }

    #[test]
    fn windows_quirks() {
        // Pause shares scan code 0x45 with NumLock; the VK tells them apart.
        assert_eq!(from_windows(0x45, false, 0x13), Some(KEY_PAUSE));
        assert_eq!(from_windows(0x45, true, 0x90), Some(KEY_NUMLOCK));
        assert_eq!(from_windows(0x36, true, 0xa1), Some(KEY_RIGHTSHIFT));
        assert_eq!(from_windows(0x5b, true, 0x5b), Some(KEY_LEFTMETA));
        assert_eq!(from_windows(0x1d, false, 0xa2), Some(KEY_LEFTCTRL));
        assert_eq!(from_windows(0x200, false, 0), None);
        assert_eq!(to_windows(0), None);
    }
}
