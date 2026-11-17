// SPDX-License-Identifier: GPL-3.0-or-later
//! Which window has focus — the input to per-app rules.
//!
//! There is no X11 path here on purpose: the C++ agent's watcher refused on
//! Wayland ("per-window rules need a native Wayland backend; XWayland cannot
//! observe all windows"), and Wayland is the only session this backend targets.
//! So focus comes from KWin itself, through `ui/kwin-focus-watch.js`, which runs
//! inside the compositor and reports the active window over the same D-Bus
//! interface the window feed already uses. That is the one place on Wayland that
//! can see every window, and it is read-only: the script reads, it never moves,
//! raises or activates anything.
//!
//! The window state names are the `_NET_WM_STATE_*` ones the rules in
//! `rules.json` already match against, so stored rules keep working unchanged.

use crate::profiles::{RuleMatch, RulesConfig, WindowInfo, evaluate_rules};
use serde_json::{Value, json};

/// A focus snapshot: the shape the `focus` event, `rules.focused` and the rules
/// evaluator all consume. Matches `Api::focusSnapshot()` field for field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Focused {
    pub valid: bool,
    pub wm_instance: String,
    pub wm_class: String,
    pub states: Vec<String>,
}

impl Focused {
    /// Parse what the KWin script sends. Treated as untrusted input even though
    /// we ship the script: a malformed report must degrade to "nothing focused",
    /// never to a panic or a half-populated rule match.
    pub fn parse(text: &str) -> Focused {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Focused::default();
        };
        let text_of = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let states = value
            .get("states")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Focused {
            valid: value.get("valid").and_then(Value::as_bool).unwrap_or(false),
            wm_instance: text_of("wmInstance"),
            wm_class: text_of("wmClass"),
            states,
        }
    }

    /// From an already-parsed JSON value (the feed hands focus over inside the
    /// window report, so the text has been parsed once already).
    pub fn from_json(value: &Value) -> Focused {
        Focused::parse(&value.to_string())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "valid": self.valid,
            "wmInstance": self.wm_instance,
            "wmClass": self.wm_class,
            // The rules UI and the C++ agent both key on the combined form.
            "wmClassFull": format!("{}.{}", self.wm_instance, self.wm_class),
            "states": self.states,
        })
    }

    pub fn window_info(&self) -> WindowInfo {
        WindowInfo {
            wm_instance: self.wm_instance.clone(),
            wm_class: self.wm_class.clone(),
            title: String::new(),
            states: self.states.clone(),
            valid: self.valid,
        }
    }
}

/// What the focus handler should do about the rules. Pure, so the decision can be
/// tested without KWin, a panel or a profile on disk; it reproduces the sequence
/// in `Api::onFocusChanged` step for step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RulePlan {
    /// Profile to apply; empty means "do nothing".
    pub target: String,
    /// The applied profile came from a rule match (not the fallback).
    pub matched: bool,
    /// Profile that was active before a rule took over, remembered so
    /// `restoreOnUnfocus` can put it back.
    pub remember_before: Option<String>,
    /// The applied-profile bookkeeping must be cleared (a restore is in progress).
    pub clear_bookkeeping: bool,
}

impl RulePlan {
    pub fn is_none(&self) -> bool {
        self.target.is_empty()
    }
}

