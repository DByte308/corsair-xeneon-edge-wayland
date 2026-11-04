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
    config: PathBuf,
    prefs: Mutex<Value>,
    children: Mutex<Vec<Child>>,
    owns_core: AtomicBool,
    connected: AtomicBool,
    quitting: AtomicBool,
    smoke: bool,
    smoke_reported: AtomicBool,
    suspended: Mutex<Vec<String>>,
    deck: tokio::sync::Mutex<Option<tablet::Deck>>,
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
            config,
            prefs: Mutex::new(prefs),
            children: Mutex::new(Vec::new()),
            owns_core: AtomicBool::new(false),
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
        Ok(())
    }
    fn ensure_agent(&self, app: &AppHandle) -> Result<()> {
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
        app.emit("dashboard-status", json!({"open":false}))?;
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
    app.emit("dashboard-status", json!({"open":true}))?;
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
#[tauri::command]
async fn edgeline(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
    method: String,
    params: Value,
) -> std::result::Result<Value, String> {
    dispatch(&app, &window, &state, &method, params)
        .await
        .map_err(|e| e.to_string())
}
async fn dispatch(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &Host,
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
            rpc::call(&state.agent, name, params["params"].clone()).await
        }
        "agent-connected" => {
            Ok(json!({"connected":state.connected.load(Ordering::Relaxed),"socket":state.agent}))
        }
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
            app.emit(
                "agent-event",
                json!({"event":"dashboard","data":params["layout"]}),
            )?;
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
fn agent_events(app: AppHandle, state: Arc<Host>) {
    tauri::async_runtime::spawn(async move {
        while !state.quitting.load(Ordering::Relaxed) {
            if let Ok(stream) = UnixStream::connect(&state.agent).await {
                state.connected.store(true, Ordering::Relaxed);
                let _ = app.emit("agent-status", json!({"connected":true}));
                let mut reader = BufReader::new(stream);
                while let Ok(Some(line)) = rpc::line(&mut reader).await {
                    if let Ok(value) = serde_json::from_slice::<Value>(&line)
                        && value.get("event").is_some()
                    {
                        let _ = app.emit("agent-event", value);
                    }
                }
            }
            state.connected.store(false, Ordering::Relaxed);
            let _ = app.emit("agent-status", json!({"connected":false}));
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
            let _ = app.emit("displays-changed", json!({"edgePresent":current.is_some()}));
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
    let state = Arc::new(Host::new(smoke).expect("Initialize Rust host"));
    let setup_state = state.clone();
    let app = tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![edgeline])
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
            setup_state.ensure_agent(&handle)?;
            agent_events(handle.clone(), setup_state.clone());
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
