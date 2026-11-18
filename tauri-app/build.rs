// SPDX-License-Identifier: GPL-3.0-or-later
use std::{fs, path::Path};
fn copy(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create UI staging directory");
    for entry in fs::read_dir(source).expect("read frontend directory") {
        let entry = entry.expect("frontend entry");
        let to = target.join(entry.file_name());
        if entry.file_type().expect("frontend file type").is_dir() {
            copy(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).expect("copy frontend asset");
        }
    }
}
/// Allow Tauri's local IPC transport in every staged page, in place. Discovery is
/// recursive because app pages live one or two directories deeper.
fn adapt_csp(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            adapt_csp(&path);
            continue;
        }
        if !path.extension().is_some_and(|s| s == "html") {
            continue;
        }
        let Ok(html) = fs::read_to_string(&path) else {
            continue;
        };
        let patched = html.replace(
            "default-src 'none';",
            "default-src 'none'; connect-src ipc: http://ipc.localhost;",
        );
        if patched != html {
            fs::write(&path, patched).expect("adapt local IPC policy");
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui = root.join("../ui");
    let stage = root.join("web-dist");
    for name in ["renderer", "assets", "apps"] {
        println!("cargo:rerun-if-changed={}", ui.join(name).display());
        copy(&ui.join(name), &stage.join(name));
    }
    for name in ["css", "webfonts"] {
        copy(
            &ui.join("node_modules/@fortawesome/fontawesome-free")
                .join(name),
            &stage
                .join("node_modules/@fortawesome/fontawesome-free")
                .join(name),
        );
    }
    // The source UI's CSP was written for Electron IPC. Adapt only the staged
    // HTML security policy to permit Tauri's local IPC transport; no UI edits.
    // App pages get the same treatment: they talk to the host through the injected
    // bridge, which needs the same transport allowance.
    adapt_csp(&stage.join("renderer"));
    adapt_csp(&stage.join("apps"));
    println!("cargo:rerun-if-changed=frontend-bridge.js");
    println!("cargo:rerun-if-changed=smoke-test.js");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["edgeline", "app_call", "app_info", "app_close"])),
    )
    .expect("build local-only Tauri command permissions");
}
