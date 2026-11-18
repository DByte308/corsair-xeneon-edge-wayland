// SPDX-License-Identifier: GPL-3.0-or-later
mod rendering;
mod rpc;
mod tablet;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::{io::BufReader, net::UnixStream};

const BRIDGE: &str = include_str!("../frontend-bridge.js");
struct Host {
    core: PathBuf,
    agent: PathBuf,
    /// The Node host-bridge socket: the app-facing methods that are neither
    /// device nor compositor concerns (registry, clipboard, MPRIS, telemetry,
    /// KDE Connect, window decoration, diagnostics). It reuses the tested
    /// `ui/host/*` modules, so the native host answers them exactly as the
    /// Electron host does instead of refusing them.
    apps: PathBuf,
    /// Webview label -> app id. One map, two readers: `apps.list` reports `open`
    /// from it and `app_call` takes the calling app's identity from it, so the
    /// list and the authorization can never disagree.
    app_windows: Mutex<std::collections::HashMap<String, String>>,
    config: PathBuf,
    prefs: Mutex<Value>,
    children: Mutex<Vec<Child>>,
    owns_core: AtomicBool,
    /// Route the web UI's whole `rpc` channel to the Rust backend instead of the
    /// C++ compatibility agent, and do not start the agent at all.
    ///
    /// This is the cutover switch: with the agent running, every datum the UI draws
    /// comes from it and the Rust backend is exercised only by the three newest
    /// calls (window feed, activation, launchpad), so a "passing" run proves
    /// nothing about the port. `EDGELINE_BACKEND=rust` is opt-in so the documented
    /// rollback stays the default.
    ///
    /// It moves three things, not one: the `rpc` data channel, the event/status
    /// channel (`agent_events`) and the agent's own start-up. Moving only `rpc`
    /// would leave the UI reporting "agent not running" while Rust serves it, and
    /// every `sensors`/`focus`/`ui.action` event would come from nobody.
    core_only: bool,
    connected: AtomicBool,
    quitting: AtomicBool,
    smoke: bool,
    smoke_reported: AtomicBool,
    suspended: Mutex<Vec<String>>,
    deck: tokio::sync::Mutex<Option<tablet::Deck>>,
}
/// Methods the bundled apps call that belong to the *host*, not to the device
/// backend. Served by `ui/host-bridge.js` over its own socket.
///
/// `apps.launch` is deliberately absent: the native host has no app windows yet
/// (only the main, home, settings, dashboard and taskbar windows), so launching
/// cannot work and pretending otherwise would be worse than refusing.
/// Injected into every app window before its own scripts run.
///
/// Exactly three functions, frozen and non-configurable, and the generic Tauri IPC
/// object is deleted afterwards: an app must not be able to reach the host's whole
/// command surface, only the three calls that are authorized per app. The `invoke`
/// handle is captured first — that is the only moment it is legitimately reachable.
const APP_BRIDGE: &str = r#"
(function () {
    var internals = window.__TAURI_INTERNALS__;
    var invoke = internals && internals.invoke ? internals.invoke.bind(internals) : null;
    // `delete` on a non-configurable property fails silently in sloppy mode, so it is
    // not enough on its own: try the delete, and if the object is still reachable,
    // overwrite the reference as well. Then report what actually happened — "we tried
    // to remove it" and "it is gone" are different promises.
    function strip(name) {
        try { delete window[name]; } catch (e) {}
        try {
            if (typeof window[name] !== 'undefined') {
                Object.defineProperty(window, name, {
                    value: undefined, writable: false, configurable: false,
                });
            }
        } catch (e) {}
    }
    // Tauri defines these non-configurable, so neither `delete` nor `defineProperty`
    // can remove them (measured: `typeof window.__TAURI_INTERNALS__` stays "object").
    // What can be taken away is the capability: replace the IPC entry point with a
    // stub that refuses, while our captured reference keeps working.
    strip('__TAURI_INTERNALS__');
    strip('__TAURI__');
    // Best effort on top: if the entry point happens to be writable, blunt it.
    var stub = 'not-writable';
    try {
        Object.defineProperty(internals || {}, 'invoke', {
            value: function () { throw new Error('generic IPC is disabled in app windows'); },
            writable: false, configurable: false,
        });
        stub = 'stubbed';
    } catch (e) { stub = 'not-writable'; }
    function need() {
        if (!invoke) throw new Error('host bridge unavailable');
        return invoke;
    }
    function call(method, args) {
        return need()('app_call', { method: String(method || ''), args: args && typeof args === 'object' ? args : {} });
    }
    function info() { return need()('app_info', {}); }
    function close() { return need()('app_close', {}); }
    Object.defineProperty(window, 'edgelineApp', {
        value: Object.freeze({ call: call, info: info, close: close }),
        writable: false, configurable: false, enumerable: true,
    });

    // The isolation claim, measured from inside the app window instead of asserted in
    // a commit message. `acl` is the second barrier: the generic command must be
    // refused for an app window even if the internals object were still reachable.
    function probe(callIt) {
        var result;
        try {
            result = callIt();
        } catch (e) {
            return Promise.resolve('blocked');
        }
        return Promise.resolve(result).then(
            function () { return 'REACHABLE'; },
            function (error) {
                var text = String((error && error.message) || error);
                return /not allowed|denied|ACL/i.test(text) ? 'refused' : 'error: ' + text;
            }
        );
    }

    // The isolation claim, measured from inside the app window. Two barriers are
    // checked separately: the ACL must refuse a forbidden command through our own
    // captured invoke, and also through the page-facing object if it is still there
    // (Tauri defines it non-configurable, so `delete`/`defineProperty` cannot remove
    // it — what matters is that it cannot be used).
    function report() {
        var descriptor = Object.getOwnPropertyDescriptor(window, 'edgelineApp') || {};
        var payload = {
            tauriGlobal: typeof window.__TAURI__,
            internals: typeof window.__TAURI_INTERNALS__,
            bridgeFrozen: Object.isFrozen(window.edgelineApp),
            bridgeLocked: descriptor.configurable === false,
            stub: stub,
            acl: 'unknown',
            genericIpc: 'unknown',
            events: 'unknown',
        };
        return probe(function () { return invoke('edgeline', { method: 'backend.info' }); })
            .then(function (verdict) {
                payload.acl = verdict;
                return probe(function () {
                    return window.__TAURI_INTERNALS__.invoke('edgeline', { method: 'backend.info' });
                });
            })
            .then(function (verdict) {
                payload.genericIpc = verdict;
                // The event bus is the other way out of a sandbox: listening leaks the
                // panel stream, emitting lets an app drive the shell.
                return probe(function () {
                    return window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
                        event: 'agent-event', target: { kind: 'Any' }, handler: 0,
                    });
                });
            })
            .then(function (verdict) {
                var listening = verdict;
                return probe(function () {
                    return window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
                        event: 'ui.action', payload: { action: 'dashboard-toggle' },
                    });
                }).then(function (emitting) {
                    payload.events = (listening === 'refused' && emitting === 'refused')
                        ? 'refused'
                        : 'listen=' + listening + ',emit=' + emitting;
                    return invoke('app_isolation', { report: payload });
                });
            });
    }
    report().catch(function () {});
})();
"#;

