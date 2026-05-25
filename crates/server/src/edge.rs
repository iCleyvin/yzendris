/// Edge-crossing detector.
///
/// Reads the virtual screen dimensions once at startup and exposes
/// `check_edge(x, y)` that returns `true` when the cursor is at the
/// configured screen edge.
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Which physical edge of the Windows screen arrangement triggers capture.
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
}

pub struct EdgeDetector {
    #[allow(dead_code)]
    edge:   Edge,
    min_x:  i32,
    min_y:  i32,
    max_x:  i32,
    max_y:  i32,
}

impl EdgeDetector {
    pub fn new(edge: Edge) -> Self {
        #[cfg(windows)]
        let (min_x, min_y, max_x, max_y) = unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            (vx, vy, vx + vw - 1, vy + vh - 1)
        };

        #[cfg(not(windows))]
        let (min_x, min_y, max_x, max_y) = (0, 0, 1919, 1079);

        tracing::debug!(
            "EdgeDetector: virtual screen [{min_x},{min_y}]–[{max_x},{max_y}], edge={edge:?}"
        );

        Self { edge, min_x, min_y, max_x, max_y }
    }

    /// Returns `true` when cursor `(x, y)` has reached (or gone past) the edge.
    #[allow(dead_code)]
    pub fn at_edge(&self, x: i32, y: i32) -> bool {
        match self.edge {
            Edge::Right  => x >= self.max_x,
            Edge::Left   => x <= self.min_x,
            Edge::Bottom => y >= self.max_y,
            Edge::Top    => y <= self.min_y,
        }
    }

    /// Center of the virtual screen — used to "park" the cursor while capturing.
    pub fn center(&self) -> (i32, i32) {
        (
            (self.min_x + self.max_x) / 2,
            (self.min_y + self.max_y) / 2,
        )
    }
}
