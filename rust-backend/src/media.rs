// SPDX-License-Identifier: GPL-3.0-or-later
//! MPRIS media playback and desktop notification monitoring, ported from the
//! legacy C++/Qt agent (`agent/src/core/MprisWatcher.cpp` and
//! `agent/src/core/NotificationWatcher.cpp`).
//!
//! MPRIS is the one piece of media playback that every Linux player agrees on:
//! browsers, Spotify, mpv, Rhythmbox and VLC all export
//! `org.mpris.MediaPlayer2.<something>` with the same properties, so reading it
//! needs no per-application knowledge. Player discovery is polled rather than
//! signal driven, exactly like the C++ agent: the snapshot is rebuilt on demand
//! and a player that appears or disappears needs no subscription bookkeeping.
//!
//! Notifications are a method call to `org.freedesktop.Notifications`, not a
//! signal, so the only way to observe them without being the daemon is to become
//! a bus *monitor* (`org.freedesktop.DBus.Monitoring.BecomeMonitor`). Once that
//! succeeds the bus refuses to let the connection send anything at all, so this
//! module can only watch and can never act on the user's behalf.
//!
//! The D-Bus layer is behind the [`Bus`] and [`Monitor`] traits so the core
//! shaping, allow-list and notification parsing are unit tested offline from
//! fixtures, while the real zbus-backed implementations stay in one place.

use anyhow::Result;
use async_trait::async_trait;
use futures_lite::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use zbus::{
    Connection, MatchRule, Message, MessageStream, Proxy,
    fdo::MonitoringProxy,
    message::Type,
    zvariant::OwnedValue,
};

const K_PREFIX: &str = "org.mpris.MediaPlayer2.";
const K_PATH: &str = "/org/mpris/MediaPlayer2";
const K_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const K_PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const K_DAEMON: &str = "org.freedesktop.Notifications";
const MAX_ART: usize = 4 * 1024 * 1024;

/// The KEEP latest notifications, matching the C++ `kKeep = 5`.
const KEEP: usize = 5;

/// What an MPRIS player is playing, or nothing. Mirrors the C++ `NowPlaying`.
#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    pub valid: bool,
    pub player: String, // "chromium", from the tail of the bus name
    pub status: String, // "Playing", "Paused" or "Stopped"
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String, // file:// or https://, whatever the player published
    pub position_us: i64,
    pub length_us: i64,
}

impl NowPlaying {
    fn invalid() -> Self {
        NowPlaying {
            valid: false,
            status: "Stopped".to_string(),
            position_us: -1,
            length_us: -1,
            ..NowPlaying::default()
        }
    }
}

/// A parsed `Metadata` property: the fields the dashboard and now-playing tile
/// read, with the artist already joined the way the C++ `joinStrings` does.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub length_us: i64,
}

/// A desktop notification as the sender submitted it. Mirrors the C++
/// `Notification` with `when` as milliseconds since the epoch (the JSON field
/// is `whenMs`, a double, because that is what `toMSecsSinceEpoch()` gives).
#[derive(Clone, Debug)]
pub struct Notification {
    pub app: String,
    pub summary: String,
    pub body: String,
    pub when_ms: i64,
}

// --- pure shaping helpers -------------------------------------------------

fn is_mpris_player(name: &str) -> bool {
    name.starts_with(K_PREFIX)
}

fn short_player_name(service: &str) -> String {
    let name = service.strip_prefix(K_PREFIX).unwrap_or(service);
    // Chromium and friends append ".instance1234", which is noise on a panel.
    match name.find('.') {
        Some(dot) => name[..dot].to_string(),
        None => name.to_string(),
    }
}

/// The transport control allow-list: lowercase action -> MPRIS method. This
/// mirrors the C++ agent's allow-list plus `play`/`pause`, which the MPRIS
/// `MediaPlayer2.Player` interface exposes and the panel/now-playing UI use.
fn command_method(action: &str) -> Option<&'static str> {
    Some(match action {
        "previous" => "Previous",
        "playpause" => "PlayPause",
        "play" => "Play",
        "pause" => "Pause",
        "next" => "Next",
        _ => return None,
    })
}

// The busctl `--json=short` metadata fixture shape: {type, data}. Each property
// is `{type: "s"|"x"|"as"|..., data: value}` and the whole thing is a{sv}. This is
// a fixture-parity path only: production reads the property through zbus and
// shapes it in `metadata_from_value`, so it exists only in tests.
#[cfg(test)]
fn unwrap_json(raw: &Value) -> &Value {
    raw.get("data").unwrap_or(raw)
}