const APP_HOST_METHODS: [&str; 6] = [
    "apps.list",
    "clipboard.read",
    "media.nowPlaying",
    "system.telemetry",
    "kdeconnect.devices",
    "windows.list",
];

fn is_app_host_method(method: &str) -> bool {
    APP_HOST_METHODS.contains(&method) || method == "diagnostics.read"
}

/// The app id a webview label belongs to, from the one map that also decides
/// authorization. The label is set by this process, never by the page.
fn app_for_label(state: &Host, label: &str) -> Option<String> {
    state
        .app_windows
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(label)
        .cloned()
}

fn open_app_ids(state: &Host) -> Vec<String> {
    let mut ids: Vec<String> = state
        .app_windows
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Whether `rpc` goes to the Rust backend and the agent stays down.
///
/// Read once at startup from `EDGELINE_BACKEND=rust` — deliberately the *same*
/// variable and value the Electron host uses (`ui/main.js`), because two names for
/// one decision would mean two rollback instructions and one trap at the acceptance
/// run: wrong variable, old path, everything looks green.
fn core_only() -> bool {
    std::env::var("EDGELINE_BACKEND").as_deref() == Ok("rust")
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is required")?,
    ))
}
fn config_home() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".config")))
}
fn read_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .context("Missing string parameter")
}
fn edge(app: &AppHandle) -> Result<Option<Value>> {
    let monitors = app.available_monitors()?;
    let matches: Vec<_> = monitors
        .iter()
        .filter(|m| {
            m.size().width == 2560 && m.size().height == 720 && (m.scale_factor() - 1.).abs() < 0.01
        })
        .collect();
    if matches.len() != 1 {
        return Ok(None);
    }
    let m = matches[0];
    let scale = m.scale_factor();
    Ok(Some(
        json!({"bounds":{"x":m.position().x as f64/scale,"y":m.position().y as f64/scale,
        "width":m.size().width as f64/scale,"height":m.size().height as f64/scale},"scale":scale,
        "workArea":{"x":m.position().x as f64/scale+96.,"y":m.position().y as f64/scale,"width":m.size().width as f64/scale-96.,"height":m.size().height as f64/scale}}),
    ))
}
fn merge_prefs(current: &Value, change: &Value) -> Value {
    let mut next = current.clone();
    for key in ["theme", "page"] {
        if let Some(value) = change[key].as_str() {
            next[key] = json!(value);
        }
    }
    if change["tiles"].is_object() {
        next["tiles"] = change["tiles"].clone();
    }
    if ["bg", "card", "text"].iter().all(|key| {
        change["userTheme"][key].as_str().is_some_and(|s| {
            s.len() == 7
                && s.starts_with('#')
                && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        })
    }) {
        next["userTheme"] = change["userTheme"].clone();
    }
    if let Some(label) = change["deviceLabel"].as_str() {
        next["deviceLabel"] = json!(label.trim().chars().take(40).collect::<String>());
    }
    next
}
impl Host {
    fn new(smoke: bool) -> Result<Self> {
        let config = std::env::var_os("EDGELINE_RUST_CONFIG")
            .map(PathBuf::from)
            .unwrap_or(config_home()?.join("edgeline-rust"));
        let prefs = if config.join("panel.json").exists() {
            read_json(&config.join("panel.json"))
        } else {
            read_json(&config_home()?.join("edgeline/panel.json"))
        };
        Ok(Self {
            core: rpc::runtime()?.join("edgeline-rust/backend.sock"),
            agent: rpc::runtime()?.join("edgeline.sock"),
            apps: rpc::runtime()?.join("edgeline-host.sock"),
            app_windows: Mutex::new(std::collections::HashMap::new()),
            config,
            prefs: Mutex::new(prefs),
            children: Mutex::new(Vec::new()),
            owns_core: AtomicBool::new(false),
            core_only: core_only(),
            connected: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            smoke,
            smoke_reported: AtomicBool::new(false),
            suspended: Mutex::new(Vec::new()),
            deck: tokio::sync::Mutex::new(None),
        })
    }
    fn save(&self, prefs: Value) -> Result<()> {
        if self.smoke {
            bail!("Settings writes are disabled in smoke-test mode");
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&self.config)?;
        std::fs::set_permissions(&self.config, std::fs::Permissions::from_mode(0o700))?;
        let temporary = self
            .config
            .join(format!("panel-{}.tmp", std::process::id()));
        // Serialize writes and publication, so rapid tile changes cannot race rename.
        let mut current = self
            .prefs
            .lock()
            .map_err(|_| anyhow::anyhow!("Preference lock poisoned"))?;
        let prefs = merge_prefs(&current, &prefs);
        std::fs::write(&temporary, serde_json::to_vec_pretty(&prefs)?)?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(temporary, self.config.join("panel.json"))?;
        *current = prefs;
        Ok(())
    }
    async fn core(&self, method: &str, params: Value) -> Result<Value> {
        rpc::call(&self.core, method, params).await
    }
    async fn place(&self) -> Result<()> {
        self.core("launchpad.action", json!({"action":"panel"}))
            .await?;
        Ok(())
    }
    async fn start_core(&self, app: &AppHandle) -> Result<()> {
        if UnixStream::connect(&self.core).await.is_err() {
            let bundled = app.path().resource_dir()?.join("backend/edgeline-backend");
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../rust-backend/target/release/edgeline-backend");
            let binary = std::env::var_os("EDGELINE_RUST_BACKEND")
                .map(PathBuf::from)
                .unwrap_or(if bundled.exists() { bundled } else { source });
            let child = Command::new(binary)
                .args(["--parent-pid", &std::process::id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?;
            self.children.lock().unwrap().push(child);
            self.owns_core.store(true, Ordering::Relaxed);
            let mut ready = false;
            for _ in 0..50 {
                if self.core("backend.info", json!({})).await.is_ok() {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if !ready {
                bail!("Rust control backend did not start");
            }
        }
        self.core("controller.ensure", json!({})).await?;
        self.start_workarea(app)?;
        Ok(())
    }
    fn start_workarea(&self, app: &AppHandle) -> Result<()> {
        if self.smoke {
            return Ok(());
        }
        let bundled = app.path().resource_dir()?.join("backend/edgeline-reserve");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rust-backend/target/release/edgeline-reserve");
        let binary = std::env::var_os("EDGELINE_WORKAREA_HELPER")
            .map(PathBuf::from)
            .unwrap_or(if bundled.exists() { bundled } else { source });
        let child = Command::new(binary)
            .args(["--parent-pid", &std::process::id().to_string(), "--wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        self.children.lock().unwrap().push(child);
        Ok(())
    }
    /// One line at startup naming the data plane. The confusion this session spent
    /// the most time on was nobody being able to say who was serving the panel.
    fn announce_backend(&self) {
        let backend = if self.core_only { "rust" } else { "agent" };
        eprintln!(
            "EdgeLine host: backend={backend} core={} agent={}",
            self.core.display(),
            self.agent.display()
        );
    }

    /// Start the Node host bridge if it is not already listening.
    fn ensure_host_bridge(&self, app: &AppHandle) -> Result<()> {
        if self.smoke || std::os::unix::net::UnixStream::connect(&self.apps).is_ok() {
            return Ok(());
        }
        let bundled = app.path().resource_dir()?.join("host-bridge.js");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ui/host-bridge.js");
        let script = if bundled.exists() { bundled } else { source };
        if !script.exists() {
            bail!("host bridge script is missing at {}", script.display());
        }
        let node = std::env::var_os("EDGELINE_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("node"));
        let child = Command::new(node)
            .arg(&script)
            .arg(&self.apps)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("cannot start the host bridge (is node installed?)")?;
        self.children.lock().unwrap().push(child);
        // Wait briefly so the first app call does not race the listener.
        for _ in 0..60 {
            if self.apps.exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("host bridge did not create {}", self.apps.display())
    }

    fn ensure_agent(&self, app: &AppHandle) -> Result<()> {
        if self.core_only {
            // No agent at all: the Rust backend owns the panel in this mode, and a
            // second hardware owner would contend for the i2c bus and the
            // dev.edgeline.Windows name.
            return Ok(());
        }
        if self.smoke || std::os::unix::net::UnixStream::connect(&self.agent).is_ok() {
            return Ok(());
        }
        let bundled = app.path().resource_dir()?.join("agent/edgeline-agent");
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../agent/build/edgeline-agent");
        let child = Command::new(if bundled.exists() { bundled } else { source })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        self.children.lock().unwrap().push(child);
        Ok(())
    }
    async fn shutdown(&self) {
        if self.owns_core.load(Ordering::Relaxed) {
            let _ = self.core("backend.stop", json!({})).await;
        }
        if let Some(mut deck) = self.deck.lock().await.take() {
            deck.stop().await;
        }
        for child in self.children.lock().unwrap().iter_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
fn create_window(
    app: &AppHandle,
    label: &str,
    page: &str,
    title: &str,
    rail: bool,
    smoke: bool,
) -> Result<WebviewWindow> {
    let bounds =
        edge(app)?.unwrap_or_else(|| json!({"bounds":{"x":0.,"y":0.,"width":2560.,"height":720.}}));
    let b = &bounds["bounds"];
    let x = b["x"].as_f64().unwrap_or(0.) + if rail { 0. } else { 96. };
    let width = if rail {
        96.
    } else {
        b["width"].as_f64().unwrap_or(2560.) - 96.
    };
    let window = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("renderer/{page}").into()),
    )
    .title(title)
    .position(x, b["y"].as_f64().unwrap_or(0.))
    .inner_size(width, b["height"].as_f64().unwrap_or(720.))
    .decorations(false)
    .resizable(true)
    .min_inner_size(
        if rail { 96. } else { 940. },
        if rail { 100. } else { 620. },
    )
    .skip_taskbar(label != "main")
    .always_on_top(label != "main")
    .focusable(label == "main")
    .focused(label == "main" && !smoke)
    .visible(false)
    .initialization_script(if smoke {
        format!("window.__EDGE_SMOKE__=true;\n{BRIDGE}")
    } else {
        BRIDGE.to_owned()
    })
    .on_page_load(move |window, payload| {
        if smoke && payload.event() == tauri::webview::PageLoadEvent::Finished {
            let _ = window.eval(include_str!("../smoke-test.js"));
        }
    })
    .build()?;
    if !smoke && edge(app)?.is_some() {
        window.show()?;
    }
    Ok(window)
}
async fn show_home(app: &AppHandle, state: &Host, on: bool) -> Result<Value> {
    if !on {
        if let Some(w) = app.get_webview_window("dashboard") {
            w.hide()?;
        }
        emit_shell(&app, "dashboard-status", json!({"open":false}));
        return Ok(json!({"ok":true}));
    }
    if edge(app)?.is_none() {
        return Ok(json!({"ok":false,"error":"The Edge is disconnected"}));
    }
    if let Some(w) = app.get_webview_window("main") {
        w.hide()?;
    }
    if let Some(w) = app.get_webview_window("dashboard") {
        w.show()?;
    } else {
        create_window(
            app,
            "dashboard",
            "dashboard.html",
            "EdgeLine dashboard",
            false,
            false,
        )?;
    }
    state.place().await?;
    emit_shell(&app, "dashboard-status", json!({"open":true}));
    Ok(json!({"ok":true}))
}
async fn show_settings(app: &AppHandle, state: &Host) -> Result<Value> {
    if edge(app)?.is_none() {
        return Ok(json!({"ok":false,"error":"The Edge is disconnected"}));
    }
    if let Some(w) = app.get_webview_window("main") {
        w.show()?;
        w.set_focus()?;
    } else {
        create_window(app, "main", "index.html", "EdgeLine", false, false)?;
    }
    state.place().await?;
    Ok(json!({"ok":true}))
}
async fn external(url: &str) -> Result<Value> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        bail!("Only HTTP(S) links are allowed");
    }
    let mut child = tokio::process::Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tauri::async_runtime::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(json!(true))
}
/// Launch an app: resolve it, create its window with the injected bridge, then ask
/// the compositor to place it.
async fn launch_app(app: &AppHandle, state: &Arc<Host>, id: &str) -> Result<Value> {
    // `rpc::call` returns the reply's `result` already, not the envelope.
    let resolved = rpc::call(&state.apps, "apps.resolve", json!({ "id": id })).await?;
    if resolved.is_null() {
        bail!("unknown app: {id}");
    }
    let app_id = string(&resolved, "id")?.to_string();
    let name = string(&resolved, "name")?.to_string();
    let entry = string(&resolved, "entry")?.to_string();
    let width = resolved["window"]["width"].as_f64().unwrap_or(1180.0);
    let height = resolved["window"]["height"].as_f64().unwrap_or(680.0);
    let resizable = resolved["window"]["resizable"].as_bool().unwrap_or(true);

    let label = format!("app-{app_id}");
    if let Some(existing) = app.get_webview_window(&label) {
        // Reuse rather than duplicate; Wayland will not raise on focus alone.
        existing.show()?;
        let _ = existing.set_focus();
        return Ok(json!({ "id": app_id, "opened": true, "reused": true }));
    }

    // Where the staged page lives, relative to web-dist: the resolver hands back an
    // absolute path in the source tree, and only the part after `/apps/` is inside
    // the bundle.
    let relative = entry
        .rfind("/apps/")
        .map(|at| format!("apps/{}", &entry[at + "/apps/".len()..]))
        .with_context(|| format!("app entry is not under an apps/ directory: {entry}"))?;

    state
        .app_windows
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(label.clone(), app_id.clone());

    let window = match WebviewWindowBuilder::new(app, &label, WebviewUrl::App(relative.into()))
        .title(&name)
        .inner_size(width, height)
        .resizable(resizable)
        .decorations(false)
        // Not skip-taskbar: the window feed drops those, and the feed is how the
        // compositor id for placement is found. An app window is a real Edge window,
        // which is also what the rail and the overview expect to show.
        .skip_taskbar(false)
        .always_on_top(false)
        .initialization_script(APP_BRIDGE)
        .data_directory(data_home()?.join("dev.edgeline.Ctl").join("apps").join(&app_id))
        // Navigation only: the app's own page is what it may load. Sub-resources
        // are not covered here — see PROJECT_STATUS on the network[] gap.
        .on_navigation(|url| {
            matches!(url.scheme(), "tauri" | "about")
                || url.host_str() == Some("tauri.localhost")
        })
        .build()
    {
        Ok(window) => window,
        Err(error) => {
            state
                .app_windows
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&label);
            return Err(error).with_context(|| format!("cannot open a window for {app_id}"));
        }
    };
    let _ = window.show();

    // Placement is the compositor's job. Find the window by app id in the feed and
    // hand its compositor id to the backend, which owns the KWin script machinery.
    let app_handle = app.clone();
    let state_for_place = state.clone();
    let id_for_place = app_id.clone();
    let name_for_place = name.clone();
    tauri::async_runtime::spawn(async move {
        // A new Wayland window lands on the active output, and the feed only reports
        // Edge windows — so before placement there may be no compositor id to match.
        // The script therefore matches on the title and a class that belongs to this
        // host, which is what the Electron host does too. The lookup below upgrades
        // that to the id once the window is in the feed, which is stricter and keeps
        // working if an app renames its own caption later.
        let rect = edge_rect(&app_handle).unwrap_or_else(|| json!({}));
        let _ = rpc::call(
            &state_for_place.core,
            "app.place",
            json!({ "title": name_for_place, "rect": rect }),
        )
        .await;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(150));
            let Ok(snapshot) = rpc::call(&state_for_place.core, "window.snapshot", json!({})).await
            else {
                continue;
            };
            let windows = snapshot["windows"].as_array().cloned().unwrap_or_default();
            if let Some(compositor_id) = windows
                .iter()
                .find(|w| w["title"].as_str() == Some(name_for_place.as_str()))
                .and_then(|w| w["id"].as_str())
            {
                let _ = rpc::call(
                    &state_for_place.core,
                    "app.place",
                    json!({ "id": compositor_id, "rect": rect }),
                )
                .await;
                return;
            }
        }
        eprintln!("app {id_for_place}: not in the window feed after 6 s (placed by title)");
    });

    Ok(json!({ "id": app_id, "opened": true, "reused": false }))
}

/// The rect an app may use: the Edge output minus the 96-pixel rail.
fn edge_rect(app: &AppHandle) -> Option<Value> {
    let monitor = edge(app).ok().flatten()?;
    let x = monitor["bounds"]["x"].as_f64().unwrap_or(0.0);
    let y = monitor["bounds"]["y"].as_f64().unwrap_or(0.0);
    let width = monitor["bounds"]["width"].as_f64().unwrap_or(2560.0);
    let height = monitor["bounds"]["height"].as_f64().unwrap_or(720.0);
    Some(json!({ "x": x + 96.0, "y": y, "width": (width - 96.0).max(96.0), "height": height }))
}

fn data_home() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".local/share")))
}

