// SPDX-License-Identifier: GPL-3.0-or-later
//! Two-finger gesture recognition over a stream of touch points.
//!
//! This is a *pure* state machine. It takes contact positions (normalised 0..1)
//! and timestamps and returns a gesture; it never opens a device, never grabs a
//! digitizer and never asks for a portal permission. Positions arrive
//! normalised and are converted to panel pixels before anything is measured,
//! because the Edge is 2560x720: a distance that is a small fraction of the
//! width is a large fraction of the height, so thresholds in normalised units
//! would make a vertical swipe far easier than a horizontal one.
//!
//! Gesture recognition is **opt-in and disabled by default**: [`GESTURES_ENABLED`]
//! is `false` and the config's `gestures.enabled` defaults to `false`, so the
//! recognizer never influences the panel unless a caller explicitly turns it on.
//! Nothing in this module performs input-device grabbing or portal requests.
//
// The recognizer is a public API surface that the (flag-disabled) touch
// pipeline will drive once input grabbing is re-enabled; only `config_snapshot`
// is wired into the RPC today. Marking the rest dead avoids a wall of warnings
// for a deliberately opt-in module.
#![allow(dead_code)]

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Gesture recognition is opt-in and OFF by default.
///
/// This is the flag that gates the recognizer at integration level. It is a
/// compile-time constant so the migration ships disabled; nothing here consults
/// a device, so turning the integration on later only means running the
/// recognizer over points that are already being streamed.
pub const GESTURES_ENABLED: bool = false;

/// A recognised gesture kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKind {
    None,
    SwipeLeft,
    SwipeRight,
    SwipeUp,
    SwipeDown,
    Tap,
    LongPress,
    PinchIn,
    PinchOut,
}

impl GestureKind {
    fn base_name(self) -> &'static str {
        match self {
            GestureKind::SwipeLeft => "swipe-left",
            GestureKind::SwipeRight => "swipe-right",
            GestureKind::SwipeUp => "swipe-up",
            GestureKind::SwipeDown => "swipe-down",
            GestureKind::Tap => "tap",
            GestureKind::LongPress => "long-press",
            GestureKind::PinchIn => "pinch-in",
            GestureKind::PinchOut => "pinch-out",
            GestureKind::None => "none",
        }
    }
}

/// A single recognised gesture, produced once per multi-finger sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    pub kind: GestureKind,
    /// How many fingers took part.
    pub contacts: i32,
    /// Travel for a swipe, or scale change for a pinch (panel pixels).
    pub distance_px: f64,
    pub duration_ms: i64,
}

impl Default for Gesture {
    fn default() -> Self {
        Self { kind: GestureKind::None, contacts: 0, distance_px: 0.0, duration_ms: 0 }
    }
}

impl Gesture {
    /// True when this is a real gesture rather than the empty/default value,
    /// mirroring the C++ `operator bool`.
    pub fn recognized(&self) -> bool {
        self.kind != GestureKind::None
    }

    /// "swipe-left", "two-finger-tap", "pinch-in", ...
    ///
    /// A pinch is inherently two-fingered, so it is named "pinch-in"/"pinch-out"
    /// rather than "two-finger-pinch-in"; the contact count is added for every
    /// other kind.
    pub fn name(&self) -> String {
        if self.kind == GestureKind::None {
            return "none".to_string();
        }
        if self.kind == GestureKind::PinchIn || self.kind == GestureKind::PinchOut {
            return self.kind.base_name().to_string();
        }
        format!("{}{}", finger_prefix(self.contacts), self.kind.base_name())
    }
}

/// Thresholds and panel geometry the recognizer measures against.
#[derive(Clone, Copy, Debug)]
pub struct GestureLimits {
    pub panel_width_px: f64,
    pub panel_height_px: f64,

    /// A swipe has to travel, be mostly along one axis, and not take all day.
    pub swipe_min_px: f64,
    pub swipe_axis_ratio: f64,
    pub swipe_max_ms: i64,

