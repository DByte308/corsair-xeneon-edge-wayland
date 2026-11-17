// SPDX-License-Identifier: GPL-3.0-or-later
//! A0: real HID device driver for the Edge Bragi channel (1b1c:1d0d).
//!
//! This builds on the pure codec in [`crate::hardware`] and adds the actual
//! I/O: discovering the hidraw node through sysfs, opening it as the current
//! user (no root), exchanging a 64-byte frame with a timeout, and a write-gate
//! so a mutation is only sent when a caller supplies an explicit confirmation
//! reason. Every exchange is appended to the user's hid log, mirroring the
//! C++ WriteGate. Device state is cached so the UI's `screen.state` returns
//! the last settled value without re-buying the device.
//!
//! No write is ever performed without a `Confirmation`; reads (the fixed,
//! state-free query) are always approved by construction.
use crate::hardware::{self, Frame, ScreenParam, ScreenParams};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Report exchange timeout, matching and exceeding the C++ `kScreenReplyMs`
/// (1000 ms). The panel answers the parameter query in about a second, so this
/// margin avoids a spurious timeout while staying bounded.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// A required, human-readable reason for performing a mutation.
///
/// Writes to the panel are gated: `exchange_write` refuses to send a frame
/// when `reason` is empty, so an accidental or tooling-driven mutation cannot
/// reach the hardware without an explicit approval on the write path.
#[derive(Clone, Debug)]
pub struct Confirmation {
    reason: String,
}

impl Confirmation {
    pub fn approve(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// A settled HID exchange outcome, mirroring the C++ `Exchange`.
#[allow(dead_code)] // `tx` is kept for log/DIAG parity even before a caller reads it.
#[derive(Clone, Debug, Default)]
pub struct Exchange {
    pub tx: Vec<u8>,
    pub rx: Vec<u8>,
    pub ok: bool,
    pub error: String,
}

fn hid_log_path() -> PathBuf {
    let mut dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".local/share/edgeline");
    if std::fs::create_dir_all(&dir).is_err() {
        dir = std::env::temp_dir();
    }
    dir.join("hid.log")
}

fn append_log(line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(hid_log_path())
    {
        let _ = writeln!(f, "{}  {}", timestamp(), line);
    }
}

// A compact UTC timestamp for log lines; the exact format is not contractual.
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}s", d.as_secs())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X} "))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Discover the Edge Bragi hidraw node under `base` (default `/sys/class/hidraw`).
///
/// The node matches on `0003:VENDOR:PRODUCT` in the symlink target. The panel
/// exposes a single vendor-page interface, so an exact usage-page match would
/// require parsing the report descriptor; product/vendor identity is the
/// pragmatic, verified signal the C++ agent also uses.
pub fn find_device(base: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(base).context("cannot list hidraw devices")?;
    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(hidnum) = name.strip_prefix("hidraw") else {
            continue;
        };
        if hidnum.parse::<u32>().is_err() {
            continue;
        }
        if let Ok(target) = fs::read_link(entry.path()) {
            let target = target.to_string_lossy().to_lowercase();
            let needle = format!(
                "0003:{:04x}:{:04x}",
                hardware::VENDOR_ID, hardware::PRODUCT_ID
            );
            if target.contains(&needle) {
                nodes.push(entry.path().join("device"));
            }
        }
    }
    // Sorted, not `read_dir` order: the hotplug watcher compares the resolved
    // path, so an unordered pick would look like a device change (and trigger a
    // full screen re-read) on every probe if two collections ever matched.
    nodes.sort();
    // Prefer the /dev node when present; otherwise infer from the device path.
    let node = nodes.first().cloned().context("no Edge Bragi hidraw device")?;
    infer_dev_node(&node)
}

fn infer_dev_node(device_dir: &Path) -> Result<PathBuf> {
    // device_dir is .../usb.../3-2:1.0/0003:1B1C:1D0D.0002/hidraw/hidrawN/device
    // The hidraw node is /dev/hidrawN. Read the basename one level up.
    let hidraw_dir = device_dir.parent().context("bad hidraw device dir")?;
    let node = hidraw_dir
        .file_name()
        .context("missing hidraw index")?
        .to_string_lossy()
        .to_string();
    Ok(PathBuf::from("/dev").join(node))
}

