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
    /// Persistent reader over a clone of `stream`. The backend interleaves
    /// unsolicited `{event,data}` lines with replies, so a fresh `BufReader` per
    /// call would both swallow buffered bytes and return an event where a reply
    /// was expected.
    reader: BufReader<UnixStream>,
    /// Events skipped while waiting for a reply, handed out by `next_event`
    /// first. Whether a broadcast lands before or after its own reply is not
    /// guaranteed: an arm that awaits between the two can flip the order, and a
    /// client that dropped these would then miss an event it asked about.
    pending_events: Vec<Value>,
    next_id: u64,
}

impl Client {
    fn connect(path: &PathBuf) -> std::io::Result<Client> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Client {
            stream,
            reader,
            pending_events: Vec::new(),
            next_id: 1,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> std::io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "id": id, "method": method, "params": params });
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        self.stream.write_all(&line)?;

        // An event that lands between the request and the reply is not the
        // reply; keep reading until the line is not an event, but keep what was
        // skipped so a later `next_event` can still see it.
        loop {
            let value = self.read_line()?;
            if value.get("event").is_none() {
                return Ok(value);
            }
            self.pending_events.push(value);
        }
    }

    /// Read the next unsolicited event, discarding any replies in between.
    fn next_event(&mut self) -> std::io::Result<Value> {
        if !self.pending_events.is_empty() {
            return Ok(self.pending_events.remove(0));
        }
        loop {
            let value = self.read_line()?;
            if value.get("event").is_some() {
                return Ok(value);
            }
        }
    }

    fn read_line(&mut self) -> std::io::Result<Value> {
        let mut got = String::new();
        if self.reader.read_line(&mut got)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "backend closed the socket",
            ));
        }
        Ok(serde_json::from_str(&got)?)
    }

    fn set_read_timeout(&self, timeout: Duration) -> std::io::Result<()> {
        self.stream.set_read_timeout(Some(timeout))
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

    /// Whether the backend is still running, for failure messages. Reads
    /// `/proc` rather than `try_wait`, which would need `&mut` and could reap it.
    fn alive(&self) -> bool {
        std::path::Path::new(&format!("/proc/{}", self.backend_pid())).exists()
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
        image
            .is_null()
            || image
                .as_str()
                .map_or(false, |s| s.starts_with("data:image/jpeg;base64,")),
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

    // 6. A dashboard action belongs to the UI process (the dashboard is a
    //    window, not a device), so it is relayed as `ui.action` and answered
    //    normally — not rejected as an unknown KWin shortcut.
    let relay = client
        .call("launchpad.action", json!({ "action": "dashboard-toggle" }))
        .unwrap();
    assert!(relay.get("error").is_none(), "{relay}");
    assert_eq!(relay["result"]["relayed"], json!(true), "{relay}");
    // Other events (the DDC queue draining) can arrive first; look for ours.
    client.set_read_timeout(Duration::from_secs(2)).unwrap();
    let mut relayed = None;
    while let Ok(event) = client.next_event() {
        if event["event"] == "ui.action" {
            relayed = Some(event);
            break;
        }
    }
    let relayed = relayed.expect("a dashboard action must be relayed as ui.action");
    assert_eq!(relayed["data"]["action"], "dashboard-toggle", "{relayed}");

    // 7. An action outside the vocabulary is refused by name, not turned into a
    //    KWin request (the controller script must not load for a typo).
    let bad_action = client
        .call("launchpad.action", json!({ "action": "definitely-not-an-action" }))
        .unwrap();
    assert_eq!(
        bad_action["error"]["message"],
        "Unknown launchpad action",
        "{bad_action}"
    );
}

/// The only ticked broadcast. `sensors.stream` must deliver `{event:"sensors"}`
/// snapshots, and each snapshot must carry the same keys as `state.all.sensors`:
/// the raw snapshot hardcodes placeholder media fields, so an unmerged event
/// blanks the dashboard's now-playing tile and notification badge every tick.
#[test]
fn b1_sensors_stream_broadcasts_merged_snapshots() {
    let har = Harness::start("events").expect("spawn edgeline-backend");
    har.wait_ready();
    let mut client = Client::connect(&har.socket()).expect("connect to backend socket");

    let started = client
        .call("sensors.stream", json!({ "enabled": true, "intervalMs": 250 }))
        .unwrap();
    assert_eq!(started["result"]["streaming"], json!(true), "{started}");

    let event = client.next_event().expect("a sensors event must arrive");
    assert_eq!(event["event"], "sensors", "{event}");
    let payload = event["data"].clone();

    // The event has to look exactly like the snapshot `state.all` serves, media
    // included. Compared key-for-key so a field added to one path only fails.
    let all = client.call("state.all", json!({})).unwrap();
    let reference = all["result"]["sensors"].clone();
    let mut event_keys: Vec<&String> =
        payload.as_object().expect("object snapshot").keys().collect();
    let mut reference_keys: Vec<&String> =
        reference.as_object().expect("object snapshot").keys().collect();
    event_keys.sort();
    reference_keys.sort();
    assert_eq!(event_keys, reference_keys, "event keys must match state.all.sensors");

    // The media keys are the ones the raw snapshot fakes; they must carry the
    // live values, not the placeholders. `nowPlaying` is compared field by field
    // minus `positionUs`/`art`, which `read_now_playing` re-reads on every call
    // and so legitimately differ between the two snapshots.
    for key in ["notifications", "notifyActive", "notifyError", "sessionBusOk"] {
        assert_eq!(
            payload.get(key),
            reference.get(key),
            "'{key}' in the sensors event must be the live media value"
        );
    }
    let playing = payload["nowPlaying"].as_object().expect("nowPlaying object");
    let reference_playing = reference["nowPlaying"]
        .as_object()
        .expect("nowPlaying object in state.all");
    for key in ["valid", "player", "title", "artist", "album", "status", "lengthUs"] {
        assert_eq!(
            playing.get(key),
            reference_playing.get(key),
            "nowPlaying.{key} in the sensors event must be the live media value"
        );
    }
    assert!(
        playing.contains_key("valid"),
        "nowPlaying must be a media object, not a placeholder: {payload}"
    );

    // Disabling and re-timing must interrupt the worker's wait. Raise the
    // interval to 3 s, drain, then lower it to 250 ms: an event within ~1.5 s
    // is only possible if the sleep was cut short.
    let slow = client
        .call("sensors.stream", json!({ "enabled": true, "intervalMs": 3000 }))
        .unwrap();
    assert_eq!(slow["result"]["streaming"], json!(true), "{slow}");
    client.set_read_timeout(Duration::from_millis(400)).unwrap();
    while client.next_event().is_ok() {}
    client
        .call("sensors.stream", json!({ "enabled": true, "intervalMs": 250 }))
        .unwrap();
    client.set_read_timeout(Duration::from_millis(1500)).unwrap();
    let quick = client
        .next_event()
        .expect("a shortened interval must deliver an event at once, not after the old 3 s");
    assert_eq!(quick["event"], "sensors", "{quick}");

    // After a disable the stream must stop. One event can still be in flight
    // (the worker may have been polling when the request landed); a second one
    // means the worker kept its timer.
    let stopped = client
        .call("sensors.stream", json!({ "enabled": false }))
        .unwrap();
    assert_eq!(stopped["result"]["streaming"], json!(false), "{stopped}");
    client.set_read_timeout(Duration::from_millis(1200)).unwrap();
    let mut stragglers = 0;
    loop {
        match client.next_event() {
            Ok(event) => {
                if event["event"] == "sensors" {
                    stragglers += 1;
                }
            }
            // Timed out: the socket is quiet apart from unrelated events.
            Err(_) => break,
        }
    }
    assert!(
        stragglers <= 1,
        "a disable must stop the sensors stream, saw {stragglers} events after it"
    );
}

/// A disconnected client must release its socket immediately. The event
/// forwarder owns the write half, so if it is left parked in `recv()` the socket
/// is never closed and every finished connection leaks one fd.
#[test]
fn b1_closed_connections_release_their_sockets() {
    let har = Harness::start("fds").expect("spawn edgeline-backend");
    har.wait_ready();
    let fd_dir = format!("/proc/{}/fd", har.backend_pid());
    // One sample is not enough on a loaded machine: `/proc/<pid>/fd` can be
    // momentarily unreadable, and that is a harness failure, not a leak. Retry,
    // and keep the reason for the panic message.
    let sample_fds =
        || std::fs::read_dir(&fd_dir).map(|dir| dir.count()).map_err(|e| e.to_string());
    let open_fds = || {
        for _ in 0..25 {
            if let Ok(count) = sample_fds() {
                return count;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        0
    };

    // Warm up: served connections, so lazily created state is in the baseline.
    for _ in 0..2 {
        let mut c = Client::connect(&har.socket()).unwrap();
        c.call("backend.info", json!({})).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    let baseline = open_fds();
    assert!(
        baseline > 0,
        "cannot read {fd_dir} (pid {} alive={}, last error {:?})",
        har.backend_pid(),
        har.alive(),
        sample_fds().err()
    );

    for _ in 0..20 {
        let mut c = Client::connect(&har.socket()).unwrap();
        c.call("backend.info", json!({})).unwrap();
        drop(c);
    }
    // Give the accept loop and the aborted forwarders a moment to unwind.
    std::thread::sleep(Duration::from_secs(1));
    let after = open_fds();
    assert!(
        after <= baseline + 2,
        "closed connections leaked fds: baseline {baseline}, after 20 connect/close cycles {after}"
    );
}

/// The screen cache must be warm for a client that connects immediately, because
/// the UI connects milliseconds after `ready` and its first `state.all` races the
/// (slow) panel read. Either the read already happened, or a `screen` event
/// arrives shortly after — both leave the client with a usable picture page. With
/// neither, `page-picture.js` renders the brightness slider disabled and nothing
/// in the UI ever calls `screen.state`, so the control stays dead.
///
/// Live half: needs a connected, accessible Edge (the HID read is real).
#[test]
fn b1_live_screen_cache_is_warm_for_an_early_client() {
    if std::env::var("EDGELINE_INTEGRATION_LIVE").as_deref() != Ok("1") {
        eprintln!(
            "skipping b1_live_screen_cache_is_warm_for_an_early_client \
             (set EDGELINE_INTEGRATION_LIVE=1)"
        );
        return;
    }
    let har = Harness::start("screen_warm").expect("spawn edgeline-backend");
    har.wait_ready();
    // Connect at t≈0, the way the host does.
    let mut client = Client::connect(&har.socket()).expect("connect to backend socket");

    let all = client.call("state.all", json!({})).unwrap();
    if all["result"]["screen"]["available"].as_bool().unwrap_or(false) {
        return;
    }
    // Cold on the first answer is allowed: the seed read is still running. What
    // must not happen is that nobody ever tells us.
    client.set_read_timeout(Duration::from_secs(5)).unwrap();
    let mut warmed = false;
    while let Ok(event) = client.next_event() {
        if event["event"] == "screen" {
            assert!(
                event["data"]["available"].as_bool().unwrap_or(false),
                "the screen event must carry the read values, got {event}"
            );
            warmed = true;
            break;
        }
    }
    assert!(
        warmed,
        "a client connected at startup must be told the screen cache is warm; \
         got neither a warm state.all nor a screen event"
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
    // Up to ~10 s: the budget is for a busy machine, not for a slow compositor.
    // The startup HID seed read makes every test backend ~1 s slower to warm up.
    for _ in 0..100 {
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
        .args([
            "--user",
            "call",
            &bus_name,
            "/Windows",
            "dev.edgeline.Windows",
            "Update",
            "s",
            payload,
        ])
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