    /// A tap barely moves. Anything slower than long_press_ms that also barely
    /// moved is a long press instead.
    pub tap_max_px: f64,
    pub tap_max_ms: i64,
    pub long_press_ms: i64,

    /// A pinch needs two contacts and a real change in their separation.
    pub pinch_min_px: f64,
}

impl Default for GestureLimits {
    fn default() -> Self {
        Self {
            panel_width_px: 2560.0,
            panel_height_px: 720.0,
            swipe_min_px: 180.0,
            swipe_axis_ratio: 1.6,
            swipe_max_ms: 900,
            tap_max_px: 26.0,
            tap_max_ms: 320,
            long_press_ms: 600,
            pinch_min_px: 120.0,
        }
    }
}

/// A contact that is down (or was lifted but not yet released as a group).
#[derive(Clone, Copy, Debug)]
struct Contact {
    id: i32,
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_y: f64,
    start_ms: i64,
    live: bool,
}

/// State machine fed one contact at a time.
///
/// A gesture is produced only when the last contact of a sequence lifts, so a
/// two-finger swipe is one gesture rather than two. This is a pure value: it
/// owns no device handle and performs no I/O.
#[derive(Default)]
pub struct GestureRecognizer {
    limits: GestureLimits,
    active: Vec<Contact>,
    finished: Vec<Contact>,
    peak_contacts: i32,
}

impl GestureRecognizer {
    pub fn new(limits: GestureLimits) -> Self {
        Self { limits, active: Vec::new(), finished: Vec::new(), peak_contacts: 0 }
    }

    pub fn reset(&mut self) {
        self.active.clear();
        self.finished.clear();
        self.peak_contacts = 0;
    }

    pub fn active_contacts(&self) -> i32 {
        self.active.len() as i32
    }

    pub fn begin(&mut self, id: i32, nx: f64, ny: f64, t_ms: i64) {
        let width = self.limits.panel_width_px;
        let height = self.limits.panel_height_px;
        // A repeated begin for a live id means we missed an end. Treat it as a
        // fresh contact rather than accumulating a duplicate.
        if let Some(existing) = self.active.iter_mut().find(|c| c.id == id) {
            existing.start_x = nx * width;
            existing.start_y = ny * height;
            existing.last_x = existing.start_x;
            existing.last_y = existing.start_y;
            existing.start_ms = t_ms;
            return;
        }
        let contact = Contact {
            id,
            start_x: nx * width,
            start_y: ny * height,
            last_x: nx * width,
            last_y: ny * height,
            start_ms: t_ms,
            live: true,
        };
        self.active.push(contact);
        self.peak_contacts = self.peak_contacts.max(self.active.len() as i32);
    }

    pub fn update(&mut self, id: i32, nx: f64, ny: f64, _t_ms: i64) {
        let width = self.limits.panel_width_px;
        let height = self.limits.panel_height_px;
        if let Some(contact) = self.active.iter_mut().find(|c| c.id == id) {
            contact.last_x = nx * width;
            contact.last_y = ny * height;
        }
    }

    /// Returns a gesture only when this was the last contact down.
    ///
    /// Mirrors the C++ contract: it always returns a [`Gesture`]; when there is
    /// no gesture (a contact that never began, a partially-lifted sequence, or
    /// a motion that matched nothing) that value has kind [`GestureKind::None`]
    /// and [`Gesture::recognized`] is `false`.
    pub fn end(&mut self, id: i32, t_ms: i64) -> Gesture {
        let position = match self.active.iter().position(|c| c.id == id) {
            Some(position) => position,
            None => return Gesture::default(),
        };
        let mut done = self.active.remove(position);
        done.live = false;
        self.finished.push(done);

        // Only classify once every finger is off the panel, so a two-finger
        // swipe is one gesture rather than two arriving a few ms apart.
        if !self.active.is_empty() {
            return Gesture::default();
        }

        let gesture = self.classify(t_ms);
        self.reset();
        gesture
    }

