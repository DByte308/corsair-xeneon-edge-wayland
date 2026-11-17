// SPDX-License-Identifier: GPL-3.0-or-later
//! The panel's touch contacts, read from its evdev node.
//!
//! The C++ agent read these from XInput2, which cannot see the panel's digitizer
//! on a Wayland session — XWayland only gets what the compositor emulates. So the
//! node is read directly: multitouch protocol B on `/dev/input/eventN`
//! (`ABS_MT_SLOT` / `ABS_MT_TRACKING_ID` / `ABS_MT_POSITION_X`/`Y`, flushed on
//! `SYN_REPORT`), normalised to 0..1 using the axis ranges the device reports.
//! That is the shape the ripple overlay, the lock zone and the gesture recogniser
//! all expect.
//!
//! Strictly read-only and non-grabbing: the events stay available to the
//! compositor, so touch keeps working for everything else. Nothing here writes to
//! the device or asks for portal permissions.

use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

/// Limits for `absinfo` from `<linux/input.h>`: `struct input_absinfo` is six
/// `__s32`s, so `EVIOCGABS` is `_IOR('E', 0x40 + abs, 24)`.
const INPUT_ABSINFO_SIZE: u64 = 24;
const fn eviocgabs(abs: u8) -> libc::c_ulong {
    ((2u64 << 30) | (INPUT_ABSINFO_SIZE << 16) | ((b'E' as u64) << 8) | (0x40 + abs as u64))
        as libc::c_ulong
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// What a contact is doing this frame. Mirrors the C++ `kPhase` strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Update,
    End,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Begin => "begin",
            Phase::Update => "update",
            Phase::End => "end",
        }
    }
}

/// One touch contact in normalised panel coordinates (0..1, origin top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchPoint {
    pub id: i32,
    pub phase: Phase,
    pub nx: f64,
    pub ny: f64,
}

/// A touchscreen device and the ranges its axes report.
pub struct Reader {
    file: fs::File,
    /// Slot state must survive across reads: protocol B sends
    /// `ABS_MT_TRACKING_ID` once at touch-down and again only as -1 at lift, so a
    /// decoder rebuilt per read would see later position frames with no slot and
    /// drop them — one `begin` per contact and nothing else.
    decoder: Decoder,
}

impl Reader {
    /// Open an evdev node and read its axis ranges.
    pub fn open(path: &Path) -> Result<Reader> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("cannot open touch device {}", path.display()))?;
        let fd = file.as_raw_fd();
        let x_range = axis_range(fd, 0x35)
            .with_context(|| format!("{} does not report ABS_MT_POSITION_X", path.display()))?;
        let y_range = axis_range(fd, 0x36)
            .with_context(|| format!("{} does not report ABS_MT_POSITION_Y", path.display()))?;
        if x_range.0 >= x_range.1 || y_range.0 >= y_range.1 {
            bail!("{} reports impossible axis ranges", path.display());
        }
        Ok(Reader {
            file,
            decoder: Decoder::new(x_range, y_range),
        })
    }

    pub fn fd(&self) -> i32 {
        self.file.as_raw_fd()
    }

    /// Read whatever is available and return the contacts it completed.
    ///
    /// Returns an empty list when nothing is pending, which is the normal case on
    /// a non-blocking node. A read error other than "would block" means the device
    /// went away and is surfaced so the caller can stop.
    pub fn poll(&mut self) -> Result<Vec<TouchPoint>> {
        let mut bytes = [0u8; EVENT_SIZE * 64];
        let mut total = 0usize;
        loop {
            match self.file.read(&mut bytes[total..]) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if total + EVENT_SIZE > bytes.len() {
                        break;
                    }
                }
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error).with_context(|| "touch device read failed");
                }
            }
        }
        if total == 0 {
            return Ok(Vec::new());
        }
        Ok(self.decoder.push(&bytes[..total]))
    }
}

/// `struct input_event` on 64-bit Linux with a 16-byte `timeval`.
const EVENT_SIZE: usize = 24;

/// Decode protocol-B multitouch events into contact phases.
///
/// Kept separate from the device so the framing, the slot bookkeeping and the
/// begin/update/end rules can be exercised without any hardware.
pub struct Decoder {
    x_range: (i32, i32),
    y_range: (i32, i32),
    /// slot -> (tracking id, x, y)
    slots: HashMap<i32, (i32, i32, i32)>,
    /// Contacts ended in the current frame, reported once on SYN_REPORT.
    ended: BTreeMap<i32, (i32, i32)>,
    /// Contacts that have not reported their first frame yet: those get `begin`
    /// instead of `update`, which is what the ripple overlay keys the touch-down
    /// animation on.
    started: std::collections::BTreeSet<i32>,
    current_slot: i32,
}

