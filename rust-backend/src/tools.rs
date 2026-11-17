// SPDX-License-Identifier: GPL-3.0-or-later
//! External-tool discovery for `system.info`, mirroring the C++ agent's
//! `Api::toolPath` / `Api::toolVersion`.
//!
//! The Rust backend used to hardcode `/usr/bin/ddcutil`, `/usr/bin/xinput` and
//! an invented `xinput` version of "1.4". On any non-FHS distribution (NixOS,
//! Guix) those paths do not exist, and on the development host `xinput` is not
//! installed at all, so the reported version was fiction — the Settings pane
//! showed a tool that was not there. Discovery now searches `PATH` and asks the
//! tool itself, once at startup.
//!
//! Both answers are deliberately allowed to be empty: "not found" is the honest
//! result, and the UI renders that as `not found` rather than a
//! plausible-looking lie.

use std::{
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};

/// One external tool: where it is and what it says its version is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tool {
    /// Absolute path, or empty when nothing on `PATH` matches.
    pub path: String,
    /// First version in `<tool> --version`, or empty when it did not run or
    /// printed nothing usable.
    pub version: String,
}

/// The tools `system.info` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    pub ddcutil: Tool,
    pub xinput: Tool,
}

impl Tools {
    /// Resolve both tools. This runs once at startup rather than on every
    /// snapshot (the C++ agent re-runs four processes per `system.info`), and
    /// the version is read from the binary we are about to report, so the two
    /// halves of the answer cannot disagree.
    pub fn detect() -> Self {
        Self {
            ddcutil: locate("ddcutil"),
            xinput: locate("xinput"),
        }
    }
}

/// Path plus version for one tool.
fn locate(exe: &str) -> Tool {
    let path = path_of(exe);
    let version = version_of(if path.is_empty() { exe } else { &path });
    Tool { path, version }
}

/// The first executable called `exe` on `PATH`, as `which` would find it.
/// Empty when there is none (or `PATH` is unset).
pub fn path_of(exe: &str) -> String {
    find_in_path(exe, std::env::var_os("PATH").as_deref())
}

/// The lookup behind [`path_of`], with `PATH` passed in so the rules can be
/// exercised without mutating the process environment (tests run in parallel,
/// and `set_var` is process-global).
///
/// An empty component means "the current directory". Those are skipped: a tool
/// found there would be reported by absolute path but run by bare name, and
/// `execvp` *does* honour an empty component — so skipping is a deliberate,
/// documented divergence from `which` in the safe direction. The two agree on
/// everything else: first match wins, and a directory, a non-executable file or
/// a dangling symlink is not a match.
fn find_in_path(exe: &str, path: Option<&OsStr>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(exe);
        if is_executable_file(&candidate) {
            return candidate.display().to_string();
        }
    }
    String::new()
}

fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Run `<exe> --version` and return the first version it prints. Empty when the
/// tool is missing, exits abnormally or prints no version.
pub fn version_of(exe: &str) -> String {
    let output = match Command::new(exe).arg("--version").output() {
        Ok(output) => output,
        Err(_) => return String::new(),
    };
    // Both tools write the banner on stdout, but ddcutil has moved it between
    // releases; reading both channels costs nothing.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    first_version(&text).unwrap_or_default()
}