    fn classify(&self, end_ms: i64) -> Gesture {
        if self.finished.is_empty() {
            return Gesture::default();
        }

        let mut gesture = Gesture { contacts: self.peak_contacts, ..Gesture::default() };
        let none = Gesture::default();
        let mut start_ms = self.finished[0].start_ms;
        for contact in &self.finished {
            start_ms = start_ms.min(contact.start_ms);
        }
        gesture.duration_ms = end_ms - start_ms;

        // Pinch first: two contacts whose separation changed substantially is a
        // pinch even if each finger also travelled far enough to look like a
        // swipe on its own.
        if self.finished.len() == 2 {
            let a = self.finished[0];
            let b = self.finished[1];
            let before = distance(a.start_x, a.start_y, b.start_x, b.start_y);
            let after = distance(a.last_x, a.last_y, b.last_x, b.last_y);
            let change = after - before;
            if change.abs() >= self.limits.pinch_min_px {
                gesture.kind = if change > 0.0 {
                    GestureKind::PinchOut
                } else {
                    GestureKind::PinchIn
                };
                gesture.distance_px = change.abs();
                return gesture;
            }
        }

        // Otherwise use the average travel of every contact, so a two-finger
        // swipe is judged by where the hand went rather than by its most eager
        // finger.
        let mut sum_dx = 0.0;
        let mut sum_dy = 0.0;
        for contact in &self.finished {
            sum_dx += contact.last_x - contact.start_x;
            sum_dy += contact.last_y - contact.start_y;
        }
        let dx = sum_dx / self.finished.len() as f64;
        let dy = sum_dy / self.finished.len() as f64;
        let adx = dx.abs();
        let ady = dy.abs();
        let travel = (dx * dx + dy * dy).sqrt();

        if travel >= self.limits.swipe_min_px && gesture.duration_ms <= self.limits.swipe_max_ms {
            // Require one axis to dominate, so a diagonal drag is not forced
            // into a direction it only barely favours.
            if adx >= ady * self.limits.swipe_axis_ratio {
                gesture.kind = if dx < 0.0 {
                    GestureKind::SwipeLeft
                } else {
                    GestureKind::SwipeRight
                };
                gesture.distance_px = adx;
                return gesture;
            }
            if ady >= adx * self.limits.swipe_axis_ratio {
                gesture.kind = if dy < 0.0 {
                    GestureKind::SwipeUp
                } else {
                    GestureKind::SwipeDown
                };
                gesture.distance_px = ady;
                return gesture;
            }
            return none; // moved a long way, but in no particular direction
        }

        if travel <= self.limits.tap_max_px {
            if gesture.duration_ms >= self.limits.long_press_ms {
                gesture.kind = GestureKind::LongPress;
                return gesture;
            }
            if gesture.duration_ms <= self.limits.tap_max_ms {
                gesture.kind = GestureKind::Tap;
                return gesture;
            }
        }
        none // a slow short drag is not a gesture, it is a fidget
    }
}

fn distance(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    (dx * dx + dy * dy).sqrt()
}

fn finger_prefix(contacts: i32) -> &'static str {
    match contacts {
        2 => "two-finger-",
        3 => "three-finger-",
        4 => "four-finger-",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Gesture / touch config
// ---------------------------------------------------------------------------

/// The reserved band against one edge of the panel.
///
/// A band is described rather than a rectangle because the panel is a strip:
/// what people want is "ignore the end my palm rests on".
#[derive(Clone, Debug, PartialEq)]
pub struct LockZone {
    pub enabled: bool,
    /// left | right | top | bottom
    pub side: String,
    /// 0.05 .. 0.9
    pub fraction: f64,
}

impl Default for LockZone {
    fn default() -> Self {
        Self { enabled: false, side: "right".to_string(), fraction: 0.4 }
    }
}

/// The gesture config: whether gestures are on, the gesture->action bindings,
/// and the lock zone.
#[derive(Clone, Debug, PartialEq)]
pub struct GestureConfig {
    pub gestures_enabled: bool,
    /// gesture name -> action id. Anything unbound, or bound to "none", does
    /// nothing.
    pub bindings: BTreeMap<String, String>,
    pub lock_zone: LockZone,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            gestures_enabled: false,
            bindings: BTreeMap::new(),
            lock_zone: LockZone::default(),
        }
    }
}

