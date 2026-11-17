// SPDX-License-Identifier: GPL-3.0-or-later
//! B1: Rust-side integration test (replaces ui/test/rust-backend.integration.js).
//!
//! Spawns the real `edgeline-backend` binary on a private Unix socket and talks
//! to it as a peer, mirroring the Node test's read-only assertions:
//!   - `backend.info` returns a Rust backend;
//!   - ten concurrent requests succeed;
//!   - `launchpad.action artwork` inlines a JPEG data URL or null;
//!   - invalid `window.activate` (not a UUID) is rejected;
//!   - an unsupported method is rejected;
//!   - (live, `EDGELINE_INTEGRATION_LIVE=1`) the real KWin window snapshot is
//!     readable and only KWin may supply window reports.
//!
//! The live half needs a connected, unambiguous 2560×720 Edge and the current
//! user's KDE session bus; it is gated so the rest runs offline.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::UnixStream,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// Unique directory per spawned harness, so parallel tests never share a socket.
/// The backend also refuses to bind a socket that is already owned, so a shared
/// path would make the second backend exit and swallow the first's socket.
static HARNESS_DIR_N: AtomicU64 = AtomicU64::new(0);

fn unique_dir(name: &str) -> PathBuf {
    let n = HARNESS_DIR_N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("edgeline-b1-{}-{n}", name))
}

/// A tiny JSON-newline RPC client over a Unix socket, same wire format as the
/// C++ agent and the Rust backend's `serve`.
struct Client {
    stream: UnixStream,
    next_id: u64,
}

impl Client {
    fn connect(path: &PathBuf) -> std::io::Result<Client> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(Client { stream, next_id: 1 })
    }

    fn call(&mut self, method: &str, params: Value) -> std::io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "id": id, "method": method, "params": params });
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        self.stream.write_all(&line)?;

        let mut reader = BufReader::new(&self.stream);
        let mut got = String::new();
        reader.read_line(&mut got)?;
        Ok(serde_json::from_str(&got)?)
    }
}

struct Harness {
    child: Child,
    dir: PathBuf,
}

