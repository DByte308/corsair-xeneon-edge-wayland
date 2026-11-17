// edgeline: checks GitHub Releases for a newer version.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust port of agent/src/core/UpdateChecker.cpp.
//
// What this does: one unauthenticated HTTPS GET to the GitHub Releases API,
// compares the latest tag against the installed version, and reports the
// result.
//
// What this deliberately does NOT do: download anything, install anything, or
// run anything. The app ships as a .deb and installing one needs root. Silently
// self-updating a root-installed package from a desktop session is a bad idea,
// so the most this offers is a link to the release page. "OTA" here means "you
// are told", not "it happens to you".
//
// The check is throttled to once a day, can be turned off, and every automatic
// failure is silent. A machine with no network must never see an error it did
// not ask for.

use anyhow::{Result, bail};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const EDGELINE_UPDATE_REPO: &str = "aabdelghani/corsair-xeneon-edge-linux";
// The app version the release feed is compared against, and the User-Agent it is
// asked with. Taken from this crate's version so it cannot drift from what
// `system.info` and `backend.info` report — the backend used to pin it by hand
// at 0.7.0 while the crate was 0.1.0, and a one-sided bump would have nagged a
// current install with "0.8.0 available". The other carriers (tauri-app Cargo +
// tauri.conf.json, ui/package.json, packaging/*.metainfo.xml, the C++ agent's
// project VERSION) still move by hand; keep them in lockstep.
const EDGELINE_VERSION: &str = env!("CARGO_PKG_VERSION");

// Once a day is plenty for a tool that ships a release every few months, and it
// keeps the app from being a rude neighbour to an unauthenticated API.
const THROTTLE_MS: i64 = 24 * 60 * 60 * 1000;
const TIMEOUT_MS: u64 = 8_000;
const SETTINGS_FILE: &str = "updates.conf";

/// Parsed semantic version. Enough for "v1.2.3" and "1.2.3-rc1"; anything it
/// cannot parse compares as invalid and is treated as "no update".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: String, // pre-release suffix, empty for a final release
    pub valid: bool,
}

impl Version {
    // Accepts "v1.2.3", "1.2.3", "v1.2.3-rc1". Trailing junk is not accepted,
    // because guessing at a tag we do not understand is worse than saying no.
    pub fn parse(text: &str) -> Version {
        let t = text.trim();
        let t = t.strip_prefix('v').or_else(|| t.strip_prefix('V')).unwrap_or(t);
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        if let Some(p) = pre {
            if p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
                return Version::invalid();
            }
        }
        let mut parts = core.split('.');
        let major_s = match parts.next() {
            Some(s) if !s.is_empty() => s,
            _ => return Version::invalid(),
        };
        let minor_s = match parts.next() {
            Some(s) if !s.is_empty() => s,
            _ => return Version::invalid(),
        };
        let patch_s = parts.next();
        if parts.next().is_some() {
            return Version::invalid();
        }
        if !major_s.chars().all(|c| c.is_ascii_digit()) || !minor_s.chars().all(|c| c.is_ascii_digit()) {
            return Version::invalid();
        }
        let major = match major_s.parse::<u64>() {
            Ok(v) => v,
            Err(_) => return Version::invalid(),
        };
        let minor = match minor_s.parse::<u64>() {
            Ok(v) => v,
            Err(_) => return Version::invalid(),
        };
        let patch = match patch_s {
            None => 0,
            Some(s) if !s.chars().all(|c| c.is_ascii_digit()) => return Version::invalid(),
            Some(s) => match s.parse::<u64>() {
                Ok(v) => v,
                Err(_) => return Version::invalid(),
            },
        };
        Version { major, minor, patch, pre: pre.unwrap_or("").to_string(), valid: true }
    }

    // Ordering follows semver: 1.2.3-rc1 sorts before 1.2.3.
    pub fn is_newer_than(&self, other: &Version) -> bool {
        if !self.valid || !other.valid {
            return false;
        }
        if self.major != other.major {
            return self.major > other.major;
        }
        if self.minor != other.minor {
            return self.minor > other.minor;
        }
        if self.patch != other.patch {
            return self.patch > other.patch;
        }
        // Same numbers: a final release beats a pre-release of the same number.
        if self.pre.is_empty() != other.pre.is_empty() {
            return self.pre.is_empty();
        }
        self.pre > other.pre
    }

    pub fn to_string(&self) -> String {
        let s = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if self.pre.is_empty() {
            s
        } else {
            format!("{}-{}", s, self.pre)
        }
    }

    fn invalid() -> Version {
        Version { major: 0, minor: 0, patch: 0, pre: String::new(), valid: false }
    }
}