/// Enumerate the hidraw node by scanning `/sys/class/hidraw` and returning the
/// resolved `/dev/hidrawN` path. Kept separate for error messages.
pub fn find_edge_node() -> Result<PathBuf> {
    find_device(Path::new("/sys/class/hidraw"))
}

/// The Edge's presence and identity, read without keeping a report channel open.
///
/// The C++ agent's `EdgeDevice::pollNow()` calls `HidEnumerator::findEdge()`
/// every 2 s and emits `stateChanged` when the result differs; this is the part
/// of that state a poll can see. `accessible` means the node exists *and* the
/// current user may open it, which is what separates "plugged in but no udev
/// rule" from "working" — the distinction the Settings pane shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presence {
    pub present: bool,
    pub accessible: bool,
    pub path: PathBuf,
    pub product: String,
    pub serial: String,
    /// The sysfs device instance (`0003:1B1C:1D0D.0002`). Its trailing counter
    /// increments on every re-enumeration even when `/dev/hidrawN` is reused, so
    /// this is what makes a fast unplug/replug visible to the comparator.
    pub instance: String,
}

impl Presence {
    /// Nothing attached. Every field stays empty rather than naming a node that
    /// is not there: the C++ `EdgeDevice::State` defaults are empty too, and the
    /// UI renders an empty path as "—".
    pub fn absent() -> Self {
        Self::default()
    }

    /// As `state.device`, so a hotplug event and `device.state` cannot disagree.
    pub fn device_info(&self) -> crate::state::DeviceInfo {
        crate::state::DeviceInfo {
            present: self.present,
            accessible: self.accessible,
            path: self.path.clone(),
            product: self.product.clone(),
            serial: self.serial.clone(),
        }
    }
}

/// Probe the real device.
pub fn probe() -> Presence {
    probe_in(Path::new("/sys/class/hidraw"))
}

/// [`probe`] against an explicit sysfs class directory. Cheap by design: one
/// `readdir`, one `readlink`, one small read and a permission test — no report
/// channel is opened, so polling it cannot disturb a panel mid-exchange.
pub fn probe_in(base: &Path) -> Presence {
    let Ok(path) = find_device(base) else {
        return Presence::absent();
    };
    let (product, serial) = sysfs_identity_in(base, &path);
    // Opened the way `Hid::open` opens it, so `accessible` means "usable": a udev
    // rule granting read without write would otherwise report a panel the app
    // cannot drive. Nothing is sent, so this cannot disturb an exchange.
    let accessible = OpenOptions::new().read(true).write(true).open(&path).is_ok();
    let instance = device_instance(base, &path);
    Presence {
        present: true,
        accessible,
        path,
        product,
        serial,
        instance,
    }
}

/// The sysfs device instance behind a hidraw node, e.g. `0003:1B1C:1D0D.0002`.
///
/// Degrades deliberately: if the class entry is not a symlink (bind mount,
/// container) or vanishes between the scan and this read, the instance is empty
/// and the comparator falls back to path/product/serial — the pre-`instance`
/// behaviour, still backed by the `dead`-flag recovery. The match is
/// case-sensitive on purpose; do not reuse this string for VID/PID matching,
/// which lowercases (see `find_device`).
fn device_instance(base: &Path, path: &Path) -> String {
    let node = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let Ok(target) = fs::read_link(base.join(node)) else {
        return String::new();
    };
    let target_text = target.display().to_string();
    target
        .components()
        .rev()
        .filter_map(|part| part.as_os_str().to_str())
        .find(|name| name.starts_with("0003:"))
        .map(str::to_string)
        // Fall back to the whole target: any re-enumeration still differs.
        .unwrap_or(target_text)
}

/// Read the Edge's product/serial identity from the hidraw sysfs uevent, so
/// `device.state` can report them like the C++ agent's HidEnumerator. Falls back
/// to the known product string and an empty serial if the sysfs node is missing.
fn sysfs_identity(path: &Path) -> (String, String) {
    sysfs_identity_in(Path::new("/sys/class/hidraw"), path)
}