/// Authorize one app call: the registry supplies which capability a method needs,
/// this process decides whether the app has it, and a bridge that cannot answer
/// means "denied" rather than "allowed".
async fn authorize_app_call(state: &Host, app_id: &str, method: &str) -> Result<()> {
    let resolved = rpc::call(&state.apps, "apps.resolve", json!({ "id": app_id })).await?;
    if resolved.is_null() {
        bail!("Not an app window");
    }
    let required = rpc::call(
        &state.apps,
        "apps.requiredCapability",
        json!({ "method": method }),
    )
    .await
    .map_err(|error| anyhow::anyhow!("host bridge unavailable, denying: {error}"))?;
    if required.is_null() {
        bail!("unknown method: {method}");
    }
    if required["implemented"].as_bool() != Some(true) {
        bail!("{method} is not implemented in this build");
    }
    let capability = string(&required, "capability")?;
    let granted = resolved["capabilities"]
        .as_array()
        .map(|caps| caps.iter().any(|c| c.as_str() == Some(capability)))
        .unwrap_or(false);
    if !granted {
        bail!("{app_id} is not granted {capability}");
    }
    Ok(())
}

#[tauri::command]
async fn app_call(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
    method: String,
    args: Value,
) -> std::result::Result<Value, String> {
    app_call_inner(&app, &window, state.inner(), &method, args)
        .await
        .map_err(|error| error.to_string())
}