/// Parse an `a{sv}` metadata map (in the busctl JSON encoding) into Metadata,
/// so the exact payload a player publishes can be tested from a fixture.
#[cfg(test)]
fn metadata_from_json(input: &Value) -> Metadata {
    let data = unwrap_json(input);
    let get = |key: &str| data.get(key).map(unwrap_json);

    // `xesam:artist` is a list, but players are inconsistent about whether it
    // arrives as one; a bare string is accepted too rather than dropped (the
    // C++ `joinStrings` behaviour).
    let artists = get("xesam:artist")
        .map(|v| {
            if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                v.as_str().unwrap_or_default().to_string()
            }
        })
        .unwrap_or_default();

    Metadata {
        title: get("xesam:title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        artist: artists,
        album: get("xesam:album")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        art_url: get("mpris:artUrl")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        length_us: get("mpris:length").and_then(|v| v.as_i64()).unwrap_or(-1),
    }
}

fn value_string(v: &OwnedValue) -> String {
    String::try_from(v.clone()).unwrap_or_default()
}

fn value_i64(v: &OwnedValue) -> i64 {
    i64::try_from(v.clone()).unwrap_or(-1)
}

/// Parse a live zbus `a{sv}` Metadata property into the same Metadata shape.
fn metadata_from_value(value: &OwnedValue) -> Metadata {
    let map = match HashMap::<String, OwnedValue>::try_from(value.clone()) {
        Ok(map) => map,
        Err(_) => return Metadata::default(),
    };
    // `xesam:artist` is a list, but players are inconsistent about whether it
    // arrives as one; a bare string is accepted too rather than dropped (the
    // C++ `joinStrings` behaviour).
    let artists = match map.get("xesam:artist") {
        Some(v) => match Vec::<String>::try_from(v.clone()) {
            Ok(list) => list,
            Err(_) => vec![value_string(v)],
        },
        None => Vec::new(),
    };
    Metadata {
        title: map.get("xesam:title").map(value_string).unwrap_or_default(),
        artist: artists.join(", "),
        album: map.get("xesam:album").map(value_string).unwrap_or_default(),
        art_url: map
            .get("mpris:artUrl")
            .map(value_string)
            .unwrap_or_default(),
        length_us: map.get("mpris:length").map(value_i64).unwrap_or(-1),
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Artwork inlined as a data URL only when the player published a `file://`
/// URL and the file is a small image; otherwise empty (rendered as null).
fn art_data_url(art_url: &str) -> String {
    if !art_url.starts_with("file://") {
        return String::new();
    }
    let path = percent_decode(&art_url["file://".len()..]);
    let Ok(metadata) = fs::metadata(&path) else {
        return String::new();
    };
    if !metadata.is_file() || metadata.len() as usize > MAX_ART {
        return String::new();
    }
    let Ok(data) = fs::read(&path) else {
        return String::new();
    };
    let mime = if data.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if data.len() >= 4 && &data[1..4] == b"PNG" {
        "image/png"
    } else if data.len() >= 3 && &data[..3] == b"GIF" {
        "image/gif"
    } else {
        return String::new();
    };
    format!("data:{mime};base64,{}", crate::base64(&data))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --- MPRIS bus abstraction ------------------------------------------------

/// The session-bus operations the MPRIS watcher needs, injected so the core
/// logic can run offline against a fake in tests.
#[async_trait]
pub trait Bus: Send + Sync {
    /// False when there is no session bus: the difference between "nothing is
    /// playing" and "this cannot work here".
    fn available(&self) -> bool;
    /// The registered `org.mpris.MediaPlayer2.*` service names.
    async fn players(&self) -> Vec<String>;
    async fn playback_status(&self, service: &str) -> Option<String>;
    async fn metadata(&self, service: &str) -> Metadata;
    /// The `Position` property, in microseconds, or -1 when unknown.
    async fn position_us(&self, service: &str) -> i64;
    /// Invoke a transport method on the player's own interface.
    async fn control(&self, service: &str, method: &str) -> Result<(), String>;
}

/// The production `Bus`, backed by a zbus session connection.
struct SessionBus {
    connection: Option<Connection>,
}

impl SessionBus {
    async fn connect() -> Self {
        // A missing session bus is not a fatal backend condition: it only means
        // media is unavailable, which `available()` reports honestly.
        let connection = Connection::session().await.ok();
        Self { connection }
    }
}

#[async_trait]
impl Bus for SessionBus {
    fn available(&self) -> bool {
        self.connection
            .as_ref()
            .map(|c| !c.is_closed())
            .unwrap_or(false)
    }

    async fn players(&self) -> Vec<String> {
        let Some(connection) = &self.connection else {
            return Vec::new();
        };
        let Ok(proxy) = Proxy::new(
            connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        else {
            return Vec::new();
        };
        let names: Vec<String> = match proxy.call("ListNames", &()).await {
            Ok(names) => names,
            Err(_) => return Vec::new(),
        };
        names.into_iter().filter(|n| is_mpris_player(n)).collect()
    }

    async fn playback_status(&self, service: &str) -> Option<String> {
        let Some(connection) = &self.connection else {
            return None;
        };
        let Ok(props) = Proxy::new(connection, service, K_PATH, K_PROPERTIES).await else {
            return None;
        };
        let value: OwnedValue = match props.call("Get", &(K_PLAYER, "PlaybackStatus")).await {
            Ok(v) => v,
            Err(_) => return None,
        };
        String::try_from(value).ok()
    }

    async fn metadata(&self, service: &str) -> Metadata {
        let Some(connection) = &self.connection else {
            return Metadata::default();
        };
        let Ok(props) = Proxy::new(connection, service, K_PATH, K_PROPERTIES).await else {
            return Metadata::default();
        };
        let value: OwnedValue = match props.call("Get", &(K_PLAYER, "Metadata")).await {
            Ok(v) => v,
            Err(_) => return Metadata::default(),
        };
        metadata_from_value(&value)
    }

    async fn position_us(&self, service: &str) -> i64 {
        let Some(connection) = &self.connection else {
            return -1;
        };
        let Ok(props) = Proxy::new(connection, service, K_PATH, K_PROPERTIES).await else {
            return -1;
        };
        let value: OwnedValue = match props.call("Get", &(K_PLAYER, "Position")).await {
            Ok(v) => v,
            Err(_) => return -1,
        };
        i64::try_from(value).unwrap_or(-1)
    }

    async fn control(&self, service: &str, method: &str) -> Result<(), String> {
        let Some(connection) = &self.connection else {
            return Err("no session bus".to_string());
        };
        let player = Proxy::new(connection, service, K_PATH, K_PLAYER)
            .await
            .map_err(|e| e.to_string())?;
        player
            .call::<_, _, ()>(method, &())
            .await
            .map_err(|e| e.to_string())
    }
}

/// Pick the player to show and remember the service, faithfully to the C++
/// `MprisWatcher::read()`: the user's currently-playing player wins over one
/// that is merely open.
async fn read_now_playing(bus: &dyn Bus) -> (NowPlaying, String) {
    if !bus.available() {
        return (NowPlaying::invalid(), String::new());
    }
    let services = bus.players().await;
    if services.is_empty() {
        return (NowPlaying::invalid(), String::new());
    }

    let mut chosen = String::new();
    let mut chosen_status = String::new();
    for service in &services {
        let status = bus
            .playback_status(service)
            .await
            .unwrap_or_else(|| "Stopped".to_string());
        let is_playing = status == "Playing";
        if chosen.is_empty() || is_playing {
            chosen = service.clone();
            chosen_status = status;
        }
        // Something actually playing wins over something merely open.
        if is_playing {
            break;
        }
    }
    if chosen.is_empty() {
        return (NowPlaying::invalid(), String::new());
    }

    let metadata = bus.metadata(&chosen).await;
    let position_us = bus.position_us(&chosen).await;
    let np = NowPlaying {
        valid: !metadata.title.is_empty(),
        player: short_player_name(&chosen),
        status: chosen_status,
        title: metadata.title,
        artist: metadata.artist,
        album: metadata.album,
        art_url: metadata.art_url,
        position_us,
        length_us: metadata.length_us,
    };
    (np, chosen)
}

// --- notification monitor -------------------------------------------------

/// The shared, monitor-fed notification state. The monitor task pushes into it
/// and the panel reads it; `clear()` empties the visible rows.
struct NotificationStore {
    recent: Mutex<Vec<Notification>>,
    active: AtomicBool,
    error: Mutex<Option<String>>,
    tried: AtomicBool,
}

impl NotificationStore {
    fn new() -> Self {
        NotificationStore {
            recent: Mutex::new(Vec::new()),
            active: AtomicBool::new(false),
            error: Mutex::new(None),
            tried: AtomicBool::new(false),
        }
    }

    fn record(&self, n: Notification) {
        let mut recent = self.recent.lock().unwrap();
        recent.insert(0, n);
        if recent.len() > KEEP {
            recent.truncate(KEEP);
        }
    }

    fn recent(&self) -> Vec<Notification> {
        self.recent.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.recent.lock().unwrap().clear();
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    fn set_active(&self, v: bool) {
        self.active.store(v, Ordering::SeqCst);
    }

    fn set_error(&self, error: &str) {
        *self.error.lock().unwrap() = Some(error.to_string());
    }

    fn tried(&self) -> bool {
        self.tried.load(Ordering::SeqCst)
    }

    fn set_tried(&self) {
        self.tried.store(true, Ordering::SeqCst);
    }
}

/// The injected monitor layer. Only the real zbus implementation talks to the
/// bus; tests drive `NotificationStore` and `parse_notify` directly.
trait Monitor: Send + Sync {
    /// Begin monitoring. Idempotent: a single call is made and a refusal is
    /// remembered (a policy decision on some systems, not a transient failure).
    fn start(&self, store: Arc<NotificationStore>);
}

struct ZbusMonitor;

impl Monitor for ZbusMonitor {
    fn start(&self, store: Arc<NotificationStore>) {
        if store.tried() {
            return;
        }
        store.set_tried();
        tokio::spawn(async move { run_monitor(store).await });
    }
}

fn notify_match_rule() -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(Type::MethodCall)
        .interface(K_DAEMON)?
        .member("Notify")?
        .build())
}

async fn run_monitor(store: Arc<NotificationStore>) {
    let rule = match notify_match_rule() {
        Ok(rule) => rule,
        Err(_) => {
            store.set_error("could not build BecomeMonitor rule");
            return;
        }
    };
    let Ok(connection) = Connection::session().await else {
        store.set_error("no session bus");
        return;
    };
    let Ok(monitor) = MonitoringProxy::new(&connection).await else {
        store.set_error("could not build the monitor proxy");
        return;
    };
    // BecomeMonitor converts the connection into a monitor; once it succeeds the
    // bus refuses to let it send anything, so this can only watch.
    if let Err(_) = monitor.become_monitor(&[rule], 0).await {
        store.set_error("BecomeMonitor refused");
        return;
    }
    store.set_active(true);
    let mut stream = MessageStream::from(connection);
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else {
            continue;
        };
        if let Some(n) = parse_notify(&msg) {
            store.record(n);
        }
    }
    // The stream ended, which means the connection was closed underneath us.
    // A dropped monitor is silent: say so rather than showing stale rows.
    store.set_active(false);
    store.set_error("the bus dropped the monitor connection");
}

/// Extract a `Notify` method call addressed to the daemon. Only calls destined
/// for `org.freedesktop.Notifications` count: GNOME Shell re-issues every
/// notification to itself under a unique name, and the destination is the only
/// reliable way to tell the original from the shell's copy.
fn parse_notify(msg: &Message) -> Option<Notification> {
    if msg.message_type() != Type::MethodCall {
        return None;
    }
    let header = msg.header();
    if header.interface().map(|s| s.as_str()) != Some(K_DAEMON) {
        return None;
    }
    if header.member().map(|s| s.as_str()) != Some("Notify") {
        return None;
    }
    if header.destination().map(|s| s.as_str()) != Some(K_DAEMON) {
        return None;
    }
    // Notify(s app_name, u replaces_id, s app_icon, s summary, s body, as, a{sv}, i)
    let body: (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedValue>, i32) =
        match msg.body().deserialize() {
            Ok(b) => b,
            Err(_) => return None,
        };
    let (app, _replaces_id, _app_icon, summary, body, _actions, _hints, _timeout) = body;
    if summary.is_empty() && body.is_empty() {
        return None;
    }
    Some(Notification {
        app,
        summary,
        body,
        when_ms: now_ms(),
    })
}

// --- the Media facade -----------------------------------------------------

pub struct Media {
    bus: Arc<dyn Bus>,
    last_service: Mutex<String>,
    notifications: Arc<NotificationStore>,
}

impl Media {
    pub async fn new() -> Self {
        let bus: Arc<dyn Bus> = Arc::new(SessionBus::connect().await);
        let notifications = Arc::new(NotificationStore::new());
        // The zbus monitor is stateless: it only needs to hand the store to a
        // background task, so it is started here and need not be retained.
        let monitor: Arc<dyn Monitor> = Arc::new(ZbusMonitor);
        monitor.start(notifications.clone());
        Self {
            bus,
            last_service: Mutex::new(String::new()),
            notifications,
        }
    }

    /// Refresh now-playing plus notifications into the JSON shape the dashboard
    /// reads, matching the C++ agent's `Api::sensorSnapshot()` fields.
    pub async fn snapshot(&self) -> Value {
        let (np, service) = read_now_playing(&*self.bus).await;
        *self.last_service.lock().unwrap() = service;
        let notes: Vec<Value> = self
            .notifications
            .recent()
            .iter()
            .map(|n| {
                json!({
                    "app": n.app,
                    "summary": n.summary,
                    "body": n.body,
                    "whenMs": n.when_ms as f64,
                })
            })
            .collect();
        json!({
            "notifications": notes,
            "notifyActive": self.notifications.active(),
            "notifyError": self.notifications.error().unwrap_or_default(),
            "nowPlaying": now_playing_json(&np),
            "sessionBusOk": self.bus.available(),
        })
    }

    /// Transport control on the current player. A fresh read keeps the chosen
    /// player current, mirroring the C++ agent's second-poll `m_lastService`.
    pub async fn control(&self, action: &str) -> Result<(), String> {
        let method = command_method(action)
            .ok_or_else(|| format!("unknown media action '{action}'"))?;
        let (_, service) = read_now_playing(&*self.bus).await;
        if service.is_empty() {
            *self.last_service.lock().unwrap() = String::new();
            return Err("no media player on the session bus".to_string());
        }
        *self.last_service.lock().unwrap() = service.clone();
        self.bus.control(&service, method).await
    }

    /// Drop the notifications collected so far (the panel's CLEAR ALL).
    pub fn clear_notifications(&self) {
        self.notifications.clear();
    }
}

fn now_playing_json(np: &NowPlaying) -> Value {
    let art = if np.art_url.is_empty() {
        Value::Null
    } else {
        let data = art_data_url(&np.art_url);
        if data.is_empty() {
            Value::Null
        } else {
            Value::String(data)
        }
    };
    json!({
        "valid": np.valid,
        "player": np.player,
        "status": np.status,
        "title": np.title,
        "artist": np.artist,
        "album": np.album,
        // Microseconds exceed what a JSON number holds exactly past a few hours
        // of track, which no track is, so a double is honest here.
        "positionUs": np.position_us as f64,
        "lengthUs": np.length_us as f64,
        "art": art,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBus {
        available_flag: bool,
        services: Vec<String>,
        statuses: HashMap<String, String>,
        metadata: HashMap<String, Metadata>,
        positions: HashMap<String, i64>,
    }

    impl FakeBus {
        fn new() -> Self {
            FakeBus {
                available_flag: true,
                services: Vec::new(),
                statuses: HashMap::new(),
                metadata: HashMap::new(),
                positions: HashMap::new(),
            }
        }
    }

    #[async_trait]
    impl Bus for FakeBus {
        fn available(&self) -> bool {
            self.available_flag
        }
        async fn players(&self) -> Vec<String> {
            self.services.clone()
        }
        async fn playback_status(&self, service: &str) -> Option<String> {
            self.statuses.get(service).cloned()
        }
        async fn metadata(&self, service: &str) -> Metadata {
            self.metadata.get(service).cloned().unwrap_or_default()
        }
        async fn position_us(&self, service: &str) -> i64 {
            self.positions.get(service).copied().unwrap_or(-1)
        }
        async fn control(&self, _service: &str, _method: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn player_name_and_command_allowlist() {
        assert!(is_mpris_player("org.mpris.MediaPlayer2.chromium"));
        assert!(!is_mpris_player("org.mpris.OtherApp.service"));
        assert_eq!(
            short_player_name("org.mpris.MediaPlayer2.chromium.instance1234"),
            "chromium"
        );
        assert_eq!(
            short_player_name("org.mpris.MediaPlayer2.spotify"),
            "spotify"
        );
        assert_eq!(command_method("playpause"), Some("PlayPause"));
        assert_eq!(command_method("play"), Some("Play"));
        assert_eq!(command_method("pause"), Some("Pause"));
        assert_eq!(command_method("next"), Some("Next"));
        assert_eq!(command_method("previous"), Some("Previous"));
        assert_eq!(command_method("Seek"), None);
        assert_eq!(command_method("../../bin/sh"), None);
    }

    #[test]
    fn metadata_shaping_matches_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../ui/test/fixtures/mpris-metadata.json"))
                .unwrap();
        let meta = metadata_from_json(&fixture);
        assert_eq!(meta.title, "🛡️ Generations Under One Guardian");
        assert_eq!(meta.artist, "Royalix");
        assert_eq!(meta.length_us, 59061000);
        assert_eq!(meta.art_url, "file:///tmp/..com.vivaldi.Vivaldi.JuYPaA");
        assert!(metadata_from_json(&json!({})).title.is_empty());

        // A player publishing a bare string (not a list) is still accepted.
        let bare = metadata_from_json(&json!({"data": {
            "xesam:artist": {"type": "s", "data": "Solo Artist"},
            "mpris:length": {"type": "x", "data": 1000},
        }}));
        assert_eq!(bare.artist, "Solo Artist");
        assert_eq!(bare.length_us, 1000);
    }

    #[test]
    fn art_inlined_only_for_local_small_images() {
        let dir = std::env::temp_dir().join(format!("edgeline-media-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let png = dir.join("cover.png");
        fs::write(
            &png,
            [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0],
        )
        .unwrap();
        let data = art_data_url(&format!("file://{}", png.display()));
        assert!(data.starts_with("data:image/png;base64,"), "png inlined");

        let jpeg = dir.join("cover.jpg");
        fs::write(&jpeg, [0xff, 0xd8, 0xff, 0xe0, 1, 2, 3]).unwrap();
        assert!(art_data_url(&format!("file://{}", jpeg.display()))
            .starts_with("data:image/jpeg;base64,"));

        let text = dir.join("cover.txt");
        fs::write(&text, "not an image").unwrap();
        assert_eq!(
            art_data_url(&format!("file://{}", text.display())),
            "",
            "a non-image file is never inlined"
        );

        let big = dir.join("big.png");
        fs::write(&big, b"x").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&big)
            .unwrap()
            .set_len(MAX_ART as u64 + 1)
            .unwrap();
        assert_eq!(
            art_data_url(&format!("file://{}", big.display())),
            "",
            "artwork over 4 MiB is never inlined"
        );

        assert_eq!(art_data_url("https://example.invalid/cover.png"), "");
        assert_eq!(art_data_url(""), "");
        assert_eq!(art_data_url(&format!("file://{}/nope.png", dir.display())), "");

        // A percent-encoded file:// path still resolves.
        let encoded = format!("file://{}", dir.join("cover.jpg").display()).replace("cover.jpg", "cover%2Ejpg");
        assert!(art_data_url(&encoded).starts_with("data:image/jpeg;base64,"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_prefers_the_playing_player() {
        let mut bus = FakeBus::new();
        bus.services = vec![
            "org.mpris.MediaPlayer2.chromium".to_string(),
            "org.mpris.MediaPlayer2.spotify".to_string(),
        ];
        bus.statuses.insert(
            "org.mpris.MediaPlayer2.chromium".to_string(),
            "Paused".to_string(),
        );
        bus.statuses.insert(
            "org.mpris.MediaPlayer2.spotify".to_string(),
            "Playing".to_string(),
        );
        bus.metadata.insert(
            "org.mpris.MediaPlayer2.spotify".to_string(),
            Metadata {
                title: "New Song".to_string(),
                artist: "An Artist".to_string(),
                album: "An Album".to_string(),
                art_url: String::new(),
                length_us: 250_000,
            },
        );
        bus.positions
            .insert("org.mpris.MediaPlayer2.spotify".to_string(), 42_000);

        let (np, service) = read_now_playing(&bus).await;
        assert!(np.valid);
        assert_eq!(np.player, "spotify");
        assert_eq!(np.status, "Playing");
        assert_eq!(np.title, "New Song");
        assert_eq!(np.position_us, 42_000);
        assert_eq!(np.length_us, 250_000);
        assert_eq!(service, "org.mpris.MediaPlayer2.spotify");
    }

    #[tokio::test]
    async fn read_is_invalid_when_no_player_or_bus() {
        let bus = FakeBus::new();
        let (np, service) = read_now_playing(&bus).await;
        assert!(!np.valid);
        assert!(service.is_empty());

        let mut no_bus = FakeBus::new();
        no_bus.available_flag = false;
        let (np, _) = read_now_playing(&no_bus).await;
        assert!(!np.valid);
    }

    #[test]
    fn notification_record_keeps_five_newest_and_clears() {
        let store = NotificationStore::new();
        for i in 0..7 {
            store.record(Notification {
                app: "app".to_string(),
                summary: format!("summary {i}"),
                body: String::new(),
                when_ms: i,
            });
        }
        let recent = store.recent();
        assert_eq!(recent.len(), 5, "only the KEEP newest rows survive");
        assert_eq!(recent[0].summary, "summary 6");
        assert_eq!(recent[4].summary, "summary 2");
        store.clear();
        assert!(store.recent().is_empty());
    }

    #[test]
    fn parse_notify_accepts_only_daemon_notify_calls() {
        let body = (
            "MyApp".to_string(),
            0u32,
            "icon".to_string(),
            "Summary".to_string(),
            "Body".to_string(),
            Vec::<String>::new(),
            HashMap::<String, OwnedValue>::new(),
            0i32,
        );
        let notify = Message::method_call(K_PATH, "Notify")
            .unwrap()
            .interface(K_DAEMON)
            .unwrap()
            .destination(K_DAEMON)
            .unwrap()
            .build(&body)
            .unwrap();
        let n = parse_notify(&notify).expect("a Notify call to the daemon is recorded");
        assert_eq!(n.app, "MyApp");
        assert_eq!(n.summary, "Summary");
        assert_eq!(n.body, "Body");

        // A Notify call re-issued to a unique name (GNOME Shell's own copy) is
        // not a notification somebody sent, so it must be dropped.
        let shell_copy = Message::method_call(K_PATH, "Notify")
            .unwrap()
            .interface(K_DAEMON)
            .unwrap()
            .destination(":1.40")
            .unwrap()
            .build(&body)
            .unwrap();
        assert!(parse_notify(&shell_copy).is_none());

        // A different member on the same interface is not a notification.
        let other = Message::method_call(K_PATH, "GetCapabilities")
            .unwrap()
            .interface(K_DAEMON)
            .unwrap()
            .destination(K_DAEMON)
            .unwrap()
            .build(&())
            .unwrap();
        assert!(parse_notify(&other).is_none());
    }

    #[tokio::test]
    async fn media_snapshot_exposes_now_playing_and_notifications() {
        let mut bus = FakeBus::new();
        bus.services = vec!["org.mpris.MediaPlayer2.spotify".to_string()];
        bus.statuses.insert(
            "org.mpris.MediaPlayer2.spotify".to_string(),
            "Playing".to_string(),
        );
        bus.metadata.insert(
            "org.mpris.MediaPlayer2.spotify".to_string(),
            Metadata {
                title: "Song".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                art_url: String::new(),
                length_us: 120_000,
            },
        );
        bus.positions
            .insert("org.mpris.MediaPlayer2.spotify".to_string(), 30_000);
        let notifications = Arc::new(NotificationStore::new());
        notifications.record(Notification {
            app: "app".to_string(),
            summary: "Summary".to_string(),
            body: "Body".to_string(),
            when_ms: 17,
        });
        let media = Media {
            bus: Arc::new(bus),
            last_service: Mutex::new(String::new()),
            notifications: notifications.clone(),
        };
        let snap = media.snapshot().await;
        assert_eq!(snap["nowPlaying"]["valid"], true);
        assert_eq!(snap["nowPlaying"]["player"], "spotify");
        assert_eq!(snap["nowPlaying"]["title"], "Song");
        assert_eq!(snap["nowPlaying"]["positionUs"], 30_000.0);
        assert_eq!(snap["nowPlaying"]["lengthUs"], 120_000.0);
        assert_eq!(snap["notifications"][0]["summary"], "Summary");
        assert_eq!(snap["notifications"][0]["whenMs"], 17.0);
        assert_eq!(snap["notifyActive"], false);
        assert_eq!(snap["sessionBusOk"], true);
    }
}
