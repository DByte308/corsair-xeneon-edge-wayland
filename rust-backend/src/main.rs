// SPDX-License-Identifier: GPL-3.0-or-later
pub mod hardware;
mod ddc;
mod gestures;
mod hid;
mod kwin;
mod media;
mod model;
mod touch;
mod sensors;
mod state;
mod profiles;
mod updates;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Notify, Semaphore, broadcast, watch},
};

const MAX_MESSAGE: usize = 1024 * 1024;
#[derive(Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}
struct Backend {
    kwin: kwin::Kwin,
    hid: std::sync::Mutex<Option<hid::Device>>,
    ddc: std::sync::Arc<ddc::Ddc>,
    touch: std::sync::Mutex<touch::Touch>,
    sensors: std::sync::Mutex<sensors::SensorSource>,
    media: media::Media,
    updates: updates::UpdateChecker,
    shutdown: watch::Sender<bool>,
    profiles: profiles::ProfileStore,
    rules: profiles::RulesStore,
    socket_path: std::path::PathBuf,
    /// Per-process fan-out of unsolicited `{event,data}` messages to every
    /// connected client, mirroring `RpcServer::broadcast()` in the C++ agent.
    /// `serve()` subscribes a receiver per connection and a forwarder task
    /// writes each event onto that connection.
    broadcast: broadcast::Sender<Value>,
    /// Wakes the sensor-streaming worker when `sensors.stream` changes, so a
    /// new interval or a disable takes effect immediately instead of after the
    /// current sleep expires.
    streaming_wake: Notify,
}