async fn app_call_inner(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &Arc<Host>,
    method: &str,
    args: Value,
) -> Result<Value> {
    let app_id = app_for_label(state, window.label())
        .ok_or_else(|| anyhow::anyhow!("Not an app window"))?;
    authorize_app_call(state, &app_id, method).await?;
    // The bridge reaches the same host methods the panel uses; the device calls come
    // from the backend.
    if is_app_host_method(method) || method == "media.command" {
        return host_bridge_call(app, state, method, &json!({ "params": args })).await;
    }
    let socket = if state.core_only { &state.core } else { &state.agent };
    rpc::call(socket, method, args).await
}

/// Receives the app window's own isolation report and puts it in the host log, so the
/// acceptance run has a line to grep instead of a claim to believe.
#[tauri::command]
async fn app_isolation(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
    report: Value,
) -> std::result::Result<Value, String> {
    let known = app_for_label(state.inner(), window.label()).is_some();
    if !known {
        return Err("Not an app window".to_string());
    }
    // `internals` is expected to stay "object" (non-configurable); what must hold is
    // that its IPC entry point is blocked and the ACL refuses the generic command.
    let ok = report["tauriGlobal"] == "undefined"
        && report["bridgeFrozen"] == true
        && report["bridgeLocked"] == true
        && report["acl"] == "refused"
        && report["genericIpc"] == "refused"
        && report["events"] == "refused";
    eprintln!(
        "app isolation {} [{}]: {}",
        if ok { "ok" } else { "VIOLATION" },
        window.label(),
        report
    );
    Ok(json!({ "ok": ok }))
}

