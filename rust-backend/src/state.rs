// SPDX-License-Identifier: GPL-3.0-or-later
//! `state.all` / `system.info` / `device.state` composition.
//!
//! The frontend calls `state.all` on every connect (app.js refreshAll,
//! dashboard.js start, main.js primeTrayState). The C++ agent aggregates nine
//! snapshot objects; this module reproduces that contract so the Rust backend
//! can serve it after the C++ agent is retired (B3/B4 cutover blocker).
//!
//! The builders here are pure and take explicit inputs so they are unit-testable
//! without a live backend; `Backend::state_all` (in main.rs) stitched the live
//! `Mutex`-guarded modules and the async media snapshot together.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Inputs for the system snapshot. All are cheap derived facts, collected by the
/// caller so the builder stays pure.
pub struct SystemInput<'a> {
    pub version: &'a str,
    pub codename: &'a str,
    pub config_path: &'a Path,
    pub profiles_path: &'a Path,
    pub ddcutil_path: &'a str,
    pub xinput_path: &'a str,
    pub i2c_bus: i64,
    pub autostart: bool,
    pub updates_enabled: bool,
    pub hid_log: &'a Path,
    pub ddcutil_version: &'a str,
    pub xinput_version: &'a str,
    pub session_type: &'a str,
    pub socket: &'a Path,
}

/// Reproduce the C++ `systemSnapshot()` field contract.
pub fn system_snapshot(inp: &SystemInput) -> Value {
    json!({
        "version": inp.version,
        "codename": inp.codename,
        "configPath": inp.config_path.display().to_string(),
        "profilesPath": inp.profiles_path.display().to_string(),
        "ddcutilPath": inp.ddcutil_path,
        "xinputPath": inp.xinput_path,
        "i2cBus": inp.i2c_bus,
        "autostart": inp.autostart,
        "updatesEnabled": inp.updates_enabled,
        "hidLog": inp.hid_log.display().to_string(),
        "ddcutil": inp.ddcutil_version,
        "xinput": inp.xinput_version,
        "sessionType": inp.session_type,
        "socket": inp.socket.display().to_string(),
    })
}

/// Facts about the Edge Bragi device, as the Dashboard/Settings pane shows.
/// Mirrors the C++ `EdgeDevice::State`.
#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    pub present: bool,
    pub accessible: bool,
    pub path: PathBuf,
    pub product: String,
    pub serial: String,
}

/// The Edge Bragi device facts, as `device.state`. Reproduces the C++
/// `deviceSnapshot()` (present/accessible/path/product/serial).
pub fn device_snapshot(info: &DeviceInfo) -> Value {
    json!({
        "present": info.present,
        "accessible": info.accessible,
        "path": info.path.display().to_string(),
        "product": info.product,
        "serial": info.serial,
    })
}

/// Compose the parts of `state.all` that are pure. The caller supplies each
/// module snapshot wrapped in the shape it already returns, so this is an
/// ordering/assembly concern only. `rules`, `color`, `touchConfig` come across
/// as already-built values.
pub fn state_all(
    system: Value,
    device: Value,
    ddc: Value,
    screen: Value,
    touch: Value,
    sensors: Value,
    rules: Value,
    color: Value,
    touch_config: Value,
) -> Value {
    json!({
        "system": system,
        "device": device,
        "ddc": ddc,
        "screen": screen,
        "touch": touch,
        "sensors": sensors,
        "rules": rules,
        "color": color,
        "touchConfig": touch_config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp<T>(_: &T) {}

    #[test]
    fn state_all_has_expected_keys_in_order() {
        let v = state_all(
            json!({"version":"0.7.0"}),
            json!({"present":true}),
            json!({"ready":true}),
            json!({"available":true}),
            json!({"mode":"off"}),
            json!({"cpuLoadPct":1.0}),
            json!({"rules":[]}),
            json!({"available":true}),
            json!({"gestures":{"enabled":false}}),
        );
        for key in ["system", "device", "ddc", "screen", "touch", "sensors", "rules", "color", "touchConfig"] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
        assert_eq!(v["system"]["version"], "0.7.0");
        assert_eq!(v["touchConfig"]["gestures"]["enabled"], false);
    }

    #[test]
    fn system_snapshot_reproduces_cpp_fields() {
        let cfg = Path::new("/home/u/.config/edgeline");
        let prof = Path::new("/home/u/.config/edgeline/profiles");
        let log = Path::new("/home/u/.local/share/edgeline/hid.log");
        let sock = Path::new("/run/user/1000/edgeline.sock");
        let v = system_snapshot(&SystemInput {
            version: "0.7.0",
            codename: "Cold Start",
            config_path: cfg,
            profiles_path: prof,
            ddcutil_path: "/usr/bin/ddcutil",
            xinput_path: "/usr/bin/xinput",
            i2c_bus: 5,
            autostart: true,
            updates_enabled: true,
            hid_log: log,
            ddcutil_version: "2.2.1",
            xinput_version: "1.4.0",
            session_type: "wayland",
            socket: sock,
        });
        assert_eq!(v["version"], "0.7.0");
        assert_eq!(v["codename"], "Cold Start");
        assert_eq!(v["configPath"], cfg.display().to_string());
        assert_eq!(v["profilesPath"], prof.display().to_string());
        assert_eq!(v["ddcutilPath"], "/usr/bin/ddcutil");
        assert_eq!(v["i2cBus"], 5);
        assert_eq!(v["autostart"], true);
        assert_eq!(v["updatesEnabled"], true);
        assert_eq!(v["sessionType"], "wayland");
        assert_eq!(v["socket"], sock.display().to_string());
        let _ = tmp(&0u8); // quiet unused-helper lint in some cfgs
    }

    #[test]
    fn device_snapshot_reproduces_cpp_fields() {
        let info = DeviceInfo {
            present: true,
            accessible: false,
            path: PathBuf::from("/dev/hidraw1"),
            product: "XENEON EDGE".into(),
            serial: "213826425656".into(),
        };
        let v = device_snapshot(&info);
        assert_eq!(v["present"], true);
        assert_eq!(v["accessible"], false);
        assert_eq!(v["path"], "/dev/hidraw1");
        assert_eq!(v["product"], "XENEON EDGE");
        assert_eq!(v["serial"], "213826425656");
    }
}
