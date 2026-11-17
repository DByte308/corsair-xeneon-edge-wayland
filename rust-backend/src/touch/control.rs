// SPDX-License-Identifier: GPL-3.0-or-later
//! Xinput touch modes, the 3x3 transformation matrix and on-panel calibration,
//! ported from agent/src/core/TouchControl.cpp.
//!
//! `xinput` is deliberately not hard-wired: commands run through a [`Runner`],
//! so the logic can be exercised offline with a fake `xinput` fixture. The
//! system runner is the default; tests pass an in-memory fake.
use crate::touch::{calibration, config, xinput};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The device name the Edge's touch controller enumerates as under X11. A
/// separate USB device (27c0:0859) from the Bragi 1b1c:1d0d control channel.
pub const TOUCH_NAME: &str = "wch.cn TouchScreen";
const EDGE_MASTER: &str = "xeneon-edge";

/// How the Edge's touchscreen behaves, matching the C++ `TouchControl::Mode`.
///
/// `Independent` is surfaced to the UI as "own-pointer" and `Indicator` as
/// "ripple"; the mode indices are the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off = 0,
    MainCursor = 1,
    Independent = 2,
    Indicator = 3,
}

impl Mode {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Mode::Off),
            "main-cursor" => Some(Mode::MainCursor),
            "own-pointer" => Some(Mode::Independent),
            "ripple" => Some(Mode::Indicator),
            _ => None,
        }
    }
    pub fn index(self) -> i32 {
        self as i32
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::MainCursor => "main-cursor",
            Mode::Independent => "own-pointer",
            Mode::Indicator => "ripple",
        }
    }
}

/// The settled state of the touch subsystem, matching `TouchControl::State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The C++ controller's pre-refresh sentinel; the Rust backend always
    /// settles to a concrete state on read, so this is never constructed but
    /// is kept to preserve the wire index of the enum.
    #[allow(dead_code)]
    Unknown = 0,
    Enabled = 1,
    Disabled = 2,
    NotFound = 3,
    Unsupported = 4,
}

impl State {
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// The result of running a command through a [`Runner`].
pub struct RunResult {
    pub stdout: String,
    pub exit_code: i32,
}

/// A command runner. `xinput`/`xrandr` are swapped in here so the touch logic
/// can be tested with a fake command script + fixture output, offline.
pub trait Runner: Send {
    fn run(&self, program: &str, args: &[&str]) -> RunResult;
}

/// Runs commands for real via `std::process::Command`.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> RunResult {
        match std::process::Command::new(program).args(args).output() {
            Ok(out) => RunResult {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                exit_code: out.status.code().unwrap_or(-1),
            },
            Err(_) => RunResult {
                stdout: String::new(),
                exit_code: -1,
            },
        }
    }
}

/// True when the session is X11 (or has a DISPLAY without being Wayland),
/// mirroring the C++ `TouchControl::isX11()`.
pub fn is_x11() -> bool {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default().to_lowercase();
    let display = std::env::var("DISPLAY").unwrap_or_default();
    session == "x11" || (!display.is_empty() && session != "wayland")
}

/// True when KDE Wayland is the active session.
pub fn wayland_active() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .eq_ignore_ascii_case("wayland")
}

fn run(runner: &dyn Runner, program: &str, args: &[&str]) -> RunResult {
    runner.run(program, args)
}

fn id_in_line(line: &str) -> Option<i32> {
    xinput::id_in_line(line)
}

/// The slave id of the digitizer in a listing line, matched by the `id=N`
/// token. The C++ agent uses a regex for `id=(\d+)` and keeps the first match.
fn parse_device_ids(listing: &str) -> Vec<i32> {
    let mut ids = Vec::new();
    for line in listing.lines() {
        if line.contains(TOUCH_NAME) {
            if let Some(id) = id_in_line(line) {
                ids.push(id);
            }
        }
    }
    ids
}

/// xinput ids of the "wch.cn TouchScreen" devices.
pub fn device_ids(runner: &dyn Runner) -> Vec<i32> {
    let out = run(runner, "xinput", &["list", "--short"]);
    if out.exit_code != 0 {
        return Vec::new();
    }
    parse_device_ids(&out.stdout)
}

