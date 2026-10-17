//! Integer screen geometry.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned rectangle; `x + w` and `y + h` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn is_valid(&self) -> bool {
        self.w > 0 && self.h > 0
    }

    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2, self.y + self.h / 2)
    }

    /// Nearest point inside the rectangle.
    pub fn clamp(&self, p: Point) -> Point {
        Point::new(
            p.x.clamp(self.x, self.right() - 1),
            p.y.clamp(self.y, self.bottom() - 1),
        )
    }

    /// Smallest rectangle covering every rect in `rects`.
    pub fn bounding(rects: &[Rect]) -> Option<Rect> {
        let first = rects.first()?;
        let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.right(), first.bottom());
        for rc in &rects[1..] {
            l = l.min(rc.x);
            t = t.min(rc.y);
            r = r.max(rc.right());
            b = b.max(rc.bottom());
        }
        Some(Rect::new(l, t, r - l, b - t))
    }

    /// Start and (exclusive) end of the edge on `side`, along its own axis.
    pub fn span(&self, side: Side) -> (i32, i32) {
        if side.is_horizontal() {
            (self.y, self.bottom())
        } else {
            (self.x, self.right())
        }
    }
}

/// A side of a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

    pub const fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }

    /// True for left/right: crossing happens along the x axis.
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }
}

/// Position of `v` within `[lo, hi)` as a fraction in `0.0..=1.0`.
pub fn fraction(v: i32, lo: i32, hi: i32) -> f64 {
    if hi - lo <= 1 {
        return 0.5;
    }
    (f64::from(v - lo) / f64::from(hi - lo - 1)).clamp(0.0, 1.0)
}

/// Inverse of [`fraction`]: the coordinate at `frac` within `[lo, hi)`.
pub fn lerp(lo: i32, hi: i32, frac: f64) -> i32 {
    let span = f64::from((hi - lo - 1).max(0));
    lo + (frac.clamp(0.0, 1.0) * span).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_and_lerp_round_trip() {
        for v in [0, 1, 539, 1079] {
            assert_eq!(lerp(0, 1080, fraction(v, 0, 1080)), v);
        }
        assert_eq!(fraction(5, 5, 5), 0.5);
        assert_eq!(lerp(10, 20, 2.0), 19);
    }

    #[test]
    fn bounding_covers_all() {
        let b = Rect::bounding(&[Rect::new(0, 0, 100, 50), Rect::new(100, -20, 50, 50)]).unwrap();
        assert_eq!(b, Rect::new(0, -20, 150, 70));
        assert!(Rect::bounding(&[]).is_none());
    }
}
