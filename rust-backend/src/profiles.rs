// SPDX-License-Identifier: GPL-3.0-or-later
//! A5: the panel-control subsystem, ported from the C++ agent.
//!
//! This is the persistence and decision logic behind the profiles page, the
//! per-app focus rules, the app-wide settings, and the colour (ICC) page.
//! Everything here is deliberately free of X11/KWin/panel hardware so it can
//! be tested offline with injectable paths and a stand-in colormgr, the same
//! way the C++ [`agent/tests`] drives [`xen::profiles`] etc.
//!
//! The concrete directory layout mirrors the C++ agent, which keeps a single
//! `~/.config/edgeline/` root:
//!   * `<root>/profiles/*.json`        named snapshots of panel picture values
//!   * `<root>/rules.json`             per-app focus rules
//!   * `<root>/settings.json`          touch/DDC/dashboard settings + active profile
//!   * `~/.config/autostart/edgeline.desktop`  the login autostart entry
//!
//! Two things a profile deliberately does not store, matching the C++ design:
//! panel input source (0x60) and panel power (0xD6), because restoring them
//! can leave the user looking at a black panel with no obvious way back.
#![allow(dead_code)]
// This module mirrors the C++ `xen::profiles`/`xen::rules`/`xen::settings`
// surface. The persistence, parsing and rule-evaluation functions are all
// exercised by the fixture-parity tests below; several are not yet wired into
// the Rust dispatch (the focus watcher and the settings page are future work),
// so dead-code analysis is deliberately relaxed for the ported API.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

// ---------------------------------------------------------------------------
// Small file helpers shared by the stores below.
// ---------------------------------------------------------------------------

/// Write `content` to `path` atomically, mirroring Qt's `QSaveFile`: the bytes
/// go to a same-directory temp file which is renamed over the target, so an
/// interrupted write can never truncate an existing file to nothing.
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("edgeline");
    let tmp = parent.join(format!(".{name}.tmp{}", std::process::id()));
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Read the whole JSON object stored at `path`, or `{}` when the file is
/// missing, unreadable, or not an object (a corrupt config costs one key, not
/// the whole file).
fn read_config_file(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|v: &Value| v.is_object())
        .unwrap_or_else(|| json!({}))
}

fn write_config_file(path: &Path, root: &Value) -> Result<()> {
    atomic_write(path, &serde_json::to_string_pretty(root)?)
}

/// Fetch a nested value at a `/`-separated path, e.g. `profiles/active`.
fn config_get<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('/') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Set a nested value at a `/`-separated path, creating intermediate objects.
fn config_set(root: &mut Value, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('/').collect();
    let (last, prefix) = parts.split_last().expect("non-empty path");
    let mut cur = root;
    for seg in prefix {
        if !cur.get(*seg).map(|v| v.is_object()).unwrap_or(false) {
            cur[*seg] = json!({});
        }
        cur = &mut cur[*seg];
    }
    cur[*last] = value;
}

/// Remove a nested value at a `/`-separated path. Missing intermediates are a
/// no-op, matching Qt's `QSettings::remove` for a key that is not there.
fn config_remove(root: &mut Value, path: &str) {
    let parts: Vec<&str> = path.split('/').collect();
    let (last, prefix) = parts.split_last().expect("non-empty path");
    let mut cur = root;
    for seg in prefix {
        match cur.get_mut(*seg) {
            Some(v @ Value::Object(_)) => cur = v,
            _ => return,
        }
    }
    if let Value::Object(map) = cur {
        map.remove(*last);
    }
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

/// A filesystem-safe name derived from a display name.
///
/// Two profiles whose names differ only by punctuation would collide, so
/// callers must check [`ProfileStore::exists`] before saving.
pub fn slug_for(name: &str) -> String {
    // Replace each run of characters outside the safe set with a single '-',
    // exactly as the C++ `QRegularExpression` + `replace` did.
    let mut out = String::with_capacity(name.len());
    let mut prev_unsafe = false;
    for c in name.trim().chars() {
        let keep = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if keep {
            out.push(c);
            prev_unsafe = false;
        } else if !prev_unsafe {
            out.push('-');
            prev_unsafe = true;
        }
    }
    // Leading dots would hide the file; leading dashes confuse command lines.
    while out.starts_with('.') || out.starts_with('-') {
        out.remove(0);
    }
    if out.is_empty() {
        out.push_str("profile");
    }
    out.chars().take(64).collect()
}

/// Named snapshots of everything the panel is set to, one JSON file per profile.
///
/// Files rather than one blob so a profile can be copied between machines, kept
/// in a dotfiles repo, or hand-edited, and so a corrupt profile costs you one
/// profile instead of all of them.
pub struct ProfileStore {
    profiles_dir: PathBuf,
    config_file: PathBuf,
}

impl ProfileStore {
    /// `profiles_dir` is `<root>/profiles`; `config_file` holds the active
    /// profile key alongside the app settings (the C++ agent keeps both in the
    /// same QSettings file).
    pub fn new(profiles_dir: PathBuf, config_file: PathBuf) -> Self {
        Self {
            profiles_dir,
            config_file,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.profiles_dir
    }

    fn file_for(&self, name: &str) -> PathBuf {
        self.profiles_dir.join(format!("{}.json", slug_for(name)))
    }

    /// Display names, sorted case-insensitively.
    pub fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.profiles_dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(object) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            // A file whose contents do not name a profile is not one; silently
            // inventing a name from the filename would hide the corruption.
            if let Some(name) = object.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        out
    }

    pub fn exists(&self, name: &str) -> bool {
        self.file_for(name).exists()
    }

    /// Empty object when missing or unreadable.
    pub fn load(&self, name: &str) -> Value {
        match fs::read_to_string(self.file_for(name)) {
            Ok(content) => match serde_json::from_str::<Value>(&content) {
                Ok(object @ Value::Object(_)) => object,
                _ => json!({}),
            },
            Err(_) => json!({}),
        }
    }

    /// Persist `body` under `name`, stamping the trimmed name in. Refuses an
    /// empty name.
    pub fn save(&self, name: &str, body: &Value) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            bail!("a profile needs a name");
        }
        let mut out = match body {
            Value::Object(map) => Value::Object(map.clone()),
            _ => json!({}),
        };
        if let Value::Object(map) = &mut out {
            map.insert("name".to_string(), json!(name));
        }
        atomic_write(&self.file_for(name), &serde_json::to_string_pretty(&out)?)?;
        Ok(name.to_string())
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.file_for(name);
        if !path.exists() {
            bail!("no profile called '{name}'");
        }
        fs::remove_file(&path)?;
        if self.active() == name {
            self.set_active("");
        }
        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        if !self.exists(from) {
            bail!("no profile called '{from}'");
        }
        let to = to.trim();
        if to.is_empty() {
            bail!("a profile needs a name");
        }
        if slug_for(from) != slug_for(to) && self.exists(to) {
            bail!("'{to}' already exists");
        }
        let body = self.load(from);
        self.save(to, &body)?;
        if slug_for(from) != slug_for(to) {
            let _ = fs::remove_file(self.file_for(from));
        }
        if self.active() == from {
            self.set_active(to);
        }
        Ok(())
    }

    fn read_config(&self) -> Value {
        read_config_file(&self.config_file)
    }

    /// The profile applied at startup and shown in the titlebar.
    pub fn active(&self) -> String {
        config_get(&self.read_config(), "profiles/active")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    pub fn set_active(&self, name: &str) {
        let mut root = self.read_config();
        config_set(&mut root, "profiles/active", json!(name));
        let _ = write_config_file(&self.config_file, &root);
    }

    /// The one-line summary the profile list shows: brightness, preset, touch mode.
    pub fn summary(&self, body: &Value) -> String {
        let vcp = body.get("vcp").cloned().unwrap_or_else(|| json!({}));
        let mut bits: Vec<String> = Vec::new();
        if let Some(v) = vcp.get("10").and_then(Value::as_i64) {
            bits.push(v.to_string());
        }
        if let Some(v) = vcp.get("14").and_then(Value::as_i64) {
            bits.push(format!("preset 0x{v:02x}"));
        }
        if let Some(mode) = body.get("touchMode").and_then(Value::as_str) {
            if !mode.is_empty() {
                bits.push(mode.to_string());
            }
        }
        bits.join(" · ")
    }
}

