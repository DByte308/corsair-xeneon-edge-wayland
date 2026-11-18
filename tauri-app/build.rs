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

/// Rewrite each staged app page's CSP to exactly its manifest policy.
///
/// The apps ship their own `<meta>` policy, but a page must not be trusted to
/// describe its own privileges: the manifest's `network` list is the promise, so the
/// staged page gets a policy derived from it — no external host unless the manifest
/// names one, and the local IPC transport it needs to talk to the host. Built-in apps
/// declare `network: []`, so in practice this is `default-src 'none'` plus self and
/// data:, which is what they already had; the point is that it no longer depends on
/// them getting it right.
fn enforce_app_csp(apps: &Path) {
    // The staged layout is apps/builtin/<id>/ and apps/<user id>/ — descend until a
    // directory actually holds an app.json, rather than assuming a depth.
    let Ok(entries) = fs::read_dir(apps) else { return };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if !dir.join("app.json").is_file() {
            enforce_app_csp(&dir);
            continue;
        }
        let manifest_path = dir.join("app.json");
        let Ok(manifest) = fs::read_to_string(&manifest_path) else { continue };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&manifest) else { continue };
        let entry_name = json["entry"].as_str().unwrap_or("index.html");
        let page = dir.join(entry_name);
        let Ok(html) = fs::read_to_string(&page) else { continue };
        let mut connect = String::from("ipc: http://ipc.localhost");
        if let Some(hosts) = json["network"].as_array() {
            for host in hosts.iter().filter_map(|h| h.as_str()) {
                // Only https, and only a bare host: a manifest is data, and data does
                // not get to widen a policy with schemes, paths or spaces.
                if host.starts_with("https://")
                    && host[8..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                {
                    connect.push(' ');
                    connect.push_str(host);
                }
            }
        }
        let policy = format!(
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; font-src 'self' data:; connect-src {connect}"
        );
        let tag = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">");
        let patched = match html.find("http-equiv=\"Content-Security-Policy\"") {
            Some(_) => {
                // Replace the whole existing tag, keeping everything else.
                let start = html[..html.find("http-equiv=\"Content-Security-Policy\"").unwrap_or(0)]
                    .rfind('<')
                    .map(|at| at)
                    .unwrap_or(0);
                let end = html[start..].find('>').map(|at| start + at + 1).unwrap_or(html.len());
                format!("{}{}{}", &html[..start], tag, &html[end..])
            }
            None => html.replacen("<head>", &format!("<head>\n{tag}"), 1),
        };
        if patched != html {
            fs::write(&page, patched).expect("enforce app CSP");
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
    enforce_app_csp(&stage.join("apps"));
    println!("cargo:rerun-if-changed=frontend-bridge.js");
    println!("cargo:rerun-if-changed=smoke-test.js");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["edgeline", "app_call", "app_info", "app_close", "app_isolation"])),
    )
    .expect("build local-only Tauri command permissions");
}