/// [`sysfs_identity`] against an explicit sysfs class directory, so the fixture
/// tree in the tests can exercise it.
fn sysfs_identity_in(base: &Path, path: &Path) -> (String, String) {
    let node = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let uevent = base
        .join(node)
        .join("device/uevent");
    let Ok(text) = fs::read_to_string(&uevent) else {
        return ("CORSAIR XENEON EDGE".into(), String::new());
    };
    let mut product = "CORSAIR XENEON EDGE".to_string();
    let mut serial = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("HID_NAME=") {
            if !v.is_empty() {
                product = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("HID_UNIQ=") {
            if !v.is_empty() {
                serial = v.to_string();
            }
        }
    }
    (product, serial)
}

/// Why a reply did not arrive. The distinction decides cache invalidation: a
/// panel that is merely slow must not be treated as a device that left, or a
/// busy moment would throw away a working channel.
enum ReplyError {
    /// No answer within [`REPLY_TIMEOUT`]; the panel is there but silent.
    TimedOut,
    /// The channel is gone (closed or a hard I/O error, e.g. ENODEV on unplug).
    Gone(String),
}

/// A fully-open HID channel to the Edge Bragi interface.
pub struct Hid {
    file: File,
    path: PathBuf,
}

impl Hid {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("cannot open hidraw node {}", path.display()))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Write a 64-byte report, then poll for a reply up to `REPLY_TIMEOUT`.
    ///
    /// Low-level: does not enforce the write-gate. Callers should use
    /// [`Device::exchange`] so reads and gated writes are handled uniformly.
    fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        let bytes = frame.bytes();
        let n = self
            .file
            .write(bytes)
            .with_context(|| format!("write failed to {}", self.path.display()))?;
        if n != bytes.len() {
            bail!("short HID write: {n} of {}", bytes.len());
        }
        Ok(())
    }

    /// Poll for a reply for at most `timeout`, classifying the outcome so the
    /// caller can tell a slow panel from a departed one.
    fn read_reply(&mut self, timeout: Duration) -> std::result::Result<Vec<u8>, ReplyError> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; hardware::REPORT_SIZE];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => return Err(ReplyError::Gone("HID channel closed".into())),
                Ok(n) => return Ok(buf[..n].to_vec()),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        if Instant::now() >= deadline {
                            return Err(ReplyError::TimedOut);
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(ReplyError::Gone(e.to_string()));
                }
            }
        }
    }
}

/// The cached, accessible HID device. Owns the open channel and remembers the
/// last settled screen parameters so `state` is cheap and truthful.
pub struct Device {
    hid: Mutex<Hid>,
    path: PathBuf,
    product: String,
    serial: String,
    screen: Mutex<Option<ScreenParams>>,
    error: Mutex<Option<String>>,
    /// Set when an exchange failed in a way that means the node is gone. The
    /// owner of this channel clears it on the next use, so a re-plug that the
    /// hotplug watcher could not see (same hidraw number, same identity) still
    /// recovers instead of failing every request until a restart.
    dead: AtomicBool,
}

impl Device {
    pub fn open() -> Result<Device> {
        let path = find_edge_node()?;
        let hid = Hid::open(&path)?;
        let (product, serial) = sysfs_identity(&path);
        Ok(Device {
            hid: Mutex::new(hid),
            path,
            product,
            serial,
            screen: Mutex::new(None),
            error: Mutex::new(None),
            dead: AtomicBool::new(false),
        })
    }

    /// Whether a previous exchange found the channel gone.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// The Bragi hidraw path (for the Dashboard/Settings device pane).
    pub fn device_info(&self) -> crate::state::DeviceInfo {
        crate::state::DeviceInfo {
            present: true,
            accessible: true,
            path: self.path.clone(),
            product: self.product.clone(),
            serial: self.serial.clone(),
        }
    }

    /// Perform a gated exchange against the panel.
    ///
    /// - A read query is approved by construction.
    /// - A write requires a non-empty [`Confirmation::reason`]; otherwise it is
    ///   refused before touching the device and logged as such.
    /// - The exchange is always appended to the hid log.
    pub fn exchange(&self, frame: &Frame, confirmation: Option<&Confirmation>) -> Exchange {
        self.exchange_within(frame, confirmation, REPLY_TIMEOUT)
    }