fn uid() -> u32 {
    // geteuid has no arguments, no failure case and no pointer preconditions.
    unsafe { libc::geteuid() }
}
fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}
fn config_root() -> Result<PathBuf> {
    Ok(home()?.join(".config").join("edgeline"))
}
fn artwork() -> Result<Value> {
    for root in [
        home()?.join(".local/share/Steam"),
        home()?.join(".steam/steam"),
    ] {
        let Ok(entries) = std::fs::read_dir(root.join("appcache/librarycache/3471800")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("library_hero.jpg");
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                continue;
            }
            let image = std::fs::read(path)?;
            if image.starts_with(&[0xff, 0xd8, 0xff]) {
                // No additional image/network dependency; data URL compatible with existing UI.
                return Ok(json!({"image":format!("data:image/jpeg;base64,{}", base64(&image))}));
            }
        }
    }
    Ok(json!({"image":null}))
}
pub(crate) fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        output.push(TABLE[a >> 2] as char);
        output.push(TABLE[((a & 3) << 4) | (b >> 4)] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[c & 63] as char
        } else {
            '='
        });
    }
    output
}
fn color_snapshot() -> Value {
    let devices = profiles::color::read_color_devices();
    let edge = profiles::color::find_edge(&devices);
    match edge {
        None => json!({
            "available": !devices.is_empty(),
            "reason": if devices.is_empty() {
                "colord is not running, or colormgr is not installed"
            } else {
                "colord does not know this panel as a display device"
            },
        }),
        Some(edge) => {
            let profiles: Vec<Value> = edge
                .profiles
                .iter()
                .map(|pr| {
                    json!({ "id": pr.id, "filename": pr.filename, "autoEdid": pr.is_auto_edid() })
                })
                .collect();
            let default = edge
                .default_profile()
                .map(|d| d.filename.clone())
                .unwrap_or_default();
            json!({
                "available": true,
                "deviceId": edge.device_id,
                "model": edge.model,
                "output": edge.xrandr_name,
                "profiles": profiles,
                "defaultProfile": default,
                "calibrated": edge.has_calibration(),
            })
        }
    }
}
impl Backend {
    /// Facts about the Edge Bragi device (device.state). Returns the cached
    /// device if it was opened, else attempts a best-effort open (a safe read)
    /// so `state.all`/`device.state` report real presence during cutover; falls
    /// back to a "not present" placeholder only when the device is genuinely
    /// unavailable, so the UI still renders rather than erroring.
    fn device_info(&self) -> state::DeviceInfo {
        let mut guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            // Opening the Bragi control channel is a pure read; never write-gated.
            *guard = hid::Device::open().ok();
        }
        if let Some(d) = guard.as_ref() {
            return d.device_info();
        }
        state::DeviceInfo {
            present: false,
            accessible: false,
            path: PathBuf::from("/dev/hidraw1"),
            product: "XENEON EDGE".into(),
            serial: String::new(),
        }
    }

    /// The screen state (backlight/contrast/brightness) from the cached HID
    /// channel, matching the C++ screenSnapshot().
    fn screen_snapshot(&self) -> Value {
        let guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
        let Some(device) = guard.as_ref() else {
            return json!({"available":false,"error":"no accessible Edge Bragi HID device"});
        };
        match (device.screen_params(), device.screen_error()) {
            (Some(params), _) => json!({"available":true,
                "brightness": params.brightness, "backlight": params.backlight,
                "contrast": params.contrast}),
            (None, Some(err)) => json!({"available":false, "error": err}),
            (None, None) => json!({"available":false,
                "error":"screen parameters have not been read"}),
        }
    }

    /// Assemble the `system` snapshot (system.info), matching the C++
    /// systemSnapshot() field contract.
    fn system_info(&self) -> Value {
        let home = home().unwrap_or_else(|_| PathBuf::from("/"));
        let cfg = config_root().unwrap_or_else(|_| home.join(".config"));
        let hid_log = home.join(".local/share/edgeline/hid.log");
        let ddcutil_version = self.ddc.tool_version();
        let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        state::system_snapshot(&state::SystemInput {
            version: env!("CARGO_PKG_VERSION"),
            codename: "Cold Start",
            config_path: &home.join(".config/edgeline"),
            profiles_path: &cfg.join("profiles"),
            ddcutil_path: "/usr/bin/ddcutil",
            xinput_path: "/usr/bin/xinput",
            i2c_bus: self.ddc.i2c_bus(),
            autostart: self.profiles.autostart_enabled(),
            updates_enabled: self.updates.enabled(),
            hid_log: &hid_log,
            ddcutil_version: &ddcutil_version,
            xinput_version: "1.4",
            session_type: &session_type,
            socket: self.socket_path.as_path(),
        })
    }

    /// Compose `state.all` from all nine module snapshots, merging the live
    /// media now-playing/notifications into the sensors snapshot so the
    /// dashboard gets them (the C++ agent put them inside sensorSnapshot).
    async fn state_all(&self) -> Result<Value> {
        // Each async module read is pulled into a scoped binding so no
        // std::sync::MutexGuard is held across an .await (the media read is
        // async) and so two guards on the SAME lock never overlap. The
        // touch.state / touch.config_snapshot calls take self.touch once each
        // in their own statement scope.
        let media = self.media.snapshot().await;
        let sensors = {
            let guard = self.sensors.lock().unwrap_or_else(|p| p.into_inner());
            with_media(sensors::snapshot_json(guard.latest()), &media)
        };
        let rules = self.rules_snapshot();
        let (touch_state, touch_config) = {
            // One lock acquisition for both read-only snapshots; avoid ever
            // holding two guards on std::sync::Mutex simultaneously (non-reentrant).
            let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
            (guard.state(), guard.config_snapshot())
        };
        Ok(state::state_all(
            self.system_info(),
            state::device_snapshot(&self.device_info()),
            self.ddc.snapshot(),
            self.screen_snapshot(),
            touch_state,
            sensors,
            rules,
            color_snapshot(),
            touch_config,
        ))

    }

    /// Push an unsolicited event to every connected client (the C++ agent's
    /// `RpcServer::broadcast`). Each `serve()` connection subscribes a receiver
    /// and forwards the `{event,data}` line. A dropped receiver (client gone)
    /// is ignored, and a lagged one just misses intermediate events.
    fn broadcast(&self, event: &str, data: Value) {
        let _ = self
            .broadcast
            .send(json!({ "event": event, "data": data }));
    }

    /// rules.json as the UI sees it. The Rust backend has no X11 focus watcher,
    /// so rules cannot be armed the way the C++ agent arms them.
    fn rules_snapshot(&self) -> Value {
        let cfg = profiles::rules_from_json(&self.rules.load());
        let mut snapshot = profiles::rules_to_json(&cfg);
        snapshot["watching"] = json!(false);
        snapshot["path"] = json!(self.rules.path().display().to_string());
        snapshot
    }

    /// Broadcast the touch state, mirroring the C++ agent's `stateChanged`
    /// push. Call only after the mutating guard on `self.touch` is dropped.
    fn broadcast_touch(&self) {
        let state = {
            let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
            guard.state()
        };
        self.broadcast("touch", state);
    }

    /// Push the sensors snapshot after a mutation that changes what it shows,
    /// but only while a client asked for the stream — the C++ agent gates these
    /// on `m_sensorStreaming` too.
    async fn broadcast_sensors_if_streaming(&self) {
        let streaming = {
            let guard = self.sensors.lock().unwrap_or_else(|p| p.into_inner());
            guard.streaming()
        };
        if !streaming {
            return;
        }
        let media = self.media.snapshot().await;
        let snapshot = {
            let guard = self.sensors.lock().unwrap_or_else(|p| p.into_inner());
            with_media(sensors::snapshot_json(guard.latest()), &media)
        };
        self.broadcast("sensors", snapshot);
    }

    async fn dispatch(&self, request: &Request) -> Result<Value> {
        let parameter = |name| {
            request
                .params
                .get(name)
                .and_then(Value::as_str)
                .context("Missing string parameter")
        };
        match request.method.as_str() {
            "backend.info" => Ok(
                json!({"name":"edgeline-backend","version":env!("CARGO_PKG_VERSION"),
                "language":"Rust","stage":"control-core migration", "uiHost":"client-selected; native Tauri preview available",
                "hardware":"legacy C++ agent (not migrated)","gestures":"disabled by default; state machine ported (flag-gated)",
                "capabilities":["kwin-window-feed","edge-window-activation","window-fitting","native-window-picker","steam-launch","local-artwork"]}),
            ),
            "backend.stop" => Ok(json!({ "stopping": true })),
            "system.info" => Ok(self.system_info()),
            "device.state" => Ok(state::device_snapshot(&self.device_info())),
            "state.all" => self.state_all().await,
            "screen.state" => {
                // Mirror Api::screenSnapshot(): surfaced by A1 per hid.rs. The
                // HID channel caches the last settled parameters; `refresh`
                // re-queries the panel for a change made from its own menu.
                let mut guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
                if guard.is_none() {
                    match hid::Device::open() {
                        Ok(device) => *guard = Some(device),
                        Err(_) => return Ok(json!({ "available": false,
                            "error": "no accessible Edge Bragi HID device" })),
                    }
                }
                let device = guard.as_ref().context("hid device unavailable")?;
                if request.params.get("refresh").and_then(Value::as_bool).unwrap_or(false) {
                    let _ = device.read_screen_params();
                }
                match device.screen_params() {
                    Some(params) => Ok(json!({ "available": true,
                        "brightness": params.brightness, "backlight": params.backlight,
                        "contrast": params.contrast })),
                    None => Ok(json!({ "available": false,
                        "error": device.screen_error().unwrap_or_else(|| "screen parameters have not been read".into()) })),
                }
            }
            "screen.set" => {
                // Brightness is the only HID-exposed field; backlight and
                // contrast stay on DDC where the rest of the app caches them.
                // Mirrors Api::setScreenParam(): a Confirmation gates the write.
                let field = parameter("field")?;
                if field != "brightness" {
                    bail!("'field' must be brightness");
                }
                let value = request.params.get("value").and_then(Value::as_i64)
                    .context("Missing integer 'value'")?;
                if !(0..=100).contains(&value) {
                    bail!("'value' must be 0..100");
                }
                let reason = request.params.get("reason").and_then(Value::as_str)
                    .unwrap_or("set screen brightness");
                let mut guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
                if guard.is_none() {
                    *guard = Some(hid::Device::open().context("no accessible Edge Bragi HID device")?);
                }
                let device = guard.as_ref().context("hid device unavailable")?;
                device.set_brightness(value as u8, &hid::Confirmation::approve(reason))?;
                let params = device.screen_params().context("screen parameters unavailable")?;
                let snapshot = json!({ "available": true, "brightness": params.brightness,
                    "backlight": params.backlight, "contrast": params.contrast });
                drop(guard);
                // Mirror Api::setScreenParam(): every other client's screen panel
                // follows the slider it did not touch.
                self.broadcast("screen", snapshot.clone());
                Ok(snapshot)
            }
            "ddc.state" => Ok(self.ddc.snapshot()),
            "ddc.get" => {
                if !self.ddc.ready() {
                    bail!("no panel on DDC yet");
                }
                let code = request.params.get("code").and_then(Value::as_i64)
                    .context("Missing integer 'code'")?;
                if !(0..=255).contains(&code) {
                    bail!("'code' must be 0..255");
                }
                self.ddc.get_vcp(code as u8);
                Ok(json!({ "queued": true }))
            }
            "ddc.set" => {
                if !self.ddc.ready() {
                    bail!("no panel on DDC yet");
                }
                let code = request.params.get("code").and_then(Value::as_i64)
                    .context("Missing integer 'code'")?;
                let value = request.params.get("value").and_then(Value::as_i64)
                    .context("Missing integer 'value'")?;
                if !(0..=255).contains(&code) {
                    bail!("'code' must be 0..255");
                }
                if !(0..=65535).contains(&value) {
                    bail!("'value' out of range");
                }
                // Refuse a write the panel never advertised rather than letting
                // ddcutil fail slowly for every slider drag.
                if !self.ddc.caps_empty() && !self.ddc.has(code as u8) {
                    bail!("this panel does not advertise VCP 0x{:02X}", code);
                }
                // Refuse an unsafe write (input source / power) unless the
                // caller supplied an explicit confirmation, reusing the A0 gate.
                let confirmation = request.params.get("reason").and_then(Value::as_str)
                    .map(|r| hid::Confirmation::approve(r));
                ddc::guard_unsafe_write(code as u8, confirmation.as_ref())?;
                self.ddc.set_vcp(code as u8, value as u16);
                Ok(json!({ "queued": true }))
            }
            "ddc.restoreDefaults" => {
                if !self.ddc.ready() {
                    bail!("no panel on DDC yet");
                }
                let scope = request.params.get("scope").and_then(Value::as_str)
                    .unwrap_or("factory");
                let code: u8 = match scope {
                    "factory" => 0x04,
                    "brightness" => 0x05,
                    "color" => 0x08,
                    _ => bail!("scope must be factory, brightness or color"),
                };
                if !self.ddc.caps_empty() && !self.ddc.has(code) {
                    bail!("this panel cannot restore that group");
                }
                self.ddc.set_vcp(code, 1);
                // The panel moves several values at once, so re-read rather than
                // guessing what it landed on.
                for picture in [0x10u8, 0x12, 0x14, 0x16, 0x18, 0x1A, 0x87] {
                    self.ddc.get_vcp(picture);
                }
                Ok(json!({ "scope": scope }))
            }
            "touch.state" => {
                let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                Ok(guard.state())
            }
            "touch.setMode" => {
                let mode = parameter("mode")?;
                let result = {
                    let mut guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                    guard.set_mode(mode)
                };
                if result.is_ok() {
                    self.broadcast_touch();
                }
                result
            }
            "touch.probe" => {
                let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                Ok(guard.probe())
            }
            "touch.stream" => {
                let on = request.params.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let mut guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                Ok(guard.set_streaming(on))
            }
            "touch.applyOutputMapping" => {
                let result = {
                    let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                    guard.apply_output_mapping()
                };
                if result.is_ok() {
                    self.broadcast_touch();
                }
                result
            }
            "touch.setMatrix" => {
                let values = request
                    .params
                    .get("matrix")
                    .and_then(Value::as_array)
                    .context("'matrix' must have 9 numbers")?;
                let matrix: Vec<f64> = values.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
                let result = {
                    let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                    guard.set_matrix(&matrix)
                };
                if result.is_ok() {
                    self.broadcast_touch();
                }
                result
            }
            "touch.calibrate" => {
                let result = {
                    let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                    guard.calibrate(&request.params)
                };
                if result.is_ok() {
                    self.broadcast_touch();
                }
                result
            }
            "touch.config" => {
                let guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                Ok(guard.config_snapshot())
            }
            "touch.setConfig" => {
                let result = {
                    let mut guard = self.touch.lock().unwrap_or_else(|p| p.into_inner());
                    guard.set_config(&request.params)
                }?;
                // Mirror Api::touch.setConfig: the saved config is pushed back so
                // every open settings pane shows what was actually stored.
                self.broadcast("touchConfig", result.clone());
                Ok(result)
            }
            "controller.ensure" => {
                self.kwin.ensure().await?;
                Ok(json!({"ok":true}))
            }
            "window.start" => {
                self.kwin.ensure_feed().await?;
                Ok(json!({"ok":true}))
            }
            "window.snapshot" => Ok(serde_json::to_value(self.kwin.snapshot().await?)?),
            "window.activate" => self.kwin.activate(parameter("id")?).await,
            "gestures.config" => {
                // Read-only gesture config snapshot. Gestures are disabled by
                // default (flag-gated) and this never enables them; the
                // recognizer is a pure state machine and does not grab the
                // digitizer or ask for any portal input permission.
                Ok(gestures::config_snapshot())
            }
            "launchpad.action" => {
                let name = parameter("action")?;
                match name {
                    "artwork" => artwork(),
                    "vanguard" | "steam" => {
                        let uri = if name == "vanguard" {
                            "steam://rungameid/3471800"
                        } else {
                            "steam://open/library"
                        };
                        let mut child = tokio::process::Command::new("xdg-open")
                            .arg(uri)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()?;
                        tokio::spawn(async move {
                            let _ = child.wait().await;
                        });
                        Ok(json!({"message":"Launch request sent to Steam"}))
                    }
                    // The dashboard lives in the UI process, so these are relayed
                    // to it instead of executed here — the same split the C++
                    // agent makes (Api::runGestureAction: "the agent owns
                    // devices, not windows"). Relaying before KWin is touched
                    // keeps an action this process cannot perform from loading
                    // the controller script.
                    _ if name.starts_with("dashboard") => {
                        self.broadcast("ui.action", json!({ "action": name }));
                        Ok(json!({ "relayed": true,
                            "message": "Dashboard action sent to the interface" }))
                    }
                    // The window vocabulary is listed here so an unknown name is
                    // rejected without loading anything into KWin.
                    "fit" | "fit-vanguard" | "restore" | "panel" | "pick" => {
                        self.kwin.action(name).await
                    }
                    _ => bail!("Unknown launchpad action"),
                }
            }
            "sensors.stream" => {
                let enabled = request
                    .params
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                // `intervalMs` is optional so a bare disable keeps the rate the
                // client chose before; only enabling without one uses the default.
                let interval_ms = request
                    .params
                    .get("intervalMs")
                    .and_then(Value::as_u64)
                    .map(|ms| ms.min(u32::MAX as u64) as u32)
                    .or(if enabled { Some(1000) } else { None });
                {
                    let mut guard =
                        self.sensors.lock().unwrap_or_else(|p| p.into_inner());
                    guard.set_streaming(enabled, interval_ms);
                }
                // Wake the worker so the new interval (or the disable) takes
                // effect now, not after the current sleep expires.
                self.streaming_wake.notify_one();
                Ok(json!({ "streaming": enabled }))
            }
            "sensors.interfaces" => Ok(sensors::interfaces_json(
                &self.sensors.lock().unwrap_or_else(|p| p.into_inner()),
            )),
            "sensors.selectInterface" => {
                let name = request
                    .params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !name.is_empty() {
                    let found = sensors::enumerate_interfaces()
                        .iter()
                        .any(|n| n.name == name);
                    if !found {
                        bail!("no interface named {}", name);
                    }
                }
                let mut guard =
                    self.sensors.lock().unwrap_or_else(|p| p.into_inner());
                guard.set_net_selection(name.to_string());
                Ok(sensors::interfaces_json(&guard))
            }
            "sensors.gpus" => {
                let mut guard =
                    self.sensors.lock().unwrap_or_else(|p| p.into_inner());
                let gpus = guard.enumerate_gpus();
                let arr: Vec<Value> = gpus.iter().map(sensors::gpu_json).collect();
                let selected = guard.gpu_selection().to_vec();
                Ok(json!({ "gpus": arr, "selected": selected }))
            }
            "sensors.selectGpus" => {
                let v = request
                    .params
                    .get("ids")
                    .context("'ids' must be an array")?;
                let ids = v.as_array().context("'ids' must be an array")?;
                let asked: Vec<String> = ids
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .filter(|s| !s.is_empty())
                    .collect();
                // Scoped so the non-Send guard is gone before the await below;
                // `drop(guard)` is not enough for the generator's Send analysis.
                let (arr, keep) = {
                    let mut guard =
                        self.sensors.lock().unwrap_or_else(|p| p.into_inner());
                    let all = guard.enumerate_gpus();
                    let arr: Vec<Value> = all.iter().map(sensors::gpu_json).collect();
                    let keep: Vec<String> = all
                        .iter()
                        .filter(|g| asked.contains(&g.id))
                        .map(|g| g.id.clone())
                        .collect();
                    if !asked.is_empty() && keep.is_empty() {
                        bail!("none of those GPUs are in this machine");
                    }
                    guard.set_gpu_selection(keep.clone());
                    (arr, keep)
                };
                // Push the change straight out so the panel redraws on the click
                // rather than on the next poll (Api.cpp:1468).
                self.broadcast_sensors_if_streaming().await;
                Ok(json!({ "gpus": arr, "selected": keep }))
            }
            "media.control" => {
                self.media
                    .control(parameter("action")?)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({ "ok": true }))
            }
            "notifications.clear" => {
                self.media.clear_notifications();
                // The badge is drawn from the sensors snapshot, so a clear has to
                // be pushed the way Api::notifications.clear does.
                self.broadcast_sensors_if_streaming().await;
                Ok(json!({ "ok": true }))
            }
            "media.snapshot" => Ok(self.media.snapshot().await),
            "settings.set" => {
                // Autostart is owned by the interface; the agent is headless so
                // "start on login" must not point at it, exactly as the C++ agent.
                bail!("nothing here to set; autostart is owned by the interface")
            }
            "profiles.list" => {
                let mut arr = Vec::new();
                for name in self.profiles.list() {
                    let body = self.profiles.load(&name);
                    arr.push(json!({"name": name, "summary": self.profiles.summary(&body)}));
                }
                Ok(json!({"profiles": arr, "active": self.profiles.active(),
                    "directory": self.profiles.directory().display().to_string()}))
            }
            "profiles.get" => {
                let name = parameter("name")?;
                if !self.profiles.exists(name) {
                    bail!("no profile called '{name}'");
                }
                Ok(self.profiles.load(name))
            }
            "profiles.save" => {
                let name = parameter("name")?;
                let overwrite = request
                    .params
                    .get("overwrite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !overwrite && self.profiles.exists(name) {
                    bail!("'{name}' already exists");
                }
                // Refuse a partial snapshot: the HID read must report the
                // panel's current picture values before we write them down.
                let body = self.capture_profile_body()?;
                let saved = self.profiles.save(name, &body)?;
                self.profiles.set_active(name);
                // Mirror Api::profiles.save: every other client reloads its list.
                self.broadcast("profiles", json!({ "changed": true }));
                Ok(json!({"name": saved}))
            }
            "profiles.delete" => {
                let name = parameter("name")?;
                self.profiles.remove(name)?;
                self.broadcast("profiles", json!({ "changed": true }));
                Ok(json!({"deleted": true}))
            }
            "profiles.rename" => {
                let from = parameter("from")?;
                let to = parameter("to")?;
                self.profiles.rename(from, to)?;
                self.broadcast("profiles", json!({ "changed": true }));
                Ok(json!({"renamed": true}))
            }
            "profiles.apply" => {
                let name = parameter("name")?;
                let result = self.apply_profile(name)?;
                // Api::profiles.apply broadcasts the newly active profile even
                // when some values were skipped, because that is still the move
                // the panel has to reflect.
                self.broadcast("profiles", json!({ "active": name }));
                Ok(result)
            }
            "rules.get" => Ok(self.rules_snapshot()),
            "rules.set" => {
                // Validate against what was sent, not against what fromJson kept:
                // fromJson silently drops a rule with no pattern, so such a rule
                // would disappear with the save reporting success.
                if let Some(arr) = request.params.get("rules").and_then(Value::as_array) {
                    for (i, value) in arr.iter().enumerate() {
                        let o = value
                            .as_object()
                            .map(|m| Value::Object(m.clone()))
                            .unwrap_or_else(|| json!({}));
                        let pattern = o.get("pattern").and_then(Value::as_str).unwrap_or("");
                        let profile = o.get("profile").and_then(Value::as_str).unwrap_or("");
                        if pattern.trim().is_empty() {
                            bail!("rule {} has nothing to match on", i + 1);
                        }
                        if profile.trim().is_empty() {
                            bail!("rule {} does not say which profile to apply", i + 1);
                        }
                    }
                }
                let cfg = profiles::rules_from_json(&request.params);
                // Refuse to persist a rule pointing at a profile that is not there.
                for rule in &cfg.rules {
                    if !rule.profile.is_empty() && !self.profiles.exists(&rule.profile) {
                        bail!("no profile called '{}'", rule.profile);
                    }
                }
                if !cfg.fallback_profile.is_empty() && !self.profiles.exists(&cfg.fallback_profile) {
                    bail!("no profile called '{}'", cfg.fallback_profile);
                }
                let snapshot = profiles::rules_to_json(&cfg);
                self.rules.save(&snapshot)?;
                // Mirror Api::rules.set: push the stored rules out so the other
                // open panes see the save without a reload.
                self.broadcast("rules", self.rules_snapshot());
                Ok(snapshot)
            }
            "color.state" => {
                let snapshot = tokio::task::spawn_blocking(color_snapshot)
                    .await
                    .context("colormgr probe panicked")?;
                Ok(snapshot)
            }
            "color.setProfile" => {
                let device_id = parameter("deviceId")?.to_string();
                let profile_id = parameter("profileId")?.to_string();
                if device_id.is_empty() || profile_id.is_empty() {
                    bail!("both 'deviceId' and 'profileId' are required");
                }
                let inv = profiles::color::ColormgrInvocation::make_default(&device_id, &profile_id);
                let result = tokio::task::spawn_blocking(move || profiles::color::invoke_colormgr(&inv))
                    .await
                    .context("colormgr probe panicked")?;
                if result.is_none() {
                    bail!("colormgr did not respond or refused the profile");
                }
                let snapshot = tokio::task::spawn_blocking(color_snapshot)
                    .await
                    .context("colormgr probe panicked")?;
                Ok(snapshot)
            }
            "updates.check" => {
                // The C++ agent defaults `manual` to true so a hand-clicked
                // check is never throttled or disabled away; preserve that.
                let manual = request
                    .params
                    .get("manual")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let outcome = match self.updates.check(manual).await {
                    updates::Outcome::Available { version, url } => {
                        json!({ "state": "available", "version": version, "url": url })
                    }
                    updates::Outcome::Current { version } => {
                        json!({ "state": "current", "version": version })
                    }
                    updates::Outcome::Failed { error } => {
                        json!({ "state": "failed", "error": error })
                    }
                    updates::Outcome::Quiet => json!({ "state": "quiet" }),
                };
                // Api forwards the UpdateChecker signals as an unsolicited
                // `update` event; `quiet` (throttled or disabled) stays local.
                if outcome.get("state").and_then(Value::as_str) != Some("quiet") {
                    self.broadcast("update", outcome.clone());
                }
                Ok(outcome)
            }
            "updates.settings" => {
                if let Some(enabled) = request.params.get("enabled").and_then(Value::as_bool) {
                    self.updates.set_enabled(enabled);
                }
                if let Some(skip) = request.params.get("skipVersion").and_then(Value::as_str) {
                    self.updates.set_skipped_version(skip);
                }
                Ok(json!({ "enabled": self.updates.enabled(),
                           "skippedVersion": self.updates.skipped_version() }))
            }
            _ => bail!("Unsupported Rust backend method: {}", request.method),
        }
    }

    // The current panel picture values, as the profile stores them. The A0 HID
    // driver reads brightness/contrast/RGB gain but not the panel preset (0x14)
    // or the C++ 0x87 code, so the snapshot is partial; touch mode and the input
    // calibration matrix are not surfaced by the Rust backend either.
    fn capture_profile_body(&self) -> Result<Value> {
        let mut guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            match hid::Device::open() {
                Ok(device) => *guard = Some(device),
                Err(error) => bail!(
                    "no panel on DDC, so there are no picture values to save: {error}"
                ),
            }
        }
        let device = guard.as_ref().context("hid device unavailable")?;
        let params = device
            .read_screen_params()
            .context("no panel on DDC, so there are no picture values to save")?;
        let mut vcp = serde_json::Map::new();
        vcp.insert("10".to_string(), json!(params.brightness));
        vcp.insert("12".to_string(), json!(params.contrast));
        vcp.insert("16".to_string(), json!(params.red));
        vcp.insert("18".to_string(), json!(params.green));
        vcp.insert("1a".to_string(), json!(params.blue));
        Ok(json!({
            "vcp": Value::Object(vcp),
            // Identity: the Rust backend does not yet drive a touch controller.
            "matrix": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }))
    }

    // Apply a profile the same path a click takes. Preset (0x14), the 0x87 code
    // and RGB gain are only writable over DDC, which the Rust backend does not
    // drive yet; only brightness/contrast are written over the A0 HID path.
    fn apply_profile(&self, name: &str) -> Result<Value> {
        if !self.profiles.exists(name) {
            bail!("no profile called '{name}'");
        }
        let body = self.profiles.load(name);
        let vcp = body.get("vcp").cloned().unwrap_or_else(|| json!({}));
        let mut written = 0;
        let mut skipped = 0;
        let mut ddc_ready = false;
        {
            let mut guard = self.hid.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_none() {
                *guard = hid::Device::open().ok();
            }
            if let Some(device) = guard.as_ref() {
                ddc_ready = true;
                let params = device.read_screen_params().ok();
                let mut apply = |code: &str, param: hardware::ScreenParam, current: u8| {
                    let Some(want) = vcp.get(code).and_then(Value::as_i64).and_then(|v| u8::try_from(v).ok()) else {
                        skipped += 1;
                        return;
                    };
                    if want == current {
                        skipped += 1;
                        return;
                    }
                    let frame = hardware::set_screen_param(param, want);
                    let ex = device.exchange(&frame, Some(&hid::Confirmation::approve("apply profile")));
                    if ex.ok {
                        written += 1;
                    } else {
                        skipped += 1;
                    }
                };
                if let Some(params) = params {
                    apply("10", hardware::ScreenParam::Brightness, params.brightness);
                    apply("12", hardware::ScreenParam::Contrast, params.contrast);
                }
            }
        }
        self.profiles.set_active(name);
        Ok(json!({
            "name": name,
            "vcpWritten": written,
            "vcpSkipped": skipped,
            "ddcReady": ddc_ready,
            "touchApplied": false,
            "matrixApplied": false,
        }))
    }
}