/// The user's update preferences, persisted in the QSettings INI section
/// shape (`[updates]`) used by the C++ agent. The Rust backend keeps its own
/// file so an in-flight migration can never clobber the agent's other keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub skipped_version: String,
    pub last_check_ms: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, skipped_version: String::new(), last_check_ms: 0 }
    }
}

fn load_settings(path: &Path) -> Settings {
    let mut settings = Settings::default();
    let Ok(content) = fs::read_to_string(path) else {
        return settings;
    };
    let mut in_updates = false;
    for line in content.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("[updates]") {
            in_updates = true;
            continue;
        }
        if in_updates && t.starts_with('[') && t.ends_with(']') {
            in_updates = false;
        }
        if in_updates {
            if let Some((k, v)) = t.split_once('=') {
                match k.trim() {
                    "enabled" => settings.enabled = v.trim().eq_ignore_ascii_case("true"),
                    "lastCheckMs" => settings.last_check_ms = v.trim().parse().unwrap_or(0),
                    "skippedVersion" => settings.skipped_version = v.trim().to_string(),
                    _ => {}
                }
            }
        }
    }
    settings
}

fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = format!(
        "[updates]\nenabled={}\nlastCheckMs={}\nskippedVersion={}\n",
        settings.enabled, settings.last_check_ms, settings.skipped_version
    );
    fs::write(path, content)?;
    Ok(())
}

/// The HTTP transport is injectable so tests can point the check at a local
/// JSON fixture and run fully offline.
pub trait Fetcher: Send + Sync {
    fn get(&self, url: &str, user_agent: &str) -> Result<String>;
}

struct CurlFetcher;