impl Decoder {
    pub fn new(x_range: (i32, i32), y_range: (i32, i32)) -> Decoder {
        Decoder {
            x_range,
            y_range,
            slots: HashMap::new(),
            ended: BTreeMap::new(),
            started: std::collections::BTreeSet::new(),
            current_slot: 0,
        }
    }

    /// Feed raw bytes; the returned contacts are those a `SYN_REPORT` completed.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<TouchPoint> {
        let mut out = Vec::new();
        for chunk in bytes.chunks_exact(EVENT_SIZE) {
            // Layout: timeval(16) | type u16 | code u16 | value i32.
            let kind = u16::from_ne_bytes([chunk[16], chunk[17]]);
            let code = u16::from_ne_bytes([chunk[18], chunk[19]]);
            let value = i32::from_ne_bytes([chunk[20], chunk[21], chunk[22], chunk[23]]);
            match kind {
                // EV_SYN / SYN_REPORT closes the frame; that is when contacts are
                // reported, so it is the only place a point is produced.
                0 if code == 0 => self.flush(&mut out),
                0 => {}
                3 => self.abs(code, value),                // EV_ABS
                _ => {}
            }
        }
        out
    }

    fn abs(&mut self, code: u16, value: i32) {
        match code {
            // ABS_MT_SLOT
            0x2f => self.current_slot = value,
            // ABS_MT_TRACKING_ID: >= 0 starts a contact, -1 lifts it.
            0x39 => {
                if value < 0 {
                    if let Some((id, x, y)) = self.slots.remove(&self.current_slot) {
                        // A tap that begins and ends between two flushes never
                        // appears in a live frame, so drop its start marker here:
                        // otherwise it accumulates, and a device that re-issues a
                        // tracking id would later report `update` instead of
                        // `begin` for a brand-new contact.
                        self.started.remove(&id);
                        self.ended.insert(id, (x, y));
                    }
                } else {
                    // A new tracking id on this slot is a touch-down; the same id
                    // repeated is just a further frame of the same contact.
                    match self.slots.get_mut(&self.current_slot) {
                        Some(slot) => {
                            if slot.0 != value {
                                *slot = (value, 0, 0);
                                self.started.insert(value);
                            }
                        }
                        None => {
                            self.slots.insert(self.current_slot, (value, 0, 0));
                            self.started.insert(value);
                        }
                    }
                }
            }
            // ABS_MT_POSITION_X / _Y
            0x35 | 0x36 => {
                if let Some(slot) = self.slots.get_mut(&self.current_slot) {
                    if code == 0x35 {
                        slot.1 = value;
                    } else {
                        slot.2 = value;
                    }
                }
            }
            _ => {}
        }
    }

    fn flush(&mut self, out: &mut Vec<TouchPoint>) {
        // Lifted first: a contact that ends in this frame reports `end` even if a
        // new one reuses the slot.
        for (id, (x, y)) in std::mem::take(&mut self.ended) {
            out.push(TouchPoint {
                id,
                phase: Phase::End,
                nx: self.normalise(x, self.x_range),
                ny: self.normalise(y, self.y_range),
            });
        }
        let mut live: Vec<(i32, i32, i32)> = self.slots.values().copied().collect();
        live.sort_unstable();
        for (id, x, y) in live {
            let phase = if self.started.remove(&id) {
                Phase::Begin
            } else {
                Phase::Update
            };
            out.push(TouchPoint {
                id,
                phase,
                nx: self.normalise(x, self.x_range),
                ny: self.normalise(y, self.y_range),
            });
        }
    }

    fn normalise(&self, value: i32, (min, max): (i32, i32)) -> f64 {
        let span = (max - min).max(1) as f64;
        (((value - min) as f64) / span).clamp(0.0, 1.0)
    }

    /// Which contacts this decoder believes are down, for the caller's start-phase
    /// bookkeeping.
    #[cfg(test)]
    pub fn live_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.slots.values().map(|slot| slot.0).collect();
        ids.sort_unstable();
        ids
    }
}

fn axis_range(fd: i32, abs: u8) -> Result<(i32, i32)> {
    let mut info = AbsInfo::default();
    // SAFETY: EVIOCGABS writes one `input_absinfo` into the supplied struct, and
    // the request constant is built from that struct's size.
    let rc = unsafe { libc::ioctl(fd, eviocgabs(abs), &mut info) };
    if rc < 0 {
        bail!("ioctl failed: {}", std::io::Error::last_os_error());
    }
    Ok((info.minimum, info.maximum))
}