async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut message = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(if message.is_empty() {
                None
            } else {
                Some(message)
            });
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let length = end.map_or(buffer.len(), |n| n + 1);
        if message.len() + length > MAX_MESSAGE {
            bail!("RPC message exceeds 1 MiB");
        }
        message.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(Some(message));
        }
    }
}

async fn serve(stream: UnixStream, backend: Arc<Backend>) -> Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        bail!("RPC peer is not the current user");
    }
    let (read, write) = stream.into_split();
    let writer = Arc::new(Mutex::new(write));
    // Fan-out channel: subscribe this connection to the per-process event
    // broadcast and relay every `{event,data}` line onto its own socket. The
    // forwarder shares `writer` with the request tasks, so all writes are
    // serialized. It is held in an `AbortOnDrop` because dropping the read half
    // does not close the socket while the forwarder still owns the write half:
    // a plain spawn would park in `recv()` after the client disconnects and leak
    // one fd per finished connection until some event happened to fail on it.
    let _forwarder = AbortOnDrop(tokio::spawn({
        let writer = writer.clone();
        let mut events = backend.broadcast.subscribe();
        async move {
            loop {
                let event = match events.recv().await {
                    Ok(v) => v,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        // Sensors tolerate a gap (a newer snapshot follows); the
                        // mutation events queued for this channel do not, so at
                        // least leave a trace rather than dropping silently.
                        eprintln!("event fan-out lagged, dropped {missed} event(s)");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let mut data = match serde_json::to_vec(&event) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                data.push(b'\n');
                if writer.lock().await.write_all(&data).await.is_err() {
                    break;
                }
            }
        }
    }));
    let limit = Arc::new(Semaphore::new(32));
    let mut reader = BufReader::new(read);
    while let Some(line) = read_message(&mut reader).await? {
        let request: Request = serde_json::from_slice(&line).context("Invalid RPC request")?;
        let permit = limit
            .clone()
            .try_acquire_owned()
            .context("Too many in-flight RPC calls")?;
        let writer = writer.clone();
        let backend = backend.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let duration = if request.method == "launchpad.action"
                && request.params.get("action").and_then(Value::as_str) == Some("pick")
            {
                125
            } else {
                15
            };
            let result =
                tokio::time::timeout(Duration::from_secs(duration), backend.dispatch(&request))
                    .await;
            let response = match result {
                Ok(Ok(result)) => json!({"id":request.id,"result":result}),
                Ok(Err(error)) => json!({"id":request.id,"error":{"message":error.to_string()}}),
                Err(_) => {
                    json!({"id":request.id,"error":{"message":"Rust backend request timed out"}})
                }
            };
            let mut data = serde_json::to_vec(&response).expect("JSON value serializes");
            data.push(b'\n');
            let _ = writer.lock().await.write_all(&data).await;
            if request.method == "backend.stop" {
                let _ = backend.shutdown.send(true);
            }
        });
    }
    Ok(())
}