/// The first `major.minor[.patch]` run in `text`, as its own token.
///
/// Deliberately not a regex (the build is `--locked` with no regex dependency).
/// A version has to start a token, so `xinput-1.6.4` reports `1.6.4`, and a
/// candidate after `:` or `/` is not considered — that keeps an IP address
/// (`192.168.1.50`) or a URL path (`/2.0/download`) from being read as the
/// version. A dotted run with four or more groups is treated as an address and
/// skipped entirely. A trailing suffix is cut at the first non-digit
/// (`1.6.4-1ubuntu1` reports `1.6.4`).
///
/// Residual ambiguity, accepted: a bare year-first date (`2024.01.15`) is
/// shaped exactly like a version. Neither tool prints one before its version,
/// and the alternative — matching only on the line naming the executable — would
/// need the caller to pass a name the banner may spell differently.
pub fn first_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let digits_at = |mut i: usize| {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > start {
            Some((&text[start..i], i))
        } else {
            None
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let starts_token = i == 0
            || !matches!(bytes[i - 1],
                b'0'..=b'9' | b'.' | b'a'..=b'z' | b'A'..=b'Z' | b'_' | b':' | b'/');
        if starts_token && bytes[i].is_ascii_digit() {
            if let Some((_, next)) = digits_at(i) {
                if bytes.get(next) == Some(&b'.') {
                    if let Some((_, next)) = digits_at(next + 1) {
                        let mut end = next;
                        // An optional patch level only counts if it is whole.
                        if bytes.get(end) == Some(&b'.') {
                            if let Some((_, patch_end)) = digits_at(end + 1) {
                                end = patch_end;
                            }
                        }
                        // A fourth group means this is an address, not a version.
                        let address = bytes.get(end) == Some(&b'.')
                            && digits_at(end + 1).is_some();
                        if !address {
                            return Some(text[i..end].to_string());
                        }
                        i = end;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn version_is_taken_from_the_first_dotted_run() {
        assert_eq!(first_version("ddcutil 2.1.4\n").as_deref(), Some("2.1.4"));
        assert_eq!(first_version("xinput version 1.6.4").as_deref(), Some("1.6.4"));
        assert_eq!(first_version("Version 1.4").as_deref(), Some("1.4"));
        assert_eq!(first_version("xinput-1.6.4").as_deref(), Some("1.6.4"));
        // Suffixes and junk after the number are cut, not glued on.
        assert_eq!(first_version("xinput 1.6.4-1ubuntu1").as_deref(), Some("1.6.4"));
        assert_eq!(first_version("1.2.3abc").as_deref(), Some("1.2.3"));
        // A number that is part of a longer token is not a version.
        assert_eq!(first_version("build 2024 thing").as_deref(), None);
        assert_eq!(first_version("no numbers here").as_deref(), None);
        assert_eq!(first_version("v1.2").as_deref(), None);
        assert_eq!(first_version("lib1.2.3").as_deref(), None);
        assert_eq!(first_version("").as_deref(), None);
    }

    /// The failure mode that matters: a string that looks like a version and is
    /// not. An IP or a URL path before the real version must not win.
    #[test]
    fn addresses_and_url_paths_are_not_read_as_versions() {
        assert_eq!(
            first_version("connecting to 192.168.1.50 port 3389\nversion 1.6.4").as_deref(),
            Some("1.6.4")
        );
        assert_eq!(
            first_version("Server 127.0.0.1:5900 ready version 1.6").as_deref(),
            Some("1.6")
        );
        assert_eq!(
            first_version("https://example.com/2.0/download xinput 1.6.4").as_deref(),
            Some("1.6.4")
        );
        // A four-group run is an address even on its own.
        assert_eq!(first_version("10.0.0.1").as_deref(), None);
        assert_eq!(first_version("1.2.3.4").as_deref(), None);
        // ddcutil's real banner, copyright line and all.
        assert_eq!(
            first_version("ddcutil 2.2.1\nCopyright (C) 2015-2025 Sanford Rockowitz").as_deref(),
            Some("2.2.1")
        );
    }

    #[test]
    fn a_missing_tool_reports_empty_rather_than_a_guess() {
        assert_eq!(path_of("edgeline-tool-that-does-not-exist"), "");
        assert_eq!(version_of("edgeline-tool-that-does-not-exist"), "");
    }

    /// `find_in_path` in full: first match wins, and only a real executable
    /// file counts. Written against fixtures so it fails if the lookup is
    /// replaced by a bare `which` spawn or loses an executable-bit check.
    ///
    /// Note what is deliberately NOT asserted here: that an empty `PATH`
    /// component (which means the current directory) is skipped. Proving it
    /// needs a tool in the cwd, and `set_current_dir` is process-global in a
    /// parallel harness, so the doc comment on `find_in_path` is the statement
    /// of record for that rule instead of an assertion that cannot fail.
    #[test]
    fn path_lookup_requires_a_real_executable_and_takes_the_first() {
        let root = TestDir::new();
        let (first, second) = (root.path.join("first"), root.path.join("second"));
        for dir in [&first, &second] {
            fs::create_dir_all(dir).unwrap();
        }
        let exe = "edgeline-fixture-tool";
        // Both directories hold the tool: the earlier PATH entry must win.
        write_exe(&first.join(exe), "#!/bin/sh\necho first 1.0.0\n");
        write_exe(&second.join(exe), "#!/bin/sh\necho second 2.0.0\n");
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            find_in_path(exe, Some(&path)),
            first.join(exe).display().to_string()
        );

        // Each of these is a plausible mistake that must not match.
        let only_bad_dir = TestDir::new();
        let only_bad = &only_bad_dir.path;
        fs::write(only_bad.join("edgeline-fixture-plain"), "not executable").unwrap();
        fs::create_dir(only_bad.join("edgeline-fixture-dir")).unwrap();
        let dangling = only_bad.join("edgeline-fixture-dangling");
        symlink(only_bad.join("nowhere"), &dangling).unwrap();
        let path = std::env::join_paths([only_bad]).unwrap();
        for name in [
            "edgeline-fixture-plain",
            "edgeline-fixture-dir",
            "edgeline-fixture-dangling",
        ] {
            assert_eq!(find_in_path(name, Some(&path)), "", "{name} must not match");
        }

        // A tool that exists nowhere on PATH, and no PATH at all.
        assert_eq!(find_in_path(exe, Some(OsStr::new(""))), "");
        assert_eq!(find_in_path(exe, None), "");
    }

    fn write_exe(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A unique temp dir, removed on drop so repeated suite runs do not litter
    /// /tmp (and so a failing assertion still cleans up).
    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            // pid + nanos + counter: unique even when tests run in parallel.
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "edgeline-tools-{}-{nanos}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
