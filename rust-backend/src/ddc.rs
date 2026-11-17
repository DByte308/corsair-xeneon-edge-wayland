// SPDX-License-Identifier: GPL-3.0-or-later
//! A1: ddcutil-backed DDC/CI control for the Edge panel, faithful to
//! `agent/src/core/DdcClient.cpp`, `agent/src/core/DdcCapabilities.cpp` and the
//! `ddc.*`/`screen.*` RPC methods in `agent/src/agent/Api.cpp`.
//!
//! The layout mirrors the compatibility agent: a small, process-driven
//! `Ddc` client runs one ddcutil child at a time (DDC is slow and stateful),
//! queues jobs and coalesces queued `setvcp` writes for the same VCP code so a
//! slider drag does not build a backlog. The `ddcutil` binary is injectable so
//! the fixture tests can run completely offline against the fake script in
//! `tests/fixtures/fake-ddcutil/`.
//!
//! The panel's capability string drives the UI instead of a mocked list: it
//! reports seven colour presets (not the mockup's four), seven input sources
//! and sharpness that tops out at 4. RGB gain is taken in percent but reported
//! on 0..255, so it is converted only on its way out via [`to_write_scale`].
//! Nothing is written without the caller-approved confirmation when the write
//! is unsafe (input source 0x60 / power 0xD6), reusing [`crate::hid::Confirmation`].
use crate::hid::Confirmation;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, broadcast};

/// Names for the VCP codes the Edge UI actually drives.
pub mod vcp {
    pub const BRIGHTNESS: u8 = 0x10;
    pub const CONTRAST: u8 = 0x12;
    pub const PRESET: u8 = 0x14;
    pub const GAIN_RED: u8 = 0x16;
    pub const GAIN_GREEN: u8 = 0x18;
    pub const GAIN_BLUE: u8 = 0x1A;
    pub const SHARPNESS: u8 = 0x87;
    pub const POWER: u8 = 0xD6;
    pub const INPUT_SOURCE: u8 = 0x60;
}

/// Codes a set is followed by a read, because a slider or a profile shows them.
const PICTURE_CODES: [u8; 7] = [
    vcp::BRIGHTNESS,
    vcp::CONTRAST,
    vcp::PRESET,
    vcp::GAIN_RED,
    vcp::GAIN_GREEN,
    vcp::GAIN_BLUE,
    vcp::SHARPNESS,
];

/// Writes that black out or re-route the panel and must not reach the hardware
/// without an explicit caller confirmation.
const UNSAFE_WRITES: [u8; 2] = [vcp::INPUT_SOURCE, vcp::POWER];

/// Whether a write to `code` mutates the panel in a risky way. See [`UNSAFE_WRITES`].
pub fn is_unsafe_write(code: u8) -> bool {
    UNSAFE_WRITES.contains(&code)
}

/// Refuse an unsafe write unless an explicit [`Confirmation`] is present.
///
/// Mirrors the A0 `hid::Confirmation` write-gate: a mutation is only sent when
/// a caller supplies a non-empty reason. Safe picture writes are not gated.
pub fn guard_unsafe_write(code: u8, confirmation: Option<&Confirmation>) -> Result<()> {
    if is_unsafe_write(code) {
        let approved = confirmation
            .map(|c| c.reason())
            .is_some_and(|reason| !reason.is_empty());
        if !approved {
            bail!("refused: VCP 0x{code:02X} is an unsafe write and needs an explicit confirmation");
        }
    }
    Ok(())
}

/// The picture values a slider or a profile sets and the UI shows. Power, input
/// and the restore commands are commands rather than values, so a write to them
/// is not followed by a read.
fn reads_back(code: u8) -> bool {
    PICTURE_CODES.contains(&code)
}

/// The Edge takes RGB gain in percent but reports it on 0..255: 50 reads back
/// as 127, and anything from 100 up as 255. Everything above this - the cache,
/// the sliders, saved profiles - holds the value as the panel reports it, so
/// gain is converted only here, on its way out, against the maximum the panel
/// reported. Rounding makes a reported value come back unchanged when it is
/// written again, which is what restoring a profile does.
fn to_write_scale(code: u8, value: u16, max: u16) -> u16 {
    if !matches!(code, vcp::GAIN_RED | vcp::GAIN_GREEN | vcp::GAIN_BLUE) {
        return value;
    }
    let max = if max == 0 { 255 } else { max };
    (((value as u32 * 100 + max as u32 / 2) / max as u32).min(100)) as u16
}