    /// [`exchange`] with an explicit reply deadline, so a test can drive the
    /// "silent panel" path through the real classification without spending the
    /// configured 1.5 s.
    fn exchange_within(
        &self,
        frame: &Frame,
        confirmation: Option<&Confirmation>,
        timeout: Duration,
    ) -> Exchange {
        let tx = frame.bytes().to_vec();
        let reason = confirmation
            .map(Confirmation::reason)
            .unwrap_or_default()
            .to_string();
        if reason.is_empty() && frame.is_write() {
            let error = "refused: no confirmation for write";
            append_log(&format!("REFUSED (no confirmation) TX={}", hex(&tx)));
            return Exchange {
                tx,
                ok: false,
                error: error.to_string(),
                ..Default::default()
            };
        }
        append_log(&format!("TX [{}]  {}", reason, hex(&tx)));
        let mut hid = match self.hid.lock() {
            Ok(g) => g,
            Err(_) => {
                return Exchange {
                    tx,
                    ok: false,
                    error: "device lock poisoned".into(),
                    ..Default::default()
                }
            }
        };
        if let Err(e) = hid.write_frame(frame) {
            let msg = format!("write failed: {e}");
            append_log(&format!("TX-FAIL {msg}"));
            // A write error on a hidraw node means the device is gone (ENODEV).
            // WouldBlock is the exception: on an O_NONBLOCK fd it means the
            // panel's output queue is momentarily full, which is not a reason to
            // discard a working channel.
            if e.downcast_ref::<std::io::Error>()
                .map(|io| io.kind() != std::io::ErrorKind::WouldBlock)
                .unwrap_or(true)
            {
                self.mark_dead();
            }
            return Exchange {
                tx,
                error: msg,
                ..Default::default()
            };
        }
        let rx = match hid.read_reply(timeout) {
            Ok(r) => r,
            Err(ReplyError::TimedOut) => {
                // A silent panel is not a departed one: keep the channel.
                let msg = "RX (none: HID reply timed out)";
                append_log(&msg);
                return Exchange {
                    tx,
                    error: "HID reply timed out".into(),
                    ..Default::default()
                };
            }
            Err(ReplyError::Gone(reason)) => {
                let msg = format!("RX (none: {reason})");
                append_log(&msg);
                self.mark_dead();
                return Exchange {
                    tx,
                    error: reason,
                    ..Default::default()
                };
            }
        };
        let ok = !rx.is_empty();
        if ok {
            append_log(&format!("RX        {}", hex(&rx)));
        }
        Exchange {
            tx,
            rx,
            ok,
            ..Default::default()
        }
    }

    /// Read current screen parameters (backlight/contrast/brightness + RGB gain)
    /// and cache them. Returns the cached value on success.
    fn mark_dead(&self) {
        self.dead.store(true, Ordering::Relaxed);
    }