impl Fetcher for CurlFetcher {
    fn get(&self, url: &str, user_agent: &str) -> Result<String> {
        let output = std::process::Command::new("curl")
            .args(["-sS", "--location", "--max-time", &TIMEOUT_MS.to_string()])
            .arg("-H")
            .arg("Accept: application/vnd.github+json")
            .arg("-H")
            .arg(format!("User-Agent: {user_agent}"))
            .arg(url)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("network error: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Result of a check, in the vocabulary the C++ signals carried.
pub enum Outcome {
    /// A newer release exists and the user has not skipped it.
    Available { version: String, url: String },
    /// Ran to completion and this build is current (or newer than the release).
    Current { version: String },
    /// Only ever reported for a manual check. Automatic failures stay quiet.
    Failed { error: String },
    /// Nothing to report: throttled, disabled, or an automatic failure.
    Quiet,
}

pub struct UpdateChecker {
    settings_path: PathBuf,
    repo: String,
    current_version: String,
    fetcher: Box<dyn Fetcher>,
    in_flight: AtomicBool,
}

impl UpdateChecker {
    pub fn new(settings_path: PathBuf) -> Self {
        Self {
            settings_path,
            repo: EDGELINE_UPDATE_REPO.to_string(),
            current_version: EDGELINE_VERSION.to_string(),
            fetcher: Box::new(CurlFetcher),
            in_flight: AtomicBool::new(false),
        }
    }

    pub fn config_dir(home: &Path) -> PathBuf {
        home.join(".config/edgeline").join(SETTINGS_FILE)
    }

    pub fn enabled(&self) -> bool {
        load_settings(&self.settings_path).enabled
    }

    pub fn set_enabled(&self, on: bool) {
        let mut settings = load_settings(&self.settings_path);
        settings.enabled = on;
        let _ = save_settings(&self.settings_path, &settings);
    }

    pub fn skipped_version(&self) -> String {
        load_settings(&self.settings_path).skipped_version
    }

    pub fn set_skipped_version(&self, tag: &str) {
        let mut settings = load_settings(&self.settings_path);
        settings.skipped_version = tag.to_string();
        let _ = save_settings(&self.settings_path, &settings);
    }

    /// Fire a check. `manual` bypasses both the daily throttle and the enabled
    /// setting, because it can only be reached by the user clicking a button.
    /// An automatic check that is throttled, disabled, or already running is a
    /// silent no-op.
    pub async fn check(&self, manual: bool) -> Outcome {
        if self.in_flight.swap(true, Ordering::SeqCst) {
            return Outcome::Quiet; // one check in flight is enough
        }
        let outcome = self.check_inner(manual).await;
        self.in_flight.store(false, Ordering::SeqCst);
        outcome
    }

    async fn check_inner(&self, manual: bool) -> Outcome {
        let mut settings = load_settings(&self.settings_path);
        let now = now_ms();

        if !manual {
            if !settings.enabled {
                return Outcome::Quiet;
            }
            if settings.last_check_ms > 0 && now - settings.last_check_ms < THROTTLE_MS {
                return Outcome::Quiet;
            }
        }

        let url = release_url(&self.repo);
        let user_agent = format!("edgeline/{}", self.current_version);
        let body = match self.fetcher.get(&url, &user_agent) {
            Ok(body) => body,
            Err(error) => {
                // Automatic checks never surface an error. A user who is
                // offline, or behind a proxy, or on a network that blocks
                // GitHub, did not ask this app for a diagnosis.
                return if manual { Outcome::Failed { error: error.to_string() } } else { Outcome::Quiet };
            }
        };

        let parsed: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                return if manual {
                    Outcome::Failed { error: "could not parse the release feed".into() }
                } else {
                    Outcome::Quiet
                };
            }
        };
        let Some(obj) = parsed.as_object() else {
            return if manual {
                Outcome::Failed { error: "could not parse the release feed".into() }
            } else {
                Outcome::Quiet
            };
        };

        let mine = Version::parse(&self.current_version);

        // Only stable releases are offered.
        if obj.get("draft").and_then(Value::as_bool) == Some(true)
            || obj.get("prerelease").and_then(Value::as_bool) == Some(true)
        {
            return Outcome::Current { version: mine.to_string() };
        }

        let tag = obj.get("tag_name").and_then(Value::as_str).unwrap_or("").to_string();
        let html = obj.get("html_url").and_then(Value::as_str).unwrap_or("").to_string();
        let latest = Version::parse(&tag);
        if !latest.valid {
            return if manual {
                Outcome::Failed { error: format!("unrecognised release tag '{tag}'") }
            } else {
                Outcome::Quiet
            };
        }

        // Only record a successful check, so a week of failures does not
        // silently throttle away the next good one.
        settings.last_check_ms = now;
        let _ = save_settings(&self.settings_path, &settings);

        if !latest.is_newer_than(&mine) {
            return Outcome::Current { version: mine.to_string() };
        }
        if !manual && settings.skipped_version == latest.to_string() {
            return Outcome::Current { version: mine.to_string() };
        }
        let url = if html.is_empty() { fallback_url(&self.repo) } else { html };
        Outcome::Available { version: latest.to_string(), url }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn release_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

fn fallback_url(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicUsize;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_settings() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edgeline-updates-{}-{}", std::process::id(), n));
        fs::create_dir_all(&dir).unwrap();
        dir.join(SETTINGS_FILE)
    }

    struct BodyFetcher {
        body: String,
        err: Option<String>,
    }

    impl Fetcher for BodyFetcher {
        fn get(&self, _url: &str, _user_agent: &str) -> Result<String> {
            if let Some(err) = &self.err {
                bail!("{}", err);
            }
            Ok(self.body.clone())
        }
    }

    fn checker(path: &Path, body: &str, current: &str) -> UpdateChecker {
        let mut c = UpdateChecker::new(path.to_path_buf());
        c.current_version = current.to_string();
        c.fetcher = Box::new(BodyFetcher { body: body.to_string(), err: None });
        c
    }

    fn release(tag: &str, html: &str) -> String {
        serde_json::json!({
            "tag_name": tag, "html_url": html, "draft": false, "prerelease": false
        })
        .to_string()
    }

    fn write_settings(path: &Path, settings: &Settings) {
        save_settings(path, settings).unwrap();
    }

    fn read_settings(path: &Path) -> Settings {
        load_settings(path)
    }

    // Mirrors the C++ fixtures in agent/tests/test_version.cpp.

    #[test]
    fn parse_accepts_github_tag_shapes() {
        assert!(Version::parse("v1.2.3").valid);
        assert!(Version::parse("1.2.3").valid);
        assert!(Version::parse("V1.2.3").valid);
        assert!(Version::parse("1.2").valid);
        let v = Version::parse("v0.2.0");
        assert_eq!((v.major, v.minor, v.patch), (0, 2, 0));
        assert!(v.pre.is_empty());
        // Two-component tags imply .0
        assert_eq!(Version::parse("v2.1").patch, 0);
    }

    #[test]
    fn parse_rejects_junk() {
        assert!(!Version::parse("").valid);
        assert!(!Version::parse("nightly").valid);
        assert!(!Version::parse("v1.2.3.4").valid);
        assert!(!Version::parse("1.2.3 (beta)").valid);
        assert!(!Version::parse("release-1.2.3").valid);
    }

    #[test]
    fn ordering_matches_semver() {
        let newer = |a: &str, b: &str| Version::parse(a).is_newer_than(&Version::parse(b));
        assert!(newer("v0.3.0", "v0.2.0"));
        assert!(newer("v1.0.0", "v0.9.9"));
        assert!(newer("v0.2.1", "v0.2.0"));
        assert!(!newer("v0.2.0", "v0.2.0"));
        assert!(!newer("v0.2.0", "v0.3.0"));
        assert!(!newer("v0.2.0", "v1.0.0"));
        // Numeric, not lexicographic.
        assert!(newer("v0.10.0", "v0.9.0"));
        assert!(newer("v1.0.10", "v1.0.9"));
        assert!(!newer("v0.9.0", "v0.10.0"));
        // Semver pre-release ordering: 1.0.0-rc1 is older than 1.0.0.
        assert!(newer("v1.0.0", "v1.0.0-rc1"));
        assert!(!newer("v1.0.0-rc1", "v1.0.0"));
        assert!(newer("v1.0.0-rc2", "v1.0.0-rc1"));
        assert!(newer("v1.0.0-rc1", "v0.9.0"));
        // An invalid version is never newer, and nothing is ever newer than it.
        assert!(!newer("nightly", "v0.2.0"));
        assert!(!newer("v0.2.0", "nightly"));
    }

    #[test]
    fn current_version_is_parseable() {
        assert!(Version::parse(EDGELINE_VERSION).valid);
        // The updater compares against the app version the release feed names, so
        // it must be the same number `system.info` reports. This fails at the
        // moment someone bumps the crate without thinking about the feed.
        assert_eq!(EDGELINE_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn to_string_normalises_and_keeps_pre() {
        assert_eq!(Version::parse("v0.3.0").to_string(), "0.3.0");
        assert_eq!(Version::parse("v1.2.3-rc1").to_string(), "1.2.3-rc1");
    }

    // Update-check behavior, driven off a fixture feed.

    #[tokio::test]
    async fn reports_available_when_feed_is_newer() {
        let path = temp_settings();
        let c = checker(&path, &release("v0.3.0", "https://github.com/x/y/releases/tag/v0.3.0"), "0.2.0");
        match c.check(false).await {
            Outcome::Available { version, url } => {
                assert_eq!(version, "0.3.0");
                assert_eq!(url, "https://github.com/x/y/releases/tag/v0.3.0");
            }
            other => panic!("expected Available, got {:?}", outcome_label(&other)),
        }
    }

    #[tokio::test]
    async fn reports_current_when_equal_or_older() {
        for (cur, tag) in [("0.3.0", "v0.3.0"), ("0.4.0", "v0.3.0")] {
            let path = temp_settings();
            let c = checker(&path, &release(tag, ""), cur);
            match c.check(false).await {
                Outcome::Current { version } => assert_eq!(version, cur),
                other => panic!("expected Current, got {:?}", outcome_label(&other)),
            }
        }
    }

    #[tokio::test]
    async fn skipped_version_silences_automatic_check() {
        let path = temp_settings();
        write_settings(&path, &Settings { skipped_version: "0.3.0".into(), ..Default::default() });
        let c = checker(&path, &release("v0.3.0", "https://github.com/x/y/releases/tag/v0.3.0"), "0.2.0");
        match c.check(false).await {
            Outcome::Current { version } => assert_eq!(version, "0.2.0"),
            other => panic!("expected Current, got {:?}", outcome_label(&other)),
        }
    }

    #[tokio::test]
    async fn skipped_version_does_not_silence_manual_check() {
        let path = temp_settings();
        write_settings(&path, &Settings { skipped_version: "0.3.0".into(), ..Default::default() });
        let c = checker(&path, &release("v0.3.0", "https://github.com/x/y/releases/tag/v0.3.0"), "0.2.0");
        match c.check(true).await {
            Outcome::Available { version, .. } => assert_eq!(version, "0.3.0"),
            other => panic!("expected Available, got {:?}", outcome_label(&other)),
        }
    }

    #[tokio::test]
    async fn disabled_automatic_check_is_quiet() {
        let path = temp_settings();
        write_settings(&path, &Settings { enabled: false, ..Default::default() });
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        assert!(matches!(c.check(false).await, Outcome::Quiet));
    }

    #[tokio::test]
    async fn manual_check_bypasses_disabled_setting() {
        let path = temp_settings();
        write_settings(&path, &Settings { enabled: false, ..Default::default() });
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        assert!(matches!(c.check(true).await, Outcome::Available { .. }));
    }

    #[tokio::test]
    async fn automatic_check_throttles_once_a_day() {
        let path = temp_settings();
        write_settings(&path, &Settings { last_check_ms: now_ms(), ..Default::default() });
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        assert!(matches!(c.check(false).await, Outcome::Quiet));
    }

    #[tokio::test]
    async fn manual_check_bypasses_throttle() {
        let path = temp_settings();
        write_settings(&path, &Settings { last_check_ms: now_ms(), ..Default::default() });
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        assert!(matches!(c.check(true).await, Outcome::Available { .. }));
    }

    #[tokio::test]
    async fn manual_failure_is_reported() {
        let path = temp_settings();
        let mut c = UpdateChecker::new(path);
        c.fetcher = Box::new(BodyFetcher { body: String::new(), err: Some("connection refused".into()) });
        match c.check(true).await {
            Outcome::Failed { error } => assert!(error.contains("connection refused")),
            other => panic!("expected Failed, got {:?}", outcome_label(&other)),
        }
    }

    #[tokio::test]
    async fn automatic_failure_stays_quiet() {
        let path = temp_settings();
        let mut c = UpdateChecker::new(path);
        c.fetcher = Box::new(BodyFetcher { body: String::new(), err: Some("connection refused".into()) });
        assert!(matches!(c.check(false).await, Outcome::Quiet));
    }

    #[tokio::test]
    async fn prerelease_and_draft_are_treated_as_current() {
        for flag in ["prerelease", "draft"] {
            let path = temp_settings();
            let body = serde_json::json!({
                "tag_name": "v9.9.9", "html_url": "", "draft": false,
                "prerelease": false, flag: true
            })
            .to_string();
            let c = checker(&path, &body, "0.2.0");
            match c.check(false).await {
                Outcome::Current { version } => assert_eq!(version, "0.2.0"),
                other => panic!("expected Current, got {:?}", outcome_label(&other)),
            }
        }
    }

    #[tokio::test]
    async fn unparseable_tag_manual_fails_automatic_is_quiet() {
        let path = temp_settings();
        let c = checker(&path, &release("nightly", ""), "0.2.0");
        assert!(matches!(c.check(true).await, Outcome::Failed { .. }));
        assert!(matches!(c.check(false).await, Outcome::Quiet));
    }

    #[tokio::test]
    async fn empty_html_url_falls_back_to_release_page() {
        let repo = EDGELINE_UPDATE_REPO;
        let path = temp_settings();
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        match c.check(false).await {
            Outcome::Available { url, .. } => {
                assert_eq!(url, format!("https://github.com/{repo}/releases/latest"));
            }
            other => panic!("expected Available, got {:?}", outcome_label(&other)),
        }
    }

    #[tokio::test]
    async fn successful_check_advances_throttle_clock() {
        let path = temp_settings();
        let c = checker(&path, &release("v0.3.0", ""), "0.2.0");
        assert!(matches!(c.check(false).await, Outcome::Available { .. }));
        assert!(read_settings(&path).last_check_ms > 0);
    }

    #[tokio::test]
    async fn failed_manual_check_does_not_advance_throttle_clock() {
        let path = temp_settings();
        let mut c = UpdateChecker::new(path.clone());
        c.fetcher = Box::new(BodyFetcher { body: String::new(), err: Some("down".into()) });
        assert!(matches!(c.check(true).await, Outcome::Failed { .. }));
        assert_eq!(read_settings(&path).last_check_ms, 0);
    }

    #[test]
    fn settings_roundtrip_preserves_keys() {
        let path = temp_settings();
        write_settings(&path, &Settings {
            enabled: false,
            skipped_version: "v0.5.0".into(),
            last_check_ms: 123456789,
        });
        let loaded = read_settings(&path);
        assert!(!loaded.enabled);
        assert_eq!(loaded.skipped_version, "v0.5.0");
        assert_eq!(loaded.last_check_ms, 123456789);
    }

    fn outcome_label(outcome: &Outcome) -> &'static str {
        match outcome {
            Outcome::Available { .. } => "Available",
            Outcome::Current { .. } => "Current",
            Outcome::Failed { .. } => "Failed",
            Outcome::Quiet => "Quiet",
        }
    }
}