// ---------------------------------------------------------------------------
// Capability-string parsing (pure, no process I/O).
// ---------------------------------------------------------------------------

/// A discrete value a non-continuous VCP feature can take, e.g. `0x0b: User 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct VcpValue {
    pub code: u8,
    pub label: String,
}

/// A single advertised VCP feature, e.g. `0x14 (Select color preset)`.
#[derive(Clone, Debug, PartialEq)]
pub struct VcpFeature {
    pub code: u8,
    pub name: String,
    /// Empty for continuous features.
    pub values: Vec<VcpValue>,
}

/// The parsed panel capability string. Never half-fails: unparseable input
/// yields empty features, which the UI reads as "this control is unsupported".
#[derive(Clone, Debug, Default)]
pub struct DdcCapabilities {
    pub model: String,
    pub mccs: String,
    pub features: Vec<VcpFeature>,
}

impl DdcCapabilities {
    pub fn find(&self, code: u8) -> Option<&VcpFeature> {
        self.features.iter().find(|f| f.code == code)
    }
    pub fn has(&self, code: u8) -> bool {
        self.find(code).is_some()
    }

    /// The preset that makes RGB gain writable. On this panel it is "User 1";
    /// matched by label because the code differs between panels. Mirrors
    /// `Api::isUserPreset`: gain only means anything under the user preset.
    #[allow(dead_code)] // surfaced for the higher profile layer, as in Api.cpp
    pub fn is_user_preset(&self, preset_code: u8) -> bool {
        let Some(preset) = self.find(vcp::PRESET) else {
            return false;
        };
        preset
            .values
            .iter()
            .any(|v| v.code == preset_code && v.label.to_lowercase().starts_with("user"))
    }
}

fn parse_hex_byte(token: &str) -> Option<u8> {
    if token.is_empty() || token.len() > 2 {
        return None;
    }
    u8::from_str_radix(token, 16).ok()
}

pub fn parse_capabilities(raw: &str) -> DdcCapabilities {
    let mut caps = DdcCapabilities::default();
    let mut in_values = false;
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(model) = t.strip_prefix("Model:") {
            caps.model = model.trim().to_string();
            continue;
        }
        if let Some(mccs) = t.strip_prefix("MCCS version:") {
            caps.mccs = mccs.trim().to_string();
            continue;
        }
        if t == "Values:" {
            in_values = true;
            continue;
        }
        if let Some(rest) = t.strip_prefix("Feature:") {
            in_values = false;
            let rest = rest.trim_start();
            let sp = rest.find(' ').unwrap_or(rest.len());
            let code = match parse_hex_byte(&rest[..sp]) {
                Some(code) => code,
                None => continue,
            };
            let mut feature = VcpFeature {
                code,
                name: String::new(),
                values: Vec::new(),
            };
            if let (Some(lp), Some(rp)) = (rest.find('('), rest.rfind(')')) {
                if rp > lp {
                    feature.name = rest[lp + 1..rp].trim().to_string();
                }
            }
            caps.features.push(feature);
            continue;
        }
        if in_values && !caps.features.is_empty() {
            if let Some(c) = t.find(':') {
                if let Some(value) = parse_hex_byte(t[..c].trim()) {
                    caps.features
                        .last_mut()
                        .expect("guarded by !is_empty")
                        .values
                        .push(VcpValue {
                            code: value,
                            label: t[c + 1..].trim().to_string(),
                        });
                    continue;
                }
            }
            continue;
        }
        if t.starts_with("Op Code:") || t == "Commands:" || t == "VCP Features:" {
            in_values = false;
        }
    }
    caps
}

// ---------------------------------------------------------------------------
// ddcutil job state machine.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum JobKind {
    Detect,
    Get,
    Set,
    Capabilities,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Job {
    kind: JobKind,
    code: u8,
    /// For a set, the value on the panel's own scale (before `to_write_scale`).
    value: u16,
}

