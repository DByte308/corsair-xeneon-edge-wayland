// SPDX-License-Identifier: GPL-3.0-or-later
use crate::model::{Snapshot, canonical_uuid, shortcut_suffix};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use zbus::{Connection, Proxy, message::Header, zvariant::OwnedValue};

const CONTROLLER: &str = include_str!("../../ui/kwin-launchpad.js");
const OVERVIEW: &str = include_str!("../../ui/kwin-window-overview.js");
const PLACEMENT: &str = include_str!("../../ui/kwin-place-panel.js");
const ACTIVATION: &str = include_str!("../../ui/kwin-activate-window.js");

struct Feed {
    snapshot: Arc<RwLock<Snapshot>>,
    owner: Arc<RwLock<String>>,
    connection: Connection,
    /// Set on the first report that carries the active window.
    focus_watching: Arc<std::sync::atomic::AtomicBool>,
    /// When the last report arrived, for the stall watchdog.
    last_report: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// The channel the rules handler listens on. Focus comes from the compositor
    /// because Wayland has no other place that can see every window (the C++
    /// agent refused there).
    focus_events: tokio::sync::mpsc::UnboundedSender<crate::focus::Focused>,
}

impl Feed {
    /// Whether the caller is KWin itself.
    ///
    /// Resolved from the bus, not only from the cached owner: the cache is filled
    /// by `ensure_scripts`, and focus reports arrive before anything has called
    /// it — trusting the cache alone rejected real KWin traffic at startup. A
    /// mismatch re-resolves once, which also survives a KWin restart (its unique
    /// name changes, the cache still holds the old one).
    async fn from_kwin(&self, header: &Header<'_>) -> bool {
        let Some(sender) = header.sender().map(|s| s.to_string()) else {
            return false;
        };
        if self.owner.read().await.as_str() == sender {
            return true;
        }
        let dbus = match Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        {
            Ok(proxy) => proxy,
            Err(_) => return false,
        };
        let resolved: Result<String, _> = dbus.call("GetNameOwner", &("org.kde.KWin",)).await;
        let Ok(resolved) = resolved else {
            return false;
        };
        *self.owner.write().await = resolved.clone();
        resolved == sender
    }
}

#[zbus::interface(name = "dev.edgeline.Windows")]
impl Feed {
    async fn update(
        &self,
        payload: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !self.from_kwin(&header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "Only KWin may publish windows".into(),
            ));
        }
        *self.last_report.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());
        let snapshot = Snapshot::parse(payload)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        *self.snapshot.write().await = snapshot;
        // The same report carries the active window: focus rides the feed rather
        // than a second script, so there is one trusted channel and one load path.
        if let Ok(value) = serde_json::from_str::<Value>(payload) {
            if let Some(focus) = value.get("focus") {
                let focused = crate::focus::Focused::from_json(focus);
                self.focus_watching.store(true, Ordering::Relaxed);
                let _ = self.focus_events.send(focused);
            }
        }
        Ok(())
    }

}

