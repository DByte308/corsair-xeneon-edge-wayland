// SPDX-License-Identifier: GPL-3.0-or-later
// Workaround is confined to this process, never the compositor/session.
use std::{env, os::unix::process::CommandExt, path::Path, process::Command};

fn needs_compat(wayland: bool, nvidia: bool, explicit_override: bool) -> bool {
    wayland && nvidia && !explicit_override
}

pub fn prepare() {
    let wayland = match env::var("GDK_BACKEND").ok().as_deref() {
        Some("wayland") => true,
        Some(_) => false,
        None => env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland"),
    };
    if !needs_compat(
        wayland,
        Path::new("/sys/module/nvidia_drm").exists(),
        env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some(),
    ) {
        return;
    }
    // A mapped WebKit surface failed with a Wayland protocol error on the
    // NVIDIA/AMD development system. The same trial stays alive on this path.
    // Honor any caller override, including 0, rather than force a global policy.
    eprintln!(
        "Rendering: app-local WebKit DMA-BUF compatibility path for NVIDIA/Wayland (WEBKIT_DISABLE_DMABUF_RENDERER=0 overrides)"
    );
    let executable = env::current_exe().expect("Locate native host executable");
    let error = Command::new(executable)
        .args(env::args_os().skip(1))
        .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
        .exec();
    eprintln!("Could not restart with rendering compatibility: {error}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workaround_is_narrow_and_respects_override() {
        assert!(needs_compat(true, true, false));
        assert!(!needs_compat(false, true, false));
        assert!(!needs_compat(true, false, false));
        assert!(!needs_compat(true, true, true));
    }
}
