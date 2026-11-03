// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
pub fn runtime() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?,
    ))
}
pub async fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let timeout = if method == "launchpad.action" && params["action"] == "pick" {
        130
    } else {
        12
    };
    tokio::time::timeout(Duration::from_secs(timeout), async {
        let mut stream = UnixStream::connect(socket).await?;
        // All backend sockets must belong to the same effective user.
        let expected = std::fs::metadata(runtime()?)?;
        use std::os::unix::fs::MetadataExt;
        if stream.peer_cred()?.uid() != expected.uid() {
            bail!("Backend socket belongs to another user");
        }
        let mut request = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))?;
        if request.len() > 1024 * 1024 {
            bail!("Backend request exceeds 1 MiB");
        }
        request.push(b'\n');
        stream.write_all(&request).await?;
        let mut reader = BufReader::new(stream);
        loop {
            let line = line(&mut reader)
                .await?
                .context("Backend disconnected before replying")?;
            let response: Value = serde_json::from_slice(&line)?;
            if response["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!(
                    "{}",
                    error
                        .as_str()
                        .or_else(|| error["message"].as_str())
                        .unwrap_or("Backend request failed")
                );
            }
            return Ok(response["result"].clone());
        }
    })
    .await
    .context("Backend request timed out")?
}
pub async fn line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(if output.is_empty() {
                None
            } else {
                Some(output)
            });
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let n = end.map_or(buffer.len(), |n| n + 1);
        if output.len() + n > 8 * 1024 * 1024 {
            bail!("Backend response exceeds 8 MiB");
        }
        output.extend_from_slice(&buffer[..n]);
        reader.consume(n);
        if end.is_some() {
            return Ok(Some(output));
        }
    }
}
