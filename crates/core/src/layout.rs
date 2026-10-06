//! Where a client's screen sits relative to the host's monitors, and how the
//! cursor travels between them.
//!
//! A client is *anchored* to one side of one host monitor. Pushing the cursor
//! through that side enters the client; pushing back through the facing edge
//! of the client returns. When another host monitor already touches the
//! anchor on that side, the client is treated as sitting *between* the two:
//! crossing their shared boundary in either direction passes through the
//! client first.

use serde::{Deserialize, Serialize};

use crate::geom::{fraction, lerp, Point, Rect, Side};

/// One physical display of a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// OS name of the output (`DP-1`, `DISPLAY1`, …).
    pub name: String,
    pub rect: Rect,
    pub primary: bool,
}

/// How far inside the host monitor the cursor reappears after returning, so
/// the next small movement does not cross straight back.
pub const RETURN_MARGIN: i32 = 16;

/// How many pixels the cursor must be pushed past a client edge before
/// control moves on. Large enough to ignore a brush against the edge, small
/// enough that a deliberate move feels instant.
pub const EXIT_PUSH: f64 = 20.0;

/// Monitors closer than this (pixels) count as touching.
const ADJACENT_SLOP: i32 = 2;

/// The resolved connection between one client and the host's monitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub anchor: Rect,
    pub side: Side,
    /// Host monitor touching the anchor on `side`, if any.
    pub neighbor: Option<Rect>,
}

/// Pick the monitor a client is anchored to: by name, else the primary, else
/// the first one.
pub fn find_monitor<'a>(monitors: &'a [Screen], name: &str) -> Option<&'a Screen> {
    let wanted = name.trim();
    monitors
        .iter()
        .find(|m| !wanted.is_empty() && m.name.eq_ignore_ascii_case(wanted))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
}

/// The monitor touching `anchor` on `side` with the largest shared edge.
pub fn neighbor_of(monitors: &[Screen], anchor: Rect, side: Side) -> Option<Rect> {
    let (lo, hi) = anchor.span(side);
    monitors
        .iter()
        .map(|m| m.rect)
        .filter(|r| *r != anchor)
        .filter(|r| {
            let gap = match side {
                Side::Right => r.x - anchor.right(),
                Side::Left => anchor.x - r.right(),
                Side::Bottom => r.y - anchor.bottom(),
                Side::Top => anchor.y - r.bottom(),
            };
            gap.abs() <= ADJACENT_SLOP
        })
        .filter_map(|r| {
            let (rlo, rhi) = r.span(side);
            let overlap = hi.min(rhi) - lo.max(rlo);
            (overlap > 0).then_some((overlap, r))
        })
        .max_by_key(|(overlap, _)| *overlap)
        .map(|(_, r)| r)
}

impl Link {
    pub fn resolve(monitors: &[Screen], monitor: &str, side: Side) -> Option<Self> {
        let anchor = find_monitor(monitors, monitor)?.rect;
        Some(Self {
            anchor,
            side,
            neighbor: neighbor_of(monitors, anchor, side),
        })
    }

    /// Does a local cursor move from `prev` to `cur` (travelling by `motion`)
    /// enter the client? Returns the client edge it enters through and the
    /// position along that edge as a fraction.
    pub fn entry(&self, prev: Point, cur: Point, motion: (i32, i32)) -> Option<(Side, f64)> {
        let horizontal = self.side.is_horizontal();
        let (lo, hi) = self.anchor.span(self.side);
        let along = if horizontal { cur.y } else { cur.x };
        let across = |p: Point| if horizontal { p.x } else { p.y };

        match self.neighbor {
            None => {
                if along < lo || along >= hi {
                    return None;
                }
                let pushing = match self.side {
                    Side::Right => cur.x >= self.anchor.right() - 1 && motion.0 > 0,
                    Side::Left => cur.x <= self.anchor.x && motion.0 < 0,
                    Side::Bottom => cur.y >= self.anchor.bottom() - 1 && motion.1 > 0,
                    Side::Top => cur.y <= self.anchor.y && motion.1 < 0,
                };
                pushing.then(|| (self.side.opposite(), fraction(along, lo, hi)))
            }
            Some(neighbor) => {
                let (nlo, nhi) = neighbor.span(self.side);
                if along < lo.max(nlo) || along >= hi.min(nhi) {
                    return None;
                }
                let boundary = match self.side {
                    Side::Right => self.anchor.right(),
                    Side::Left => self.anchor.x,
                    Side::Bottom => self.anchor.bottom(),
                    Side::Top => self.anchor.y,
                };
                // Is a point on the anchor's side of the shared boundary?
                let on_anchor = |p: Point| match self.side {
                    Side::Right | Side::Bottom => across(p) < boundary,
                    Side::Left | Side::Top => across(p) >= boundary,
                };
                let frac = fraction(along, lo, hi);
                match (on_anchor(prev), on_anchor(cur)) {
                    (true, false) => Some((self.side.opposite(), frac)),
                    (false, true) => Some((self.side, frac)),
                    _ => None,
                }
            }
        }
    }