// ---------------------------------------------------------------------------
// Rules: match the focused window against per-app rules.
// ---------------------------------------------------------------------------

/// What a rule looks at. The design names exactly these two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchOn {
    WmClass,
    WindowState,
}

impl MatchOn {
    pub fn name(&self) -> &'static str {
        match self {
            Self::WindowState => "window state",
            Self::WmClass => "WM_CLASS",
        }
    }

    /// Default is WM_CLASS; only the exact (case-insensitive) "window state"
    /// is the state match, so nonsense names degrade safely.
    pub fn from_name(name: &str) -> Self {
        if name.to_lowercase() == "window state" {
            Self::WindowState
        } else {
            Self::WmClass
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppRule {
    pub app: String,
    pub pattern: String,
    pub match_on: MatchOn,
    pub profile: String,
    pub enabled: bool,
}

/// A description of the focused window; `valid` is false when nothing is focused.
#[derive(Clone, Debug, Default)]
pub struct WindowInfo {
    pub wm_instance: String,
    pub wm_class: String,
    pub title: String,
    pub states: Vec<String>,
    pub valid: bool,
}

/// The outcome of evaluating the rules against a window.
#[derive(Clone, Debug, Default)]
pub struct RuleMatch {
    pub matched: bool,
    pub index: i32,
    pub profile: String,
    pub from_fallback: bool,
}

/// "blender.Blender" against instance "blender" class "Blender".
///
/// Matching is case-insensitive, because WM_CLASS capitalisation is not
/// something a user should have to get exactly right by hand. A pattern with no
/// dot matches either half, so "firefox" catches "firefox.Navigator".
pub fn wm_class_matches(pattern: &str, win: &WindowInfo) -> bool {
    if pattern.is_empty() {
        return false;
    }
    let pat = pattern.to_lowercase();
    let inst = win.wm_instance.to_lowercase();
    let cls = win.wm_class.to_lowercase();
    match pat.find('.') {
        Some(dot) => pat[..dot] == inst && pat[dot + 1..] == cls,
        None => pat == inst || pat == cls,
    }
}

/// First match wins, which is what the design's "first match wins" note means.
/// Disabled rules are skipped. When nothing matches, `fallback` is returned if
/// non-empty, otherwise the result is "no opinion" and the caller should leave
/// the panel alone rather than resetting it.
pub fn evaluate_rules(win: &WindowInfo, rules: &[AppRule], fallback: &str) -> RuleMatch {
    let mut out = RuleMatch {
        index: -1,
        ..Default::default()
    };

    if win.valid {
        for (i, rule) in rules.iter().enumerate() {
            if !rule.enabled || rule.profile.is_empty() {
                continue;
            }
            let hit = match rule.match_on {
                MatchOn::WmClass => wm_class_matches(&rule.pattern, win),
                MatchOn::WindowState => {
                    let want = rule.pattern.to_lowercase();
                    win.states.iter().any(|s| s.to_lowercase() == want)
                }
            };
            if hit {
                out.matched = true;
                out.index = i as i32;
                out.profile = rule.profile.clone();
                return out;
            }
        }
    }

    // Nothing matched. An empty fallback means "leave the panel alone", which
    // is different from "apply nothing": resetting on every unmatched window
    // would fight the user every time they alt-tabbed to a terminal.
    if !fallback.is_empty() {
        out.profile = fallback.to_string();
        out.from_fallback = true;
    }
    out
}

/// Persistence for per-app rules.
pub struct RulesStore {
    path: PathBuf,
}

impl RulesStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Value {
        read_config_file(&self.path)
    }

    pub fn save(&self, cfg: &Value) -> Result<()> {
        atomic_write(&self.path, &serde_json::to_string_pretty(cfg)?)
    }
}

/// The rules config the agent runs on.
#[derive(Clone, Debug)]
pub struct RulesConfig {
    pub enabled: bool,
    pub restore_on_unfocus: bool,
    pub fallback_profile: String,
    pub rules: Vec<AppRule>,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            restore_on_unfocus: true,
            fallback_profile: String::new(),
            rules: Vec::new(),
        }
    }
}

