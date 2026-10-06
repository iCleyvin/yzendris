//! The host's focus state machine.
//!
//! [`Switch`] decides which machine receives input. Platform back-ends feed it
//! raw local input (`on_motion`, `on_key`, …) and it answers two questions:
//! should this event be hidden from the local desktop, and what has to happen
//! as a consequence ([`Effect`]s: messages for a client, moving the local
//! cursor, starting or stopping capture).
//!
//! It performs no I/O, so every rule — edge crossing, shortcuts, the handling
//! of keys held across a switch — is covered by plain unit tests.

use crate::geom::{Point, Rect, Side};
use crate::keymap::*;
use crate::layout::{enter_point, snap, Link, RemoteCursor, Screen, Step};
use crate::proto::Msg;

/// Opaque handle for a client, assigned by the caller.
pub type ClientId = u32;

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Deliver `msg` to a client.
    Send { client: ClientId, msg: Msg },
    /// Start (`true`) or stop (`false`) intercepting local input.
    Capture(bool),
    /// Move the local cursor.
    Warp(Point),
    /// Input moved between machines (`None` is the host itself). The daemon
    /// uses this to carry the clipboard across.
    Focus {
        from: Option<ClientId>,
        to: Option<ClientId>,
    },
}

/// Static description of one paired client.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientSpec {
    pub id: ClientId,
    pub monitor: String,
    pub side: Side,
    pub enabled: bool,
}

#[derive(Debug)]
struct Slot {
    spec: ClientSpec,
    link: Option<Link>,
    /// Screens reported by the client; `None` while it is offline.
    screens: Option<Vec<Rect>>,
    cursor: RemoteCursor,
    /// Whether `cursor` holds a position from an earlier visit.
    visited: bool,
}

impl Slot {
    fn ready(&self) -> bool {
        self.spec.enabled && self.screens.as_ref().is_some_and(|s| !s.is_empty())
    }
}

/// Fixed-size set of key/button codes.
#[derive(Debug, Clone, Default)]
struct CodeSet([u64; 12]);

impl CodeSet {
    fn set(&mut self, code: u16, on: bool) {
        let (word, bit) = (usize::from(code) / 64, 1u64 << (code % 64));
        if let Some(w) = self.0.get_mut(word) {
            if on {
                *w |= bit
            } else {
                *w &= !bit
            }
        }
    }

    fn has(&self, code: u16) -> bool {
        self.0
            .get(usize::from(code) / 64)
            .is_some_and(|w| w & (1 << (code % 64)) != 0)
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(|w| *w == 0)
    }