    /// Where the host cursor lands when the client cursor leaves through
    /// `client_side` at `frac`. `None` when that edge leads nowhere.
    pub fn exit(&self, client_side: Side, frac: f64) -> Option<Point> {
        let (lo, hi) = self.anchor.span(self.side);
        let along = lerp(lo, hi, frac);
        let (target, toward) = if client_side == self.side.opposite() {
            (self.anchor, self.side)
        } else if client_side == self.side {
            (self.neighbor?, self.side.opposite())
        } else {
            return None;
        };
        // `toward` is the edge of `target` that faces the client.
        let p = match toward {
            Side::Right => Point::new(target.right() - 1 - RETURN_MARGIN, along),
            Side::Left => Point::new(target.x + RETURN_MARGIN, along),
            Side::Bottom => Point::new(along, target.bottom() - 1 - RETURN_MARGIN),
            Side::Top => Point::new(along, target.y + RETURN_MARGIN),
        };
        Some(target.clamp(p))
    }

    /// Default spot for the host cursor when control comes home by hotkey.
    pub fn home(&self) -> Point {
        self.exit(self.side.opposite(), 0.5)
            .unwrap_or_else(|| self.anchor.center())
    }
}

/// Point on the `edge` of a client's desktop at `frac`, snapped onto an
/// actual screen (the bounding box of several screens can have empty areas).
pub fn enter_point(screens: &[Rect], edge: Side, frac: f64) -> Option<Point> {
    let bbox = Rect::bounding(screens)?;
    let (lo, hi) = bbox.span(edge);
    let along = lerp(lo, hi, frac);
    let p = match edge {
        Side::Left => Point::new(bbox.x, along),
        Side::Right => Point::new(bbox.right() - 1, along),
        Side::Top => Point::new(along, bbox.y),
        Side::Bottom => Point::new(along, bbox.bottom() - 1),
    };
    Some(snap(screens, p))
}

/// Nearest point to `p` that lies on one of `screens`.
pub fn snap(screens: &[Rect], p: Point) -> Point {
    screens
        .iter()
        .map(|r| r.clamp(p))
        .min_by_key(|c| {
            let (dx, dy) = (i64::from(c.x - p.x), i64::from(c.y - p.y));
            dx * dx + dy * dy
        })
        .unwrap_or(p)
}

/// Result of moving the remote cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Still,
    Moved(Point),
    /// The cursor was pushed out through `side` at `frac` along it.
    Exit {
        side: Side,
        frac: f64,
    },
}

/// The host's model of the cursor on a client's desktop.
///
/// The host owns this position and sends it as absolute coordinates, so the
/// client never needs to ask its compositor where the cursor is.
#[derive(Debug, Clone, Default)]
pub struct RemoteCursor {
    x: f64,
    y: f64,
    /// Accumulated push against each desktop edge, indexed like [`Side::ALL`].
    push: [f64; 4],
}

impl RemoteCursor {
    pub fn place(&mut self, p: Point) {
        self.x = f64::from(p.x);
        self.y = f64::from(p.y);
        self.push = [0.0; 4];
    }

    pub fn point(&self) -> Point {
        Point::new(self.x.round() as i32, self.y.round() as i32)
    }

