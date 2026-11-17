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
    sync::Mutex,
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

    fn read_reply(&mut self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut buf = [0u8; hardware::REPORT_SIZE];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => bail!("HID channel closed"),
                Ok(n) => return Ok(buf[..n].to_vec()),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        if Instant::now() >= deadline {
                            bail!("HID reply timed out");
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
    }
}

/// The cached, accessible HID device. Owns the open channel and remembers the
/// last settled screen parameters so `state` is cheap and truthful.
pub struct Device {
    hid: Mutex<Hid>,
    screen: Mutex<Option<ScreenParams>>,
    error: Mutex<Option<String>>,
}

impl Device {
    pub fn open() -> Result<Device> {
        let path = find_edge_node()?;
        let hid = Hid::open(&path)?;
        Ok(Device {
            hid: Mutex::new(hid),
            screen: Mutex::new(None),
            error: Mutex::new(None),
        })
    }

    /// Perform a gated exchange against the panel.
    ///
    /// - A read query is approved by construction.
    /// - A write requires a non-empty [`Confirmation::reason`]; otherwise it is
    ///   refused before touching the device and logged as such.
    /// - The exchange is always appended to the hid log.
    pub fn exchange(&self, frame: &Frame, confirmation: Option<&Confirmation>) -> Exchange {
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
            return Exchange {
                tx,
                error: msg,
                ..Default::default()
            };
        }
        let rx = match hid.read_reply() {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("RX (none: {e})");
                append_log(&msg);
                return Exchange {
                    tx,
                    error: e.to_string(),
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
            screen: Mutex::new(None),
            error: Mutex::new(None),
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
            screen: Mutex::new(None),
            error: Mutex::new(None),
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
}
