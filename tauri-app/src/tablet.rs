// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, Command},
};
pub struct Deck {
    child: Child,
    pub connection: Value,
}
impl Deck {
    pub async fn start() -> Result<Self> {
        let root = std::env::var_os("EDGELINE_TABLET_DECK_DIR")
            .map(PathBuf::from)
            .unwrap_or(
                PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
                    .join("Dokumente/tablet-deck"),
            );
        if !root.join("layouts/edge.json").is_file() {
            bail!("Tablet Deck with the native Edge layout is not installed");
        }
        let mut child = Command::new("node")
            .arg(root.join("server.js"))
            .args(["--dry-run", "--loopback", "--port", "8790", "--ui"])
            .env("TABLET_DECK_KEYS", "none")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let output = child
            .stdout
            .take()
            .context("Tablet Deck has no output channel")?;
        let mut reader = BufReader::new(output);
        let connection = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = crate::rpc::line(&mut reader).await? {
                if let Some(value) = std::str::from_utf8(&line)
                    .ok()
                    .and_then(|s| s.strip_prefix("@ui "))
                {
                    let event: Value = serde_json::from_str(value)?;
                    if event["t"] == "ready" {
                        if event["dryRun"] != true || event["port"] != 8790 {
                            bail!("Unsafe Tablet Deck startup refused");
                        }
                        let token = event["token"]
                            .as_str()
                            .context("Pairing token is missing")?;
                        if token.is_empty() || token.len() > 256 {
                            bail!("Invalid pairing token");
                        }
                        let mut url = tauri::Url::parse("http://127.0.0.1:8790/")?;
                        url.query_pairs_mut()
                            .append_pair("device", "edge")
                            .append_pair("t", token);
                        return Ok(json!({"url":url.to_string(),"dryRun":true}));
                    }
                }
            }
            bail!("Tablet Deck exited before becoming ready; port 8790 may be in use")
        })
        .await
        .context("Tablet Deck startup timed out")??;
        // Consume remaining output privately; it includes pairing information.
        tauri::async_runtime::spawn(async move {
            while let Ok(Some(_)) = crate::rpc::line(&mut reader).await {}
        });
        Ok(Self { child, connection })
    }
    pub async fn stop(&mut self) {
        if let Some(input) = self.child.stdin.as_mut() {
            let _ = input.write_all(b"quit\n").await;
        }
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}