    /// Move by `(dx, dy)` within `screens`, sliding along edges.
    pub fn advance(&mut self, dx: f64, dy: f64, screens: &[Rect]) -> Step {
        let Some(bbox) = Rect::bounding(screens) else {
            return Step::Still;
        };
        let before = self.point();
        let (tx, ty) = (self.x + dx, self.y + dy);
        let target = Point::new(tx.round() as i32, ty.round() as i32);
        let on_screen = |p: Point| screens.iter().any(|r| r.contains(p));

        if on_screen(target) {
            self.x = tx;
            self.y = ty;
            self.push = [0.0; 4];
            return if target == before {
                Step::Still
            } else {
                Step::Moved(target)
            };
        }

        // Blocked: keep whichever axis can still move, clamp the other to the
        // screen the cursor is on.
        let here = screens
            .iter()
            .find(|r| r.contains(before))
            .copied()
            .unwrap_or(bbox);
        let cx = if on_screen(Point::new(target.x, before.y)) {
            tx
        } else {
            tx.clamp(f64::from(here.x), f64::from(here.right() - 1))
        };
        let cy = if on_screen(Point::new(before.x, target.y)) {
            ty
        } else {
            ty.clamp(f64::from(here.y), f64::from(here.bottom() - 1))
        };
        let (ex, ey) = (tx - cx, ty - cy);
        self.x = cx;
        self.y = cy;
        let now = self.point();

        // Only a push against the *outer* boundary of the desktop counts.
        let pushed = [
            (Side::Left, -ex, now.x <= bbox.x),
            (Side::Right, ex, now.x >= bbox.right() - 1),
            (Side::Top, -ey, now.y <= bbox.y),
            (Side::Bottom, ey, now.y >= bbox.bottom() - 1),
        ];
        for (i, (side, amount, at_boundary)) in pushed.into_iter().enumerate() {
            if amount > 0.0 && at_boundary {
                self.push[i] += amount;
                if self.push[i] >= EXIT_PUSH {
                    self.push[i] = 0.0;
                    let (lo, hi) = bbox.span(side);
                    let along = if side.is_horizontal() { now.y } else { now.x };
                    return Step::Exit {
                        side,
                        frac: fraction(along, lo, hi),
                    };
                }
            } else {
                self.push[i] = 0.0;
            }
        }
        if now == before {
            Step::Still
        } else {
            Step::Moved(now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, x: i32, y: i32, w: i32, h: i32, primary: bool) -> Screen {
        Screen {
            name: name.into(),
            rect: Rect::new(x, y, w, h),
            primary,
        }
    }

    fn dual() -> Vec<Screen> {
        vec![
            screen("A", 0, 0, 1920, 1080, true),
            screen("B", 1920, 0, 1920, 1080, false),
        ]
    }

    #[test]
    fn monitor_lookup_falls_back_to_primary() {
        let mons = dual();
        assert_eq!(find_monitor(&mons, "b").unwrap().name, "B");
        assert_eq!(find_monitor(&mons, "missing").unwrap().name, "A");
        assert_eq!(find_monitor(&mons, "").unwrap().name, "A");
        assert!(find_monitor(&[], "A").is_none());
    }

    #[test]
    fn outer_edge_needs_an_outward_push() {
        let link = Link::resolve(&dual(), "B", Side::Right).unwrap();
        assert!(link.neighbor.is_none());
        let at_edge = Point::new(3839, 540);
        assert_eq!(
            link.entry(at_edge, at_edge, (3, 0)).map(|e| e.0),
            Some(Side::Left)
        );
        assert!(
            link.entry(at_edge, at_edge, (-3, 0)).is_none(),
            "moving away"
        );
        assert!(
            link.entry(at_edge, at_edge, (0, 4)).is_none(),
            "sliding along"
        );
        assert!(link
            .entry(Point::new(3000, 540), Point::new(3100, 540), (100, 0))
            .is_none());
    }

    #[test]
    fn outer_edge_ignores_other_monitors_span() {
        // Client above A only: the top edge of B must not trigger.
        let link = Link::resolve(&dual(), "A", Side::Top).unwrap();
        let on_b = Point::new(2500, 0);
        assert!(link.entry(on_b, on_b, (0, -5)).is_none());
        let on_a = Point::new(960, 0);
        let (edge, frac) = link.entry(on_a, on_a, (0, -5)).unwrap();
        assert_eq!(edge, Side::Bottom);
        assert!((frac - 0.5).abs() < 0.01);
    }

    #[test]
    fn between_two_monitors_routes_both_directions() {
        let link = Link::resolve(&dual(), "A", Side::Right).unwrap();
        assert_eq!(link.neighbor, Some(Rect::new(1920, 0, 1920, 1080)));

        // A → client enters through the client's left edge.
        let e = link
            .entry(Point::new(1915, 300), Point::new(1925, 300), (10, 0))
            .unwrap();
        assert_eq!(e.0, Side::Left);
        // B → client enters through the client's right edge.
        let e = link
            .entry(Point::new(1925, 300), Point::new(1915, 300), (-10, 0))
            .unwrap();
        assert_eq!(e.0, Side::Right);
        // Moving within one monitor never triggers.
        assert!(link
            .entry(Point::new(100, 300), Point::new(900, 300), (800, 0))
            .is_none());

        // Leaving the client's right edge lands on B, its left edge on A.
        let to_b = link.exit(Side::Right, 0.5).unwrap();
        assert!(Rect::new(1920, 0, 1920, 1080).contains(to_b));
        assert_eq!(to_b.x, 1920 + RETURN_MARGIN);
        let to_a = link.exit(Side::Left, 0.5).unwrap();
        assert_eq!(to_a.x, 1919 - RETURN_MARGIN);
        assert!(link.exit(Side::Top, 0.5).is_none());
    }

    #[test]
    fn stacked_monitors_are_detected_as_neighbors() {
        let mons = vec![
            screen("top", 0, 0, 1920, 1080, true),
            screen("low", 200, 1080, 1280, 720, false),
        ];
        let link = Link::resolve(&mons, "top", Side::Bottom).unwrap();
        assert_eq!(link.neighbor, Some(Rect::new(200, 1080, 1280, 720)));
        // Outside the shared band there is no crossing at all.
        assert!(link
            .entry(Point::new(50, 1079), Point::new(50, 1079), (0, 9))
            .is_none());
        let e = link
            .entry(Point::new(600, 1075), Point::new(600, 1085), (0, 10))
            .unwrap();
        assert_eq!(e.0, Side::Top);
    }

    #[test]
    fn exit_without_neighbor_only_leads_home() {
        let link = Link::resolve(&dual(), "B", Side::Right).unwrap();
        assert_eq!(
            link.exit(Side::Left, 0.0),
            Some(Point::new(3839 - RETURN_MARGIN, 0))
        );
        assert!(link.exit(Side::Right, 0.5).is_none());
        assert_eq!(link.home(), Point::new(3839 - RETURN_MARGIN, 540));
    }

    #[test]
    fn enter_point_lands_on_a_real_screen() {
        // Two client screens of different heights: bbox has an empty corner.
        let screens = [Rect::new(0, 0, 1000, 500), Rect::new(1000, 0, 1000, 1000)];
        let p = enter_point(&screens, Side::Left, 1.0).unwrap();
        assert!(screens.iter().any(|r| r.contains(p)), "{p:?} is off-screen");
        assert_eq!(
            enter_point(&screens, Side::Right, 0.5),
            Some(Point::new(1999, 500))
        );
        assert!(enter_point(&[], Side::Left, 0.5).is_none());
    }

    #[test]
    fn cursor_moves_and_slides() {
        let screens = [Rect::new(0, 0, 100, 100)];
        let mut c = RemoteCursor::default();
        c.place(Point::new(50, 50));
        assert_eq!(
            c.advance(10.0, 5.0, &screens),
            Step::Moved(Point::new(60, 55))
        );
        // Blocked on x, still slides on y.
        c.place(Point::new(99, 50));
        assert_eq!(
            c.advance(5.0, 5.0, &screens),
            Step::Moved(Point::new(99, 55))
        );
        assert_eq!(c.advance(0.2, 0.0, &screens), Step::Still);
    }

    #[test]
    fn exit_requires_a_sustained_push() {
        let screens = [Rect::new(0, 0, 100, 100)];
        let mut c = RemoteCursor::default();
        c.place(Point::new(0, 40));
        assert_eq!(c.advance(-8.0, 0.0, &screens), Step::Still);
        // Backing off forgets the partial push.
        assert!(matches!(c.advance(3.0, 0.0, &screens), Step::Moved(_)));
        assert!(matches!(
            c.advance(-3.0 - 8.0, 0.0, &screens),
            Step::Moved(_)
        ));
        assert_eq!(c.advance(-8.0, 0.0, &screens), Step::Still);
        match c.advance(-8.0, 0.0, &screens) {
            Step::Exit {
                side: Side::Left,
                frac,
            } => assert!((frac - 40.0 / 99.0).abs() < 0.01),
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn diagonal_push_into_a_corner_still_exits() {
        let screens = [Rect::new(0, 0, 100, 100)];
        let mut c = RemoteCursor::default();
        c.place(Point::new(99, 99));
        let mut exits = Vec::new();
        for _ in 0..6 {
            if let Step::Exit { side, .. } = c.advance(6.0, 7.0, &screens) {
                exits.push(side);
            }
        }
        assert!(
            exits.contains(&Side::Right) && exits.contains(&Side::Bottom),
            "{exits:?}"
        );
    }

    #[test]
    fn inner_edges_between_client_screens_never_exit() {
        // The short screen's bottom edge is not the desktop's bottom edge.
        let screens = [Rect::new(0, 0, 100, 50), Rect::new(100, 0, 100, 100)];
        let mut c = RemoteCursor::default();
        c.place(Point::new(50, 49));
        for _ in 0..10 {
            assert!(!matches!(c.advance(0.0, 30.0, &screens), Step::Exit { .. }));
        }
        // And crossing between the two screens just works.
        c.place(Point::new(95, 20));
        assert_eq!(
            c.advance(10.0, 0.0, &screens),
            Step::Moved(Point::new(105, 20))
        );
    }
}