pub struct Kwin {
    connection: Connection,
    snapshot: Arc<RwLock<Snapshot>>,
    owner: Arc<RwLock<String>>,
    directory: PathBuf,
    bus_name: String,
    prefix: String,
    scripts: Arc<Mutex<Vec<String>>>,
    sequence: AtomicU64,
    gate: Mutex<()>,
    picker: Mutex<()>,
    /// Whether a focus report has arrived from the feed, i.e. whether rules can
    /// actually arm. `rules.get` reports this as `watching`.
    focus_watching: Arc<std::sync::atomic::AtomicBool>,
    /// When the window feed was last asked for, and when it last reported. A
    /// KWin script can load and never execute (measured: above ~4 concurrent
    /// deploys, 2 of 5 silently did nothing), and without these two stamps the
    /// result is an empty window list forever with no error anywhere.
    feed_started: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    last_report: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl Kwin {
    /// Build a `Kwin`; focus reports from the KWin script arrive on `focus_events`.
    pub async fn new(
        directory: &Path,
        focus_events: tokio::sync::mpsc::UnboundedSender<crate::focus::Focused>,
    ) -> Result<Self> {
        let connection = Connection::session().await?;
        let bus_name = format!("dev.edgeline.Windows.rust.p{}", std::process::id());
        let snapshot = Arc::new(RwLock::new(Snapshot::default()));
        let owner = Arc::new(RwLock::new(String::new()));
        let focus_watching = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let feed_started = Arc::new(std::sync::Mutex::new(None));
        let last_report = Arc::new(std::sync::Mutex::new(None));
        connection
            .object_server()
            .at(
                "/Windows",
                Feed {
                    snapshot: snapshot.clone(),
                    owner: owner.clone(),
                    connection: connection.clone(),
                    focus_watching: focus_watching.clone(),
                    last_report: last_report.clone(),
                    focus_events,
                },
            )
            .await?;
        connection.request_name(bus_name.as_str()).await?;
        let kwin = Self {
            connection,
            snapshot,
            owner,
            directory: directory.to_owned(),
            bus_name,
            prefix: format!("EdgeLineRust{}_", std::process::id()),
            scripts: Arc::new(Mutex::new(Vec::new())),
            sequence: AtomicU64::new(0),
            gate: Mutex::new(()),
            picker: Mutex::new(()),
            focus_watching,
            feed_started: feed_started.clone(),
            last_report: last_report.clone(),
        };
        // Scripts a previous process left loaded would keep their bus names (and
        // KWin would hand back the old script for a reused tag), so clear them
        // before this process deploys anything.
        kwin.unload_stale_scripts().await;
        Ok(kwin)
    }


    /// Whether the feed has reported the active window since startup.
    pub fn focus_watching(&self) -> bool {
        self.focus_watching.load(Ordering::Relaxed)
    }

    async fn scripting(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(
            &self.connection,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .await?)
    }

    async fn owner(&self) -> Result<String> {
        let dbus = Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        Ok(dbus.call("GetNameOwner", &("org.kde.KWin",)).await?)
    }

    /// Where the tags this backend loaded are recorded, so a later process can
    /// unload what a crash left behind.
    fn script_state_path(&self) -> PathBuf {
        self.directory
            .parent()
            .unwrap_or(&self.directory)
            .join("kwin-scripts.json")
    }

    fn recorded_tags(&self) -> Vec<String> {
        std::fs::read_to_string(self.script_state_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            .unwrap_or_default()
    }

    fn write_recorded_tags(&self, tags: &[String]) {
        if let Ok(text) = serde_json::to_string(tags) {
            let _ = std::fs::write(self.script_state_path(), text);
        }
    }

    /// Unload scripts a previous process left loaded in KWin.
    ///
    /// KWin keys a script by its tag: loading a tag it already knows returns the
    /// *old* script and never evaluates the new file. A crashed process therefore
    /// poisons that tag for every later run, which is what silently kept the focus
    /// watcher from ever running. Tags are unique per process, and this drains the
    /// leftovers.
    pub async fn unload_stale_scripts(&self) {
        let tags = self.recorded_tags();
        if tags.is_empty() {
            return;
        }
        let Ok(scripting) = self.scripting().await else {
            return;
        };
        for tag in &tags {
            let loaded: zbus::Result<bool> = scripting.call("isScriptLoaded", &(tag.as_str(),)).await;
            if matches!(loaded, Ok(true)) {
                let _: zbus::Result<()> = scripting.call("unloadScript", &(tag.as_str(),)).await;
            }
        }
        self.write_recorded_tags(&[]);
    }

    async fn deploy(&self, tag: &str, source: &str) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.directory.join(format!("{tag}.js"));
        std::fs::write(&path, source)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let scripting = self.scripting().await?;
        // A script left loaded by an earlier process (crash, SIGKILL, or a
        // timeout in development) keeps the tag, and KWin refuses to load a
        // second script under a name it already has. Taking the tag over keeps
        // startup idempotent instead of failing until KWin restarts.
        let loaded: zbus::Result<bool> = scripting.call("isScriptLoaded", &(tag,)).await;
        if let Ok(true) = loaded {
            let _: zbus::Result<()> = scripting.call("unloadScript", &(tag,)).await;
            // KWin runs scripting calls on its own event loop, so an unload is not
            // finished when the call returns. Loading straight away gives the
            // script a new id that the still-pending unload then removes — matched
            // by tag, so it deletes the script we just loaded and nothing reports.
            // Wait for the tag to actually be free.
            for _ in 0..40 {
                let now: zbus::Result<bool> = scripting.call("isScriptLoaded", &(tag,)).await;
                if !matches!(now, Ok(true)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        let id: i32 = scripting
            .call("loadScript", &(path.to_string_lossy().as_ref(), tag))
            .await?;
        if id < 0 {
            bail!("KWin refused script {tag}");
        }
        let object = format!("/Scripting/Script{id}");
        let script = Proxy::new(
            &self.connection,
            "org.kde.KWin",
            object.as_str(),
            "org.kde.kwin.Script",
        )
        .await?;
        // KWin registers the script id synchronously but finishes loading it on
        // its own event loop; a `run` that arrives first goes to an object that
        // is not ready and the script is silently never evaluated.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _: () = script.call("run", &()).await?;
        self.scripts.lock().await.push(tag.to_owned());
        let mut recorded = self.recorded_tags();
        recorded.push(tag.to_owned());
        self.write_recorded_tags(&recorded);
        Ok(())
    }

    pub async fn ensure(&self) -> Result<()> {
        self.ensure_scripts(true).await
    }

    pub async fn ensure_feed(&self) -> Result<()> {
        self.ensure_scripts(false).await
    }

    async fn ensure_scripts(&self, include_controller: bool) -> Result<()> {
        let _guard = self.gate.lock().await;
        let owner = self.owner().await.context("KDE KWin is not available")?;
        if *self.owner.read().await != owner {
            *self.owner.write().await = owner;
            *self.snapshot.write().await = Snapshot::default();
        }
        for (kind, source) in [
            (
                "controller",
                CONTROLLER.replace(
                    "registerShortcut('EdgeLine",
                    &format!("registerShortcut('{}", self.prefix),
                ),
            ),
            (
                "overview",
                OVERVIEW.replace("__EDGELINE_BUS__", &serde_json::to_string(&self.bus_name)?),
            ),
        ] {
            if kind == "controller" && !include_controller {
                continue;
            }
            if kind == "overview" {
                // Stamp the *first* request only. Re-stamping on every
                // `window.snapshot` would restart the clock each time and the
                // stall would never become visible.
                let mut started = self.feed_started.lock().unwrap_or_else(|p| p.into_inner());
                if started.is_none() {
                    *started = Some(std::time::Instant::now());
                }
                drop(started);
            }
            let tag = format!("edgeline-rust-{kind}-{}", std::process::id());
            let loaded: bool = self
                .scripting()
                .await?
                .call("isScriptLoaded", &(tag.as_str(),))
                .await?;
            if !loaded {
                self.deploy(&tag, &source).await?;
            }
        }
        Ok(())
    }

    async fn one_shot(&self, source: &str) -> Result<()> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let tag = format!("edgeline-rust-once-{}-{sequence}", std::process::id());
        self.deploy(&tag, source).await?;
        // KWin queues run(): do not unload until it has had time to execute.
        let connection = self.connection.clone();
        let scripts = self.scripts.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if let Ok(proxy) = Proxy::new(
                &connection,
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting",
            )
            .await
            {
                let _: zbus::Result<bool> = proxy.call("unloadScript", &(tag.as_str(),)).await;
            }
            scripts.lock().await.retain(|name| name != &tag);
        });
        Ok(())
    }

    /// Why the window feed is not delivering, if it was asked for and stayed
    /// silent. `None` means healthy (or not asked for yet).
    ///
    /// A KWin script deploy can succeed and never run — `loadScript` returns an id,
    /// `run` returns Ok, and no report ever arrives. That is indistinguishable from
    /// an empty desktop unless it is reported, and it is exactly the failure a
    /// stale tag after a crash produces, so it is surfaced instead of left silent.
    pub fn feed_stall(&self) -> Option<String> {
        let started = *self.feed_started.lock().unwrap_or_else(|p| p.into_inner());
        let last = *self.last_report.lock().unwrap_or_else(|p| p.into_inner());
        stall_reason(started, last, std::time::Instant::now())
    }

    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.ensure_feed().await?;
        Ok(self.snapshot.read().await.clone())
    }

