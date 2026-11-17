// SPDX-License-Identifier: GPL-3.0-or-later
//! Gesture bindings and the lock zone, ported from agent/src/core/TouchConfig.cpp.
//!
//! The lock zone is the reserved band a user can rest their palm against
//! without triggering anything. A band is described rather than a rectangle
//! because the panel is a strip: what people want is "ignore the end my palm
//! rests on".
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The reserved-band geometry applied before gestures see a touch.
#[derive(Clone, Debug, PartialEq)]
pub struct LockZone {
    /// Whether the band is active at all.
    pub enabled: bool,
    /// Which edge the reserved band sits against: left | right | top | bottom.
    pub side: String,
    /// How much of the panel the band takes, clamped to 0.05..=0.9.
    pub fraction: f64,
}

impl Default for LockZone {
    fn default() -> Self {
        Self {
            enabled: false,
            side: "right".to_string(),
            fraction: 0.4,
        }
    }
}

/// Gesture configuration. Unbound names, or a binding to "none", do nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub gestures_enabled: bool,
    /// gesture name -> action id.
    pub bindings: BTreeMap<String, String>,
    pub lock_zone: LockZone,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gestures_enabled: false,
            bindings: BTreeMap::new(),
            lock_zone: LockZone::default(),
        }
    }
}

const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];

/// The actions a gesture can be bound to, and a one-line description each.
pub fn actions() -> Vec<(&'static str, &'static str)> {
    vec![
        ("none", "do nothing"),
        ("profile-next", "next profile"),
        ("profile-previous", "previous profile"),
        ("blank-toggle", "blank or unblank the panel"),
        ("brightness-up", "brightness up 10"),
        ("brightness-down", "brightness down 10"),
        ("dashboard-toggle", "show or hide the dashboard"),
        ("dashboard-page-next", "next dashboard page"),
    ]
}

pub fn is_known_action(id: &str) -> bool {
    actions().iter().any(|(name, _)| *name == id)
}

/// True when the point falls inside the reserved band.
///
/// Consumed by the touch-point handler in the compatibility agent; the Rust
/// backend does not yet read touch events (greyed out), so this is kept for
/// parity and pinned by tests rather than reached from the dispatch path.
#[allow(dead_code)]
pub fn in_lock_zone(z: &LockZone, nx: f64, ny: f64) -> bool {
    if !z.enabled {
        return false;
    }
    let f = z.fraction.clamp(0.05, 0.9);
    match z.side.as_str() {
        "left" => nx <= f,
        "right" => nx >= 1.0 - f,
        "top" => ny <= f,
        "bottom" => ny >= 1.0 - f,
        _ => false,
    }
}

pub fn actions_map() -> Map<String, Value> {
    let mut map = Map::new();
    for (name, description) in actions() {
        map.insert(name.to_string(), Value::String(description.to_string()));
    }
    map
}

pub fn to_json(cfg: &Config) -> Value {
    let mut binds = Map::new();
    for (name, action) in &cfg.bindings {
        binds.insert(name.clone(), Value::String(action.clone()));
    }
    json!({
        "gestures": {
            "enabled": cfg.gestures_enabled,
            "bindings": binds,
        },
        "lockZone": {
            "enabled": cfg.lock_zone.enabled,
            "side": cfg.lock_zone.side,
            "fraction": cfg.lock_zone.fraction,
        },
    })
}

pub fn from_json(obj: &Value) -> Config {
    let mut cfg = Config::default();
    if let Some(g) = obj.get("gestures") {
        cfg.gestures_enabled = g.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        if let Some(binds) = g.get("bindings").and_then(Value::as_object) {
            for (name, action) in binds {
                if let Some(action) = action.as_str() {
                    // An unknown action is dropped on load rather than kept as a
                    // binding that silently does nothing.
                    if is_known_action(action) {
                        cfg.bindings.insert(name.clone(), action.to_string());
                    }
                }
            }
        }
    }
    if let Some(z) = obj.get("lockZone") {
        cfg.lock_zone.enabled = z.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let side = z.get("side").and_then(Value::as_str).unwrap_or("right");
        cfg.lock_zone.side = if SIDES.contains(&side) {
            side.to_string()
        } else {
            "right".to_string()
        };
        let fraction = z.get("fraction").and_then(Value::as_f64).unwrap_or(0.4);
        cfg.lock_zone.fraction = fraction.clamp(0.05, 0.9);
    }
    cfg
}

/// Where the touch gesture configuration lives on disk.
pub fn touch_path() -> PathBuf {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        });
    config_dir.join("edgeline").join("touch.json")
}

pub fn load_from(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Config::default(),
    };
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(_) => return Config::default(),
    };
    from_json(&value)
}