/// Find the Edge's touchscreen node.
///
/// Matched on the device name the panel reports, then confirmed by the presence of
/// the multitouch position axes in sysfs — a device that merely shares the name
/// would otherwise be opened and silently deliver nothing.
pub fn find_touch_node() -> Result<PathBuf> {
    find_touch_node_in(Path::new("/sys/class/input"))
}

pub fn find_touch_node_in(base: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(base).context("cannot list input devices")?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("event") {
            continue;
        }
        let device = entry.path().join("device");
        let reported = fs::read_to_string(device.join("name")).unwrap_or_default();
        if !reported.trim().eq_ignore_ascii_case("wch.cn TouchScreen") {
            continue;
        }
        let caps = fs::read_to_string(device.join("capabilities/abs")).unwrap_or_default();
        if !abs_capability_has(&caps, 0x35) || !abs_capability_has(&caps, 0x36) {
            continue;
        }
        candidates.push(PathBuf::from("/dev/input").join(&name));
    }
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .context("no multitouch device named 'wch.cn TouchScreen'")
}

/// The sysfs capability bitmap is space-separated 64-bit words, least significant
/// first: bit N of code N/64, bit N%64.
pub fn abs_capability_has(bitmap: &str, code: u16) -> bool {
    let words: Vec<u64> = bitmap
        .split_whitespace()
        .filter_map(|word| u64::from_str_radix(word, 16).ok())
        .collect();
    let word = (code / 64) as usize;
    let bit = code % 64;
    words
        .get(word)
        .map(|value| value & (1u64 << bit) != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one `input_event`. The timestamp is irrelevant to decoding.
    fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
        let mut bytes = [0u8; EVENT_SIZE];
        bytes[16..18].copy_from_slice(&kind.to_ne_bytes());
        bytes[18..20].copy_from_slice(&code.to_ne_bytes());
        bytes[20..24].copy_from_slice(&value.to_ne_bytes());
        bytes
    }

    fn frame(events: &[(u16, u16, i32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (kind, code, value) in events {
            out.extend_from_slice(&event(*kind, *code, *value));
        }
        // EV_SYN / SYN_REPORT closes the frame.
        out.extend_from_slice(&event(0, 0, 0));
        out
    }

    fn decoder() -> Decoder {
        Decoder::new((0, 1000), (0, 500))
    }

    /// A whole contact life: begin, move, lift — the three phases the ripple
    /// overlay and the gesture recogniser consume.
    #[test]
    fn a_contact_reports_begin_update_and_end() {
        let mut d = decoder();
        let started = d.push(&frame(&[(3, 0x39, 7), (3, 0x35, 250), (3, 0x36, 100)]));
        assert_eq!(started.len(), 1, "{started:?}");
        assert_eq!(started[0].phase, Phase::Begin, "the first frame is a touch-down");
        assert!((started[0].nx - 0.25).abs() < 1e-9, "{started:?}");
        assert!((started[0].ny - 0.2).abs() < 1e-9, "{started:?}");

        let moved = d.push(&frame(&[(3, 0x35, 500)]));
        assert_eq!(moved[0].phase, Phase::Update);
        assert!((moved[0].nx - 0.5).abs() < 1e-9, "{moved:?}");

        let lifted = d.push(&frame(&[(3, 0x39, -1)]));
        assert_eq!(lifted.len(), 1, "{lifted:?}");
        assert_eq!(lifted[0].phase, Phase::End);
        assert_eq!(lifted[0].id, 7);
        assert!(d.live_ids().is_empty());
    }

    /// Two fingers occupy two slots and report independently; ending one does not
    /// disturb the other.
    #[test]
    fn slots_track_contacts_independently() {
        let mut d = decoder();
        d.push(&frame(&[
            (3, 0x2f, 0), (3, 0x39, 1), (3, 0x35, 100), (3, 0x36, 50),
            (3, 0x2f, 1), (3, 0x39, 2), (3, 0x35, 900), (3, 0x36, 450),
        ]));
        assert_eq!(d.live_ids(), vec![1, 2]);

        let one_lifted = d.push(&frame(&[(3, 0x2f, 0), (3, 0x39, -1)]));
        let phases: Vec<(i32, Phase)> = one_lifted.iter().map(|p| (p.id, p.phase)).collect();
        assert!(phases.contains(&(1, Phase::End)), "{phases:?}");
        assert!(phases.contains(&(2, Phase::Update)), "{phases:?}");
        assert_eq!(d.live_ids(), vec![2]);
    }

    /// Coordinates are normalised per axis and clamped, so a device that reports
    /// outside its own range cannot produce off-panel positions.
    #[test]
    fn positions_are_normalised_and_clamped() {
        let mut d = Decoder::new((100, 1100), (0, 400));
        let out = d.push(&frame(&[(3, 0x39, 1), (3, 0x35, 100), (3, 0x36, 0)]));
        assert!(out[0].nx.abs() < 1e-9, "{out:?}");
        assert!(out[0].ny.abs() < 1e-9, "{out:?}");
        let out = d.push(&frame(&[(3, 0x35, 5000), (3, 0x36, -50)]));
        assert!((out[0].nx - 1.0).abs() < 1e-9, "{out:?}");
        assert!(out[0].ny.abs() < 1e-9, "{out:?}");
    }

    /// Partial reads are normal: evdev delivers whole events but a read can end
    /// anywhere, so the decoder must ignore an incomplete tail instead of panicking.
    #[test]
    fn a_truncated_tail_is_ignored() {
        let mut d = decoder();
        let mut bytes = frame(&[(3, 0x39, 3), (3, 0x35, 100)]);
        bytes.truncate(bytes.len() - 7);
        let out = d.push(&bytes);
        // No SYN_REPORT survived, so nothing is reported yet.
        assert!(out.is_empty(), "{out:?}");
    }

    /// Protocol B repeats the tracking id only at touch-down and at lift, so the
    /// decoder has to keep its slots between reads. This is the regression the
    /// audit caught: a decoder per read produced one `begin` and nothing after.
    #[test]
    fn later_frames_without_a_tracking_id_still_update_and_end() {
        let mut d = decoder();
        // Frame 1: down.
        let down = d.push(&frame(&[(3, 0x39, 100), (3, 0x35, 250), (3, 0x36, 100)]));
        assert_eq!(down[0].phase, Phase::Begin);
        // Frame 2: movement only — no tracking id at all.
        let moved = d.push(&frame(&[(3, 0x35, 500), (3, 0x36, 200)]));
        assert_eq!(moved.len(), 1, "the slot must still be known: {moved:?}");
        assert_eq!(moved[0].id, 100);
        assert_eq!(moved[0].phase, Phase::Update);
        // Frame 3: lift only.
        let lifted = d.push(&frame(&[(3, 0x39, -1)]));
        assert_eq!(lifted[0].phase, Phase::End);
        assert_eq!(lifted[0].id, 100);
        assert!(d.live_ids().is_empty());
    }

    /// A tap that starts and ends inside one frame must not leave a start marker
    /// behind: the id could be re-issued, and the next contact would then report
    /// `update` instead of `begin`.
    #[test]
    fn a_tap_inside_one_frame_does_not_leak_its_start_marker() {
        let mut d = decoder();
        d.push(&frame(&[(3, 0x39, 7), (3, 0x35, 100)]));
        d.push(&frame(&[(3, 0x39, -1)]));
        // A new contact reusing the same tracking id is a fresh touch-down.
        let again = d.push(&frame(&[(3, 0x39, 7), (3, 0x35, 100)]));
        assert_eq!(again[0].phase, Phase::Begin, "{again:?}");
    }

    /// Slot reuse: ending one contact and starting another on the same slot must
    /// report the lift before the new touch-down.
    #[test]
    fn a_slot_reused_in_one_frame_ends_the_old_contact_first() {
        let mut d = decoder();
        d.push(&frame(&[(3, 0x2f, 0), (3, 0x39, 11), (3, 0x35, 100)]));
        let both = d.push(&frame(&[(3, 0x39, -1), (3, 0x39, 12), (3, 0x35, 900)]));
        let phases: Vec<(i32, Phase)> = both.iter().map(|p| (p.id, p.phase)).collect();
        assert_eq!(phases[0], (11, Phase::End), "{phases:?}");
        assert_eq!(phases[1], (12, Phase::Begin), "{phases:?}");
    }

    #[test]
    fn capability_bitmap_is_read_little_first() {
        // ABS_MT_POSITION_X is 0x35 = 53, i.e. bit 53 of the first word
        // (0x20000000000000 = 1 << 53).
        assert!(abs_capability_has("20000000000000 0", 0x35));
        assert!(!abs_capability_has("0 0", 0x35));
        // 0x3f lives in the first word, 0x40 in the second.
        assert!(abs_capability_has("8000000000000000 1", 0x3f));
        assert!(!abs_capability_has("20000000000000 0", 0x39), "0x39 is ABS_MT_TRACKING_ID");
        assert!(abs_capability_has("0 1", 0x40));
        assert!(!abs_capability_has("", 0x35));
    }
}
