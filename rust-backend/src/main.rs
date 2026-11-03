// SPDX-License-Identifier: GPL-3.0-or-later
mod kwin;
mod model;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Semaphore, watch},
};

const MAX_MESSAGE: usize = 1024 * 1024;
#[derive(Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}
struct Backend {
    kwin: kwin::Kwin,
    shutdown: watch::Sender<bool>,
}

fn uid() -> u32 {
    // geteuid has no arguments, no failure case and no pointer preconditions.
    unsafe { libc::geteuid() }
}
fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}
fn artwork() -> Result<Value> {
    for root in [
        home()?.join(".local/share/Steam"),
        home()?.join(".steam/steam"),
    ] {
        let Ok(entries) = std::fs::read_dir(root.join("appcache/librarycache/3471800")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("library_hero.jpg");
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                continue;
            }
            let image = std::fs::read(path)?;
            if image.starts_with(&[0xff, 0xd8, 0xff]) {
                // No additional image/network dependency; data URL compatible with existing UI.
                return Ok(json!({"image":format!("data:image/jpeg;base64,{}", base64(&image))}));
            }
        }
    }
    Ok(json!({"image":null}))
}
fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        output.push(TABLE[a >> 2] as char);
        output.push(TABLE[((a & 3) << 4) | (b >> 4)] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[c & 63] as char
        } else {
            '='
        });
    }
    output
}
impl Backend {
    async fn dispatch(&self, request: &Request) -> Result<Value> {
        let parameter = |name| {
            request
                .params
                .get(name)
                .and_then(Value::as_str)
                .context("Missing string parameter")
        };
        match request.method.as_str() {
            "backend.info" => Ok(
                json!({"name":"edgeline-backend","version":env!("CARGO_PKG_VERSION"),
                "language":"Rust","stage":"control-core migration", "uiHost":"client-selected; native Tauri preview available",
                "hardware":"legacy C++ agent (not migrated)","gestures":"disabled; Rust port pending",
                "capabilities":["kwin-window-feed","edge-window-activation","window-fitting","native-window-picker","steam-launch","local-artwork"]}),
            ),
            "backend.stop" => Ok(json!({"stopping":true})),
            "controller.ensure" => {
                self.kwin.ensure().await?;
                Ok(json!({"ok":true}))
            }
            "window.start" => {
                self.kwin.ensure_feed().await?;
                Ok(json!({"ok":true}))
            }
            "window.snapshot" => Ok(serde_json::to_value(self.kwin.snapshot().await?)?),
            "window.activate" => self.kwin.activate(parameter("id")?).await,
            "launchpad.action" => {
                let name = parameter("action")?;
                match name {
                    "artwork" => artwork(),
                    "vanguard" | "steam" => {
                        let uri = if name == "vanguard" {
                            "steam://rungameid/3471800"
                        } else {
                            "steam://open/library"
                        };
                        let mut child = tokio::process::Command::new("xdg-open")
                            .arg(uri)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()?;
                        tokio::spawn(async move {
                            let _ = child.wait().await;
                        });
                        Ok(json!({"message":"Launch request sent to Steam"}))
                    }
                    _ => self.kwin.action(name).await,
                }
            }
            _ => bail!("Unsupported Rust backend method: {}", request.method),
        }
    }
}

async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut message = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(if message.is_empty() {
                None
            } else {
                Some(message)
            });
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let length = end.map_or(buffer.len(), |n| n + 1);
        if message.len() + length > MAX_MESSAGE {
            bail!("RPC message exceeds 1 MiB");
        }
        message.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(Some(message));
        }
    }
}

async fn serve(stream: UnixStream, backend: Arc<Backend>) -> Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        bail!("RPC peer is not the current user");
    }
    let (read, write) = stream.into_split();
    let writer = Arc::new(Mutex::new(write));
    let limit = Arc::new(Semaphore::new(32));
    let mut reader = BufReader::new(read);
    while let Some(line) = read_message(&mut reader).await? {
        let request: Request = serde_json::from_slice(&line).context("Invalid RPC request")?;
        let permit = limit
            .clone()
            .try_acquire_owned()
            .context("Too many in-flight RPC calls")?;
        let writer = writer.clone();
        let backend = backend.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let duration = if request.method == "launchpad.action"
                && request.params.get("action").and_then(Value::as_str) == Some("pick")
            {
                125
            } else {
                15
            };
            let result =
                tokio::time::timeout(Duration::from_secs(duration), backend.dispatch(&request))
                    .await;
            let response = match result {
                Ok(Ok(result)) => json!({"id":request.id,"result":result}),
                Ok(Err(error)) => json!({"id":request.id,"error":{"message":error.to_string()}}),
                Err(_) => {
                    json!({"id":request.id,"error":{"message":"Rust backend request timed out"}})
                }
            };
            let mut data = serde_json::to_vec(&response).expect("JSON value serializes");
            data.push(b'\n');
            let _ = writer.lock().await.write_all(&data).await;
            if request.method == "backend.stop" {
                let _ = backend.shutdown.send(true);
            }
        });
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
        bail!("Backend directory must be private (0700), owned by this user, and not a symlink");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut socket = None;
    let mut parent = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(args.next().context("--socket needs a path")?))
            }
            "--parent-pid" => {
                parent = Some(
                    args.next()
                        .context("--parent-pid needs a PID")?
                        .parse::<u32>()?,
                )
            }
            _ => bail!("Unknown option {arg}"),
        }
    }
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?);
    let directory = runtime.join("edgeline-rust");
    private_directory(&directory)?;
    let socket = socket.unwrap_or_else(|| directory.join("backend.sock"));
    private_directory(socket.parent().context("Socket needs a parent directory")?)?;
    if socket.symlink_metadata().is_ok() {
        let metadata = std::fs::symlink_metadata(&socket)?;
        if !metadata.file_type().is_socket() || metadata.uid() != uid() {
            bail!("Refusing to replace a non-owned socket path");
        }
        if UnixStream::connect(&socket).await.is_ok() {
            bail!("A Rust backend is already running at this socket");
        }
        std::fs::remove_file(&socket)?;
    }
    let scripts = directory.join(format!("scripts-{}", std::process::id()));
    private_directory(&scripts)?;
    let (shutdown, mut signal) = watch::channel(false);
    let backend = Arc::new(Backend {
        kwin: kwin::Kwin::new(&scripts).await?,
        shutdown,
    });
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    println!("{}", json!({"event":"ready","socket":socket}));
    let parent_shutdown = backend.shutdown.clone();
    if let Some(expected) = parent {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                // getppid is a value-only libc call with no pointer preconditions.
                if unsafe { libc::getppid() } as u32 != expected {
                    let _ = parent_shutdown.send(true);
                    break;
                }
            }
        });
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            _ = signal.changed() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let backend = backend.clone();
                tokio::spawn(async move { if let Err(error) = serve(stream, backend).await { eprintln!("RPC connection: {error}"); } });
            }
        }
    }
    backend.kwin.close().await;
    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_dir_all(scripts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }
    #[tokio::test]
    async fn rpc_reads_separate_lines_and_caps_size() {
        let mut input = BufReader::new(&b"one\ntwo\n"[..]);
        assert_eq!(read_message(&mut input).await.unwrap().unwrap(), b"one\n");
        assert_eq!(read_message(&mut input).await.unwrap().unwrap(), b"two\n");
        assert!(read_message(&mut input).await.unwrap().is_none());
        let oversized = vec![b'x'; MAX_MESSAGE + 1];
        assert!(
            read_message(&mut BufReader::new(oversized.as_slice()))
                .await
                .is_err()
        );
    }
}
