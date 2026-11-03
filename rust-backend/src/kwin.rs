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
}

#[zbus::interface(name = "dev.edgeline.Windows")]
impl Feed {
    async fn update(
        &self,
        payload: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let owner = self.owner.read().await;
        if header.sender().map(|s| s.as_str()) != Some(owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Only KWin may publish windows".into(),
            ));
        }
        let snapshot = Snapshot::parse(payload)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        *self.snapshot.write().await = snapshot;
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
}

impl Kwin {
    pub async fn new(directory: &Path) -> Result<Self> {
        let connection = Connection::session().await?;
        let bus_name = format!("dev.edgeline.Windows.rust.p{}", std::process::id());
        let snapshot = Arc::new(RwLock::new(Snapshot::default()));
        let owner = Arc::new(RwLock::new(String::new()));
        connection
            .object_server()
            .at(
                "/Windows",
                Feed {
                    snapshot: snapshot.clone(),
                    owner: owner.clone(),
                },
            )
            .await?;
        connection.request_name(bus_name.as_str()).await?;
        Ok(Self {
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
        })
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

    async fn deploy(&self, tag: &str, source: &str) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.directory.join(format!("{tag}.js"));
        std::fs::write(&path, source)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let scripting = self.scripting().await?;
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
        let _: () = script.call("run", &()).await?;
        self.scripts.lock().await.push(tag.to_owned());
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
