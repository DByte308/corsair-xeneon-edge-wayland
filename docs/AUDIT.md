# AUDIT — EdgeLine / Corsair Xeneon Edge

Audit date: 2026-11-18. Branch: `feature/rust-backend` @ `4bbc087`.
Playbook: existing-projects (Phase 1). Status: verified facts vs. assumptions vs. unknowns.

## Purpose

Native Linux control for the Corsair Xeneon Edge, a 2560×720 touch strip with no
Linux software. EdgeLine provides picture control (ddcutil + HID), touch modes
(xinput / XInput2), a dashboard, profiles, per-app rules and a sidebar/rail UI.

## Layout

```
agent/        C++17, Qt6 (Core/Network/DBus). Owns every device. No windows.
rust-backend/ Rust 2024. "control-core" migration: KWin feed, window fit/select/
              activate, placement, Steam launch, local artwork, RPC.
tauri-app/    Rust 2024 + Tauri 2 + WebKitGTK. Native host (window/tray/rail/
              dashboard/control), reuses the same web UI files.
ui/           Electron 33 app (legacy rollback host). Vanilla HTML/CSS/JS, no
              framework. Owns no hardware.
packaging/    deb build, AppImage, desktop entry, AppStream, icons.
udev/         60-corsair-xeneon.rules
docs/         TOUCH.md, PROTOCOL.md, WORKAREA.md, RUST-MIGRATION.md, APPS.md,
              APP-DEVELOPMENT.md, NEXT-STEPS.md + screenshots.
agent/build/  CMake build tree (present, binaries exist).
```

## Frameworks / languages / runtime

| Area | Tech | Version evidence |
|---|---|---|
| UI | Electron (legacy host) | `ui/package.json` → `electron ^45.2.1` |
| UI language | vanilla HTML/CSS/JS | renderer/ + host modules; Node `node test/...` runners |
| Agent | C++ | CMake `CXX_STANDARD 17`, `Qt6` (Core/Network/DBus) |
| Control backend | Rust | `rust-backend/Cargo.toml`, `edition 2024` |
| Native host | Rust + Tauri 2 | `tauri-app/Cargo.toml`, `tauri = 2`, `tauri-build = 2` |
| Wayland | zwlr_layer_shell_v1 | rust-backend `edgeline-reserve.rs`; `wayland-client 0.31` |
| IPC | newline-delimited JSON over private Unix socket | `$XDG_RUNTIME_DIR/edgeline.sock` (agent) and `edgeline-rust/backend.sock` (rust) |
| RPC | zbus 5.19 (Rust) | `zbus = 5.19` |

## Verified findings

- Repo is clean; single distro-free `~/projects/corsair-xeneon-edge-wayland`.
- Git is a fork lineage: upstream `aabdelghani/corsair-xeneon-edge-linux`; public
  fork `DByte308/corsair-xeneon-edge-wayland`; local branch `feature/rust-backend`
  tracks `origin/feature/rust-backend`.
- **Tests all pass at the audit baseline:**
  - Agent `ctest` (agent/build): 14/14 passed (~5.5s).
  - Rust control backend: 13 passed (bin `edgeline-backend` 10 + `edgeline-reserve` 3).
  - Tauri host: 2 passed (`rendering::workaround_*`, `preferences_reject_partial_colors`).
  - UI: 19 suites pass (`npm test`, exit 0).
- `backed by docs`: RUST-MIGRATION.md documents the staged migration, the
  WEBKIT_DISABLE_DMABUF_RENDERER=1 Wayland workaround, the preserved legacy route,
  and the pending work list.
- Native host currently requires an **unscaled, unambiguous 2560×720** Edge
  (compositor scripts' coordinate contract).
- Rust backend is a **pure codec for HID frames** so far (`hardware.rs`): it
  neither opens the device nor writes over RPC. The C++ agent still owns hardware
  I/O.
- The rail is a reserved 96px column; work area on the Edge becomes 2464×720.
- App system: capability-gated registry, app drawer, built-in apps
  (now-playing, system-monitor, clipboard, ambient, edge-doctor, snap, phone,
  profiles, app-drawer). Apps are discovered/installed, not hard-coded.
- Architecture is deliberately **layered/socket-gated** so the whole agent can be
  driven from a shell; this kept agent and UI bugs separable.

## Likely findings (need confirmation)

- `tauri-app` bundle is disabled (`bundle.active=false`); no native distro
  packaging yet → native host is preview only, not deployed.
- "Current app" still default = legacy Electron UI; native binary is the preview.
  (per NEXT-STEPS / RUST-MIGRATION).
- Agent's `docs` also still describe the C++/Qt agent as the hardware owner;
  RUST-MIGRATION says C++ agent "remains for now".
- Gestures built/tested in the C++ agent but "touches do not yet reach them on the
  real panel"; greyed out in the Touch page. Rust gesture port is pending and
  **not enabled**.
- Tablet Deck is a separate third-party Node server, in **no-key preview** mode.

## Unknowns / not verified

- Full visual rendering acceptance (visible trial was bounded; only Home/Settings
  navigation confirmed). Not complete feature parity.
- Physical touch coverage, display hotplug acceptance, detailed tray/app-switching
  tests, window picker, Tablet Deck embedding — all marked pending in NEXT-STEPS.
- Performance: no measured end-to-end comparison of the Rust vs. legacy path.
- Not verified that the C++ agent's Qt build is reproducible from a clean CMake
  configure on this host (we ran `ctest` against the existing build tree).
- No `AUDIT.md`/`RESOURCE_INVENTORY.md`/`TARGET_REQUIREMENTS.md` existed before
  this run; docs list is maintained but not the playbook set.
- `cargo clippy` and `cargo fmt` are not confirmed installed on this host (system
  cargo; NEXT-STEPS notes clippy not installed on the host).

## Observed issues (further investigation)

- Dual-host duplication: `ui/` (Electron) and `tauri-app/` both stage the same
  web UI and both own window/tray/rail. This is intentional (rollback) but is the
  main source of maintenance surface.
- C++ agent is the single hardware owner; the Rust backend is only a "control
  core". Replacing the C++/Qt hardware service with Rust services is the largest
  pending item.
- `docs/` splits effort across RUST-MIGRATION.md / NEXT-STEPS.md / WORKAREA.md /
  APPS.md / APP-DEVELOPMENT.md — some content overlaps (the runtime restart/cutover
  guidance appears in more than one place).
- Touch stack is X11/XInput2 only; Wayland touch remains out of scope per README.