impl Job {
    fn new(kind: JobKind, code: u8, value: u16) -> Self {
        Self { kind, code, value }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct VcpState {
    /// -1 when the panel has not reported a value yet.
    current: i32,
    max: u16,
}

struct Core {
    ddcutil: PathBuf,
    /// Injected for offline tests; set as `FAKE_DDCUTIL_STATE` on the child.
    fake_state: Option<PathBuf>,
    queue: VecDeque<Job>,
    bus: i32,
    max: HashMap<u8, u16>,
    vcp: HashMap<u8, VcpState>,
    caps: DdcCapabilities,
    ready: bool,
    message: String,
    log: VecDeque<String>,
    /// Log lines this job produced, drained by `process_job` to emit the
    /// `ddcLog` broadcast. Kept apart from `log` because that one is capped and
    /// drops from the front, so a length check could not identify new lines.
    pending_log: Vec<String>,
    seed_standard: bool,
}

impl Core {
    /// Append a ddcutil log line, and queue it for the `ddcLog` broadcast.
    fn log_line(&mut self, line: String) {
        self.log.push_back(line.clone());
        self.pending_log.push(line);
    }
}

/// Configuration for [`Ddc::new`].
pub struct DdcConfig {
    /// The ddcutil binary to run. `None` uses the `ddcutil` on `PATH`.
    pub ddcutil: Option<PathBuf>,
    /// After the bus is found, fetch capabilities and seed the standard reads
    /// so a UI connecting later has real values instead of placeholders.
    pub seed_standard: bool,
}

impl Default for DdcConfig {
    fn default() -> Self {
        Self {
            ddcutil: None,
            seed_standard: true,
        }
    }
}

/// The async DDC/CI client. Shareable across the RPC dispatch and a background
/// worker task; the ddcutil child is run one at a time.
pub struct Ddc {
    core: Arc<Mutex<Core>>,
    notify: Arc<Notify>,
    /// Where finished jobs are announced, so the RPC layer can fan them out as
    /// the C++ agent's `ddc`/`ddcLog` events. Set once by `main`; a `Ddc` used
    /// without a sink (every unit test) simply stays silent.
    events: Arc<Mutex<Option<broadcast::Sender<Value>>>>,
}

impl Ddc {
    pub fn new(config: DdcConfig) -> Self {
        Self {
            core: Arc::new(Mutex::new(Core {
                ddcutil: config
                    .ddcutil
                    .unwrap_or_else(|| PathBuf::from("ddcutil")),
                fake_state: None,
                queue: VecDeque::new(),
                bus: -1,
                max: HashMap::new(),
                vcp: HashMap::new(),
                caps: DdcCapabilities::default(),
                ready: false,
                message: String::new(),
                log: VecDeque::new(),
                pending_log: Vec::new(),
                seed_standard: config.seed_standard,
            })),
            notify: Arc::new(Notify::new()),
            events: Arc::new(Mutex::new(None)),
        }
    }

    /// Announce completed jobs on this channel as `ddc`/`ddcLog` events.
    pub fn set_event_sink(&self, sink: broadcast::Sender<Value>) {
        *self.events.lock().unwrap_or_else(|p| p.into_inner()) = Some(sink);
    }

    /// Inject a fake-ddcutil state path for offline tests.
    #[cfg(test)]
    fn set_fake_state(&self, path: PathBuf) {
        self.core.lock().unwrap().fake_state = Some(path);
    }

    /// Kick off bus discovery.
    pub fn start(&self) {
        self.enqueue(Job::new(JobKind::Detect, 0, 0));
    }

    pub fn get_vcp(&self, code: u8) {
        self.enqueue(Job::new(JobKind::Get, code, 0));
    }

    /// Coalesce: replace a queued (not yet running) set for the same code so a
    /// slider drag does not build a backlog. A running set is not in the queue,
    /// so a second write still goes out.
    pub fn set_vcp(&self, code: u8, value: u16) {
        let mut core = self.core.lock().unwrap();
        if let Some(job) = core
            .queue
            .iter_mut()
            .find(|job| job.kind == JobKind::Set && job.code == code)
        {
            job.value = value;
            return;
        }
        core.queue.push_back(Job::new(JobKind::Set, code, value));
        drop(core);
        self.notify.notify_one();
    }

    #[allow(dead_code)] // public API mirroring DdcClient::fetchCapabilities
    pub fn fetch_capabilities(&self) {
        self.enqueue(Job::new(JobKind::Capabilities, 0, 0));
    }

    fn enqueue(&self, job: Job) {
        {
            let mut core = self.core.lock().unwrap();
            core.queue.push_back(job);
        }
        self.notify.notify_one();
    }

    pub fn ready(&self) -> bool {
        self.core.lock().unwrap().ready
    }

    /// The /dev/i2c-N bus the Edge was found on, or -1 if not known.
    pub fn i2c_bus(&self) -> i64 {
        self.core.lock().unwrap().bus as i64
    }

    /// Best-effort ddcutil version string (first line of `ddcutil --version`).
    /// Empty if ddcutil is missing or fails; the UI shows it only as info.
    pub fn tool_version(&self) -> String {
        let core = self.core.lock().unwrap();
        let ddc = core.ddcutil.clone();
        drop(core);
        if let Ok(out) = std::process::Command::new(&ddc).arg("--version").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(first) = s.lines().next() {
                return first.trim().to_string();
            }
        }
        String::new()
    }

    pub fn caps_empty(&self) -> bool {
        self.core.lock().unwrap().caps.features.is_empty()
    }

    pub fn has(&self, code: u8) -> bool {
        self.core.lock().unwrap().caps.has(code)
    }

    /// The `ddc.state` RPC payload.
    pub fn snapshot(&self) -> Value {
        let core = self.core.lock().unwrap();
        let mut values = serde_json::Map::new();
        for (code, state) in &core.vcp {
            values.insert(
                format!("{code:02x}"),
                json!({ "value": state.current, "max": state.max }),
            );
        }
        let features: Vec<Value> = core
            .caps
            .features
            .iter()
            .map(|f| {
                let values: Vec<Value> = f
                    .values
                    .iter()
                    .map(|v| json!({ "code": v.code, "label": v.label }))
                    .collect();
                json!({
                    "code": f.code,
                    "name": f.name,
                    "continuous": f.values.is_empty(),
                    "values": values,
                })
            })
            .collect();
        let log: Vec<Value> = core.log.iter().map(|l| json!(l)).collect();
        json!({
            "ready": core.ready,
            "message": core.message,
            "model": core.caps.model,
            "mccs": core.caps.mccs,
            "values": values,
            "features": features,
            "log": log,
        })
    }

    fn args_for(&self, core: &Core, job: Job) -> Vec<String> {
        let bus = core.bus;
        match job.kind {
            JobKind::Detect => vec!["detect".into(), "--brief".into()],
            JobKind::Capabilities => {
                vec!["--bus".into(), bus.to_string(), "capabilities".into()]
            }
            JobKind::Get => vec![
                "--bus".into(),
                bus.to_string(),
                "--sleep-multiplier".into(),
                ".4".into(),
                "getvcp".into(),
                format!("{code:x}", code = job.code),
            ],
            JobKind::Set => {
                let max = core.max.get(&job.code).copied().unwrap_or(255);
                let write = to_write_scale(job.code, job.value, max);
                vec![
                    "--bus".into(),
                    bus.to_string(),
                    "--noverify".into(),
                    "--sleep-multiplier".into(),
                    ".4".into(),
                    "setvcp".into(),
                    format!("{code:x}", code = job.code),
                    write.to_string(),
                ]
            }
        }
    }

    /// Process every currently queued job to completion. The service spawns
    /// this in a loop after each notification; tests drive it directly.
    pub async fn process_all(&self) {
        loop {
            let job = {
                let mut core = self.core.lock().unwrap();
                match core.queue.pop_front() {
                    Some(job) => Some(job),
                    None => None,
                }
            };
            let Some(job) = job else { break };
            self.process_job(job).await;
        }
    }

    /// The service entry point: wake on notification, drain the queue.
    pub async fn run(&self) {
        loop {
            self.notify.notified().await;
            self.process_all().await;
        }
    }

    async fn process_job(&self, job: Job) {
        let (bin, args, fake_state) = {
            let core = self.core.lock().unwrap();
            (
                core.ddcutil.clone(),
                self.args_for(&core, job),
                core.fake_state.clone(),
            )
        };
        let (ok, text) = run_ddcutil(&bin, &args, fake_state.as_deref()).await;
        let mut core = self.core.lock().unwrap();
        match job.kind {
            JobKind::Detect => {
                if let Some(bus) = find_bus(&text) {
                    core.bus = bus;
                    core.ready = true;
                    core.message = format!("Edge found on i2c bus {bus}");
                    core.log_line(format!("detect: bus {bus}"));
                    if core.seed_standard {
                        core.queue
                            .push_back(Job::new(JobKind::Capabilities, 0, 0));
                        for code in [
                            PICTURE_CODES[0],
                            PICTURE_CODES[1],
                            PICTURE_CODES[2],
                            PICTURE_CODES[3],
                            PICTURE_CODES[4],
                            PICTURE_CODES[5],
                            PICTURE_CODES[6],
                            vcp::INPUT_SOURCE,
                            vcp::POWER,
                        ] {
                            core.queue.push_back(Job::new(JobKind::Get, code, 0));
                        }
                    }
                } else {
                    core.ready = false;
                    core.message =
                        "Edge not found by ddcutil (is it connected via DisplayPort/HDMI?)"
                            .to_string();
                    core.log_line("detect: Edge not found".to_string());
                }
            }
            JobKind::Capabilities => {
                if ok {
                    core.caps = parse_capabilities(&text);
                    core.log_line("capabilities read".to_string());
                } else {
                    core.log_line(format!("capabilities failed: {}", text.trim()));
                }
            }
            JobKind::Get => {
                if !ok {
                    core.log_line(format!("getvcp 0x{:x} failed: {}", job.code, text.trim()));
                } else {
                    match parse_getvcp(&text) {
                        Some(GetValue::Continuous { current, max }) => {
                            if max > 0 {
                                core.max.insert(job.code, max);
                            }
                            core.vcp.insert(
                                job.code,
                                VcpState {
                                    current: current as i32,
                                    max,
                                },
                            );
                            core.log_line(format!("getvcp {:02x} -> {}", job.code, current));
                        }
                        Some(GetValue::NonContinuous(value)) => {
                            core.vcp.insert(
                                job.code,
                                VcpState {
                                    current: value as i32,
                                    max: 0,
                                },
                            );
                            core.log_line(format!("getvcp {:02x} -> {}", job.code, value));
                        }
                        None => {
                            core.log_line(format!(
                                "getvcp 0x{:x}: unparsed output: {}",
                                job.code,
                                text.trim()
                            ));
                        }
                    }
                }
            }
            JobKind::Set => {
                if !ok {
                    core.log_line(format!("setvcp 0x{:x} failed: {}", job.code, text.trim()));
                } else {
                    let max = core.max.get(&job.code).copied().unwrap_or(0);
                    core.vcp.insert(job.code, VcpState {
                        current: job.value as i32,
                        max,
                    });
                    core.log_line(format!("setvcp {:02x} {} -> ok", job.code, job.value));
                    // --noverify keeps writes fast, so ask the panel what it made
                    // of this one rather than trusting it. Skipped while another
                    // write or a read of the same code is still queued, since
                    // that one settles it anyway.
                    let settled_later = core.queue.iter().any(|queued| {
                        queued.code == job.code
                            && (queued.kind == JobKind::Set || queued.kind == JobKind::Get)
                    });
                    if reads_back(job.code) && !settled_later {
                        core.queue.push_back(Job::new(JobKind::Get, job.code, 0));
                    }
                }
            }
        }
        while core.log.len() > 200 {
            core.log.pop_front();
        }
        // Announce this job the way the C++ agent does: one `ddcLog` line per
        // appended message, then the refreshed `ddc` snapshot (Api.cpp:129-205).
        // The guard is dropped first so `snapshot()` may take it again.
        let lines: Vec<String> = core.pending_log.drain(..).collect();
        drop(core);
        let sink = self
            .events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(sink) = sink {
            for line in lines {
                let _ = sink.send(json!({ "event": "ddcLog", "data": { "line": line } }));
            }
            let _ = sink.send(json!({ "event": "ddc", "data": self.snapshot() }));
        }
    }
}

/// Find the Edge's `/dev/i2c-N` bus in a `ddcutil detect --brief` dump.
fn find_bus(text: &str) -> Option<i32> {
    const MARKER: &str = "/dev/i2c-";
    for block in text.split("Display ") {
        if !block.contains("XENEON EDGE") {
            continue;
        }
        let idx = block.find(MARKER)?;
        let rest = &block[idx + MARKER.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        return digits.parse().ok();
    }
    None
}

/// A parsed `getvcp` result: continuous features report current/max, discrete
/// (preset, input source) features report a single hex value.
#[derive(Clone, Copy, Debug)]
enum GetValue {
    Continuous { current: u16, max: u16 },
    NonContinuous(u16),
}

fn digits_after(text: &str, needle: &str, sep: char) -> Option<u32> {
    let idx = text.find(needle)?;
    let rest = &text[idx + needle.len()..];
    let eq = rest.find(sep)?;
    let tail = rest[eq + 1..].trim_start();
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn parse_getvcp(text: &str) -> Option<GetValue> {
    if text.contains("current value") {
        return Some(GetValue::Continuous {
            current: digits_after(text, "current value", '=')? as u16,
            max: digits_after(text, "max value", '=')? as u16,
        });
    }
    for needle in ["(0x", "sl=0x"] {
        if let Some(idx) = text.find(needle) {
            let rest = &text[idx + needle.len()..];
            let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if !hex.is_empty() {
                if let Ok(value) = u16::from_str_radix(&hex, 16) {
                    return Some(GetValue::NonContinuous(value));
                }
            }
        }
    }
    None
}

/// Run one ddcutil invocation, returning (exit success, merged output).
async fn run_ddcutil(bin: &Path, args: &[String], fake_state: Option<&Path>) -> (bool, String) {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    if let Some(state) = fake_state {
        cmd.env("FAKE_DDCUTIL_STATE", state);
    }
    match cmd.output().await {
        Ok(output) => {
            let ok = output.status.success();
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            (ok, text)
        }
        Err(_) => (false, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Absolute path to the checked-in fake ddcutil script.
    fn fake_ddcutil() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-ddcutil/ddcutil")
    }

    fn fixture_capabilities() -> String {
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/xeneon-edge-capabilities.txt"),
        )
        .expect("fixture present")
    }

    #[test]
    fn capabilities_parse_matches_cpp_fixture() {
        let caps = parse_capabilities(&fixture_capabilities());
        assert_eq!(caps.model, "RTK");
        assert_eq!(caps.mccs, "2.2");

        for code in [
            0x10, 0x12, 0x14, 0x16, 0x18, 0x1A, 0x87, 0x60, 0xD6, 0x04, 0x05, 0x08,
        ] {
            assert!(caps.has(code), "panel must advertise VCP 0x{code:02X}");
        }
        assert!(!caps.has(0x99));
        assert!(caps.find(0x99).is_none());

        let bright = caps.find(0x10).unwrap();
        assert!(bright.values.is_empty());
        assert_eq!(bright.name, "Brightness");

        let preset = caps.find(0x14).unwrap();
        assert!(!preset.values.is_empty());
        assert_eq!(preset.values.len(), 7);
        assert_eq!(preset.values[0].code, 0x01);
        assert_eq!(preset.values[0].label, "sRGB");
        assert!(
            preset.values.iter().any(|v| v.code == 0x0B && v.label == "User 1"),
            "RGB gain unlocks on User 1"
        );
        assert!(caps.is_user_preset(0x0B), "User 1 unlocks RGB gain");
        assert!(!caps.is_user_preset(0x01), "sRGB does not unlock RGB gain");
        assert!(!caps.is_user_preset(0x99), "unknown preset is not a user preset");

        let input = caps.find(0x60).unwrap();
        assert_eq!(input.values.len(), 7);
        assert!(
            !input.values.iter().any(|v| v.label.contains("USB")),
            "the panel advertises no USB-C input source"
        );

        assert!(parse_capabilities("").features.is_empty());
        assert!(!parse_capabilities("").has(0x10));
        assert!(parse_capabilities("this is not a capability string\nat all\n")
            .features
            .is_empty());
        assert!(parse_capabilities("VCP Features:\n   Feature: ZZ (Nonsense)\n")
            .features
            .is_empty());
    }

    #[test]
    fn to_write_scale_matches_cpp_roundtrip() {
        // The panel reports gain on 0..255; the write goes out in percent.
        for value in [2u16, 63, 127, 163, 252, 255, 0] {
            let written = to_write_scale(vcp::GAIN_RED, value, 255);
            // The fake ddcutil converts the percent back to 0..255 the same way
            // the real panel does (percent * 255 / 100).
            let read_back = (written as u32 * 255) / 100;
            assert_eq!(read_back, value as u32, "gain must round-trip");
        }
        // Sharpness is not scaled like gain: 2 stays 2, not 50 of 4.
        assert_eq!(to_write_scale(vcp::SHARPNESS, 2, 4), 2);
        // A missing max falls back to 255 so gain is still scaled.
        assert_eq!(to_write_scale(vcp::GAIN_BLUE, 127, 0), 50);
    }

    #[test]
    fn unsafe_writes_need_a_confirmation() {
        assert!(is_unsafe_write(0x60));
        assert!(is_unsafe_write(0xD6));
        assert!(!is_unsafe_write(0x10));
        assert!(guard_unsafe_write(0x60, None).is_err());
        assert!(guard_unsafe_write(0xD6, None).is_err());
        assert!(guard_unsafe_write(0xD6, Some(&Confirmation::approve("blank the panel"))).is_ok());
        assert!(guard_unsafe_write(0x10, None).is_ok());
    }

    async fn detect_and_read(ddc: &Ddc) {
        ddc.start();
        ddc.process_all().await;
        assert!(ddc.ready(), "detect must find the Edge on the fake bus");
    }

    #[tokio::test]
    async fn ddcclient_behavior_matches_cpp() {
        let dir = tempdir_state();
        let ddc = Ddc::new(DdcConfig {
            ddcutil: Some(fake_ddcutil()),
            seed_standard: false,
        });
        ddc.set_fake_state(dir.join("state"));
        detect_and_read(&ddc).await;

        // A gain write is followed by a read of what the panel took.
        ddc.set_vcp(vcp::GAIN_RED, 127);
        ddc.process_all().await;
        let red = getvcp_value(&ddc, vcp::GAIN_RED);
        assert_eq!(red.current, 127);
        assert_eq!(red.max, 255);

        // Two writes to one code: both are queued before processing, so the
        // second coalesces into the first and a single settle-read lands after.
        ddc.set_vcp(vcp::GAIN_GREEN, 30);
        ddc.set_vcp(vcp::GAIN_GREEN, 40);
        ddc.process_all().await;
        let green = getvcp_value(&ddc, vcp::GAIN_GREEN);
        assert_eq!(green.current, 40);
        assert_eq!(getvcp_log_count(&ddc, vcp::GAIN_GREEN), 1);

        // A caller that already queued its own read does not get a second one.
        ddc.set_vcp(vcp::BRIGHTNESS, 40);
        ddc.get_vcp(vcp::BRIGHTNESS);
        ddc.process_all().await;
        let bright = getvcp_value(&ddc, vcp::BRIGHTNESS);
        assert_eq!(bright.current, 40);
        assert_eq!(getvcp_log_count(&ddc, vcp::BRIGHTNESS), 1);

        // Power is a command, not a value a slider shows, so it is not read back.
        ddc.set_vcp(vcp::POWER, 0x05);
        ddc.process_all().await;
        assert_eq!(getvcp_log_count(&ddc, vcp::POWER), 0);
        assert_eq!(getvcp_value(&ddc, vcp::POWER).current, 5);

        // RGB gain goes out in percent; the panel reads it back on 0..255. Both
        // ends of the slider reach both ends of the range, and a value as the
        // panel reported it comes back unchanged when written again.
        for value in [255u16, 0, 2, 63, 127, 163, 252] {
            ddc.set_vcp(vcp::GAIN_BLUE, value);
            ddc.process_all().await;
            assert_eq!(getvcp_value(&ddc, vcp::GAIN_BLUE).current, value as i32);
        }

        // Sharpness tops out at 4 on this panel and is not scaled like gain.
        ddc.set_vcp(vcp::SHARPNESS, 3);
        ddc.process_all().await;
        let sharpness = getvcp_value(&ddc, vcp::SHARPNESS);
        assert_eq!(sharpness.current, 3);
        assert_eq!(sharpness.max, 4);
    }

    #[tokio::test]
    async fn on_ready_seeds_capabilities_and_real_values() {
        // The service path: once the bus is found it fetches the capability
        // string and seeds the standard reads, so a UI connecting later has
        // real values instead of placeholders. This exercises capabilities
        // parsing and the reported maxima offline.
        let dir = tempdir_state();
        let ddc = Ddc::new(DdcConfig {
            ddcutil: Some(fake_ddcutil()),
            seed_standard: true,
        });
        ddc.set_fake_state(dir.join("state"));
        ddc.start();
        ddc.process_all().await;

        assert!(ddc.ready());
        let snap = ddc.snapshot();
        assert_eq!(snap["model"], "RTK");
        assert_eq!(snap["mccs"], "2.2");
        assert!(ddc.has(0x14));
        assert!(ddc.has(0x60));

        // The seeding reads every picture code and reports the panel maxima.
        let sharpness = getvcp_value(&ddc, vcp::SHARPNESS);
        assert_eq!(sharpness.max, 4, "sharpness tops out at 4 on this panel");
        let preset = getvcp_value(&ddc, vcp::PRESET);
        assert_eq!(preset.current, 0x0b, "default preset is User 1");
        assert_eq!(getvcp_value(&ddc, vcp::GAIN_RED).max, 255);
    }

    #[tokio::test]
    async fn a_finished_job_announces_ddc_and_its_log_lines() {
        // The RPC layer's `ddc`/`ddcLog` fan-out depends on this sink: the C++
        // agent broadcasts both from DdcClient's signals, and without it the
        // picture sliders have no live feedback on the Rust path.
        let dir = tempdir_state();
        let ddc = Ddc::new(DdcConfig {
            ddcutil: Some(fake_ddcutil()),
            seed_standard: false,
        });
        let (tx, mut rx) = broadcast::channel(32);
        ddc.set_event_sink(tx);
        ddc.set_fake_state(dir.join("state"));

        ddc.start();
        ddc.process_all().await;

        // One snapshot event per finished job, plus one ddcLog line per message.
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        let snapshots: Vec<&Value> = events.iter().filter(|e| e["event"] == "ddc").collect();
        assert!(!snapshots.is_empty(), "a finished job must announce `ddc`");
        let last = snapshots.last().unwrap();
        assert_eq!(last["data"]["ready"], json!(true), "{last}");
        assert!(
            last["data"]["message"]
                .as_str()
                .unwrap_or("")
                .starts_with("Edge found on i2c bus"),
            "{last}"
        );

        let logs: Vec<&Value> = events.iter().filter(|e| e["event"] == "ddcLog").collect();
        assert!(
            logs.iter().any(|e| e["data"]["line"]
                .as_str()
                .unwrap_or("")
                .starts_with("detect: bus")),
            "the detect line must reach ddcLog, got {events:?}"
        );
        // Drained: a second job does not re-announce the first one's lines.
        ddc.get_vcp(vcp::BRIGHTNESS);
        ddc.process_all().await;
        let mut second = Vec::new();
        while let Ok(event) = rx.try_recv() {
            second.push(event);
        }
        assert!(
            !second
                .iter()
                .any(|e| e["event"] == "ddcLog"
                    && e["data"]["line"].as_str().unwrap_or("").starts_with("detect: bus")),
            "log lines must be announced once: {second:?}"
        );
    }

    // Helpers for the fixture test: pull the last settled value / count reads.
    fn getvcp_value(ddc: &Ddc, code: u8) -> VcpState {
        ddc.core
            .lock()
            .unwrap()
            .vcp
            .get(&code)
            .copied()
            .unwrap_or_default()
    }

    fn getvcp_log_count(ddc: &Ddc, code: u8) -> usize {
        let prefix = format!("getvcp {code:02x} ->");
        let core = ddc.core.lock().unwrap();
        core.log.iter().filter(|line| line.starts_with(&prefix)).count()
    }

    /// A unique temp dir for the fake state file, removed on drop. No external
    /// crate is used so the build stays `--locked`.
    struct TestTempDir {
        path: PathBuf,
    }
    impl TestTempDir {
        fn new() -> Self {
            let unique = format!(
                "edgeline-ddc-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Create a unique temp dir for the fake state file. Callers must keep the
    /// returned handle alive so the directory is not removed early.
    fn tempdir_state() -> TestTempDir {
        TestTempDir::new()
    }
}