impl Harness {
    /// Spawn the real backend binary on a freshly created private socket.
    fn start(name: &str) -> std::io::Result<Harness> {
        let dir = unique_dir(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let socket = dir.join("backend.sock");
        let bin = env!("CARGO_BIN_EXE_edgeline-backend");
        let child = Command::new(bin)
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(Harness { child, dir })
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("backend.sock")
    }

    /// The spawned backend process id, used to derive its DBus bus name
    /// (dev.edgeline.Windows.rust.p{backend_pid}).
    fn backend_pid(&self) -> u32 {
        self.child.id()
    }

    /// Remove the backend's own `scripts-{pid}` runtime dir (belt-and-suspenders
    /// in case the graceful SIGTERM did not run its cleanup). Only touches the
    /// dir for this exact backend pid, never another live backend's.
    fn cleanup_runtime_dir(&self) {
        let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") else {
            return;
        };
        let dir = PathBuf::from(runtime)
            .join("edgeline-rust")
            .join(format!("scripts-{}", self.backend_pid()));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Wait until the socket accepts a connection (the backend prints a ready
    /// line after binding). Bound so a failed start fails fast.
    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(self.socket()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("edgeline-backend did not become ready");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Graceful shutdown so the backend cleans up its own runtime dir on the
        // SIGTERM path (main.rs removes the socket and its `scripts-*` tree on
        // SIGTERM/ctrl-c). SIGKILL without a timeout would orphan them.
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        // Bound the graceful wait; if it is hung, escalate to SIGKILL.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && self.child.try_wait().ok().flatten().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.cleanup_runtime_dir();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn b1_read_only_rpc_contract() {
    let har = Harness::start("read_only").expect("spawn edgeline-backend");
    har.wait_ready();
    let mut client = Client::connect(&har.socket()).expect("connect to backend socket");

    // 1. backend.info identifies a Rust backend.
    let info = client.call("backend.info", json!({})).unwrap();
    assert_eq!(info["result"]["language"], "Rust");
    assert_eq!(info["result"]["name"], "edgeline-backend");

    // 2. Ten concurrent requests. The backend caps in-flight calls at 32; issue
    //    ten independent commands and confirm each gets an answer, mirroring the
    //    Node test's Promise.all({length:10}).
    let mut clients: Vec<Client> = Vec::new();
    for _ in 0..10 {
        clients.push(Client::connect(&har.socket()).unwrap());
    }
    for c in clients.iter_mut() {
        let r = c.call("backend.info", json!({})).unwrap();
        assert_eq!(r["result"]["language"], "Rust");
    }

    // 3. Artwork is inlined only when available; otherwise null. Never a junk
    //    URL and never an error.
    let art = client.call("launchpad.action", json!({ "action": "artwork" })).unwrap();
    let image = &art["result"]["image"];
    assert!(
        image.is_null() || image.as_str().map_or(false, |s| s.starts_with("data:image/jpeg;base64,")),
        "artwork must be null or a JPEG data URL, got {art}"
    );

    // 4. Invalid window.activate (not a UUID) is rejected.
    let bad = client.call("window.activate", json!({ "id": "not-a-uuid" })).unwrap();
    assert!(
        bad.get("error").is_some(),
        "window.activate with a non-UUID must be rejected, got {bad}"
    );

    // 5. An unsupported method is rejected.
    let unsup = client.call("run-arbitrary-command", json!({})).unwrap();
    assert!(
        unsup.get("error").is_some(),
        "unsupported method must be rejected, got {unsup}"
    );
}

/// Live half, gated behind EDGELINE_INTEGRATION_LIVE=1. Requires a connected,
/// unambiguous 2560×720 Edge and the current user's KDE session bus.
#[test]
fn b1_live_kwin_window_feed() {
    if std::env::var("EDGELINE_INTEGRATION_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping b1_live_kwin_window_feed (set EDGELINE_INTEGRATION_LIVE=1)");
        return;
    }
    let har = Harness::start("live").expect("spawn edgeline-backend");
    har.wait_ready();
    let mut client = Client::connect(&har.socket()).expect("connect to backend socket");

    // Start the window feed and read a real snapshot. The snapshot is only
    // `available` once KWin has reported a valid 2560×720 Edge.
    client.call("window.start", json!({})).unwrap();
    let mut snapshot = Value::Null;
    for _ in 0..30 {
        snapshot = client.call("window.snapshot", json!({})).unwrap();
        if snapshot["result"]["available"].as_bool().unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        snapshot["result"]["available"].as_bool().unwrap_or(false),
        "connect a unique 2560×720 Edge for this integration test"
    );

    // Concurrent read-only requests against the live feed.
    let mut clients: Vec<Client> = Vec::new();
    for _ in 0..10 {
        clients.push(Client::connect(&har.socket()).unwrap());
    }
    for c in clients.iter_mut() {
        let r = c.call("backend.info", json!({})).unwrap();
        assert_eq!(r["result"]["language"], "Rust");
    }

    // Only KWin may supply window reports. An untrusted DBus caller must be
    // rejected by the backend's sender check (kwin.rs header.sender() vs the
    // registered KWin owner). The bus name the backend owns is
    // dev.edgeline.Windows.rust.p{backend_pid}; send a foreign Update and
    // require a refusal. Mirrors busctl --user call?
    //   busctl --user call dev.edgeline.Windows.rust.p{pid}
    //       /Windows dev.edgeline.Windows Update s '<payload>'
    let bus_name = format!("dev.edgeline.Windows.rust.p{}", har.backend_pid());
    let payload = r#"{"available":false,"windows":[]}"#;
    let out = std::process::Command::new("busctl")
        .args(["--user", "call", &bus_name, "/Windows", "dev.edgeline.Windows", "Update", "s", payload])
        .output();
    // This is the live-gated test: a real desktop (KDE) session always has
    // busctl, so a missing tool must not be silently skipped (that would hide a
    // coverage hole). Fail loudly instead.
    match out {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let denied = stderr.contains("Only KWin")
                || stderr.contains("Access denied")
                || stderr.contains("AccessDenied");
            assert!(
                denied,
                "foreign window report must be denied, got stderr: {stderr}"
            );
        }
        Err(error) => panic!(
            "untrusted-DBus denial could not be checked: busctl not available ({error})"
        ),
    }
}