/// The actions a gesture can be bound to, and a one-line description each.
const ACTIONS: &[(&str, &str)] = &[
    ("none", "do nothing"),
    ("profile-next", "next profile"),
    ("profile-previous", "previous profile"),
    ("blank-toggle", "blank or unblank the panel"),
    ("brightness-up", "brightness up 10"),
    ("brightness-down", "brightness down 10"),
    ("dashboard-toggle", "show or hide the dashboard"),
    ("dashboard-page-next", "next dashboard page"),
];

pub fn actions() -> BTreeMap<String, String> {
    ACTIONS.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

pub fn is_known_action(id: &str) -> bool {
    ACTIONS.iter().any(|(k, _)| *k == id)
}

/// True when the point falls inside the reserved band.
pub fn in_lock_zone(zone: &LockZone, nx: f64, ny: f64) -> bool {
    if !zone.enabled {
        return false;
    }
    let fraction = clamp(zone.fraction, 0.05, 0.9);
    match zone.side.as_str() {
        "left" => nx <= fraction,
        "right" => nx >= 1.0 - fraction,
        "top" => ny <= fraction,
        "bottom" => ny >= 1.0 - fraction,
        _ => false,
    }
}

/// Off unless the file says otherwise: a config that fails to parse must not
/// start changing the panel when someone brushes the glass.
pub fn from_json(obj: &Value) -> GestureConfig {
    let mut cfg = GestureConfig::default();

    if let Some(gestures) = obj.get("gestures").and_then(Value::as_object) {
        cfg.gestures_enabled = gestures.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        if let Some(bindings) = gestures.get("bindings").and_then(Value::as_object) {
            for (name, action) in bindings {
                if let Some(action_id) = action.as_str() {
                    if is_known_action(action_id) {
                        cfg.bindings.insert(name.clone(), action_id.to_string());
                    }
                }
            }
        }
    }

    if let Some(zone) = obj.get("lockZone").and_then(Value::as_object) {
        cfg.lock_zone.enabled = zone.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let side = zone.get("side").and_then(Value::as_str).unwrap_or("right");
        cfg.lock_zone.side = match side {
            "left" | "right" | "top" | "bottom" => side.to_string(),
            _ => "right".to_string(),
        };
        let fraction = zone.get("fraction").and_then(Value::as_f64).unwrap_or(0.4);
        cfg.lock_zone.fraction = clamp(fraction, 0.05, 0.9);
    }

    cfg
}

pub fn to_json(cfg: &GestureConfig) -> Value {
    json!({
        "gestures": {
            "enabled": cfg.gestures_enabled,
            "bindings": &cfg.bindings,
        },
        "lockZone": {
            "enabled": cfg.lock_zone.enabled,
            "side": &cfg.lock_zone.side,
            "fraction": cfg.lock_zone.fraction,
        },
    })
}

/// The read-only snapshot returned by the `touch.config` RPC.
///
/// This returns the default config with gestures off and never enables
/// anything; the recognizer is disabled and takes no device or portal
/// interaction. No additional "streaming"/"path"/"lockZoneEffective" fields are
/// advertised because those imply device ownership, which this migration does
/// not perform.
pub fn config_snapshot() -> Value {
    let mut object = to_json(&GestureConfig::default());
    let mut available = Map::new();
    for (id, description) in ACTIONS {
        available.insert((*id).to_string(), Value::String((*description).to_string()));
    }
    object
        .as_object_mut()
        .expect("config snapshot is an object")
        .insert("availableActions".to_string(), Value::Object(available));
    object
}

fn clamp(value: f64, low: f64, high: f64) -> f64 {
    value.max(low).min(high)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Normalised helpers for a 2560x720 panel: it is far wider than it is
    // tall, so the same normalised distance means very different things per
    // axis.
    const PANEL_W: f64 = 2560.0;
    const PANEL_H: f64 = 720.0;
    fn nx(px: f64) -> f64 {
        px / PANEL_W
    }
    fn ny(px: f64) -> f64 {
        px / PANEL_H
    }

    // One finger, straight line, given duration.
    fn one_finger(x0: f64, y0: f64, x1: f64, y1: f64, ms: i64) -> Gesture {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(x0), ny(y0), 0);
        r.update(1, nx((x0 + x1) / 2.0), ny((y0 + y1) / 2.0), ms / 2);
        r.update(1, nx(x1), ny(y1), ms);
        r.end(1, ms)
    }

    // Two fingers moving together.
    fn two_finger(dx: f64, dy: f64, ms: i64) -> Gesture {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(800.0), ny(300.0), 0);
        r.begin(2, nx(900.0), ny(300.0), 5);
        r.update(1, nx(800.0 + dx), ny(300.0 + dy), ms);
        r.update(2, nx(900.0 + dx), ny(300.0 + dy), ms);
        r.end(1, ms);
        r.end(2, ms)
    }

    #[test]
    fn swipes_follow_the_hand() {
        assert_eq!(one_finger(1200.0, 360.0, 600.0, 360.0, 300).kind, GestureKind::SwipeLeft);
        assert_eq!(one_finger(600.0, 360.0, 1200.0, 360.0, 300).kind, GestureKind::SwipeRight);
        assert_eq!(one_finger(1200.0, 600.0, 1200.0, 150.0, 300).kind, GestureKind::SwipeUp);
        assert_eq!(one_finger(1200.0, 150.0, 1200.0, 600.0, 300).kind, GestureKind::SwipeDown);
    }

    #[test]
    fn vertical_swipe_is_possible_on_a_short_panel() {
        // If the thresholds were in normalised units this would need most of
        // the height.
        let g = one_finger(1200.0, 550.0, 1200.0, 300.0, 250);
        assert_eq!(g.kind, GestureKind::SwipeUp);
        assert!(g.distance_px > 200.0);
    }

    #[test]
    fn short_slow_or_diagonal_is_not_a_swipe() {
        // Too short to be deliberate.
        assert!(one_finger(1200.0, 360.0, 1100.0, 360.0, 200).kind != GestureKind::SwipeLeft);
        // Long enough but too slow: a drag, not a swipe.
        assert_eq!(one_finger(1200.0, 360.0, 600.0, 360.0, 2000).kind, GestureKind::None);
        // Diagonal, favouring neither axis clearly enough to guess.
        assert_eq!(one_finger(600.0, 200.0, 1100.0, 560.0, 300).kind, GestureKind::None);
    }

    #[test]
    fn taps_and_long_presses_are_distinguished() {
        assert_eq!(one_finger(1200.0, 360.0, 1205.0, 362.0, 90).kind, GestureKind::Tap);
        assert_eq!(one_finger(1200.0, 360.0, 1205.0, 362.0, 900).kind, GestureKind::LongPress);
        // Between a tap and a long press, moving barely at all, is neither.
        assert_eq!(one_finger(1200.0, 360.0, 1205.0, 362.0, 450).kind, GestureKind::None);
    }

    #[test]
    fn two_finger_swipe_is_one_gesture_with_contact_count() {
        let g = two_finger(-500.0, 0.0, 300);
        assert_eq!(g.kind, GestureKind::SwipeLeft);
        assert_eq!(g.contacts, 2);
        assert_eq!(g.name(), "two-finger-swipe-left");
    }

    #[test]
    fn three_finger_tap_names_the_count() {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(800.0), ny(300.0), 0);
        r.begin(2, nx(900.0), ny(300.0), 5);
        r.begin(3, nx(1000.0), ny(300.0), 8);
        r.end(1, 100);
        r.end(2, 100);
        let g = r.end(3, 100);
        assert_eq!(g.kind, GestureKind::Tap);
        assert_eq!(g.name(), "three-finger-tap");
    }

    #[test]
    fn gesture_is_reported_once_when_last_finger_lifts() {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(1200.0), ny(300.0), 0);
        r.begin(2, nx(1300.0), ny(300.0), 5);
        r.update(1, nx(700.0), ny(300.0), 200);
        r.update(2, nx(800.0), ny(300.0), 200);
        assert!(!r.end(1, 220).recognized()); // first lift says nothing
        assert!(r.end(2, 220).recognized()); // second produces the gesture
        assert_eq!(r.active_contacts(), 0);
    }

    #[test]
    fn pinch_out_is_not_named_as_a_swipe() {
        // Two fingers moving apart is a pinch out, even though each finger
        // travels far enough to look like a swipe on its own.
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(1100.0), ny(360.0), 0);
        r.begin(2, nx(1300.0), ny(360.0), 5);
        r.update(1, nx(900.0), ny(360.0), 300);
        r.update(2, nx(1500.0), ny(360.0), 300);
        r.end(1, 320);
        let g = r.end(2, 320);
        assert_eq!(g.kind, GestureKind::PinchOut);
        assert_eq!(g.name(), "pinch-out"); // not "two-finger-pinch-out"
    }

    #[test]
    fn pinch_in_is_detected() {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(900.0), ny(360.0), 0);
        r.begin(2, nx(1500.0), ny(360.0), 5);
        r.update(1, nx(1150.0), ny(360.0), 300);
        r.update(2, nx(1250.0), ny(360.0), 300);
        r.end(1, 320);
        assert_eq!(r.end(2, 320).kind, GestureKind::PinchIn);
    }

    #[test]
    fn fingers_moving_together_is_a_swipe_not_a_pinch() {
        // Two fingers moving together is a swipe, not a pinch: their separation
        // barely changes.
        assert_eq!(two_finger(-500.0, 0.0, 300).kind, GestureKind::SwipeLeft);
    }

    #[test]
    fn end_for_a_contact_that_never_began_is_no_gesture() {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        assert!(!r.end(99, 100).recognized());
    }

    #[test]
    fn duplicate_begin_restarts_not_accumulates() {
        // A duplicate begin (a missed end) restarts that contact rather than
        // accumulating a phantom one.
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(100.0), ny(300.0), 0);
        r.begin(1, nx(1200.0), ny(300.0), 10);
        r.update(1, nx(600.0), ny(300.0), 300);
        let g = r.end(1, 320);
        assert_eq!(g.contacts, 1);
        assert_eq!(g.kind, GestureKind::SwipeLeft);
    }

    #[test]
    fn reset_forgets_active_contacts() {
        let mut r = GestureRecognizer::new(GestureLimits::default());
        r.begin(1, nx(1200.0), ny(300.0), 0);
        r.reset();
        assert_eq!(r.active_contacts(), 0);
        assert!(!r.end(1, 100).recognized());
    }

    #[test]
    fn empty_gesture_is_none() {
        assert_eq!(Gesture::default().name(), "none");
        assert!(!Gesture::default().recognized());
    }

    // ---------------------------------------------------------------- config

    #[test]
    fn lock_zone_disabled_is_always_out() {
        let zone = LockZone { enabled: false, side: "right".to_string(), fraction: 0.9 };
        assert!(!in_lock_zone(&zone, 0.99, 0.5));
    }

    #[test]
    fn right_hand_band_geometry() {
        let zone = LockZone { enabled: true, side: "right".to_string(), fraction: 0.4 };
        assert!(in_lock_zone(&zone, 0.95, 0.5));
        assert!(in_lock_zone(&zone, 0.60, 0.5)); // exactly on the boundary
        assert!(!in_lock_zone(&zone, 0.59, 0.5));
        assert!(!in_lock_zone(&zone, 0.05, 0.5));
        // The band spans the full height.
        assert!(in_lock_zone(&zone, 0.95, 0.01));
        assert!(in_lock_zone(&zone, 0.95, 0.99));
    }

    #[test]
    fn left_hand_band_geometry() {
        let zone = LockZone { enabled: true, side: "left".to_string(), fraction: 0.25 };
        assert!(in_lock_zone(&zone, 0.10, 0.5));
        assert!(in_lock_zone(&zone, 0.25, 0.5));
        assert!(!in_lock_zone(&zone, 0.26, 0.5));
    }

    #[test]
    fn bottom_and_top_bands() {
        let bottom = LockZone { enabled: true, side: "bottom".to_string(), fraction: 0.3 };
        assert!(in_lock_zone(&bottom, 0.5, 0.85));
        assert!(!in_lock_zone(&bottom, 0.5, 0.5));
        let top = LockZone { enabled: true, side: "top".to_string(), fraction: 0.3 };
        assert!(in_lock_zone(&top, 0.5, 0.15));
        assert!(!in_lock_zone(&top, 0.5, 0.5));
    }

    #[test]
    fn nonsense_side_reserves_nothing() {
        let zone = LockZone { enabled: true, side: "diagonal".to_string(), fraction: 0.5 };
        assert!(!in_lock_zone(&zone, 0.9, 0.9));
    }

    #[test]
    fn fraction_is_clamped_so_a_corrupt_config_cannot_reserve_everything() {
        let mut zone = LockZone { enabled: true, side: "right".to_string(), fraction: 5.0 };
        assert!(!in_lock_zone(&zone, 0.05, 0.5)); // clamped to 0.9, so the left edge is free
        zone.fraction = -1.0;
        assert!(!in_lock_zone(&zone, 0.5, 0.5)); // clamped to 0.05
        assert!(in_lock_zone(&zone, 0.99, 0.5));
    }

    #[test]
    fn config_defaults_are_off() {
        let cfg = from_json(&json!({}));
        assert!(!cfg.gestures_enabled);
        assert!(!cfg.lock_zone.enabled);
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn unknown_action_is_dropped_on_load() {
        let cfg = from_json(&json!({
            "gestures": {
                "enabled": true,
                "bindings": {
                    "swipe-left": "profile-next",
                    "swipe-up": "nonsense",
                },
            },
        }));
        assert!(cfg.gestures_enabled);
        assert_eq!(cfg.bindings.get("swipe-left").map(String::as_str), Some("profile-next"));
        assert!(!cfg.bindings.contains_key("swipe-up"));
    }

    #[test]
    fn config_round_trips() {
        let mut cfg = GestureConfig::default();
        cfg.gestures_enabled = true;
        cfg.bindings.insert("two-finger-tap".to_string(), "blank-toggle".to_string());
        cfg.lock_zone = LockZone { enabled: true, side: "left".to_string(), fraction: 0.22 };
        let back = from_json(&to_json(&cfg));
        assert!(back.gestures_enabled);
        assert_eq!(back.bindings.get("two-finger-tap").map(String::as_str), Some("blank-toggle"));
        assert!(back.lock_zone.enabled);
        assert_eq!(back.lock_zone.side, "left");
        assert!(back.lock_zone.fraction > 0.21 && back.lock_zone.fraction < 0.23);
    }

    #[test]
    fn known_action_lookup() {
        assert!(is_known_action("profile-next"));
        assert!(is_known_action("none"));
        assert!(!is_known_action("launch-nukes"));
    }

    #[test]
    fn snapshot_is_disabled_and_lists_actions() {
        // The RPC snapshot must never enable anything.
        let snap = config_snapshot();
        assert_eq!(snap["gestures"]["enabled"], false);
        assert_eq!(snap["availableActions"]["profile-next"], "next profile");
        assert!(snap["availableActions"].is_object());
    }

    #[test]
    fn flag_is_off_by_default() {
        assert!(!GESTURES_ENABLED);
    }
}
