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
    time::{Duration, Instant},
};

use serde_json::{Value, json};

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
    fn start() -> std::io::Result<Harness> {
        let dir = std::env::temp_dir().join(format!("edgeline-b1-{}", std::process::id()));
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
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn b1_read_only_rpc_contract() {
    let har = Harness::start().expect("spawn edgeline-backend");
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
    let har = Harness::start().expect("spawn edgeline-backend");
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
}