#[tauri::command]
async fn app_info(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
) -> std::result::Result<Value, String> {
    app_info_inner(&window, state.inner())
        .await
        .map_err(|error| error.to_string())
}

async fn app_info_inner(
    window: &WebviewWindow,
    state: &Arc<Host>,
) -> Result<Value> {
    let app_id = app_for_label(state, window.label())
        .ok_or_else(|| anyhow::anyhow!("Not an app window"))?;
    let resolved = rpc::call(&state.apps, "apps.resolve", json!({ "id": app_id })).await?;
    Ok(json!({
        "id": resolved["id"],
        "name": resolved["name"],
        "capabilities": resolved["capabilities"],
    }))
}

#[tauri::command]
async fn app_close(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
) -> std::result::Result<Value, String> {
    let label = window.label().to_string();
    let known = app_for_label(state.inner(), &label).is_some();
    let state = state.inner();
    state
        .app_windows
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&label);
    let _ = window.destroy();
    Ok(json!({ "closed": known }))
}

#[tauri::command]
async fn edgeline(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
    method: String,
    params: Value,
) -> std::result::Result<Value, String> {
    dispatch(&app, &window, state.inner(), &method, params)
        .await
        .map_err(|e| e.to_string())
}
async fn dispatch(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &Arc<Host>,
    method: &str,
    params: Value,
) -> Result<Value> {
    if state.smoke
        && !matches!(
            method,
            "host.test-report"
                | "rpc"
                | "agent-connected"
                | "app-info"
                | "edge-display"
                | "autostart-get"
                | "device-label-get"
                | "ripple"
        )
    {
        bail!("Host mutation disabled during smoke testing");
    }
    match method {
        "host.test-report" if state.smoke => {
            let ok = params["api"] == true
                && params["runtime"] == "Tauri"
                && params["pictureLength"].as_u64().unwrap_or(0) > 100
                && params["agentState"] == true
                && params.get("error").is_none()
                && params["errors"].as_array().is_some_and(Vec::is_empty);
            println!("{}", json!({"tauriSmoke":ok,"report":params}));
            state.smoke_reported.store(true, Ordering::Relaxed);
            app.exit(if ok { 0 } else { 1 });
            Ok(json!(ok))
        }
        "rpc" => {
            let name = string(&params, "method")?;
            if state.smoke
                && !matches!(
                    name,
                    "state.all"
                        | "profiles.list"
                        | "sensors.gpus"
                        | "sensors.interfaces"
                        | "touch.state"
                )
            {
                bail!("Mutation RPC disabled during smoke testing");
            }
            if name == "apps.launch" {
                return launch_app(&app, state, string(&params, "id").unwrap_or_default()).await;
            }
            if is_app_host_method(name) {
                // Host responsibilities, answered by the bridge in both modes so
                // the bundled apps behave the same whichever backend serves the
                // panel.
                return host_bridge_call(&app, state, name, &params).await;
            }
            // The cutover switch: the data plane is the Rust backend when asked for
            // it, otherwise the agent that still owns it today.
            let socket = if state.core_only {
                &state.core
            } else {
                &state.agent
            };
            rpc::call(socket, name, params["params"].clone()).await
        }
        "agent-connected" => Ok(json!({
            "connected": state.connected.load(Ordering::Relaxed),
            "socket": if state.core_only { &state.core } else { &state.agent },
            "core": state.core_only,
        })),
        "app-info" => {
            let distro = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
            let prefs = state.prefs.lock().unwrap().clone();
            Ok(
                json!({"runtime":"Tauri","version":env!("CARGO_PKG_VERSION"),"panelPrefs":prefs,"edgePresent":edge(app)?.is_some(),
                "prefersDark":!matches!(window.theme(),Ok(tauri::Theme::Light)),"distro":if distro.contains("fedora")||distro.contains("nobara"){"fedora"}else if distro.contains("nixos"){"nixos"}else{"ubuntu"},
                "electron":null,"chrome":null,"node":null,"startTab":null,"startTheme":null,"panelTheme":null,"panelPage":null}),
            )
        }
        "edge-display" => Ok(edge(app)?.unwrap_or(Value::Null)),
        "edge-windows" => state.core("window.snapshot", json!({})).await,
        "edge-window-activate" => {
            let result = state.core("window.activate", params).await?;
            for id in ["main", "dashboard"] {
                if let Some(w) = app.get_webview_window(id) {
                    w.set_always_on_top(false)?;
                }
            }
            Ok(result)
        }
        "launchpad-action" => {
            let action = string(&params, "action")?.to_owned();
            let result = state.core("launchpad.action", params).await?;
            if ["fit", "fit-vanguard", "pick"].contains(&action.as_str())
                && result["cancelled"] != true
                && let Some(w) = app.get_webview_window("dashboard")
            {
                w.set_always_on_top(false)?;
            }
            Ok(result)
        }
        "edge-taskbar-action" => match string(&params, "action")? {
            "home" => show_home(app, state, true).await,
            "settings" => show_settings(app, state).await,
            _ => bail!("Unknown taskbar action"),
        },
        "dashboard" => show_home(app, state, params["on"].as_bool().unwrap_or(false)).await,
        "dashboard-state" => Ok(
            json!({"open":app.get_webview_window("dashboard").is_some_and(|w|w.is_visible().unwrap_or(false))}),
        ),
        "dashboard-layout" => {
            state.save(params["layout"].clone())?;
            // Shell-only, like every other host event: `app.emit` would also reach
            // app windows, and "host events never cross into an app window" should be
            // true by structure, not by a permission that a later edit could widen.
            emit_shell(
                &app,
                "agent-event",
                json!({"event":"dashboard","data":params["layout"]}),
            );
            Ok(json!(true))
        }
        "device-label-get" => Ok(json!(
            state.prefs.lock().unwrap()["deviceLabel"]
                .as_str()
                .unwrap_or("")
        )),
        "device-label-set" => {
            let label: String = string(&params, "label")?.trim().chars().take(40).collect();
            state.save(json!({"deviceLabel":label}))?;
            Ok(json!(label))
        }
        "tablet-deck-open" => {
            let mut deck = state.deck.lock().await;
            if deck.is_none() {
                *deck = Some(tablet::Deck::start().await?);
            }
            Ok(deck.as_ref().unwrap().connection.clone())
        }
        "window-action" => {
            match string(&params, "action")? {
                "close" => window.hide()?,
                "minimize" => window.minimize()?,
                "maximize" => {
                    if window.is_maximized()? {
                        window.unmaximize()?;
                    } else {
                        window.maximize()?;
                    }
                }
                _ => bail!("Unknown window action"),
            };
            Ok(json!(true))
        }
        "open-external" => external(string(&params, "url")?).await,
        "autostart-get" => {
            Ok(json!({"enabled":config_home()?.join("autostart/edgeline-rust.desktop").exists()}))
        }
        "autostart-set" => {
            if state.smoke {
                bail!("Autostart writes disabled in smoke mode");
            }
            let file = config_home()?.join("autostart/edgeline-rust.desktop");
            let on = params["on"].as_bool().unwrap_or(false);
            if on {
                std::fs::create_dir_all(file.parent().unwrap())?;
                let exe = std::env::current_exe()?
                    .to_string_lossy()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
                    .replace('`', "\\`");
                std::fs::write(
                    file,
                    format!(
                        "[Desktop Entry]\nType=Application\nName=EdgeLine Rust\nExec=\"{exe}\" --hidden\nTerminal=false\n"
                    ),
                )?;
            } else if file.exists() {
                std::fs::remove_file(file)?;
            }
            Ok(json!({"enabled":on}))
        }
        "ripple" if params["on"] != true => Ok(json!({"ok":true})),
        "close-calibration" => Ok(json!(true)),
        "open-calibration" | "ripple" => {
            bail!("This Wayland feature has not been ported to the Rust host")
        }
        _ => bail!("Unsupported host method: {method}"),
    }
}
/// Facts only the host has: which app windows are open, and the host block the
/// diagnostics schema expects. Assembled here rather than in the bridge, so the
/// bridge stays a thin adapter over the tested `ui/host/*` modules.
fn host_bridge_context(app: &AppHandle, state: &Host) -> Value {
    // The same map that authorizes `app_call`, so "listed as open" and "is an app
    // window" cannot disagree.
    let open_apps: Vec<String> = open_app_ids(state);
    let version = app.package_info().version.to_string();
    // The diagnostics schema wants the host's display list; without it the doctor
    // app reports "Edge display — not found" on a machine that has one.
    let displays: Vec<Value> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let size = monitor.size();
            let position = monitor.position();
            let name = monitor.name().cloned().unwrap_or_default();
            let is_edge = monitor.size().width == 2560
                && monitor.size().height == 720
                && (monitor.scale_factor() - 1.).abs() < 0.01;
            json!({
                "id": name,
                "label": name,
                "edge": is_edge,
                "size": { "width": size.width, "height": size.height },
                "bounds": {
                    "x": position.x, "y": position.y,
                    "width": size.width, "height": size.height,
                },
                "workArea": {
                    "x": position.x, "y": position.y,
                    "width": size.width, "height": size.height,
                },
                "scaleFactor": monitor.scale_factor(),
                "internal": false,
            })
        })
        .collect();
    json!({
        "openApps": open_apps,
        "diagnostics": {
            "railWidth": 96,
            "displays": displays,
            // The doctor matches a display by id; the Edge is the one marked above.
            "edgeId": displays
                .iter()
                .find(|d| d["edge"].as_bool() == Some(true))
                .and_then(|d| d["id"].as_str())
                .map(str::to_string),
            "host": {
                "runtime": "Tauri",
                "version": version,
                "pid": std::process::id(),
                "session": std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into()),
                "desktop": std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into()),
                "backend": if state.core_only { "rust" } else { "agent" },
                "safeMode": false,
                "rendering": std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").unwrap_or_default(),
            },
        },
    })
}