    pub fn read_screen_params(&self) -> Result<ScreenParams> {
        let frame = hardware::screen_params_query();
        let ex = self.exchange(&frame, Some(&Confirmation::approve("get screen params")));
        if !ex.ok {
            let error = if ex.error.is_empty() {
                "the panel did not answer the parameter query".into()
            } else {
                ex.error.clone()
            };
            *self.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.clone());
            bail!("{error}");
        }
        let params = hardware::parse_screen_params(&ex.rx)
            .context("panel answered with an unexpected parameter frame")?;
        *self.screen.lock().unwrap_or_else(|p| p.into_inner()) = Some(params);
        *self.error.lock().unwrap_or_else(|p| p.into_inner()) = None;
        Ok(params)
    }

    /// Cached screen params (no device I/O), if a read has succeeded once.
    #[allow(dead_code)] // surfaced by A1 screen.state
    pub fn screen_params(&self) -> Option<ScreenParams> {
        *self.screen.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[allow(dead_code)] // surfaced by A1 screen.state
    pub fn screen_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Write the panel's brightness (HID-only field). Value must be 0..100.
    /// Requires an explicit confirmation on the write path.
    #[allow(dead_code)] // surfaced by A1 screen.set
    pub fn set_brightness(&self, value: u8, confirmation: &Confirmation) -> Result<()> {
        if value > 100 {
            bail!("'value' must be 0..100");
        }
        let frame = hardware::set_screen_param(ScreenParam::Brightness, value);
        let ex = self.exchange(&frame, Some(confirmation));
        if !ex.ok {
            bail!("{}", if ex.error.is_empty() { "the panel did not acknowledge the change" } else { &ex.error });
        }
        // Settle the cache from the panel, not the requested value.
        let _ = self.read_screen_params();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_gates_writes() {
        let d = Device {
            hid: Mutex::new(Hid {
                // Deliberately invalid so it is never reached: the gate must
                // refuse the write *before* any device I/O.
                file: File::open("/dev/null").unwrap(),
                path: PathBuf::from("/dev/null"),
            }),
            path: PathBuf::from("/dev/null"),
            product: "test".to_string(),
            serial: "test".to_string(),
            screen: Mutex::new(None),
            error: Mutex::new(None),
            dead: AtomicBool::new(false),
        };
        let frame = hardware::set_screen_param(ScreenParam::Brightness, 40);
        let ex = d.exchange(&frame, None);
        assert!(!ex.ok, "write without confirmation must be refused");
        assert!(ex.error.contains("no confirmation"));
    }

    #[test]
    fn read_query_is_not_gated() {
        let d = Device {
            hid: Mutex::new(Hid {
                file: File::open("/dev/null").unwrap(),
                path: PathBuf::from("/dev/null"),
            }),
            path: PathBuf::from("/dev/null"),
            product: "test".to_string(),
            serial: "test".to_string(),
            screen: Mutex::new(None),
            error: Mutex::new(None),
            dead: AtomicBool::new(false),
        };
        // A read frame carries `is_write == false`, so it is not refused even
        // though no confirmation is supplied. It will fail in the device read,
        // not at the gate, which is the intended, explicit behavior.
        let frame = hardware::screen_params_query();
        let ex = d.exchange(&frame, None);
        assert!(!ex.error.contains("no confirmation"), "reads are not write-gated");
    }

    #[test]
    fn frame_write_flag_reflects_cmd() {
        // is_write is derived from the command byte at offset 1: read GET is
        // not gated, write SET is gated.
        assert!(hardware::set_screen_param(ScreenParam::Brightness, 50).is_write());
        assert!(!hardware::screen_params_query().is_write());
    }

    /// Build the sysfs shape `find_device` matches: a `hidrawN` entry whose
    /// symlink target names the Edge's vendor/product, with a `uevent` beside it
    /// carrying the identity. Returns the class directory.
    fn fixture_class_dir(name: &str) -> PathBuf {
        let root = fixture_root(name);
        let class = root.join("class");
        let device = root.join("device/hidraw");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::create_dir_all(&class).unwrap();
        // 0003:1B1C:1D0D is the Edge Bragi interface the C++ enumerator matches.
        let target = root.join("0003:1B1C:1D0D.0002/hidraw/hidraw7");
        std::fs::create_dir_all(target.join("device")).unwrap();
        std::fs::write(
            target.join("device/uevent"),
            "HID_NAME=CORSAIR XENEON EDGE\nHID_UNIQ=SYNTH0000000001\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, class.join("hidraw7")).unwrap();
        class
    }

    fn fixture_root(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "edgeline-hid-{name}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A hotplug is detected by comparing probes, so the probe has to be cheap,
    /// side-effect free and complete: presence, identity and access, and nothing
    /// invented when the device is gone.
    #[test]
    fn probe_reports_presence_identity_and_absence() {
        let class = fixture_class_dir("present");
        let presence = probe_in(&class);
        assert!(presence.present, "the fixture node must be found");
        assert_eq!(presence.product, "CORSAIR XENEON EDGE");
        assert_eq!(presence.serial, "SYNTH0000000001");
        assert!(presence.path.starts_with("/dev/"), "got {:?}", presence.path);
        // The instance counter is what makes a fast replug on the same hidraw
        // number visible to the watcher; without it the comparator sees nothing.
        assert_eq!(presence.instance, "0003:1B1C:1D0D.0002");
        // /dev/hidraw7 does not exist for a fixture tree, so it cannot be opened.
        assert!(!presence.accessible, "a fixture node is not an open device");
        let _ = std::fs::remove_dir_all(class.parent().unwrap());

        // An empty class directory is the unplugged case: every field empty, no
        // fabricated node name.
        let empty = fixture_root("absent").join("class");
        std::fs::create_dir_all(&empty).unwrap();
        let gone = probe_in(&empty);
        assert_eq!(gone, Presence::absent());
        assert_eq!(gone.device_info().path, PathBuf::new());
        let _ = std::fs::remove_dir_all(empty.parent().unwrap());
    }

    /// A channel that failed hard must report itself dead so the owner clears it.
    #[test]
    fn a_gone_channel_is_marked_dead() {
        // /dev/null accepts the write and reads as EOF: the channel is gone.
        let gone = device_on(File::open("/dev/null").unwrap(), Path::new("/dev/null"));
        assert!(!gone.is_dead());
        let ex = gone.exchange(&hardware::screen_params_query(), None);
        assert!(!ex.ok);
        assert!(
            gone.is_dead(),
            "EOF on the channel means the device left: {}",
            ex.error
        );
    }

    /// A panel that is merely slow must not be treated as a departed one, or a
    /// busy moment would discard a working channel. Driven through `exchange`,
    /// so this exercises the real policy (write, classify, mark) rather than the
    /// classifier alone: a socketpair whose peer never reads accepts the frame
    /// and then reads as WouldBlock until the deadline, exactly like a panel that
    /// has not answered yet.
    #[test]
    fn a_silent_channel_times_out_without_marking_the_device_dead() {
        let (ours, _peer) = socket_pair();
        let device = device_on(ours, Path::new("socketpair"));
        let ex = device.exchange_within(
            &hardware::screen_params_query(),
            None,
            Duration::from_millis(30),
        );
        assert!(!ex.ok, "a silent peer cannot produce a reply");
        assert!(
            ex.error.contains("timed out"),
            "the timeout must be reported as one, got {:?}",
            ex.error
        );
        // Real now: `exchange` is what sets the flag, and it must not set it here.
        assert!(!device.is_dead(), "a timeout must keep the channel");
    }

    /// A non-blocking socket and its still-open peer. The peer is returned so the
    /// caller keeps it alive: dropping it would close our end too.
    fn socket_pair() -> (File, File) {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair failed");
        // O_NONBLOCK on our end, as `Hid::open` sets on the real channel.
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        unsafe {
            use std::os::fd::FromRawFd;
            (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1]))
        }
    }

    fn device_on(file: File, path: &std::path::Path) -> Device {
        Device {
            hid: Mutex::new(Hid {
                file,
                path: path.to_path_buf(),
            }),
            path: path.to_path_buf(),
            product: "test".into(),
            serial: "test".into(),
            screen: Mutex::new(None),
            error: Mutex::new(None),
            dead: AtomicBool::new(false),
        }
    }

    /// The comparison the watcher is built on: identity or access changing is a
    /// change, a repeated probe is not (otherwise it would broadcast every 2 s).
    #[test]
    fn presence_comparison_ignores_repeated_probes() {
        let a = Presence {
            present: true,
            accessible: true,
            path: PathBuf::from("/dev/hidraw7"),
            product: "CORSAIR XENEON EDGE".into(),
            serial: "SYNTH0000000001".into(),
            instance: "0003:1B1C:1D0D.0002".into(),
        };
        assert_eq!(a.clone(), a, "an unchanged device is not an event");
        let mut inaccessible = a.clone();
        inaccessible.accessible = false;
        assert_ne!(a, inaccessible, "a udev rule appearing is an event");
        let mut replugged = a.clone();
        replugged.path = PathBuf::from("/dev/hidraw9");
        assert_ne!(a, replugged, "the same panel on a new node is an event");
        let mut reenumerated = a.clone();
        reenumerated.instance = "0003:1B1C:1D0D.0003".into();
        assert_ne!(
            a, reenumerated,
            "a replug that reuses /dev/hidraw7 still gets a new instance"
        );
        assert_ne!(a, Presence::absent(), "unplugging is an event");
    }
}