/// Aborts its task when dropped. Used for a connection's event forwarder so
/// every `serve()` exit path tears it down, including `?` returns.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Poll the sensor source on the blocking pool and return its raw snapshot.
/// `SensorSource::poll` shells out to `nvidia-smi`, `systemctl` and `lspci`, so
/// running it on a tokio worker would block that thread for the duration of the
/// spawns (tens of ms, seconds under GPU/systemd pressure) while every other
/// `sensors.*` arm queues on the same mutex.
///
/// `None` means the poll itself failed (the blocking task panicked or was
/// cancelled). The caller must not broadcast that: clients replace
/// `state.sensors` wholesale, so an error object would blank every tile.
async fn poll_sensors(backend: &Arc<Backend>) -> Option<Value> {
    let inner = backend.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = inner.sensors.lock().unwrap_or_else(|p| p.into_inner());
        guard.poll();
        sensors::snapshot_json(guard.latest())
    })
    .await
    .map_err(|error| eprintln!("sensor poll failed: {error}"))
    .ok()
}

/// Merge the live media now-playing/notifications into a sensors snapshot, as
/// `SensorSource::snapshot` does in the C++ agent. `snapshot_json` hardcodes
/// placeholders for these keys, so every caller that hands the snapshot to the
/// UI — `state.all` and the `sensors` broadcast alike — has to merge them, or
/// the now-playing tile, the notification badge and the session-bus indicator
/// get blanked.
fn with_media(mut snapshot: Value, media: &Value) -> Value {
    if let Some(obj) = snapshot.as_object_mut() {
        for key in [
            "notifications",
            "notifyActive",
            "notifyError",
            "nowPlaying",
            "sessionBusOk",
        ] {
            if let Some(value) = media.get(key) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
    snapshot
}

fn private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        bail!("Backend directory must be private (0700), owned by this user, and not a symlink");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {

    let mut socket = None;
    let mut parent = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(args.next().context("--socket needs a path")?))
            }
            "--parent-pid" => {
                parent = Some(
                    args.next()
                        .context("--parent-pid needs a PID")?
                        .parse::<u32>()?,
                )
            }
            _ => bail!("Unknown option {arg}"),
        }
    }
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?);
    let directory = runtime.join("edgeline-rust");
    private_directory(&directory)?;
    let socket = socket.unwrap_or_else(|| directory.join("backend.sock"));
    private_directory(socket.parent().context("Socket needs a parent directory")?)?;
    if socket.symlink_metadata().is_ok() {
        let metadata = std::fs::symlink_metadata(&socket)?;
        if !metadata.file_type().is_socket() || metadata.uid() != uid() {
            bail!("Refusing to replace a non-owned socket path");
        }
        if UnixStream::connect(&socket).await.is_ok() {
            bail!("A Rust backend is already running at this socket");
        }
        std::fs::remove_file(&socket)?;
    }
    let scripts = directory.join(format!("scripts-{}", std::process::id()));
    private_directory(&scripts)?;
    let config_root = config_root()?;
    let profiles = profiles::ProfileStore::new(
        config_root.join("profiles"),
        config_root.join("settings.json"),
    );
    let rules = profiles::RulesStore::new(config_root.join("rules.json"));
    let (shutdown, mut signal) = watch::channel(false);
    // A1: DDC/CI via ddcutil. The client runs one ddcutil child at a time; a
    // background task drains the job queue after each enqueue. On bus discovery
    // it fetches capabilities and seeds the standard reads so a UI connecting
    // later has real values instead of placeholders.
    let ddc = Arc::new(ddc::Ddc::new(ddc::DdcConfig::default()));
    // Created before the backend so the DDC worker can announce its finished
    // jobs (`ddc`/`ddcLog`) on the same fan-out channel the RPC layer uses.
    let (broadcast_tx, broadcast_rx) = broadcast::channel(256);
    ddc.set_event_sink(broadcast_tx.clone());
    ddc.start();
    let ddc_worker = ddc.clone();
    tokio::spawn(async move { ddc_worker.run().await; });
    let updates_settings = updates::UpdateChecker::config_dir(&home()?);
    let backend = Arc::new(Backend {
        kwin: kwin::Kwin::new(&scripts).await?,
        hid: std::sync::Mutex::new(None),
        ddc,
        touch: std::sync::Mutex::new(touch::Touch::new(touch::config::touch_path())),
        sensors: std::sync::Mutex::new(sensors::SensorSource::new()),
        media: media::Media::new().await,
        updates: updates::UpdateChecker::new(updates_settings),
        shutdown,
        profiles,
        rules,
        socket_path: socket.clone(),
        broadcast: broadcast_tx,
        streaming_wake: Notify::new(),
    });
    // The initial receiver is not needed: `broadcast` closes only when the
    // Sender is dropped, and later `subscribe()` calls work with zero
    // receivers. It is dropped here rather than parked, so the sender's `send`
    // result reflects real subscribers.
    drop(broadcast_rx);
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    println!("{}", json!({"event":"ready","socket":socket}));
    let parent_shutdown = backend.shutdown.clone();
    if let Some(expected) = parent {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                // getppid is a value-only libc call with no pointer preconditions.
                if unsafe { libc::getppid() } as u32 != expected {
                    let _ = parent_shutdown.send(true);
                    break;
                }
            }
        });
    }
    // Sensor streaming broadcast: while `sensors.stream` has been enabled, poll
    // the source at its requested interval and push the snapshot to every
    // connected client. This is the only ticked broadcast; the snapshot events
    // are otherwise emitted by their dispatch arms after a mutation. The
    // interval sleep races `streaming_wake`, so enabling, disabling or changing
    // the interval takes effect immediately instead of after up to one interval.
    {
        let backend = backend.clone();
        tokio::spawn(async move {
            loop {
                let (enabled, interval_ms) = {
                    let guard = backend.sensors.lock().unwrap_or_else(|p| p.into_inner());
                    (guard.streaming(), guard.streaming_interval())
                };
                if !enabled {
                    backend.streaming_wake.notified().await;
                    continue;
                }
                // The poll shells out (nvidia-smi, systemctl, lspci); keep it off
                // the runtime worker so other RPC arms are not stalled by it.
                let Some(snapshot) = poll_sensors(&backend).await else {
                    // Never broadcast the failure (it would blank the dashboard);
                    // back off a second so a stuck poll cannot spin this loop.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                };
                // Media is merged in here, exactly as `state.all` does: the
                // dashboard replaces `state.sensors` wholesale on each event,
                // so the bare snapshot's placeholder now-playing/notifications
                // would blank those tiles every tick.
                let media = backend.media.snapshot().await;
                backend.broadcast("sensors", with_media(snapshot, &media));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(interval_ms as u64)) => {}
                    _ = backend.streaming_wake.notified() => {}
                }
            }
        });
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            _ = signal.changed() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let backend = backend.clone();
                tokio::spawn(async move { if let Err(error) = serve(stream, backend).await { eprintln!("RPC connection: {error}"); } });
            }
        }
    }
    backend.kwin.close().await;
    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_dir_all(scripts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }
    #[tokio::test]
    async fn rpc_reads_separate_lines_and_caps_size() {
        let mut input = BufReader::new(&b"one\ntwo\n"[..]);
        assert_eq!(read_message(&mut input).await.unwrap().unwrap(), b"one\n");
        assert_eq!(read_message(&mut input).await.unwrap().unwrap(), b"two\n");
        assert!(read_message(&mut input).await.unwrap().is_none());
        let oversized = vec![b'x'; MAX_MESSAGE + 1];
        assert!(
            read_message(&mut BufReader::new(oversized.as_slice()))
                .await
                .is_err()
        );
    }
}
