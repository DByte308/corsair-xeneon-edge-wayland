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
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui = root.join("../ui");
    let stage = root.join("web-dist");
    for name in ["renderer", "assets"] {
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
    for entry in fs::read_dir(stage.join("renderer")).expect("staged renderer") {
        let path = entry.expect("staged renderer entry").path();
        if path.extension().is_some_and(|s| s == "html") {
            let html = fs::read_to_string(&path).expect("staged HTML");
            fs::write(
                path,
                html.replace(
                    "default-src 'none';",
                    "default-src 'none'; connect-src ipc: http://ipc.localhost;",
                ),
            )
            .expect("adapt local IPC policy");
        }
    }
    println!("cargo:rerun-if-changed=frontend-bridge.js");
    println!("cargo:rerun-if-changed=smoke-test.js");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["edgeline"])),
    )
    .expect("build local-only Tauri command permissions");
}