/// Decide what a focus change means for the rules.
///
/// `applied` is the profile a rule last applied (empty for none), `before` the
/// profile that was active before that, and `active` the profile in force now.
/// `exists` is passed in so this stays pure.
pub fn plan_rule_application(
    cfg: &RulesConfig,
    win: &WindowInfo,
    applied: &str,
    before: &str,
    active: &str,
    exists: impl Fn(&str) -> bool,
) -> RulePlan {
    let mut plan = RulePlan::default();
    if !cfg.enabled {
        return plan;
    }
    let found: RuleMatch = evaluate_rules(win, &cfg.rules, &cfg.fallback_profile);
    let mut target = found.profile.clone();
    if target.is_empty() {
        // Nothing matched and there is no fallback: put back whatever was active
        // before a rule took over, if that was asked for. Otherwise leave the
        // panel alone rather than resetting it.
        if cfg.restore_on_unfocus && !applied.is_empty() && !before.is_empty() {
            target = before.to_string();
            plan.clear_bookkeeping = true;
        } else {
            return plan;
        }
    } else if found.matched && applied.is_empty() {
        plan.remember_before = Some(active.to_string());
    }
    plan.matched = found.matched;
    // Already there, or a profile that is not on disk: nothing to do. Checked
    // before the target is reported so a stale rule cannot look like a success.
    if target == active || !exists(&target) {
        return RulePlan::default();
    }
    plan.target = target;
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{AppRule, MatchOn};

    fn rules(enabled: bool, restore: bool, fallback: &str, rule: Option<AppRule>) -> RulesConfig {
        RulesConfig {
            enabled,
            restore_on_unfocus: restore,
            fallback_profile: fallback.to_string(),
            rules: rule.into_iter().collect(),
        }
    }

    fn rule(pattern: &str, profile: &str) -> AppRule {
        AppRule {
            app: String::new(),
            pattern: pattern.into(),
            match_on: MatchOn::WmClass,
            profile: profile.into(),
            enabled: true,
        }
    }

    fn window(instance: &str, class: &str) -> WindowInfo {
        WindowInfo {
            wm_instance: instance.into(),
            wm_class: class.into(),
            title: String::new(),
            states: Vec::new(),
            valid: true,
        }
    }

    fn always(_: &str) -> bool {
        true
    }

    #[test]
    fn a_matching_rule_applies_its_profile_and_remembers_what_was_there() {
        let cfg = rules(true, true, "", Some(rule("blender.Blender", "Painting")));
        let plan =
            plan_rule_application(&cfg, &window("blender", "Blender"), "", "", "Reading", always);
        assert_eq!(plan.target, "Painting");
        assert!(plan.matched);
        assert_eq!(plan.remember_before.as_deref(), Some("Reading"));
    }

    #[test]
    fn a_disabled_config_and_the_flat_no_match_case_do_nothing() {
        let off = rules(false, true, "", Some(rule("blender.Blender", "Painting")));
        let plan =
            plan_rule_application(&off, &window("blender", "Blender"), "", "", "Reading", always);
        assert!(plan.is_none());

        let on = rules(true, true, "", Some(rule("blender.Blender", "Painting")));
        // Nothing matches, no fallback, nothing applied before: leave it alone.
        let plan =
            plan_rule_application(&on, &window("firefox", "Navigator"), "", "", "Reading", always);
        assert!(plan.is_none());
    }

    #[test]
    fn restore_on_unfocus_puts_back_the_profile_a_rule_replaced() {
        let cfg = rules(true, true, "", Some(rule("blender.Blender", "Painting")));
        // Now focusing something else, with a rule still applied.
        let plan = plan_rule_application(
            &cfg,
            &window("firefox", "Navigator"),
            "Painting",
            "Reading",
            "Painting",
            always,
        );
        assert_eq!(plan.target, "Reading");
        assert!(plan.clear_bookkeeping);
        assert!(!plan.matched, "a restore is not a match");

        // Without the flag it stays put.
        let keep = rules(true, false, "", Some(rule("blender.Blender", "Painting")));
        let plan = plan_rule_application(
            &keep,
            &window("firefox", "Navigator"),
            "Painting",
            "Reading",
            "Painting",
            always,
        );
        assert!(plan.is_none());
    }

    #[test]
    fn the_fallback_profile_is_used_when_nothing_matches() {
        let cfg = rules(true, true, "Reading", None);
        let plan = plan_rule_application(
            &cfg,
            &window("firefox", "Navigator"),
            "",
            "",
            "Painting",
            always,
        );
        assert_eq!(plan.target, "Reading");
        assert!(!plan.matched, "the fallback is not a rule match");
    }

    #[test]
    fn an_unknown_or_already_active_profile_is_not_applied() {
        let cfg = rules(true, true, "", Some(rule("blender.Blender", "Painting")));
        // Already there: no churn on the panel.
        let plan =
            plan_rule_application(&cfg, &window("blender", "Blender"), "", "", "Painting", always);
        assert!(plan.is_none());
        // A rule pointing at a profile that was deleted on disk.
        let plan = plan_rule_application(
            &cfg,
            &window("blender", "Blender"),
            "",
            "",
            "Reading",
            |_| false,
        );
        assert!(plan.is_none());
    }

    #[test]
    fn a_window_state_rule_fires_on_the_net_wm_state_name() {
        let mut state_rule = rule("_NET_WM_STATE_FULLSCREEN", "Fullscreen");
        state_rule.match_on = MatchOn::WindowState;
        let cfg = rules(true, true, "", Some(state_rule));
        let mut fullscreen = window("vlc", "vlc");
        fullscreen.states.push("_NET_WM_STATE_FULLSCREEN".into());
        let plan = plan_rule_application(&cfg, &fullscreen, "", "", "Reading", always);
        assert_eq!(plan.target, "Fullscreen");

        // KWin reports the same window without that state: no match.
        let plain = window("vlc", "vlc");
        let plan = plan_rule_application(&cfg, &plain, "", "", "Reading", always);
        assert!(plan.is_none());
    }

    #[test]
    fn focus_snapshot_matches_the_cpp_fields_and_parses_leniently() {
        let focused = Focused::parse(
            r#"{"valid":true,"wmInstance":"blender","wmClass":"Blender",
                "states":["_NET_WM_STATE_FULLSCREEN"]}"#,
        );
        assert_eq!(focused.to_json()["wmClassFull"], "blender.Blender");

        // Junk, wrong types and missing keys degrade to "nothing focused".
        assert_eq!(Focused::parse("not json"), Focused::default());
        assert_eq!(Focused::parse(r#"{"valid":"yes","states":7}"#), Focused::default());
        assert_eq!(Focused::parse(""), Focused::default());
    }
}
