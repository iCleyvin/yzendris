/// Screen-geometry helpers: edge parsing and virtual-screen metrics.
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::hook::Rect;

/// Which physical edge of the Windows screen arrangement triggers capture
/// (classic single-flow mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Right,
    Left,
    Top,
    Bottom,
}

impl Edge {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "right"  => Some(Edge::Right),
            "left"   => Some(Edge::Left),
            "top"    => Some(Edge::Top),
            "bottom" => Some(Edge::Bottom),
            _ => None,
        }
    }

    /// Numeric side encoding used by the hook: 0=right 1=left 2=bottom 3=top.
    pub fn side(self) -> u8 {
        match self {
            Edge::Right  => 0,
            Edge::Left   => 1,
            Edge::Bottom => 2,
            Edge::Top    => 3,
        }
    }
}

/// Bounding rect of the whole Windows virtual screen (all monitors).
pub fn virtual_screen() -> Rect {
    #[cfg(windows)]
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        Rect { left: vx, top: vy, right: vx + vw, bottom: vy + vh }
    }

    #[cfg(not(windows))]
    Rect { left: 0, top: 0, right: 1920, bottom: 1080 }
}