// "Device Enabled[^:]*:\s*(\d)" : true only when the captured value is 1.
fn device_enabled(props: &str) -> bool {
    for line in props.lines() {
        if !line.contains("Device Enabled") {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let value = line[colon + 1..].trim();
            if value.starts_with('1') {
                return true;
            }
        }
    }
    false
}

// "pointer\s*\((\d+)\)" : the master a slave is attached to, from the short list.
fn pointer_master_id(line: &str) -> Option<i32> {
    let bytes = line.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 7 <= n {
        if &bytes[i..i + 7] == b"pointer" {
            let mut j = i + 7;
            while j < n && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < n && bytes[j] == b'(' {
                let mut k = j + 1;
                let mut value = 0i64;
                while k < n && bytes[k].is_ascii_digit() {
                    value = value * 10 + (bytes[k] - b'0') as i64;
                    k += 1;
                }
                if k > j + 1 && k < n && bytes[k] == b')' {
                    return Some(value as i32);
                }
            }
        }
        i += 1;
    }
    None
}

/// True when the slave at `id` moves the pointer: enabled AND attached to a
/// master pointer.
pub fn device_moves_pointer(runner: &dyn Runner, id: i32) -> bool {
    let props = run(runner, "xinput", &["list-props", &id.to_string()]);
    let enabled = device_enabled(&props.stdout);
    let listing = run(runner, "xinput", &["list", "--short"]).stdout;
    let floating = listing.lines().any(|line| {
        line.contains(&format!("id={id}"))
            && line.contains(TOUCH_NAME)
            && line.contains("floating")
    });
    enabled && !floating
}

/// The master this slave is attached to, or -1 when floating.
pub fn device_master_id(runner: &dyn Runner, id: i32) -> i32 {
    let listing = run(runner, "xinput", &["list", "--short"]).stdout;
    for line in listing.lines() {
        if line.contains(&format!("id={id}")) && line.contains(TOUCH_NAME) {
            if line.contains("floating") {
                return -1;
            }
            if let Some(master) = pointer_master_id(line) {
                return master;
            }
        }
    }
    -1
}

/// The "Virtual core pointer" id from the short list, or -1.
pub fn master_pointer_id(runner: &dyn Runner) -> i32 {
    let listing = run(runner, "xinput", &["list", "--short"]).stdout;
    for line in listing.lines() {
        if line.contains("master pointer") {
            if let Some(id) = id_in_line(line) {
                return id;
            }
        }
    }
    -1
}

/// The id of the "xeneon-edge" master pointer, creating it if it is missing.
pub fn ensure_edge_master(runner: &dyn Runner) -> i32 {
    let id = edge_master_id(runner);
    if id > 0 {
        return id;
    }
    run(runner, "xinput", &["create-master", EDGE_MASTER]);
    edge_master_id(runner)
}

fn edge_master_id(runner: &dyn Runner) -> i32 {
    let listing = run(runner, "xinput", &["list", "--short"]).stdout;
    let lines = xinput::split_lines(&listing);
    xinput::find_master_pointer_id(&lines, EDGE_MASTER)
}

/// Remove the Edge master pointer when it no longer has real slaves, so
/// leaving Own-pointer mode does not leave a stray master keyboard behind
/// that splits keyboard focus.
pub fn remove_edge_master_if_empty(runner: &dyn Runner) {
    let listing = run(runner, "xinput", &["list", "--short"]).stdout;
    let lines = xinput::split_lines(&listing);
    let master_id = xinput::find_master_pointer_id(&lines, EDGE_MASTER);
    if master_id < 0 {
        return;
    }
    if xinput::master_has_real_slaves(&lines, master_id) {
        return;
    }
    run(runner, "xinput", &["remove-master", &master_id.to_string()]);
}

/// The xrandr output showing the 2560x720 Edge, or an empty string.
pub fn edge_output_name(runner: &dyn Runner) -> String {
    let out = run(runner, "xrandr", &["--query"]);
    if out.exit_code != 0 {
        return String::new();
    }
    for line in out.stdout.lines() {
        if !line.contains(" connected ") {
            continue;
        }
        if line.contains("2560x720+") {
            if let Some(name) = line.split_whitespace().next() {
                return name.to_string();
            }
        }
    }
    String::new()
}

