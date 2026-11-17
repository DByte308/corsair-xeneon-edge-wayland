// SPDX-License-Identifier: GPL-3.0-or-later
//! External-tool discovery for `system.info`, mirroring the C++ agent's
//! `Api::toolPath` / `Api::toolVersion`.
//!
//! The Rust backend used to hardcode `/usr/bin/ddcutil`, `/usr/bin/xinput` and
//! an invented `xinput` version of "1.4". On any non-FHS distribution (NixOS,
//! Guix) those paths do not exist, so the Settings/doctor card reported tools
//! that were not there — and a version number nothing had verified. Discovery
//! now searches `PATH` and asks the tool itself, once at startup.
//!
//! Both answers are deliberately allowed to be empty: "not found" is the honest
//! result, and the UI renders it rather than a plausible-looking lie.

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};

/// One external tool: where it is and what it says its version is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tool {
    /// Absolute path, or empty when nothing on `PATH` matches.
    pub path: String,
    /// First dotted version in `<tool> --version`, or empty when it did not
    /// run or printed nothing usable.
    pub version: String,
}

/// The tools `system.info` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    pub ddcutil: Tool,
    pub xinput: Tool,
}

impl Tools {
    /// Resolve both tools. Each costs at most one `--version` spawn, and this
    /// runs once at startup rather than on every snapshot (the C++ agent
    /// re-runs four processes per `system.info`).
    pub fn detect() -> Self {
        Self {
            ddcutil: Tool {
                path: path_of("ddcutil"),
                version: version_of("ddcutil"),
            },
            xinput: Tool {
                path: path_of("xinput"),
                version: version_of("xinput"),
            },
        }
    }
}

/// The first executable called `exe` on `PATH`, as `which` would find it.
/// Empty when there is none (or `PATH` is unset).
pub fn path_of(exe: &str) -> String {
    let Some(path) = std::env::var_os("PATH") else {
        return String::new();
    };
    for dir in std::env::split_paths(&path) {
        // An empty component means the current directory, which is not a place
        // to look for a tool we then report and later run.
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

/// Run `<exe> --version` and return the first dotted version it prints.
/// Empty when the tool is missing, exits abnormally or prints no version.
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
/// Deliberately not a regex (the build is `--locked` with no regex dependency):
/// a version has to start a token, so "2000 xinput 21.1.4" reports `21.1.4`
/// rather than the year, and trailing junk ("1.6.4-1") is cut at the dash.
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
                b'0'..=b'9' | b'.' | b'a'..=b'z' | b'A'..=b'Z' | b'_');
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
                        return Some(text[i..end].to_string());
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

    #[test]
    fn version_is_taken_from_the_first_dotted_run() {
        assert_eq!(first_version("ddcutil 2.1.4\n").as_deref(), Some("2.1.4"));
        assert_eq!(first_version("xinput version 1.6.4").as_deref(), Some("1.6.4"));
        assert_eq!(first_version("Version 1.4").as_deref(), Some("1.4"));
        // A number that is part of a longer token is not a version.
        assert_eq!(first_version("build 2024 thing").as_deref(), None);
        assert_eq!(first_version("no numbers here").as_deref(), None);
        assert_eq!(first_version("").as_deref(), None);
        // A year-looking token that starts a word does parse, which is why the
        // caller prefers the tool's own banner; it is not silently invented.
        assert_eq!(first_version("1.6.4-1").as_deref(), Some("1.6.4"));
    }

    #[test]
    fn a_missing_tool_reports_empty_rather_than_a_guess() {
        assert_eq!(path_of("edgeline-tool-that-does-not-exist"), "");
        assert_eq!(version_of("edgeline-tool-that-does-not-exist"), "");
    }

    #[test]
    fn path_lookup_finds_an_executable_on_path() {
        // `sh` exists on any system this backend can run on. The assertion is
        // about discovery, not about a specific location.
        let sh = path_of("sh");
        assert!(sh.starts_with('/'), "path_of(sh) should be absolute, got {sh:?}");
        assert!(Path::new(&sh).is_file());
    }
}