pub fn save_to(path: &Path, cfg: &Config, error_out: &mut String) -> bool {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            *error_out = format!("could not write {}: {e}", path.display());
            return false;
        }
    }
    let text = match serde_json::to_string_pretty(&to_json(cfg)) {
        Ok(text) => text,
        Err(e) => {
            *error_out = format!("could not save {}: {e}", path.display());
            return false;
        }
    };
    if let Err(e) = std::fs::write(path, text) {
        *error_out = format!("could not write {}: {e}", path.display());
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_zone_reserves_nothing() {
        let z = LockZone {
            enabled: false,
            side: "right".to_string(),
            fraction: 0.9,
        };
        assert!(!in_lock_zone(&z, 0.99, 0.5));
    }

    #[test]
    fn right_hand_band_geometry() {
        let z = LockZone {
            enabled: true,
            side: "right".to_string(),
            fraction: 0.4,
        };
        assert!(in_lock_zone(&z, 0.95, 0.5));
        assert!(in_lock_zone(&z, 0.60, 0.5)); // exactly on the boundary
        assert!(!in_lock_zone(&z, 0.59, 0.5));
        assert!(!in_lock_zone(&z, 0.05, 0.5));
        // The band spans the full height: a palm anywhere in it is ignored.
        assert!(in_lock_zone(&z, 0.95, 0.01));
        assert!(in_lock_zone(&z, 0.95, 0.99));
    }

    #[test]
    fn left_band_geometry() {
        let z = LockZone {
            enabled: true,
            side: "left".to_string(),
            fraction: 0.25,
        };
        assert!(in_lock_zone(&z, 0.10, 0.5));
        assert!(in_lock_zone(&z, 0.25, 0.5));
        assert!(!in_lock_zone(&z, 0.26, 0.5));
    }

    #[test]
    fn bottom_band_geometry() {
        let z = LockZone {
            enabled: true,
            side: "bottom".to_string(),
            fraction: 0.3,
        };
        assert!(in_lock_zone(&z, 0.5, 0.85));
        assert!(!in_lock_zone(&z, 0.5, 0.5));
    }

    #[test]
    fn top_band_geometry() {
        let z = LockZone {
            enabled: true,
            side: "top".to_string(),
            fraction: 0.3,
        };
        assert!(in_lock_zone(&z, 0.5, 0.15));
        assert!(!in_lock_zone(&z, 0.5, 0.5));
    }

    #[test]
    fn nonsense_side_reserves_nothing() {
        let z = LockZone {
            enabled: true,
            side: "diagonal".to_string(),
            fraction: 0.5,
        };
        assert!(!in_lock_zone(&z, 0.9, 0.9));
    }

    #[test]
    fn fraction_out_of_range_is_clamped() {
        let z = LockZone {
            enabled: true,
            side: "right".to_string(),
            fraction: 5.0,
        };
        assert!(!in_lock_zone(&z, 0.05, 0.5)); // clamped to 0.9, so the left edge is free
        let z = LockZone {
            enabled: true,
            side: "right".to_string(),
            fraction: -1.0,
        };
        assert!(!in_lock_zone(&z, 0.5, 0.5)); // clamped to 0.05
        assert!(in_lock_zone(&z, 0.99, 0.5));
    }

    #[test]
    fn defaults_are_off() {
        let c = from_json(&json!({}));
        assert!(!c.gestures_enabled);
        assert!(!c.lock_zone.enabled);
        assert!(c.bindings.is_empty());
    }

    #[test]
    fn unknown_action_is_dropped_on_load() {
        let c = from_json(&json!({
            "gestures": {
                "enabled": true,
                "bindings": {
                    "swipe-left": "profile-next",
                    "swipe-up": "nonsense"
                }
            }
        }));
        assert!(c.gestures_enabled);
        assert_eq!(c.bindings.get("swipe-left").map(String::as_str), Some("profile-next"));
        assert!(!c.bindings.contains_key("swipe-up"));
    }

    #[test]
    fn round_trip_preserves_everything_that_matters() {
        let mut c = Config::default();
        c.gestures_enabled = true;
        c.bindings.insert("two-finger-tap".to_string(), "blank-toggle".to_string());
        c.lock_zone = LockZone {
            enabled: true,
            side: "left".to_string(),
            fraction: 0.22,
        };
        let back = from_json(&to_json(&c));
        assert!(back.gestures_enabled);
        assert_eq!(
            back.bindings.get("two-finger-tap").map(String::as_str),
            Some("blank-toggle")
        );
        assert!(back.lock_zone.enabled);
        assert_eq!(back.lock_zone.side, "left");
        assert!(back.lock_zone.fraction > 0.21 && back.lock_zone.fraction < 0.23);
    }

    #[test]
    fn known_and_unknown_actions() {
        assert!(is_known_action("profile-next"));
        assert!(is_known_action("none"));
        assert!(!is_known_action("launch-nukes"));
    }

    #[test]
    fn save_and_load_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("edgeline-touchcfg-{}", std::process::id()));
        let path = dir.join("touch.json");
        let mut cfg = Config::default();
        cfg.gestures_enabled = true;
        cfg.bindings.insert("swipe-left".to_string(), "profile-next".to_string());
        cfg.lock_zone = LockZone {
            enabled: true,
            side: "right".to_string(),
            fraction: 0.45,
        };
        let mut err = String::new();
        assert!(save_to(&path, &cfg, &mut err), "save failed: {err}");
        let back = load_from(&path);
        assert_eq!(back, cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