    pub async fn activate(&self, id: &str) -> Result<Value> {
        let id = canonical_uuid(id)?;
        if !self
            .snapshot
            .read()
            .await
            .windows
            .iter()
            .any(|w| canonical_uuid(&w.id).ok().as_deref() == Some(id.as_str()))
        {
            bail!("That app is no longer on the Edge");
        }
        let source = ACTIVATION.replace("__EDGELINE_WINDOW_ID__", &serde_json::to_string(&id)?);
        self.one_shot(&source).await?;
        Ok(json!({"message":"App selected on the Edge"}))
    }

    async fn shortcut(&self, suffix: &str) -> Result<()> {
        let proxy = Proxy::new(
            &self.connection,
            "org.kde.kglobalaccel",
            "/component/kwin",
            "org.kde.kglobalaccel.Component",
        )
        .await?;
        let action = format!("{}{suffix}", self.prefix);
        let _: () = proxy.call("invokeShortcut", &(action,)).await?;
        Ok(())
    }

    pub async fn action(&self, name: &str) -> Result<Value> {
        self.ensure().await?;
        if name == "panel" {
            self.one_shot(PLACEMENT).await?;
            return Ok(json!({"message":"Edge windows placed by Rust/KWin"}));
        }
        if name == "pick" {
            let _guard = self
                .picker
                .try_lock()
                .context("Window selection is already active; click a window or press Esc")?;
            let proxy =
                Proxy::new(&self.connection, "org.kde.KWin", "/KWin", "org.kde.KWin").await?;
            let result: zbus::Result<HashMap<String, OwnedValue>> =
                tokio::time::timeout(Duration::from_secs(120), proxy.call("queryWindowInfo", &()))
                    .await
                    .context("Window selection timed out; press Esc to cancel the crosshair")?;
            let map = match result {
                Ok(map) => map,
                Err(error) if error.to_string().contains("UserCancel") => {
                    return Ok(json!({"message":"Window selection cancelled","cancelled":true}));
                }
                Err(error) => return Err(error.into()),
            };
            let text = |key: &str| {
                map.get(key)
                    .and_then(|v| <&str>::try_from(v).ok())
                    .unwrap_or("")
            };
            if text("caption").starts_with("EdgeLine")
                || text("resourceClass").eq_ignore_ascii_case("edgeline")
            {
                bail!("Choose an app window, not EdgeLine");
            }
            self.shortcut(&format!("Window{}", shortcut_suffix(text("uuid"))?))
                .await?;
            return Ok(json!({"message":"Selected app sent to the Edge"}));
        }
        let suffix = match name {
            "fit" => "Fit",
            "fit-vanguard" => "VanguardFit",
            "restore" => "Restore",
            _ => bail!("Unknown launchpad action"),
        };
        self.shortcut(suffix).await?;
        Ok(json!({"message":"Window request sent to KWin; check the target window"}))
    }