/// The current touch mode, read back from the live xinput topology.
pub fn mode(runner: &dyn Runner) -> Mode {
    let ids = device_ids(runner);
    if ids.is_empty() {
        return Mode::Off;
    }
    // Enabled? any disabled -> Off.
    for id in &ids {
        let props = run(runner, "xinput", &["list-props", &id.to_string()]).stdout;
        if !device_enabled(&props) {
            return Mode::Off;
        }
    }
    let core = master_pointer_id(runner);
    let mut any_floating = false;
    let mut any_non_core = false;
    for id in &ids {
        let master = device_master_id(runner, *id);
        if master < 0 {
            any_floating = true;
        } else if master != core {
            any_non_core = true;
        }
    }
    if any_floating {
        Mode::Indicator // floating: drives no pointer, ripple-only
    } else if any_non_core {
        Mode::Independent
    } else {
        Mode::MainCursor
    }
}

/// Apply a touch mode. Returns false when there is nothing to change or a
/// master could not be resolved; the C++ agent reports the same via refresh().
pub fn set_mode(runner: &dyn Runner, m: Mode) -> bool {
    let ids = device_ids(runner);
    if ids.is_empty() {
        return false;
    }
    match m {
        Mode::Off => {
            for id in &ids {
                run(runner, "xinput", &["disable", &id.to_string()]);
            }
            true
        }
        Mode::Indicator => {
            // Float the devices so they drive NO pointer at all; raw touch
            // events still fire and feed the ripple overlay.
            for id in &ids {
                run(runner, "xinput", &["enable", &id.to_string()]);
                run(runner, "xinput", &["float", &id.to_string()]);
            }
            apply_output_mapping(runner); // keep the valuator geometry consistent
            remove_edge_master_if_empty(runner);
            true
        }
        _ => {
            let master = if m == Mode::MainCursor {
                master_pointer_id(runner)
            } else {
                ensure_edge_master(runner)
            };
            let ok = master > 0;
            for id in &ids {
                run(runner, "xinput", &["enable", &id.to_string()]);
                if master > 0 {
                    run(runner, "xinput", &["reattach", &id.to_string(), &master.to_string()]);
                }
            }
            apply_output_mapping(runner);
            if m == Mode::MainCursor {
                remove_edge_master_if_empty(runner); // leaving Independent may orphan its master
            }
            ok
        }
    }
}

// "Coordinate Transformation Matrix[^:]*:\s*([^\n]+)" then split on commas.
fn matrix_from_props(props: &str) -> Option<[f64; 9]> {
    for line in props.lines() {
        if line.contains("Coordinate Transformation Matrix") {
            let after = line.splitn(2, ':').nth(1)?;
            let mut vals = Vec::with_capacity(9);
            for token in after.split(',') {
                let token = token.trim();
                let v = token.parse::<f64>().ok()?;
                vals.push(v);
            }
            if vals.len() == 9 {
                let mut m = [0.0; 9];
                m.copy_from_slice(&vals);
                return Some(m);
            }
            return None;
        }
    }
    None
}

/// The first touch device's 3x3 matrix, or identity on failure.
pub fn read_matrix(runner: &dyn Runner) -> [f64; 9] {
    let ident = calibration::K_IDENTITY;
    let ids = device_ids(runner);
    if ids.is_empty() {
        return ident;
    }
    let props = run(runner, "xinput", &["list-props", &ids[0].to_string()]).stdout;
    matrix_from_props(&props).unwrap_or(ident)
}

/// Format a f64 the way Qt's `QString::number(v, 'g', 10)` does closely enough
/// for xinput to parse the same value: shortest round-trip decimal.
fn format_float(v: f64) -> String {
    v.to_string()
}

/// Map the touch devices to the Edge output (geometric mapping).
pub fn apply_output_mapping(runner: &dyn Runner) -> bool {
    let output = edge_output_name(runner);
    if output.is_empty() {
        return false;
    }
    let mut ok = true;
    for id in device_ids(runner) {
        let r = run(runner, "xinput", &["map-to-output", &id.to_string(), &output]);
        if r.exit_code != 0 {
            ok = false;
        }
    }
    ok
}