async fn host_bridge_call(
    app: &AppHandle,
    state: &Host,
    method: &str,
    params: &Value,
) -> Result<Value> {
    let context = host_bridge_context(app, state);
    rpc::call_with_context(&state.apps, method, params["params"].clone(), context).await
}

/// Shell windows. Host events go to these by name rather than with `emit`, which
/// would also reach app windows — and an app has no business reading the panel event
/// stream (focus, touch, sensors) or writing events the shell acts on.
const SHELL_WINDOWS: [&str; 3] = ["main", "dashboard", "taskbar"];

fn emit_shell(app: &AppHandle, event: &str, payload: Value) {
    for label in SHELL_WINDOWS {
        let _ = app.emit_to(label, event, payload.clone());
    }
}

fn agent_events(app: AppHandle, state: Arc<Host>) {
    tauri::async_runtime::spawn(async move {
        while !state.quitting.load(Ordering::Relaxed) {
            let socket = if state.core_only {
                &state.core
            } else {
                &state.agent
            };
            if let Ok(stream) = UnixStream::connect(socket).await {
                state.connected.store(true, Ordering::Relaxed);
                emit_shell(&app, "agent-status", json!({"connected":true}));
                let mut reader = BufReader::new(stream);
                while let Ok(Some(line)) = rpc::line(&mut reader).await {
                    if let Ok(value) = serde_json::from_slice::<Value>(&line)
                        && value.get("event").is_some()
                    {
                        emit_shell(&app, "agent-event", value);
                    }
                }
            }
            state.connected.store(false, Ordering::Relaxed);
            emit_shell(&app, "agent-status", json!({"connected":false}));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
fn watch_displays(app: AppHandle, state: Arc<Host>) {
    tauri::async_runtime::spawn(async move {
        let mut previous = edge(&app).ok().flatten();
        while !state.quitting.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = edge(&app).ok().flatten();
            if current == previous {
                continue;
            }
            emit_shell(&app, "displays-changed", json!({"edgePresent":current.is_some()}));
            if current.is_none() {
                let mut visible = Vec::new();
                for label in ["main", "dashboard", "taskbar"] {
                    if let Some(window) = app.get_webview_window(label) {
                        if window.is_visible().unwrap_or(false) {
                            visible.push(label.to_owned());
                        }
                        let _ = window.hide();
                    }
                }
                *state.suspended.lock().unwrap() = visible;
            } else {
                if app.get_webview_window("taskbar").is_none() {
                    let _ = create_window(
                        &app,
                        "taskbar",
                        "taskbar.html",
                        "EdgeLine taskbar",
                        true,
                        false,
                    );
                }
                let visible = std::mem::take(&mut *state.suspended.lock().unwrap());
                for label in visible
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("taskbar"))
                {
                    if let Some(window) = app.get_webview_window(label) {
                        let _ = window.show();
                    }
                }
                let _ = state.place().await;
            }
            previous = current;
        }
    });
}
fn install_tray(app: &AppHandle) -> Result<()> {
    use tauri::menu::{Menu, MenuItem};
    let home = MenuItem::with_id(app, "home", "Home", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit EdgeLine Rust", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&home, &settings, &quit])?;
    tauri::tray::TrayIconBuilder::with_id("edgeline-rust")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../../ui/assets/icons/24x24.png"
        ))?)
        .tooltip("EdgeLine Rust")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let app = app.clone();
            let id = event.id().as_ref().to_owned();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<Arc<Host>>();
                match id.as_str() {
                    "home" => {
                        let _ = show_home(&app, &state, true).await;
                    }
                    "settings" => {
                        let _ = show_settings(&app, &state).await;
                    }
                    "quit" => app.exit(0),
                    _ => {}
                }
            });
        })
        .build(app)?;
    Ok(())
}
fn main() {
    rendering::prepare();
    let smoke = std::env::args().any(|s| s == "--smoke-test");
    let hidden = std::env::args().any(|s| s == "--hidden");
    // A second native host must not create duplicate taskbars or own a shared backend.
    let _instance_lock = if smoke {
        None
    } else {
        use std::os::unix::fs::OpenOptionsExt;
        let path = rpc::runtime()
            .expect("User runtime directory")
            .join("edgeline-tauri.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .expect("Native host lock");
        if let Err(error) = file.try_lock() {
            eprintln!("EdgeLine Rust is already running or its lock is unavailable: {error}");
            std::process::exit(1);
        }
        Some(file)
    };
    // Same flag the Electron host accepts, so a run can open one app without a
    // pointer (and the acceptance run does not depend on clicking the rail).
    let open_app = std::env::args()
        .position(|arg| arg == "--open-app")
        .and_then(|at| std::env::args().nth(at + 1))
        .or_else(|| std::env::var("EDGELINE_OPEN_APP").ok())
        .filter(|id| !id.is_empty());
    let state = Arc::new(Host::new(smoke).expect("Initialize Rust host"));
    let setup_state = state.clone();
    let app = tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            edgeline,
            app_call,
            app_info,
            app_close,
            app_isolation
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Arc<Host>>();
                if !state.quitting.load(Ordering::Relaxed) {
                    api.prevent_close();
                    if window.label() != "taskbar" {
                        let _ = window.hide();
                    }
                }
            }
            if let tauri::WindowEvent::ThemeChanged(theme) = event {
                let _ = window.emit(
                    "system-theme",
                    json!({"prefersDark":!matches!(theme,tauri::Theme::Light)}),
                );
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            setup_state.announce_backend();
            setup_state.ensure_agent(&handle)?;
            if let Err(error) = setup_state.ensure_host_bridge(&handle) {
                // App-only surface: the panel and the device pages work without it.
                eprintln!("host bridge unavailable: {error}");
            }
            agent_events(handle.clone(), setup_state.clone());
            if let Some(id) = open_app.clone() {
                let app_for_open = handle.clone();
                let state_for_open = setup_state.clone();
                tauri::async_runtime::spawn(async move {
                    // Give the panel a moment to appear before an app window is placed
                    // next to it.
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    match launch_app(&app_for_open, &state_for_open, &id).await {
                        Ok(result) => eprintln!("opened app: {result}"),
                        Err(error) => eprintln!("cannot open app {id}: {error}"),
                    }
                });
            }
            if smoke {
                create_window(
                    &handle,
                    "main",
                    "index.html",
                    "EdgeLine Rust smoke",
                    false,
                    true,
                )?;
                let app = handle.clone();
                let state = setup_state.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(25)).await;
                    if !state.smoke_reported.load(Ordering::Relaxed) {
                        eprintln!("Tauri smoke timed out");
                        app.exit(1);
                    }
                });
            } else {
                install_tray(&handle)?;
                let state = setup_state.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = state.start_core(&handle).await {
                        eprintln!("Rust backend: {error}");
                        return;
                    }
                    watch_displays(handle.clone(), state.clone());
                    if edge(&handle).ok().flatten().is_some() {
                        let _ = create_window(
                            &handle,
                            "taskbar",
                            "taskbar.html",
                            "EdgeLine taskbar",
                            true,
                            false,
                        );
                        if !hidden {
                            let _ = show_settings(&handle, &state).await;
                        }
                        let _ = state.place().await;
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Build Tauri host");
    app.run(move |app, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event
            && !state.quitting.swap(true, Ordering::Relaxed)
        {
            api.prevent_exit();
            let app = app.clone();
            let state = state.clone();
            let code = code.unwrap_or(0);
            tauri::async_runtime::spawn(async move {
                state.shutdown().await;
                app.exit(code);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_reject_partial_colors() {
        let original = json!({"page":"launchpad","userTheme":{"bg":"#112233","card":"#334455","text":"#ffffff"}});
        let changed = merge_prefs(
            &original,
            &json!({"page":"tablet-deck","userTheme":{"bg":"bad"}}),
        );
        assert_eq!(changed["page"], "tablet-deck");
        assert_eq!(changed["userTheme"], original["userTheme"]);
    }
}