    pub async fn close(&self) {
        if let Ok(proxy) = self.scripting().await {
            for tag in self.scripts.lock().await.iter() {
                let _: zbus::Result<bool> = proxy.call("unloadScript", &(tag.as_str(),)).await;
            }
        }
    }
}

/// Why the feed counts as stalled, as a pure decision so it can be tested.
///
/// `None` = healthy: not requested yet, a report arrived since the request, or the
/// request is younger than [`FEED_STALL_AFTER`].
pub fn stall_reason(
    started: Option<std::time::Instant>,
    last_report: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<String> {
    const FEED_STALL_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
    let started = started?;
    if last_report.is_some_and(|last| last >= started) {
        return None;
    }
    let waited = now.saturating_duration_since(started);
    if waited < FEED_STALL_AFTER {
        return None;
    }
    Some(format!(
        "the KWin window feed was started {} s ago and has not reported; the KWin script \
         loaded but did not run (a stale script tag after a crash is the usual cause)",
        waited.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_silent_feed_is_only_a_stall_when_it_was_asked_for_and_waited() {
        let now = Instant::now();
        let six_ago = now.checked_sub(Duration::from_secs(6));
        let two_ago = now.checked_sub(Duration::from_secs(2));

        // Never asked: nothing to report.
        assert!(stall_reason(None, None, now).is_none());
        // Asked, still inside the grace period.
        assert!(stall_reason(two_ago, None, now).is_none());
        // Asked over 5 s ago and nothing arrived: this is the silent KWin deploy.
        let reason = stall_reason(six_ago, None, now).expect("stall");
        assert!(reason.contains("has not reported"), "{reason}");
        // A report since the request means the feed is alive.
        assert!(stall_reason(six_ago, Some(now), now).is_none());
        // A report from before the request does not count.
        let old_report = now.checked_sub(Duration::from_secs(30));
        assert!(stall_reason(six_ago, old_report, now).is_some());
    }
}