/// Set an explicit 3x3 matrix on all touch devices (for calibration).
/// Returns false when the matrix is not 3x3 or xinput refuses it.
pub fn set_matrix(runner: &dyn Runner, m: &[f64]) -> bool {
    if m.len() != 9 {
        return false;
    }
    let mut ok = true;
    for id in device_ids(runner) {
        let mut cmd: Vec<String> = vec![
            "set-prop".to_string(),
            id.to_string(),
            "Coordinate Transformation Matrix".to_string(),
        ];
        for v in m {
            cmd.push(format_float(*v));
        }
        let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
        let r = run(runner, "xinput", &refs);
        if r.exit_code != 0 {
            ok = false;
        }
    }
    ok
}

fn refresh(runner: &dyn Runner) -> (State, String) {
    if wayland_active() {
        // The Rust backend does not integrate KWin's touch D-Bus surface yet;
        // present the feature as greyed out rather than pretending to work.
        return (
            State::Unsupported,
            "Rust backend does not yet integrate KDE Wayland touch; X11 (xinput) only.".to_string(),
        );
    }
    if !is_x11() {
        return (
            State::Unsupported,
            "Touch toggle needs X11 (xinput). This session isn't X11.".to_string(),
        );
    }
    let ids = device_ids(runner);
    if ids.is_empty() {
        return (State::NotFound, format!("No \"{TOUCH_NAME}\" device found."));
    }
    let any_moves = ids.iter().any(|id| device_moves_pointer(runner, *id));
    if any_moves {
        (
            State::Enabled,
            format!(
                "Touching the Edge moves the mouse pointer ({} input device(s)).",
                ids.len()
            ),
        )
    } else {
        (State::Disabled, "Touch does not move the pointer (disabled or detached).".to_string())
    }
}

fn num(value: &Value, name: &str) -> f64 {
    value.get(name).and_then(Value::as_f64).unwrap_or(0.0)
}