/// Serialise a rules config to the JSON the UI persists and reads.
pub fn rules_to_json(cfg: &RulesConfig) -> Value {
    let rules: Vec<Value> = cfg
        .rules
        .iter()
        .map(|r| {
            json!({
                "app": r.app,
                "pattern": r.pattern,
                "matchOn": r.match_on.name(),
                "profile": r.profile,
                "enabled": r.enabled,
            })
        })
        .collect();
    json!({
        "enabled": cfg.enabled,
        "restoreOnUnfocus": cfg.restore_on_unfocus,
        "fallbackProfile": cfg.fallback_profile,
        "rules": rules,
    })
}

/// Parse a rules config from JSON. A rule with no pattern is dropped, the same
/// as the C++ `fromJson`; rules stay off unless the file says otherwise, so a
/// config that fails to parse can never silently start moving the panel.
pub fn rules_from_json(obj: &Value) -> RulesConfig {
    let mut cfg = RulesConfig::default();
    cfg.enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    cfg.restore_on_unfocus = obj
        .get("restoreOnUnfocus")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    cfg.fallback_profile = obj
        .get("fallbackProfile")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(arr) = obj.get("rules").and_then(Value::as_array) {
        for value in arr {
            let rule_obj = match value {
                Value::Object(map) => Value::Object(map.clone()),
                _ => json!({}),
            };
            let mut rule = AppRule {
                app: String::new(),
                pattern: String::new(),
                match_on: MatchOn::WmClass,
                profile: String::new(),
                enabled: true,
            };
            rule.app = str_field(&rule_obj, "app");
            rule.pattern = str_field(&rule_obj, "pattern");
            rule.match_on = MatchOn::from_name(&str_field(&rule_obj, "matchOn"));
            rule.profile = str_field(&rule_obj, "profile");
            rule.enabled = rule_obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            if !rule.pattern.is_empty() {
                cfg.rules.push(rule);
            }
        }
    }
    cfg
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Settings: QSettings-backed persistence for touch mode, DDC values, dashboard
// selections, and the login autostart entry.
// ---------------------------------------------------------------------------

pub struct SettingsStore {
    config_file: PathBuf,
    autostart_path: PathBuf,
}

impl SettingsStore {
    pub fn new(config_file: PathBuf, autostart_path: PathBuf) -> Self {
        Self {
            config_file,
            autostart_path,
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.config_file
    }

    fn read(&self) -> Value {
        read_config_file(&self.config_file)
    }

    /// Touch mode (stored as the `TouchControl::Mode` int value).
    pub fn save_touch_mode(&self, mode: i64) {
        let mut root = self.read();
        config_set(&mut root, "touch/mode", json!(mode));
        let _ = write_config_file(&self.config_file, &root);
    }

    pub fn load_touch_mode(&self, fallback: i64) -> i64 {
        config_get(&self.read(), "touch/mode")
            .and_then(Value::as_i64)
            .unwrap_or(fallback)
    }

    /// DDC/CI VCP values keyed by VCP code, so they can be restored at login.
    pub fn save_vcp(&self, code: u16, value: i64) {
        let mut root = self.read();
        config_set(
            &mut root,
            &format!("ddc/vcp_{code:02x}"),
            json!(value),
        );
        let _ = write_config_file(&self.config_file, &root);
    }

    pub fn load_vcps(&self) -> BTreeMap<u16, i64> {
        let mut out = BTreeMap::new();
        let root = self.read();
        if let Some(ddc) = root.get("ddc").and_then(Value::as_object) {
            for (key, value) in ddc {
                if let Some(hex) = key.strip_prefix("vcp_") {
                    if let Ok(code) = u16::from_str_radix(hex, 16) {
                        if let Some(v) = value.as_i64() {
                            out.insert(code, v);
                        }
                    }
                }
            }
        }
        out
    }

    /// Which GPUs the dashboard shows. Empty means automatic (the card with the
    /// most VRAM); automatic is stored as the absence of the key so a machine
    /// that later gains a card gets the sensible default instead of an empty tile.
    pub fn save_gpu_selection(&self, ids: &[String]) {
        let mut root = self.read();
        if ids.is_empty() {
            config_remove(&mut root, "dashboard/gpus");
        } else {
            config_set(&mut root, "dashboard/gpus", json!(ids));
        }
        let _ = write_config_file(&self.config_file, &root);
    }

    pub fn load_gpu_selection(&self) -> Vec<String> {
        config_get(&self.read(), "dashboard/gpus")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Which network interface the dashboard reads; empty means automatic.
    pub fn save_net_interface(&self, name: &str) {
        let mut root = self.read();
        if name.is_empty() {
            config_remove(&mut root, "dashboard/netInterface");
        } else {
            config_set(&mut root, "dashboard/netInterface", json!(name));
        }
        let _ = write_config_file(&self.config_file, &root);
    }

    pub fn load_net_interface(&self) -> String {
        config_get(&self.read(), "dashboard/netInterface")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    pub fn autostart_enabled(&self) -> bool {
        self.autostart_path.exists()
    }

    /// Write/remove the login autostart entry. The Exec path is the current
    /// executable (the UI owns the entry for this build, so this is not exposed
    /// over RPC).
    pub fn set_autostart(&self, enabled: bool) -> Result<()> {
        if !enabled {
            if self.autostart_path.exists() {
                fs::remove_file(&self.autostart_path)?;
            }
            return Ok(());
        }
        if let Some(parent) = self.autostart_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let exe = std::env::current_exe().context("cannot find the agent executable")?;
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=EdgeLine agent\n\
             Comment=Owns the Xeneon Edge and restores its settings at login\n\
             Exec={}\n\
             X-GNOME-Autostart-Delay=4\n\
             X-GNOME-Autostart-enabled=true\n\
             NoDisplay=true\n",
            exe.display()
        );
        atomic_write(&self.autostart_path, &content)
    }
}

/// One-time migration from the pre-0.4.0 `xeneon-ctl` config, mirroring the
/// C++ `settings::init`. Returns true when a migration actually happened.
pub fn migrate_settings(config_file: &Path, legacy_config: &Path) -> bool {
    if config_file.exists() {
        return false;
    }
    if !legacy_config.exists() {
        return false;
    }
    let content = match fs::read_to_string(legacy_config) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if atomic_write(config_file, &content).is_err() {
        return false;
    }
    // The old file is left alone: if this turns out badly the user still has it.
    true
}

// ---------------------------------------------------------------------------
// Colour profiles: the panel's ICC profile, through colord/colormgr.
// ---------------------------------------------------------------------------

pub mod color {
    #[derive(Clone, Debug, Default)]
    pub struct IccProfile {
        pub id: String,
        pub filename: String,
    }

    impl IccProfile {
        /// True for colord's automatic EDID-derived profile. Present on every
        /// display, means nothing was measured.
        pub fn is_auto_edid(&self) -> bool {
            basename(&self.filename).starts_with("edid-")
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct ColorDevice {
        pub object_path: String,
        pub device_id: String,
        pub model: String,
        pub vendor: String,
        pub xrandr_name: String,
        pub profiles: Vec<IccProfile>,
    }

    impl ColorDevice {
        /// The profile colord would use, which is the first one listed.
        pub fn default_profile(&self) -> Option<&IccProfile> {
            self.profiles.first()
        }

        /// A real calibration is bound, as opposed to the automatic EDID profile.
        pub fn has_calibration(&self) -> bool {
            self.profiles.iter().any(|p| !p.is_auto_edid())
        }
    }

    fn trim(s: &str) -> &str {
        s.trim()
    }

    fn basename(path: &str) -> &str {
        path.rsplit('/').next().unwrap_or(path)
    }

    /// Parse `colormgr get-devices-by-kind display` output. A block with no
    /// device id is not a usable device and is dropped.
    pub fn parse_color_devices(raw: &str) -> Vec<ColorDevice> {
        let mut out: Vec<ColorDevice> = Vec::new();
        let mut expect_profile_path = false;

        for line in raw.lines() {
            let t = trim(line);

            if t.starts_with("Object Path:") {
                out.push(ColorDevice {
                    object_path: t["Object Path:".len()..].trim().to_string(),
                    ..Default::default()
                });
                expect_profile_path = false;
                continue;
            }
            let Some(device) = out.last_mut() else {
                continue; // preamble before the first device
            };

            // A profile id is followed by an indented line holding its path.
            if expect_profile_path {
                expect_profile_path = false;
                if t.starts_with('/') && !device.profiles.is_empty() {
                    device.profiles.last_mut().unwrap().filename = t.to_string();
                    continue;
                }
            }

            if t.starts_with("Device ID:") {
                device.device_id = t["Device ID:".len()..].trim().to_string();
                continue;
            }
            if t.starts_with("Model:") {
                device.model = t["Model:".len()..].trim().to_string();
                continue;
            }
            if t.starts_with("Vendor:") {
                device.vendor = t["Vendor:".len()..].trim().to_string();
                continue;
            }
            if t.starts_with("Metadata:") {
                let meta = t["Metadata:".len()..].trim();
                if let Some(value) = meta.strip_prefix("XRANDR_name=") {
                    device.xrandr_name = value.to_string();
                }
                continue;
            }
            if t.starts_with("Profile ") {
                if let Some(colon) = t.find(':') {
                    let id = t[colon + 1..].trim().to_string();
                    device.profiles.push(IccProfile {
                        id,
                        filename: String::new(),
                    });
                    expect_profile_path = true;
                }
                continue;
            }
        }

        out.retain(|d| !d.device_id.is_empty());
        out
    }

    /// Pick the Edge out of a device list by model, since the device id carries
    /// vendor strings and trailing padding that differ between machines.
    pub fn find_edge(devices: &[ColorDevice]) -> Option<&ColorDevice> {
        devices
            .iter()
            .find(|d| d.model.to_uppercase().contains("XENEON EDGE"))
    }

    /// A colormgr invocation, separable so tests can assert the locale is
    /// pinned without running a real (or fake) binary.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ColormgrInvocation {
        pub program: String,
        pub args: Vec<String>,
        pub env: Vec<(String, String)>,
    }

    impl ColormgrInvocation {
        /// The display-device listing. `LC_ALL` is pinned to `C.UTF-8` because
        /// colormgr translates its labels and the parser reads the English ones
        /// (gettext: LC_ALL beats LC_MESSAGES beats LANG; C.UTF-8 rather than C
        /// keeps non-ASCII in profile paths intact).
        pub fn new_display() -> Self {
            Self {
                program: "colormgr".to_string(),
                args: vec!["get-devices-by-kind".to_string(), "display".to_string()],
                env: vec![("LC_ALL".to_string(), "C.UTF-8".to_string())],
            }
        }

        /// Bind a profile as the default for a device.
        pub fn make_default(device_id: &str, profile_id: &str) -> Self {
            Self {
                program: "colormgr".to_string(),
                args: vec![
                    "device-make-profile-default".to_string(),
                    device_id.to_string(),
                    profile_id.to_string(),
                ],
                env: vec![("LC_ALL".to_string(), "C.UTF-8".to_string())],
            }
        }
    }

    /// Run a colormgr invocation and return its merged stdout/stderr, or `None`
    /// when the binary is missing or exits non-zero. Kept out of the pure
    /// parser so the process side can be faked in tests.
    pub fn invoke_colormgr(inv: &ColormgrInvocation) -> Option<String> {
        let mut cmd = std::process::Command::new(&inv.program);
        cmd.args(&inv.args);
        for (k, v) in &inv.env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::null());
        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let mut data = output.stdout;
        data.extend_from_slice(&output.stderr);
        Some(String::from_utf8_lossy(&data).into_owned())
    }

    /// Run `colormgr get-devices-by-kind display` and parse it. Empty when
    /// colord is not running, colormgr is not installed, or it fails.
    pub fn read_color_devices_with(
        invoke: impl Fn(&ColormgrInvocation) -> Option<String>,
    ) -> Vec<ColorDevice> {
        let inv = ColormgrInvocation::new_display();
        invoke(&inv)
            .map(|raw| parse_color_devices(&raw))
            .unwrap_or_default()
    }

    pub fn read_color_devices() -> Vec<ColorDevice> {
        read_color_devices_with(invoke_colormgr)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const EN_FIXTURE: &str = "\
Object Path:   /org/freedesktop/ColorManager/devices/xrandr_AOC_CU34G2XP_1Q1R4HA008350_q_1000
Owner:         q
Created:       September 10 2026, 06:41:42 AM
Modified:      September 10 2026, 06:41:42 AM
Type:          display
Enabled:       Yes
Embedded:      No
Model:         CU34G2XP
Vendor:        AOC
Serial:        1Q1R4HA008350
Scope:         temp
Colorspace:    rgb
Device ID:     xrandr-AOC-CU34G2XP-1Q1R4HA008350
Profile 1:     icc-1adb2630cd563701d8e6979ea49228ec
               /home/q/.local/share/icc/edid-f22b939d364e6d961af8ce94af8cdfa9.icc
Metadata:      OutputEdidMd5=f22b939d364e6d961af8ce94af8cdfa9
Metadata:      OutputPriority=primary
Metadata:      XRANDR_name=DP-4

Object Path:   /org/freedesktop/ColorManager/devices/xrandr_Cyrix_Corporation_XENEON_EDGE_______________q_1000
Owner:         q
Created:       September 10 2026, 07:14:10 AM
Modified:      September 10 2026, 07:14:10 AM
Type:          display
Enabled:       Yes
Embedded:      No
Model:         XENEON EDGE
Vendor:        Cyrix
Serial:                     
Scope:         temp
Colorspace:    rgb
Device ID:     xrandr-Cyrix Corporation-XENEON EDGE-             
Profile 1:     icc-7423f5ca4e818246893cfc537577201d
               /home/q/.local/share/icc/edid-696263bf5b2f1861dc7bf60d28bbb5d7.icc
Metadata:      OutputEdidMd5=696263bf5b2f1861dc7bf60d28bbb5d7
Metadata:      OutputPriority=secondary
Metadata:      XRANDR_name=DisplayPort-1-4
";

    const DE_FIXTURE: &str = "\
Objektpfad:    /org/freedesktop/ColorManager/devices/xrandr_LG_Electronics_27GN950_110NTAB01234_q_1000
Besitzer:      q
Angelegt:      18.September.2026, 21:30:16
Geändert:     6.Oktober.2026, 00:14:22
Typ:           display
Enabled:       Yes
Embedded:      No
Modell:        27GN950
Hersteller:    LG
Seriennummer:  110NTAB01234
Geltungsbereich:temp
Farbraum:      rgb
Gerätekennung:xrandr-LG Electronics-27GN950-110NTAB01234
Profil 1:      icc-ea0ac7d1592f53e0df627a7d8bff2045
               /home/q/.local/share/icc/lg27gn950-srgb-d65-g22.icc
Profil 2:      icc-445f8e98b59a90b9c272d92d154a82d1
               /home/q/.local/share/icc/edid-5d03bbca975ff70e744c847a734d5cfd.icc
Metadaten:     OutputEdidMd5=5d03bbca975ff70e744c847a734d5cfd
Metadaten:     OutputPriority=primary
Metadaten:     XRANDR_name=DP-3

Objektpfad:    /org/freedesktop/ColorManager/devices/xrandr_Cyrix_Corporation_XENEON_EDGE_100000000001__q_1000
Besitzer:      q
Angelegt:      2.Oktober.2026, 12:55:38
Geändert:     6.Oktober.2026, 06:11:01
Typ:           display
Enabled:       Yes
Embedded:      No
Modell:        XENEON EDGE
Hersteller:    Cyrix
Seriennummer:  100000000001 
Geltungsbereich:temp
Farbraum:      rgb
Gerätekennung:xrandr-Cyrix Corporation-XENEON EDGE-100000000001 
Profil 1:      icc-9d629a8cf6f949ba0bd71158ac79b5d3
               /home/q/.local/share/icc/edge-user1-d65-g22.icc
Profil 2:      icc-537140a51ca674fbbe7e22b783251b3a
               /home/q/.local/share/icc/edge-5000k-d65-g22.icc
Profil 3:      icc-8e9e4128951641795cc744124536533d
               /home/q/.local/share/icc/edid-51bfe46cfb1c896d21d51ec0db06fbda.icc
Metadaten:     OutputEdidMd5=51bfe46cfb1c896d21d51ec0db06fbda
Metadaten:     OutputPriority=secondary
Metadaten:     XRANDR_name=HDMI-1
";

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "edgeline-profiles-{tag}-{}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ------------------------------------------------------------- profiles

    #[test]
    fn slug_for_collapses_unsafe_runs_and_is_stable() {
        assert_eq!(slug_for("  Blender  "), "Blender");
        assert_eq!(slug_for("a b"), "a-b");
        assert_eq!(slug_for("a  b"), "a-b");
        assert_eq!(slug_for(".hidden"), "hidden");
        assert_eq!(slug_for("-dash"), "dash");
        assert_eq!(slug_for("..."), "profile");
        assert_eq!(slug_for(""), "profile");
        assert_eq!(slug_for(&"x".repeat(80)).len(), 64);
    }

    #[test]
    fn profile_store_save_list_load_delete_rename_and_active() {
        let tmp = TempDir::new("store");
        let profiles_dir = tmp.path().join("profiles");
        let config_file = tmp.path().join("settings.json");
        let store = ProfileStore::new(profiles_dir.clone(), config_file.clone());

        assert!(store.list().is_empty());
        assert!(!store.exists("Desk"));

        // Save snapshots current panel state; the name is stamped in.
        let body = json!({"vcp":{"10":50,"14":4},"touchMode":"main-cursor"});
        assert_eq!(store.save("Desk", &body).unwrap(), "Desk");
        assert!(store.exists("Desk"));
        assert_eq!(store.load("Desk").get("name").and_then(Value::as_str), Some("Desk"));

        // Save refuses an empty name.
        assert!(store.save("  ", &body).is_err());

        // list returns names, created dir is reported.
        assert_eq!(store.list(), vec!["Desk"]);
        assert_eq!(store.directory(), profiles_dir.as_path());

        // summary: brightness + preset + touch mode.
        assert_eq!(store.summary(&store.load("Desk")), "50 · preset 0x04 · main-cursor");

        // rename updates active.
        store.set_active("Desk");
        assert_eq!(store.active(), "Desk");
        assert_eq!(store.rename("Desk", "Night").unwrap(), ());
        assert!(!store.exists("Desk"));
        assert!(store.exists("Night"));
        assert_eq!(store.active(), "Night");

        // delete clears active when it pointed at the deleted profile.
        store.remove("Night").unwrap();
        assert!(!store.exists("Night"));
        assert_eq!(store.active(), "");
    }

    #[test]
    fn profile_rename_checks_existing_target_and_requires_name() {
        let tmp = TempDir::new("rename");
        let store = ProfileStore::new(
            tmp.path().join("profiles"),
            tmp.path().join("settings.json"),
        );
        store.save("A", &json!({})).unwrap();
        store.save("B", &json!({})).unwrap();

        // Renaming to an existing name is refused.
        assert!(store.rename("A", "B").is_err());
        // Renaming to an empty name is refused.
        assert!(store.rename("A", "  ").is_err());
        // Renaming a missing profile is refused.
        assert!(store.rename("missing", "C").is_err());
        // Renaming to a name that differs only by punctuation preserves the
        // profile under the new slug (the old file is removed, as in C++).
        assert_eq!(store.rename("A", "a.!").unwrap(), ());
        assert!(!store.exists("A"));
        assert!(store.exists("a.!"));
    }

    #[test]
    fn profile_list_ignores_files_that_do_not_name_a_profile() {
        let tmp = TempDir::new("corrupt");
        let profiles_dir = tmp.path().join("profiles");
        fs::create_dir_all(&profiles_dir).unwrap();
        // A valid profile.
        fs::write(profiles_dir.join("good.json"), r#"{"name":"Good"}"#).unwrap();
        // A corrupt/nameless file is not reported.
        fs::write(profiles_dir.join("junk.json"), r#"{"x":1}"#).unwrap();
        fs::write(profiles_dir.join("bad.json"), "not json").unwrap();
        fs::write(profiles_dir.join("noext"), r#"{"name":"NoExt"}"#).unwrap();

        let store = ProfileStore::new(profiles_dir, tmp.path().join("settings.json"));
        assert_eq!(store.list(), vec!["Good"]);
    }

    // ------------------------------------------------------------- rules

    fn win(inst: &str, class: &str, states: &[&str]) -> WindowInfo {
        WindowInfo {
            wm_instance: inst.to_string(),
            wm_class: class.to_string(),
            states: states.iter().map(|s| s.to_string()).collect(),
            valid: true,
            ..Default::default()
        }
    }

    fn rule(pattern: &str, profile: &str, on: MatchOn) -> AppRule {
        AppRule {
            app: String::new(),
            pattern: pattern.to_string(),
            match_on: on,
            profile: profile.to_string(),
            enabled: true,
        }
    }

    fn design_rules() -> Vec<AppRule> {
        vec![
            rule("blender.Blender", "Game", MatchOn::WmClass),
            rule("firefox.Navigator", "Desk", MatchOn::WmClass),
            rule("darktable.darktable", "sRGB print", MatchOn::WmClass),
            rule("_NET_WM_STATE_FULLSCREEN", "Night", MatchOn::WindowState),
        ]
    }

    #[test]
    fn wm_class_matching_is_case_insensitive_and_flexible() {
        assert!(wm_class_matches("blender.Blender", &win("blender", "Blender", &[])));
        assert!(!wm_class_matches("blender.Blender", &win("firefox", "Navigator", &[])));
        // Capitalisation is not something a user should have to get right.
        assert!(wm_class_matches("BLENDER.blender", &win("blender", "Blender", &[])));
        assert!(wm_class_matches("blender.blender", &win("Blender", "BLENDER", &[])));
        // A bare word matches either half.
        assert!(wm_class_matches("firefox", &win("firefox", "Navigator", &[])));
        assert!(wm_class_matches("Navigator", &win("firefox", "Navigator", &[])));
        assert!(!wm_class_matches("fire", &win("firefox", "Navigator", &[])));
        // Both halves must match when a dot is given.
        assert!(!wm_class_matches("firefox.Blender", &win("firefox", "Navigator", &[])));
        assert!(!wm_class_matches("", &win("firefox", "Navigator", &[])));
    }

    #[test]
    fn rules_first_match_wins_and_skips_disabled_or_profileless() {
        // First match wins.
        let rules = vec![rule("firefox", "Desk", MatchOn::WmClass), rule("firefox", "Night", MatchOn::WmClass)];
        let m = evaluate_rules(&win("firefox", "Navigator", &[]), &rules, "");
        assert!(m.matched);
        assert_eq!(m.index, 0);
        assert_eq!(m.profile, "Desk");
        assert!(!m.from_fallback);

        // A disabled rule is skipped and the next one gets its turn.
        let mut rules = rules;
        rules[0].enabled = false;
        let m = evaluate_rules(&win("firefox", "Navigator", &[]), &rules, "");
        assert!(m.matched);
        assert_eq!(m.index, 1);
        assert_eq!(m.profile, "Night");

        // A rule naming no profile cannot fire.
        let rules = vec![rule("firefox", "", MatchOn::WmClass), rule("firefox", "Night", MatchOn::WmClass)];
        let m = evaluate_rules(&win("firefox", "Navigator", &[]), &rules, "");
        assert_eq!(m.index, 1);
    }

    #[test]
    fn rules_window_state_matching_and_ordering() {
        let rules = design_rules();
        let m = evaluate_rules(&win("mpv", "mpv", &["_NET_WM_STATE_FULLSCREEN"]), &rules, "");
        assert!(m.matched);
        assert_eq!(m.profile, "Night");
        assert_eq!(m.index, 3);

        // A WM_CLASS rule earlier in the list beats a later state rule.
        let m = evaluate_rules(
            &win("blender", "Blender", &["_NET_WM_STATE_FULLSCREEN"]),
            &rules,
            "",
        );
        assert_eq!(m.profile, "Game");
        assert_eq!(m.index, 0);

        // A non-matching state does not fire.
        let m = evaluate_rules(&win("mpv", "mpv", &["_NET_WM_STATE_MAXIMIZED_VERT"]), &rules, "");
        assert!(!m.matched);
    }

    #[test]
    fn rules_fallback_and_nothing_focused() {
        let rules = design_rules();
        // Nothing matched and no fallback: leave the panel alone.
        let m = evaluate_rules(&win("xterm", "XTerm", &[]), &rules, "");
        assert!(!m.matched);
        assert!(m.profile.is_empty());
        assert!(!m.from_fallback);

        // Fallback applies when nothing matches.
        let m = evaluate_rules(&win("xterm", "XTerm", &[]), &rules, "Desk");
        assert!(!m.matched);
        assert!(m.from_fallback);
        assert_eq!(m.profile, "Desk");

        // Nothing focused still honours the fallback but never claims a rule matched.
        let m = evaluate_rules(&WindowInfo::default(), &rules, "Desk");
        assert!(!m.matched);
        assert!(m.from_fallback);
        assert_eq!(m.profile, "Desk");
        assert_eq!(m.index, -1);

        let m = evaluate_rules(&WindowInfo::default(), &[], "");
        assert!(!m.matched);
        assert!(m.profile.is_empty());
    }

    #[test]
    fn rules_match_on_names() {
        assert_eq!(MatchOn::WmClass.name(), "WM_CLASS");
        assert_eq!(MatchOn::WindowState.name(), "window state");
        assert_eq!(MatchOn::from_name("window state"), MatchOn::WindowState);
        assert_eq!(MatchOn::from_name("WINDOW STATE"), MatchOn::WindowState);
        assert_eq!(MatchOn::from_name("WM_CLASS"), MatchOn::WmClass);
        assert_eq!(MatchOn::from_name("nonsense"), MatchOn::WmClass);
    }

    #[test]
    fn rules_to_from_json_round_trips_and_drops_patternless_rules() {
        let cfg = RulesConfig {
            enabled: true,
            restore_on_unfocus: false,
            fallback_profile: "Desk".to_string(),
            rules: vec![
                AppRule {
                    app: "Blender".to_string(),
                    pattern: "blender.Blender".to_string(),
                    match_on: MatchOn::WmClass,
                    profile: "Game".to_string(),
                    enabled: true,
                },
                AppRule {
                    app: "mpv".to_string(),
                    pattern: "_NET_WM_STATE_FULLSCREEN".to_string(),
                    match_on: MatchOn::WindowState,
                    profile: "Night".to_string(),
                    enabled: false,
                },
            ],
        };
        let json = rules_to_json(&cfg);
        assert_eq!(json["enabled"], true);
        assert_eq!(json["restoreOnUnfocus"], false);
        assert_eq!(json["fallbackProfile"], "Desk");
        assert_eq!(json["rules"][0]["matchOn"], "WM_CLASS");
        assert_eq!(json["rules"][1]["matchOn"], "window state");

        let back = rules_from_json(&json);
        assert_eq!(back.enabled, true);
        assert_eq!(back.restore_on_unfocus, false);
        assert_eq!(back.fallback_profile, "Desk");
        assert_eq!(back.rules.len(), 2);
        assert_eq!(back.rules[1].match_on, MatchOn::WindowState);
        assert_eq!(back.rules[1].enabled, false);

        // Rules stay off unless the file says otherwise; a patternless rule is dropped.
        let parsed = rules_from_json(&json!({
            "rules": [
                {"pattern":"","profile":"X"},
                {"pattern":"firefox","profile":"Desk"}
            ]
        }));
        assert!(!parsed.enabled);
        assert!(parsed.restore_on_unfocus);
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].pattern, "firefox");
    }

    #[test]
    fn rules_store_save_load_round_trips() {
        let tmp = TempDir::new("rules");
        let path = tmp.path().join("rules.json");
        let store = RulesStore::new(path.clone());
        let cfg = rules_to_json(&RulesConfig {
            enabled: true,
            restore_on_unfocus: true,
            fallback_profile: "Desk".to_string(),
            rules: vec![AppRule {
                app: "Blender".to_string(),
                pattern: "blender.Blender".to_string(),
                match_on: MatchOn::WmClass,
                profile: "Game".to_string(),
                enabled: true,
            }],
        });
        store.save(&cfg).unwrap();
        assert_eq!(store.path(), path.as_path());
        let back = rules_from_json(&store.load());
        assert_eq!(back.enabled, true);
        assert_eq!(back.rules.len(), 1);
        assert_eq!(back.rules[0].profile, "Game");
    }

    // ------------------------------------------------------------- settings

    #[test]
    fn settings_touch_vcp_gpu_and_net_round_trip() {
        let tmp = TempDir::new("settings");
        let settings = SettingsStore::new(
            tmp.path().join("settings.json"),
            tmp.path().join("autostart/edgeline.desktop"),
        );

        settings.save_touch_mode(2);
        assert_eq!(settings.load_touch_mode(-1), 2);

        settings.save_vcp(0x10, 100);
        settings.save_vcp(0x12, 51);
        let vcps = settings.load_vcps();
        assert_eq!(vcps.get(&0x10), Some(&100));
        assert_eq!(vcps.get(&0x12), Some(&51));
        assert_eq!(vcps.len(), 2);

        settings.save_gpu_selection(&["gpu0".to_string()]);
        assert_eq!(settings.load_gpu_selection(), vec!["gpu0"]);
        // Empty means automatic: stored as the absence of the key.
        settings.save_gpu_selection(&[]);
        assert!(settings.load_gpu_selection().is_empty());

        settings.save_net_interface("enp3s0");
        assert_eq!(settings.load_net_interface(), "enp3s0");
        settings.save_net_interface("");
        assert!(settings.load_net_interface().is_empty());
    }

    #[test]
    fn settings_autostart_writes_and_removes_desktop_entry() {
        let tmp = TempDir::new("autostart");
        let path = tmp.path().join("autostart/edgeline.desktop");
        let settings = SettingsStore::new(tmp.path().join("settings.json"), path.clone());
        assert!(!settings.autostart_enabled());

        settings.set_autostart(true).unwrap();
        assert!(settings.autostart_enabled());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[Desktop Entry]"));
        assert!(content.contains("X-GNOME-Autostart-enabled=true"));

        settings.set_autostart(false).unwrap();
        assert!(!settings.autostart_enabled());
    }

    #[test]
    fn settings_migration_copies_legacy_config_once() {
        let tmp = TempDir::new("migrate");
        let config_file = tmp.path().join("settings.json");
        let legacy = tmp.path().join("legacy.conf");
        fs::write(&legacy, "[touch]\nmode=1\n").unwrap();
        assert!(migrate_settings(&config_file, &legacy));
        assert!(config_file.exists());
        // Second time: target already exists, no migration.
        assert!(!migrate_settings(&config_file, &legacy));
        // Missing legacy: no migration.
        assert!(!migrate_settings(&tmp.path().join("other.json"), &tmp.path().join("nope.conf")));
    }

    // ------------------------------------------------------------- colour

    #[test]
    fn colord_parses_fixture_and_finds_edge_by_model() {
        let devices = color::parse_color_devices(EN_FIXTURE);
        assert_eq!(devices.len(), 2);

        let edge = color::find_edge(&devices).expect("Edge found by model");
        assert_eq!(edge.model, "XENEON EDGE");
        assert_eq!(edge.xrandr_name, "DisplayPort-1-4");
        assert!(!edge.device_id.is_empty());
        assert!(!edge.object_path.is_empty());

        assert_eq!(edge.profiles.len(), 1);
        let def = edge.default_profile().expect("Edge has a default profile");
        assert_eq!(def.id, "icc-7423f5ca4e818246893cfc537577201d");
        assert!(def.filename.contains("/edid-"));
        assert!(def.is_auto_edid());

        // The automatic EDID profile is not a calibration, so gain stays editable.
        assert!(!edge.has_calibration());

        // The other display parses too and is not mistaken for the Edge.
        let aoc = devices.iter().find(|d| d.model == "CU34G2XP").expect("AOC present");
        assert_eq!(aoc.vendor, "AOC");
        assert_eq!(aoc.profiles.len(), 1);
        assert!(aoc.profiles[0].is_auto_edid());
    }

    #[test]
    fn colord_calibration_detection() {
        // A real calibration is recognised as one.
        let mut device = color::ColorDevice {
            device_id: "x".to_string(),
            ..Default::default()
        };
        device.profiles.push(color::IccProfile {
            id: "icc-aaa".to_string(),
            filename: "/home/q/.local/share/icc/edge-d65.icc".to_string(),
        });
        assert!(!device.profiles[0].is_auto_edid());
        assert!(device.has_calibration());

        // A calibration listed after the auto profile still counts; the default
        // stays the auto one.
        let mut device = color::ColorDevice {
            device_id: "x".to_string(),
            ..Default::default()
        };
        device.profiles.push(color::IccProfile {
            id: "icc-aaa".to_string(),
            filename: "/home/q/.local/share/icc/edid-1234.icc".to_string(),
        });
        device.profiles.push(color::IccProfile {
            id: "icc-bbb".to_string(),
            filename: "/home/q/.local/share/icc/measured.icc".to_string(),
        });
        assert!(device.has_calibration());
        assert!(device.default_profile().unwrap().is_auto_edid());

        // A profile with no filename cannot be assumed to be a calibration.
        let mut device = color::ColorDevice {
            device_id: "x".to_string(),
            ..Default::default()
        };
        device.profiles.push(color::IccProfile {
            id: "icc-ccc".to_string(),
            filename: String::new(),
        });
        assert!(!device.profiles[0].is_auto_edid());
    }

    #[test]
    fn colord_junk_does_not_produce_phantom_devices() {
        assert!(color::parse_color_devices("").is_empty());
        assert!(color::parse_color_devices("no devices found\n").is_empty());
        // A block with no Device ID is not a usable device.
        assert!(color::parse_color_devices("Object Path:   /org/x\nModel:  Nothing\n").is_empty());
    }

    #[test]
    fn colormgr_invocation_pins_locale_and_parses() {
        let inv = color::ColormgrInvocation::new_display();
        assert_eq!(inv.program, "colormgr");
        assert_eq!(inv.args, vec!["get-devices-by-kind", "display"]);
        assert_eq!(
            inv.env.iter().find(|(k, _)| k == "LC_ALL").map(|(_, v)| v.as_str()),
            Some("C.UTF-8")
        );
    }

    #[test]
    fn colormgr_read_color_devices_with_injected_runner() {
        // The production runner pins LC_ALL=C.UTF-8; a stand-in colormgr that
        // answers in German unless the locale is pinned therefore yields the
        // English fixture and the Edge is found.
        let invoke = |inv: &color::ColormgrInvocation| {
            let pinned = inv.env.iter().any(|(k, v)| k == "LC_ALL" && v == "C.UTF-8");
            if pinned {
                Some(EN_FIXTURE.to_string())
            } else {
                Some(DE_FIXTURE.to_string())
            }
        };
        let devices = color::read_color_devices_with(invoke);
        let edge = color::find_edge(&devices).expect("Edge found with a pinned locale");
        assert_eq!(edge.model, "XENEON EDGE");
    }

    #[test]
    fn colormgr_german_labels_do_not_parse() {
        // colormgr translates its labels; with German output the English parser
        // reads nothing, which is why the caller has to pin the locale.
        let devices = color::parse_color_devices(DE_FIXTURE);
        assert!(devices.is_empty());
        assert!(color::find_edge(&devices).is_none());
    }

    #[test]
    fn colormgr_invocation_failure_returns_empty() {
        // An invocation that produces no output (e.g. colord not running) is empty.
        let devices = color::read_color_devices_with(|_| None);
        assert!(devices.is_empty());
    }
}
