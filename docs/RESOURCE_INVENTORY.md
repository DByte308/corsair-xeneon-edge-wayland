# RESOURCE INVENTORY — EdgeLine / Corsair Xeneon Edge

Playbook: existing-projects (Phase 2). Companion to `AUDIT.md`. Purpose: catalog the
resources the project depends on, so replacement/migration decisions are grounded.
No secret values are recorded here — only variable names / references.

## Source modules & packages

| Resource | Where | Purpose | Active? | Notes |
|---|---|---|---|---|
| C++ agent core | `agent/src/core/*` | HID, DDC, sensors, profiles, rules, touch, gestures, app rules, colormgr | yes (active hardware owner) | ~30 .cpp/.h |
| C++ IPC | `agent/src/ipc/*` | RPC server/client over `$XDG_RUNTIME_DIR/edgeline.sock` | yes | |
| C++ proto | `agent/src/proto/*` | Bragi frame, commands | yes | the frame the Rust hardware.rs codec reimplements |
| C++ transport | `agent/src/transport/*` | HidEnumerator/Recon/Transport | yes | |
| C++ X11 | `agent/src/x11/*` | FocusWatcher, TouchEventSource, TouchProbe | yes | X11-only touch/focus |
| Rust backend | `rust-backend/src/*` | KWin feed, window fit/activate/placement, Steam, artwork, RPC | in progress (control core) | `model.rs`, `kwin.rs`, `hardware.rs`, `main.rs` |
| Rust reserve helper | `rust-backend/src/bin/edgeline-reserve.rs` | Wayland layer-shell 96px rail reservation | yes | owned by both hosts |
| Tauri host | `tauri-app/src/*` | native windows/tray/rail/dashboard/control, autostart, preferences | in progress (preview) | |
| Electron UI | `ui/` (main + renderer + host) | legacy host; owns no hardware | yes (default deployed) | 1413-line `main.js` |
| Renderer pages | `ui/renderer/*` | dashboard, picture, touch, profiles, settings, developer, calibrate | yes | shared web UI, staged in both hosts |
| App registry | `ui/apps/registry.js` | capability-gated app discovery/enable/disable | yes | apps run as separate windows |
| Built-in apps | `ui/apps/builtin/` | now-playing, system-monitor, clipboard, ambient, edge-doctor, app-drawer, snap, phone, profiles | yes | |
| Host modules | `ui/host/*` | media(MPRIS), telemetry, clipboard(klipper), kdeconnect, snap, edge-area | yes | pure JS, testable |

## Frameworks & third-party libraries

| Dependency | Where | Version | Role |
|---|---|---|---|
| Qt6 (Core/Network/DBus) | agent CMake | (find_package Qt6) | C++ agent |
| Electron | ui | ^45.2.1 | legacy host |
| electron-builder | ui | ^24.13.3 | packaging |
| Font Awesome Free | ui | ^6.5.2 | bundled icons |
| Tauri 2 / tauri-build | tauri-app | 2 / 2 | native host |
| wayland-client 0.31 | rust-backend | 0.31 | layer-shell |
| zbus 5.19 | rust-backend | 5.19 | DBus |
| tokio 1.45 | rust-backend | 1.45 | async RPC |
| anyhow/serde/serde_json/uuid/libc | rust-backend | pinned | rust control core |

## Databases / schema / migrations

- None. State lives in profiles/rules/settings as JSON files under the user's
  config dir (agent-owned). Rust preferences use its own `edgeline-rust` config
  separate from legacy for rollback. No SQL database or migrations.

## External services & interfaces

| Service | Interface | Notes |
|---|---|---|
| HID panel (`wch.cn 27c0:0859`) | hidraw (agent) | no root daemon, udev rule |
| DDC/CI | ddcutil + i2c | picture control; needs `i2c` group |
| XInput2 | xinput | touch modes/transformation matrix (X11 only) |
| MPRIS | session bus (agent) | now playing |
| Notification daemon | DBus monitor | notifications |
| NVIDIA / AMD | nvidia-smi / amdgpu sysfs | dashboard GPU tile |
| KWin scripting | DBus (KWin) | window placement/fit, snap |
| Steam | `steam://uris` (rust) | launch + local artwork |
| KDE Connect | `org.kde.kdeconnect` (ui host) | device list |
| colormgr/colord | DBus | colour profiles |
| Klipper | DBus (ui host) | clipboard app |
| AppImageUpdate / GitHub releases | network | update check only, no download-in-app |
| i2c group, ddcutil | system | i2c + ddcutil are external; experimentally used for blanking/power |

## Auth / authorization / secrets

- No secrets in repo. `/etc/` udev rule only gives the HID hidraw node.
- i2c access is via group membership (`usermod -aG i2c`), not granted by package.
- Rust housekeeping: socket jobject owned by current user, mode 0600; RPC peers
  must match `uid()`; private 0700 runtime dir; socket/scripts in private dir.
- Window reports accepted only from KWin's current DBus owner (spoofing guarded).
- App sandbox: renderer runs `contextIsolation`, no node integration, sandbox on;
  CSP allows no remote origins.

## AI / agent components

- No LLM/agent system in the app itself. "Agent" here = the C++ hardware daemon.
- The app is not a copilot; it is a device-control UI. (The `hitl`/AI-assist
  framing in some notes refers to the development workflow, not shipped code.)

## Storage / media / infra / jobs

- Uploads/media: none. Local artwork read from Steam library cache (`librarycache/3471800`),
  inlined as JPEG data URL, ≤4MB.
- File storage: no object store. Config/profiles/rules as files.
- Infra: runs as a desktop user process. No cloud services.
- Scheduled jobs: MPRIS/notification watchers (DBus), sensors poll loop, update
  check once/day (network request only).

## Env / config references (names only)

- `EDGELINE_BACKEND=rust` — opt legacy UI into new services.
- `EDGELINE_RUST_BACKEND` — explicit rust binary path.
- `WEBKIT_DISABLE_DMABUF_RENDERER` — native Wayland rendering workaround
  (`0` opts out). Applied only on NVIDIA/Wayland unless explicitly set.
- `XDG_RUNTIME_DIR` — socket/scripts location.
- `HOME` — artwork/profiles/Steam lookup.
- `GDK_BACKEND=wayland` — smoke-test invocation.

## Tests / fixtures / tooling

| Resource | Location | Count |
|---|---|---|
| C++ tests | agent/tests (+ CMake add_executable) | 14 ctest cases |
| Rust backend tests | rust-backend/src | 13 |
| Tauri host tests | tauri-app/src | 2 |
| UI test suites | ui/test/* | 19 suites (package.json `test`) |
| Rust integration | ui/test/rust-backend.integration.js | hidden, needs live Edge+session bus |
| Smoke test | tauri-app `--smoke-test` | hidden, mutation-blocked |
| Bridge tests | tauri-app/smoke-test.js, test-bridge.js | |
| Fixtures | ui/test/fixtures | app snapshot fixtures |

Notes:
- `npm ci` determinism depends on `ui/package-lock.json`.
- Rust deps pinned with `=` in Cargo.toml; `Cargo.lock` committed.
- `cargo clippy`/`cargo fmt` not confirmed installed on this host.