fn points_from(value: Option<&Value>) -> Vec<calibration::CalPoint> {
    let mut out = Vec::new();
    if let Some(array) = value.and_then(Value::as_array) {
        for item in array {
            out.push(calibration::CalPoint {
                x: item.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                y: item.get("y").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
    }
    out
}

/// Persist the touch mode alongside the touch config. Best-effort: a failure to
/// write the setting does not roll back an already-applied mode.
fn save_touch_mode(config_path: &Path, index: i32) {
    let Some(dir) = config_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let settings = dir.join("settings.json");
    let mut value: Value = std::fs::read_to_string(&settings)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    if let Value::Object(map) = &mut value {
        map.insert("touchMode".to_string(), Value::from(index));
    }
    let _ = std::fs::write(&settings, serde_json::to_string_pretty(&value).unwrap_or_default());
}

/// The high-level touch controller used by the Rust backend's RPC dispatch.
/// Bundles the command runner, the on-disk gesture config and the global
/// per-client stream switch.
pub struct Touch {
    runner: Box<dyn Runner>,
    config_path: PathBuf,
    config: config::Config,
    streaming: bool,
}

impl Touch {
    /// Create a controller with the system runner and the default config path.
    pub fn new(config_path: PathBuf) -> Self {
        Self::with_runner(Box::new(SystemRunner), config_path)
    }

    /// Create a controller running commands through `runner`.
    pub fn with_runner(runner: Box<dyn Runner>, config_path: PathBuf) -> Self {
        let config = config::load_from(&config_path);
        Self {
            runner,
            config_path,
            config,
            streaming: false,
        }
    }

    fn refresh(&self) -> (State, String) {
        refresh(self.runner.as_ref())
    }

    fn snapshot(&self) -> Value {
        let current = mode(self.runner.as_ref());
        let ids = device_ids(self.runner.as_ref());
        let (state, detail) = self.refresh();
        json!({
            "mode": current.name(),
            "modeIndex": current.index(),
            "state": state.index(),
            "detail": detail,
            "deviceIds": ids,
            "matrix": read_matrix(self.runner.as_ref()),
            "streaming": self.streaming,
        })
    }

    /// Re-read xinput and report the world, as `touch.state`.
    pub fn state(&self) -> Value {
        self.snapshot()
    }

    /// Apply a named mode and return the refreshed snapshot.
    pub fn set_mode(&mut self, name: &str) -> Result<Value> {
        let m = Mode::from_name(name)
            .context("mode must be off, main-cursor, own-pointer or ripple")?;
        if wayland_active() {
            bail!("Rust backend does not yet integrate KDE Wayland touch; use an X11 session (xinput).");
        }
        if !is_x11() {
            bail!("Touch modes need X11 (xinput). This session isn't X11.");
        }
        if !set_mode(self.runner.as_ref(), m) {
            bail!("could not apply the mode (xinput failed)");
        }
        save_touch_mode(&self.config_path, m.index());
        self.sync_streaming();
        Ok(self.snapshot())
    }

    /// Switch the per-client touch event stream on or off globally.
    ///
    /// The switch exists so the UI can reflect the requested state, but raw
    /// touch events do not yet reach the recognizer on the real panel, so it is
    /// presented (greyed out) rather than promising live events.
    pub fn set_streaming(&mut self, on: bool) -> Value {
        self.streaming = on;
        json!({ "streaming": self.streaming })
    }

    /// Read-only probe of the kernel touch stack (greyed out: the X-server half
    /// and live contact capture are not wired into the Rust backend yet).
    pub fn probe(&self) -> Value {
        let base = "/sys/bus/hid/devices";
        let mut have = false;
        let mut interfaces: Vec<Value> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.contains("27C0:0859") {
                    continue;
                }
                have = true;
                let mut iface = json!({
                    "hidId": name,
                    "driver": "none",
                    "inputName": "",
                    "eventNode": "",
                    "descriptorBytes": 0,
                    "fingerCollections": 0,
                    "contactCountMax": 0,
                    "configReportId": -1,
                });
                if let Ok(target) = std::fs::read_link(entry.path().join("driver")) {
                    if let Some(file_name) = target.file_name() {
                        iface["driver"] = Value::String(file_name.to_string_lossy().into_owned());
                    }
                }
                if let Ok(inputs) = std::fs::read_dir(entry.path().join("input")) {
                    for input in inputs.flatten() {
                        if let Ok(name_text) = std::fs::read_to_string(input.path().join("name")) {
                            iface["inputName"] = Value::String(name_text.trim().to_string());
                        }
                        if let Ok(subs) = std::fs::read_dir(input.path()) {
                            for sub in subs.flatten() {
                                let sname = sub.file_name().to_string_lossy().to_string();
                                if sname.starts_with("event") {
                                    iface["eventNode"] = Value::String(format!("/dev/input/{sname}"));
                                }
                            }
                        }
                    }
                }
                interfaces.push(iface);
            }
        }
        let verdict = if have {
            "Digitizer interface(s) found; the Rust backend has not read the X-server half of this report."
                .to_string()
        } else {
            "No Xeneon Edge digitizer (27c0:0859) found on this system.".to_string()
        };
        json!({
            "haveDigitizer": have,
            "usbId": "27c0:0859",
            "interfaces": interfaces,
            "xdevices": [],
            "verdict": verdict,
            "note": "Read-only sysfs scan only. XInput2 inspection, descriptor counting and live contact capture are not wired into the Rust backend yet."
        })
    }

    /// Map the touch values to the Edge output and return the settled matrix.
    pub fn apply_output_mapping(&self) -> Result<Value> {
        if wayland_active() {
            bail!("Rust backend does not yet integrate KDE Wayland touch.");
        }
        if !apply_output_mapping(self.runner.as_ref()) {
            bail!("could not map touch to the Edge output");
        }
        Ok(json!({ "matrix": read_matrix(self.runner.as_ref()) }))
    }

    /// Apply an explicit 3x3 matrix to every touch device.
    pub fn set_matrix(&self, m: &[f64]) -> Result<Value> {
        if wayland_active() {
            bail!("Rust backend does not yet integrate KDE Wayland touch.");
        }
        if m.len() != 9 {
            bail!("'matrix' must have 9 numbers");
        }
        if !set_matrix(self.runner.as_ref(), m) {
            bail!("xinput refused the matrix");
        }
        Ok(json!({ "matrix": read_matrix(self.runner.as_ref()) }))
    }

    /// Solve a calibration and, only when `apply` is set, write it via xinput.
    /// The residual is reported before anything is applied.
    pub fn calibrate(&self, params: &Value) -> Result<Value> {
        let mut input = calibration::CalibrationInput {
            virtual_width: num(params, "virtualWidth"),
            virtual_height: num(params, "virtualHeight"),
            origin_x: num(params, "originX"),
            origin_y: num(params, "originY"),
            base: calibration::K_IDENTITY,
            ..Default::default()
        };
        input.targets = points_from(params.get("targets"));
        input.measured = points_from(params.get("measured"));

        match params.get("base").and_then(Value::as_array) {
            Some(base) if base.len() == 9 => {
                for (i, v) in base.iter().enumerate() {
                    input.base[i] = v.as_f64().unwrap_or(0.0);
                }
            }
            _ => {
                // No base matrix supplied: use the one that was in effect (the
                // live matrix) so the correction is not applied twice.
                input.base = read_matrix(self.runner.as_ref());
            }
        }

        let res = calibration::solve_calibration(&input);
        if !res.ok {
            bail!("{}", res.error);
        }

        let apply = params.get("apply").and_then(Value::as_bool).unwrap_or(false);
        let mut applied = false;
        if apply {
            applied = set_matrix(self.runner.as_ref(), &res.matrix);
            if !applied {
                bail!("solved, but xinput refused the matrix");
            }
        }
        Ok(json!({
            "matrix": res.matrix,
            "residualsPx": res.residuals_px,
            "rmsPx": res.rms_px,
            "worstPx": res.worst_px,
            "applied": applied,
        }))
    }

    /// Snapshot the gesture config, plus the actions and stream state.
    pub fn config_snapshot(&self) -> Value {
        let mut obj = config::to_json(&self.config);
        if let Value::Object(map) = &mut obj {
            map.insert(
                "availableActions".to_string(),
                Value::Object(config::actions_map()),
            );
            map.insert("streaming".to_string(), Value::Bool(self.streaming));
            let lock_zone_effective = mode(self.runner.as_ref()) == Mode::Indicator;
            map.insert("lockZoneEffective".to_string(), Value::Bool(lock_zone_effective));
            map.insert(
                "path".to_string(),
                Value::String(self.config_path.to_string_lossy().into_owned()),
            );
        }
        obj
    }

    /// Validate and persist a gesture config, then return the snapshot.
    pub fn set_config(&mut self, params: &Value) -> Result<Value> {
        if let Some(binds) = params
            .get("gestures")
            .and_then(|g| g.get("bindings"))
            .and_then(Value::as_object)
        {
            for (_, action) in binds {
                let action = action.as_str().unwrap_or("");
                if !config::is_known_action(action) {
                    bail!("'{action}' is not an action");
                }
            }
        }
        let cfg = config::from_json(params);
        let mut error = String::new();
        if !config::save_to(&self.config_path, &cfg, &mut error) {
            bail!("{error}");
        }
        self.config = cfg;
        self.sync_streaming();
        Ok(self.config_snapshot())
    }

    fn sync_streaming(&mut self) {
        let ripple = mode(self.runner.as_ref()) == Mode::Indicator;
        let wanted = ripple
            || self.config.gestures_enabled
            || (self.config.lock_zone.enabled && ripple);
        self.streaming = wanted;
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // Captured xinput topologies, shared with the xinput::tests fixtures.
    const ORPHANED_MASTER: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎜   ↳ Virtual core XTEST pointer              	id=4	[slave  pointer  (2)]
⎜   ↳ Logitech USB Receiver Mouse             	id=10	[slave  pointer  (2)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
    ↳ Virtual core XTEST keyboard             	id=5	[slave  keyboard (3)]
⎡ xeneon-edge pointer                     	id=23	[master pointer  (24)]
⎜   ↳ xeneon-edge XTEST pointer               	id=25	[slave  pointer  (23)]
⎣ xeneon-edge keyboard                    	id=24	[master keyboard (23)]
    ↳ xeneon-edge XTEST keyboard              	id=26	[slave  keyboard (24)]
∼ wch.cn TouchScreen                      	id=22	[floating slave]
"#;

    const MASTER_IN_USE: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
⎡ xeneon-edge pointer                     	id=23	[master pointer  (24)]
⎜   ↳ xeneon-edge XTEST pointer               	id=25	[slave  pointer  (23)]
⎜   ↳ wch.cn TouchScreen                      	id=22	[slave  pointer  (23)]
⎣ xeneon-edge keyboard                    	id=24	[master keyboard (23)]
    ↳ xeneon-edge XTEST keyboard              	id=26	[slave  keyboard (24)]
"#;

    const NO_EDGE_MASTER: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎜   ↳ wch.cn TouchScreen                      	id=22	[slave  pointer  (2)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
"#;

    const PROPS: &str = "Device Enabled (145):\t1\n\nCoordinate Transformation Matrix (151):\t1.000000, 0.000000, 0.000000, 0.000000, 1.000000, 0.000000, 0.000000, 0.000000, 1.000000\n";
    const EDGE_RANDR: &str = "DP-1 connected primary 3440x1440+0+0 (normal left inverted right x axis y axis)\nDP-2 connected 2560x720+0+1440 (normal left inverted right x axis y axis)\n";

    #[derive(Clone)]
    struct FakeRunner {
        list: String,
        props: String,
        edge: String,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn new(list: &str, props: &str, edge: &str) -> Self {
            Self {
                list: list.to_string(),
                props: props.to_string(),
                edge: edge.to_string(),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn has(&self, program: &str, args: &[&str]) -> bool {
            self.calls.borrow().iter().any(|(p, a)| {
                p == program && a.iter().map(String::as_str).eq(args.iter().copied())
            })
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> RunResult {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.iter().map(|s| s.to_string()).collect()));
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            match (program, args.as_slice()) {
                ("xinput", a) if a == &["list".to_string(), "--short".to_string()] => RunResult {
                    stdout: self.list.clone(),
                    exit_code: 0,
                },
                ("xinput", a) if a.first().map(String::as_str) == Some("list-props") => RunResult {
                    stdout: self.props.clone(),
                    exit_code: 0,
                },
                ("xrandr", a) if a == &["--query".to_string()] => RunResult {
                    stdout: self.edge.clone(),
                    exit_code: 0,
                },
                _ => RunResult {
                    stdout: String::new(),
                    exit_code: 0,
                },
            }
        }
    }

    #[test]
    fn device_ids_are_parsed_from_the_listing() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(device_ids(&r), vec![22]);
        assert_eq!(device_ids(&r), vec![22]);
    }

    #[test]
    fn mode_is_derived_from_topology() {
        let floating = FakeRunner::new(ORPHANED_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(mode(&floating), Mode::Indicator);

        let own = FakeRunner::new(MASTER_IN_USE, PROPS, EDGE_RANDR);
        assert_eq!(mode(&own), Mode::Independent);

        let core = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(mode(&core), Mode::MainCursor);
    }

    #[test]
    fn disabled_device_reports_off() {
        let r = FakeRunner::new(NO_EDGE_MASTER, "Device Enabled (145):\t0\n", EDGE_RANDR);
        assert_eq!(mode(&r), Mode::Off);
    }

    #[test]
    fn device_master_and_moves_pointer_are_read() {
        let own = FakeRunner::new(MASTER_IN_USE, PROPS, EDGE_RANDR);
        assert_eq!(device_master_id(&own, 22), 23);
        assert!(device_moves_pointer(&own, 22));

        let floating = FakeRunner::new(ORPHANED_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(device_master_id(&floating, 22), -1);
        assert!(!device_moves_pointer(&floating, 22));
    }

    #[test]
    fn mode_off_disables_the_devices() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert!(set_mode(&r, Mode::Off));
        assert!(r.has("xinput", &["disable", "22"]));
    }

    #[test]
    fn main_cursor_reattaches_to_the_core_master() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert!(set_mode(&r, Mode::MainCursor));
        assert!(r.has("xinput", &["enable", "22"]));
        assert!(r.has("xinput", &["reattach", "22", "2"]));
        assert!(r.has("xinput", &["map-to-output", "22", "DP-2"]));
    }

    #[test]
    fn own_pointer_reattaches_to_the_edge_master() {
        let r = FakeRunner::new(MASTER_IN_USE, PROPS, EDGE_RANDR);
        assert!(set_mode(&r, Mode::Independent));
        assert!(r.has("xinput", &["reattach", "22", "23"]));
    }

    #[test]
    fn matrix_is_read_and_written() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(read_matrix(&r), calibration::K_IDENTITY);
        let custom = [0.5, 0.0, 0.25, 0.0, 0.5, 0.25, 0.0, 0.0, 1.0];
        assert!(set_matrix(&r, &custom));
        assert!(r.has("xinput", &["set-prop", "22", "Coordinate Transformation Matrix", "0.5", "0", "0.25", "0", "0.5", "0.25", "0", "0", "1"]));
    }

    #[test]
    fn matrix_wrong_len_is_rejected() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert!(!set_matrix(&r, &[1.0, 0.0, 0.0]));
    }

    #[test]
    fn apply_output_mapping_maps_to_the_edge_output() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        assert_eq!(edge_output_name(&r), "DP-2");
        assert!(apply_output_mapping(&r));
        assert!(r.has("xinput", &["map-to-output", "22", "DP-2"]));

        let no_output = FakeRunner::new(NO_EDGE_MASTER, PROPS, "");
        assert!(!apply_output_mapping(&no_output));
    }

    #[test]
    fn config_validates_and_persists_a_gesture_binding() {
        let dir = std::env::temp_dir().join(format!("edgeline-touchctl-{}", std::process::id()));
        let path = dir.join("touch.json");
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        let mut t = Touch::with_runner(Box::new(r), path.clone());

        let params = json!({
            "gestures": {"enabled": true, "bindings": {"swipe-left": "profile-next"}},
            "lockZone": {"enabled": true, "side": "left", "fraction": 0.22},
        });
        let snap = t.set_config(&params).unwrap();
        assert_eq!(snap["gestures"]["enabled"], true);
        assert_eq!(snap["gestures"]["bindings"]["swipe-left"], "profile-next");
        assert_eq!(snap["lockZone"]["side"], "left");
        assert!(snap["availableActions"]["brightness-up"].is_string());
        // Gestures are enabled, so the (best-effort) stream switch follows.
        assert_eq!(snap["streaming"], true);
        assert!(snap["lockZoneEffective"].is_boolean());
        assert_eq!(snap["path"], path.to_string_lossy().as_ref());

        // The persisted file round-trips.
        let reloaded = config::load_from(&path);
        assert!(reloaded.gestures_enabled);
        assert_eq!(reloaded.bindings.get("swipe-left").map(String::as_str), Some("profile-next"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_rejects_an_unknown_action() {
        let dir = std::env::temp_dir().join(format!("edgeline-touchctl-bad-{}", std::process::id()));
        let path = dir.join("touch.json");
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        let mut t = Touch::with_runner(Box::new(r), path.clone());
        let params = json!({
            "gestures": {"enabled": true, "bindings": {"swipe-left": "launch-nukes"}},
        });
        let err = t.set_config(&params).unwrap_err().to_string();
        assert!(err.contains("launch-nukes"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calibrate_reports_residual_before_apply() {
        let r = FakeRunner::new(NO_EDGE_MASTER, PROPS, EDGE_RANDR);
        let t = Touch::with_runner(Box::new(r), std::env::temp_dir().join("touch.json"));
        let params = json!({
            "virtualWidth": 3440, "virtualHeight": 2160, "originX": 0, "originY": 1440,
            "targets": [
                {"x": 307.2, "y": 129.6}, {"x": 2252.8, "y": 129.6}, {"x": 1280.0, "y": 360.0},
                {"x": 307.2, "y": 590.4}, {"x": 2252.8, "y": 590.4}
            ],
            "measured": [
                {"x": 307.2, "y": 129.6}, {"x": 2252.8, "y": 129.6}, {"x": 1280.0, "y": 360.0},
                {"x": 307.2, "y": 590.4}, {"x": 2252.8, "y": 590.4}
            ],
            "apply": false,
        });
        let out = t.calibrate(&params).unwrap();
        assert_eq!(out["applied"], false);
        assert!(out["matrix"].as_array().map(|a| a.len()) == Some(9));
        assert!(out["residualsPx"].as_array().map(|a| a.len()) == Some(5));
        assert!(out["rmsPx"].as_f64().unwrap() < 1e-6);
    }
}