    fn clear(&mut self) {
        self.0 = [0; 12];
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Goal {
    Local,
    Remote(ClientId),
}

#[derive(Debug)]
pub struct Switch {
    monitors: Vec<Screen>,
    slots: Vec<Slot>,
    /// Client currently receiving input.
    focus: Option<ClientId>,
    locked: bool,
    edges: bool,
    hotkeys: bool,
    speed: f64,
    /// Physical keys and buttons currently held on the host.
    held: CodeSet,
    /// Shortcut keys we hid; their release must be hidden too.
    swallowed: CodeSet,
    /// Ctrl+Shift+Alt were all down at some point since they were last all up.
    trio_armed: bool,
    /// Another key was pressed while the trio was held.
    chord_used: bool,
    /// A switch waiting for every key to be released.
    pending: Option<Goal>,
    /// Last known local cursor position.
    local: Option<Point>,
    /// Where the local cursor goes when control returns without an edge.
    return_to: Option<Point>,
    /// Sub-pixel remainder of relative motion.
    rel_rest: (f64, f64),
    out: Vec<Effect>,
}

impl Default for Switch {
    fn default() -> Self {
        Self::new()
    }
}

impl Switch {
    pub fn new() -> Self {
        Self {
            monitors: Vec::new(),
            slots: Vec::new(),
            focus: None,
            locked: false,
            edges: true,
            hotkeys: true,
            speed: 1.0,
            held: CodeSet::default(),
            swallowed: CodeSet::default(),
            trio_armed: false,
            chord_used: false,
            pending: None,
            local: None,
            return_to: None,
            rel_rest: (0.0, 0.0),
            out: Vec::new(),
        }
    }

    // ── Configuration ───────────────────────────────────────────────────────

    pub fn set_options(&mut self, edges: bool, hotkeys: bool, speed: f64) {
        self.edges = edges;
        self.hotkeys = hotkeys;
        self.speed = speed.clamp(0.2, 4.0);
    }

    pub fn set_monitors(&mut self, monitors: Vec<Screen>) {
        self.monitors = monitors;
        self.relink();
    }

    /// Replace the client list, keeping the live state of clients that remain.
    pub fn set_clients(&mut self, specs: Vec<ClientSpec>) {
        let mut old = std::mem::take(&mut self.slots);
        for spec in specs {
            let slot = match old.iter().position(|s| s.spec.id == spec.id) {
                Some(i) => {
                    let mut slot = old.swap_remove(i);
                    slot.spec = spec;
                    slot
                }
                None => Slot {
                    spec,
                    link: None,
                    screens: None,
                    cursor: RemoteCursor::default(),
                    visited: false,
                },
            };
            self.slots.push(slot);
        }
        self.relink();
        if let Some(id) = self.focus {
            if !self.slot(id).is_some_and(Slot::ready) {
                self.go_local(None, false);
            }
        }
    }

    fn relink(&mut self) {
        for slot in &mut self.slots {
            slot.link = Link::resolve(&self.monitors, &slot.spec.monitor, slot.spec.side);
        }
    }

    // ── Client lifecycle ────────────────────────────────────────────────────

    pub fn client_online(&mut self, id: ClientId, screens: Vec<Rect>) {
        if let Some(slot) = self.slot_mut(id) {
            slot.screens = Some(screens);
            slot.visited = false;
        }
    }

    pub fn client_screens(&mut self, id: ClientId, screens: Vec<Rect>) {
        if let Some(slot) = self.slot_mut(id) {
            if slot.screens.is_some() {
                let p = snap(&screens, slot.cursor.point());
                slot.cursor.place(p);
                slot.screens = Some(screens);
            }
        }
    }

    pub fn client_offline(&mut self, id: ClientId) {
        if let Some(slot) = self.slot_mut(id) {
            slot.screens = None;
        }
        if self.pending == Some(Goal::Remote(id)) {
            self.pending = None;
        }
        if self.focus == Some(id) {
            self.go_local(None, false);
        }
    }

    // ── Queries ─────────────────────────────────────────────────────────────

    pub fn focus(&self) -> Option<ClientId> {
        self.focus
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Effects produced since the last call. Callers drain this after every
    /// call into the switch.
    pub fn drain(&mut self) -> std::vec::Drain<'_, Effect> {
        self.out.drain(..)
    }

    // ── Input ───────────────────────────────────────────────────────────────

    /// Pointer motion by `(dx, dy)`. `at` is the resulting local cursor
    /// position when the platform can observe it. Returns whether to hide the
    /// event from the local desktop.
    pub fn on_motion(&mut self, dx: f64, dy: f64, at: Option<Point>) -> bool {
        let Some(id) = self.focus else {
            self.local_motion(dx, dy, at);
            return self.focus.is_some();
        };
        let speed = self.speed;
        if self.locked {
            let (fx, fy) = (dx * speed + self.rel_rest.0, dy * speed + self.rel_rest.1);
            let (ix, iy) = (fx.trunc(), fy.trunc());
            self.rel_rest = (fx - ix, fy - iy);
            if ix != 0.0 || iy != 0.0 {
                self.send(
                    id,
                    Msg::PointerRel {
                        dx: ix as i32,
                        dy: iy as i32,
                    },
                );
            }
            return true;
        }
        let Some(slot) = self.slots.iter_mut().find(|s| s.spec.id == id) else {
            return true;
        };
        let screens = slot.screens.as_deref().unwrap_or(&[]);
        match slot.cursor.advance(dx * speed, dy * speed, screens) {
            Step::Still => {}
            Step::Moved(p) => self.send(id, Msg::PointerAbs { x: p.x, y: p.y }),
            Step::Exit { side, frac } => {
                // Never leave mid-drag or mid-keystroke: the release would
                // land on the wrong machine.
                let target = slot.link.and_then(|l| l.exit(side, frac));
                if let (Some(p), true) = (target, self.held.is_empty()) {
                    self.go_local(Some(p), true);
                }
            }
        }
        true
    }

    fn local_motion(&mut self, dx: f64, dy: f64, at: Option<Point>) {
        let Some(cur) = at else { return };
        let prev = self.local.replace(cur).unwrap_or(cur);
        if !self.edges || self.locked || self.pending.is_some() || !self.held.is_empty() {
            return;
        }
        let motion = (dx.round() as i32, dy.round() as i32);
        let hit = self.slots.iter().filter(|s| s.ready()).find_map(|s| {
            let link = s.link?;
            let (edge, frac) = link.entry(prev, cur, motion)?;
            let p = enter_point(s.screens.as_deref()?, edge, frac)?;
            // Coming home by shortcut puts the cursor back beside the edge it
            // left through.
            Some((s.spec.id, p, link.exit(edge, frac)))
        });
        if let Some((id, p, back)) = hit {
            self.return_to = back.or(Some(prev));
            self.go_remote(id, p);
        }
    }

    /// Key press or release (`code` is a Linux `KEY_*`). Auto-repeat presses
    /// may be passed in; they are recognised and dropped.
    pub fn on_key(&mut self, code: u16, down: bool) -> bool {
        if code == 0 || code > KEY_MAX {
            return self.focus.is_some();
        }
        if self.swallowed.has(code) {
            if !down {
                self.swallowed.set(code, false);
                self.held.set(code, false);
                self.after_release();
            }
            return true;
        }
        let repeat = down && self.held.has(code);
        let trio_before = self.trio();
        self.held.set(code, down);

        if self.hotkeys && !repeat {
            if down && trio_before && !is_modifier(code) {
                self.chord_used = true;
                if self.chord(code) {
                    self.swallowed.set(code, true);
                    return true;
                }
            }
            if self.trio() {
                self.trio_armed = true;
            }
        }

        let swallow = match self.focus {
            Some(id) => {
                if !repeat {
                    self.send(id, Msg::Key { code, down });
                }
                true
            }
            None => false,
        };

        if !down {
            if self.trio_armed && !self.any_trio_key() {
                // Ctrl+Shift+Alt tapped on their own: bring control home.
                if !self.chord_used && self.focus.is_some() {
                    self.pending = Some(Goal::Local);
                }
                self.trio_armed = false;
                self.chord_used = false;
            }
            self.after_release();
        }
        swallow
    }

    /// Mouse button (`code` is a Linux `BTN_*`).
    pub fn on_button(&mut self, code: u16, down: bool) -> bool {
        self.held.set(code, down);
        let swallow = match self.focus {
            Some(id) => {
                self.send(id, Msg::Button { code, down });
                true
            }
            None => false,
        };
        if !down {
            self.after_release();
        }
        swallow
    }

    /// Wheel motion in 1/120 notches.
    pub fn on_scroll(&mut self, dx: i32, dy: i32) -> bool {
        match self.focus {
            Some(id) => {
                self.send(id, Msg::Scroll { dx, dy });
                true
            }
            None => false,
        }
    }

    /// Forget every held key, e.g. after the platform lost track of input
    /// (session lock, device unplugged).
    pub fn reset_held(&mut self) {
        self.held.clear();
        self.swallowed.clear();
        self.trio_armed = false;
        self.chord_used = false;
        self.after_release();
    }

    // ── Shortcuts ───────────────────────────────────────────────────────────

    fn trio(&self) -> bool {
        let h = &self.held;
        (h.has(KEY_LEFTCTRL) || h.has(KEY_RIGHTCTRL))
            && (h.has(KEY_LEFTSHIFT) || h.has(KEY_RIGHTSHIFT))
            // Left Alt only: AltGr is reported as Ctrl+RightAlt on Windows and
            // must keep typing characters, not trigger shortcuts.
            && h.has(KEY_LEFTALT)
    }

    fn any_trio_key(&self) -> bool {
        [
            KEY_LEFTCTRL,
            KEY_RIGHTCTRL,
            KEY_LEFTSHIFT,
            KEY_RIGHTSHIFT,
            KEY_LEFTALT,
        ]
        .iter()
        .any(|k| self.held.has(*k))
    }

    /// Handle a key pressed while Ctrl+Shift+Alt are held. Returns whether it
    /// was one of ours.
    fn chord(&mut self, code: u16) -> bool {
        let goal = match code {
            KEY_RIGHT => self.cycle(1),
            KEY_LEFT => self.cycle(-1),
            KEY_0 => Some(Goal::Local),
            KEY_1..=10 => {
                let ready: Vec<ClientId> = self.ready_ids();
                ready
                    .get(usize::from(code - KEY_1))
                    .map(|id| Goal::Remote(*id))
            }
            KEY_L => {
                self.set_locked(!self.locked);
                return true;
            }
            _ => return false,
        };
        if let Some(goal) = goal {
            self.pending = Some(goal);
        }
        true
    }

    fn ready_ids(&self) -> Vec<ClientId> {
        self.slots
            .iter()
            .filter(|s| s.ready())
            .map(|s| s.spec.id)
            .collect()
    }

    /// Next machine in the ring `host, client 1, client 2, …`.
    fn cycle(&self, step: i32) -> Option<Goal> {
        let ring: Vec<Goal> = std::iter::once(Goal::Local)
            .chain(self.ready_ids().into_iter().map(Goal::Remote))
            .collect();
        if ring.len() < 2 {
            return None;
        }
        let here = match self.focus {
            Some(id) => Goal::Remote(id),
            None => Goal::Local,
        };
        let i = ring.iter().position(|g| *g == here).unwrap_or(0) as i32;
        Some(ring[(i + step).rem_euclid(ring.len() as i32) as usize])
    }

    /// Lock the pointer to the current machine. While a client is focused this
    /// also switches it to raw relative motion, which is what games need.
    pub fn set_locked(&mut self, locked: bool) {
        if self.locked == locked {
            return;
        }
        self.locked = locked;
        self.rel_rest = (0.0, 0.0);
        if let Some(id) = self.focus {
            self.send(id, Msg::Relative(locked));
        }
    }

    /// Ask for input to move to `client` (or home with `None`) as soon as no
    /// key is held.
    pub fn request(&mut self, client: Option<ClientId>) {
        self.pending = Some(client.map_or(Goal::Local, Goal::Remote));
        self.after_release();
    }

    // ── Transitions ─────────────────────────────────────────────────────────

    /// Run a deferred switch once every key and button is up, so presses and
    /// releases are never split across two machines.
    fn after_release(&mut self) {
        if !self.held.is_empty() {
            return;
        }
        match self.pending.take() {
            Some(Goal::Local) => self.go_local(None, true),
            Some(Goal::Remote(id)) if self.focus != Some(id) => {
                let Some(slot) = self.slot(id).filter(|s| s.ready()) else {
                    return;
                };
                let screens = slot.screens.as_deref().unwrap_or(&[]);
                let p = if slot.visited {
                    snap(screens, slot.cursor.point())
                } else {
                    screens.first().map(Rect::center).unwrap_or_default()
                };
                if self.focus.is_none() {
                    self.return_to = self.local;
                }
                self.go_remote(id, p);
            }
            _ => {}
        }
    }

    fn go_remote(&mut self, id: ClientId, at: Point) {
        let from = self.focus;
        match from {
            Some(old) => self.send(old, Msg::Leave),
            None => self.out.push(Effect::Capture(true)),
        }
        if let Some(slot) = self.slot_mut(id) {
            slot.cursor.place(at);
            slot.visited = true;
        }
        self.focus = Some(id);
        self.rel_rest = (0.0, 0.0);
        self.send(
            id,
            Msg::Enter {
                x: at.x,
                y: at.y,
                relative: self.locked,
            },
        );
        self.out.push(Effect::Focus { from, to: Some(id) });
    }

    /// Return input to the host. `notify` is false when the client is gone.
    fn go_local(&mut self, at: Option<Point>, notify: bool) {
        let Some(id) = self.focus.take() else { return };
        if notify {
            self.send(id, Msg::Leave);
        }
        self.out.push(Effect::Capture(false));
        if let Some(p) = at.or(self.return_to.take()) {
            self.local = Some(p);
            self.out.push(Effect::Warp(p));
        }
        self.return_to = None;
        // Presses that went to the client must not count as held locally.
        self.swallowed.clear();
        self.out.push(Effect::Focus {
            from: Some(id),
            to: None,
        });
    }

    fn send(&mut self, client: ClientId, msg: Msg) {
        self.out.push(Effect::Send { client, msg });
    }

    fn slot(&self, id: ClientId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.spec.id == id)
    }

    fn slot_mut(&mut self, id: ClientId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.spec.id == id)
    }
}

fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        KEY_LEFTCTRL
            | KEY_RIGHTCTRL
            | KEY_LEFTSHIFT
            | KEY_RIGHTSHIFT
            | KEY_LEFTALT
            | KEY_RIGHTALT
            | KEY_LEFTMETA
            | KEY_RIGHTMETA
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::RETURN_MARGIN;
    use crate::proto::button;

    const A: ClientId = 1;
    const B: ClientId = 2;
    const KEY_A: u16 = 30;
    const KEY_T: u16 = 20;

    fn spec(id: ClientId, side: Side) -> ClientSpec {
        ClientSpec {
            id,
            monitor: String::new(),
            side,
            enabled: true,
        }
    }

    /// One 1920×1080 host monitor, client A on its right, client B on its left.
    fn rig() -> Switch {
        let mut sw = Switch::new();
        sw.set_monitors(vec![Screen {
            name: "M".into(),
            rect: Rect::new(0, 0, 1920, 1080),
            primary: true,
        }]);
        sw.set_clients(vec![spec(A, Side::Right), spec(B, Side::Left)]);
        sw.client_online(A, vec![Rect::new(0, 0, 1280, 720)]);
        sw.client_online(B, vec![Rect::new(0, 0, 2560, 1440)]);
        sw
    }

    fn fx(sw: &mut Switch) -> Vec<Effect> {
        sw.drain().collect()
    }

    fn sent(effects: &[Effect], client: ClientId) -> Vec<Msg> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send { client: c, msg } if *c == client => Some(msg.clone()),
                _ => None,
            })
            .collect()
    }

    fn cross_right(sw: &mut Switch) -> Vec<Effect> {
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));
        fx(sw)
    }

    fn tap_trio(sw: &mut Switch, chord: Option<u16>) {
        sw.on_key(KEY_LEFTCTRL, true);
        sw.on_key(KEY_LEFTSHIFT, true);
        sw.on_key(KEY_LEFTALT, true);
        if let Some(k) = chord {
            sw.on_key(k, true);
            sw.on_key(k, false);
        }
        sw.on_key(KEY_LEFTALT, false);
        sw.on_key(KEY_LEFTSHIFT, false);
        sw.on_key(KEY_LEFTCTRL, false);
    }

    #[test]
    fn crossing_an_edge_enters_the_client_at_the_matching_height() {
        let mut sw = rig();
        let effects = cross_right(&mut sw);
        assert_eq!(sw.focus(), Some(A));
        assert_eq!(effects[0], Effect::Capture(true));
        // Half-way down the host monitor → half-way down the client's left edge.
        assert_eq!(
            sent(&effects, A),
            [Msg::Enter {
                x: 0,
                y: 360,
                relative: false
            }]
        );
        assert!(effects.contains(&Effect::Focus {
            from: None,
            to: Some(A)
        }));
    }

    #[test]
    fn motion_is_forwarded_as_absolute_positions() {
        let mut sw = rig();
        cross_right(&mut sw);
        assert!(sw.on_motion(100.0, -10.0, None));
        assert_eq!(sent(&fx(&mut sw), A), [Msg::PointerAbs { x: 100, y: 350 }]);
        // Sub-pixel motion accumulates instead of being lost.
        for _ in 0..4 {
            sw.on_motion(0.3, 0.0, None);
        }
        assert_eq!(sent(&fx(&mut sw), A), [Msg::PointerAbs { x: 101, y: 350 }]);
    }

    #[test]
    fn pushing_back_through_the_edge_returns_home() {
        let mut sw = rig();
        cross_right(&mut sw);
        sw.on_motion(-15.0, 0.0, None);
        assert_eq!(sw.focus(), Some(A), "a light brush must not leave");
        sw.on_motion(-15.0, 0.0, None);
        assert_eq!(sw.focus(), None);
        let effects = fx(&mut sw);
        assert_eq!(sent(&effects, A), [Msg::Leave]);
        assert!(effects.contains(&Effect::Capture(false)));
        assert!(effects.contains(&Effect::Warp(Point::new(1919 - RETURN_MARGIN, 540))));
        assert!(effects.contains(&Effect::Focus {
            from: Some(A),
            to: None
        }));
    }

    #[test]
    fn far_edge_of_a_client_leads_nowhere() {
        let mut sw = rig();
        cross_right(&mut sw);
        sw.on_motion(5000.0, 0.0, None);
        for _ in 0..5 {
            sw.on_motion(50.0, 0.0, None);
        }
        assert_eq!(sw.focus(), Some(A));
    }

    #[test]
    fn keys_and_buttons_follow_focus() {
        let mut sw = rig();
        assert!(!sw.on_key(KEY_A, true), "local keys pass through");
        assert!(!sw.on_key(KEY_A, false));
        assert!(fx(&mut sw).is_empty());

        cross_right(&mut sw);
        assert!(sw.on_key(KEY_A, true));
        assert!(sw.on_key(KEY_A, true), "auto-repeat is hidden…");
        assert!(sw.on_key(KEY_A, false));
        assert!(sw.on_button(button::LEFT, true));
        assert!(sw.on_button(button::LEFT, false));
        assert!(sw.on_scroll(0, -120));
        assert_eq!(
            sent(&fx(&mut sw), A),
            [
                Msg::Key {
                    code: KEY_A,
                    down: true
                }, // …and not sent twice
                Msg::Key {
                    code: KEY_A,
                    down: false
                },
                Msg::Button {
                    code: button::LEFT,
                    down: true
                },
                Msg::Button {
                    code: button::LEFT,
                    down: false
                },
                Msg::Scroll { dx: 0, dy: -120 },
            ]
        );
    }

    #[test]
    fn no_switch_while_dragging() {
        let mut sw = rig();
        sw.on_button(button::LEFT, true);
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(!sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));
        assert_eq!(sw.focus(), None);
        sw.on_button(button::LEFT, false);
        assert!(sw.on_motion(3.0, 0.0, Some(Point::new(1919, 540))));

        // Same on the way back.
        sw.on_button(button::LEFT, true);
        sw.on_motion(-500.0, 0.0, None);
        assert_eq!(sw.focus(), Some(A));
    }

    #[test]
    fn offline_and_disabled_clients_are_skipped() {
        let mut sw = rig();
        sw.client_offline(A);
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(!sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));

        let mut sw = rig();
        sw.set_clients(vec![ClientSpec {
            enabled: false,
            ..spec(A, Side::Right)
        }]);
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(!sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));
    }

    #[test]
    fn losing_the_active_client_brings_input_home() {
        let mut sw = rig();
        cross_right(&mut sw);
        sw.client_offline(A);
        assert_eq!(sw.focus(), None);
        let effects = fx(&mut sw);
        assert!(
            sent(&effects, A).is_empty(),
            "nothing is sent to a dead link"
        );
        assert!(effects.contains(&Effect::Capture(false)));
        assert!(effects.iter().any(|e| matches!(e, Effect::Warp(_))));
    }

    #[test]
    fn shortcut_cycles_through_machines_after_keys_are_released() {
        let mut sw = rig();
        sw.on_motion(0.0, 0.0, Some(Point::new(400, 300)));
        sw.on_key(KEY_LEFTCTRL, true);
        sw.on_key(KEY_LEFTSHIFT, true);
        sw.on_key(KEY_LEFTALT, true);
        assert!(sw.on_key(KEY_RIGHT, true), "the shortcut key is hidden");
        assert!(sw.on_key(KEY_RIGHT, false));
        assert_eq!(sw.focus(), None, "waits for the modifiers");
        sw.on_key(KEY_LEFTALT, false);
        sw.on_key(KEY_LEFTSHIFT, false);
        sw.on_key(KEY_LEFTCTRL, false);
        assert_eq!(sw.focus(), Some(A));
        // First visit by shortcut lands in the middle of the client's screen.
        assert_eq!(
            sent(&fx(&mut sw), A),
            [Msg::Enter {
                x: 640,
                y: 360,
                relative: false
            }]
        );

        tap_trio(&mut sw, Some(KEY_RIGHT));
        assert_eq!(sw.focus(), Some(B));
        let effects = fx(&mut sw);
        assert!(sent(&effects, A).contains(&Msg::Leave));
        assert!(
            !effects.contains(&Effect::Capture(false)),
            "client → client stays captured"
        );

        tap_trio(&mut sw, Some(KEY_RIGHT));
        assert_eq!(sw.focus(), None);
        // …and the cursor goes back where it was before leaving.
        assert!(fx(&mut sw).contains(&Effect::Warp(Point::new(400, 300))));

        tap_trio(&mut sw, Some(KEY_LEFT));
        assert_eq!(sw.focus(), Some(B));
        tap_trio(&mut sw, Some(KEY_1));
        assert_eq!(sw.focus(), Some(A));
        tap_trio(&mut sw, Some(KEY_0));
        assert_eq!(sw.focus(), None);
    }

    #[test]
    fn tapping_the_trio_alone_comes_home() {
        let mut sw = rig();
        cross_right(&mut sw);
        tap_trio(&mut sw, None);
        assert_eq!(sw.focus(), None);
        // …but not when it was part of a shortcut for the remote app.
        cross_right(&mut sw);
        tap_trio(&mut sw, Some(KEY_T));
        assert_eq!(sw.focus(), Some(A));
        assert!(sent(&fx(&mut sw), A).contains(&Msg::Key {
            code: KEY_T,
            down: true
        }));
        // Tapping it locally does nothing.
        tap_trio(&mut sw, None);
        tap_trio(&mut sw, None);
        assert_eq!(sw.focus(), None);
    }

    #[test]
    fn altgr_never_triggers_shortcuts() {
        let mut sw = rig();
        cross_right(&mut sw);
        // AltGr+Shift+digit as Windows reports it: Ctrl + RightAlt + Shift.
        sw.on_key(KEY_LEFTCTRL, true);
        sw.on_key(KEY_RIGHTALT, true);
        sw.on_key(KEY_LEFTSHIFT, true);
        sw.on_key(KEY_1, true);
        sw.on_key(KEY_1, false);
        sw.on_key(KEY_LEFTSHIFT, false);
        sw.on_key(KEY_RIGHTALT, false);
        sw.on_key(KEY_LEFTCTRL, false);
        assert_eq!(sw.focus(), Some(A));
        assert!(sent(&fx(&mut sw), A).contains(&Msg::Key {
            code: KEY_1,
            down: true
        }));
    }

    #[test]
    fn lock_disables_edges_and_sends_relative_motion() {
        let mut sw = rig();
        cross_right(&mut sw);
        tap_trio(&mut sw, Some(KEY_L));
        assert!(sw.locked());
        assert!(sent(&fx(&mut sw), A).contains(&Msg::Relative(true)));
        sw.on_motion(-900.5, 3.0, None);
        sw.on_motion(-900.5, 0.0, None);
        assert_eq!(sw.focus(), Some(A), "edges are off while locked");
        assert_eq!(
            sent(&fx(&mut sw), A),
            [
                Msg::PointerRel { dx: -900, dy: 3 },
                Msg::PointerRel { dx: -901, dy: 0 }
            ]
        );
        tap_trio(&mut sw, Some(KEY_L));
        assert!(!sw.locked());

        // Locked on the host: the cursor cannot escape.
        let mut sw = rig();
        sw.set_locked(true);
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(!sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));
    }

    #[test]
    fn options_can_disable_edges_and_shortcuts() {
        let mut sw = rig();
        sw.set_options(false, false, 1.0);
        sw.on_motion(5.0, 0.0, Some(Point::new(1900, 540)));
        assert!(!sw.on_motion(30.0, 0.0, Some(Point::new(1919, 540))));
        tap_trio(&mut sw, Some(KEY_RIGHT));
        assert_eq!(sw.focus(), None);
        sw.request(Some(A));
        assert_eq!(sw.focus(), Some(A), "explicit requests still work");
    }

    #[test]
    fn pointer_speed_scales_remote_motion() {
        let mut sw = rig();
        sw.set_options(true, true, 2.0);
        cross_right(&mut sw);
        sw.on_motion(10.0, 0.0, None);
        assert_eq!(sent(&fx(&mut sw), A), [Msg::PointerAbs { x: 20, y: 360 }]);
    }

    #[test]
    fn removing_the_focused_client_releases_capture() {
        let mut sw = rig();
        cross_right(&mut sw);
        sw.set_clients(vec![spec(B, Side::Left)]);
        assert_eq!(sw.focus(), None);
        assert!(fx(&mut sw).contains(&Effect::Capture(false)));
    }

    #[test]
    fn screens_update_keeps_the_cursor_on_screen() {
        let mut sw = rig();
        cross_right(&mut sw);
        sw.on_motion(1200.0, 600.0, None);
        sw.client_screens(A, vec![Rect::new(0, 0, 800, 600)]);
        fx(&mut sw);
        sw.on_motion(-1.0, 0.0, None);
        assert_eq!(sent(&fx(&mut sw), A), [Msg::PointerAbs { x: 798, y: 599 }]);
    }
}
