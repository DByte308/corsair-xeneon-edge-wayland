# Rust backend migration — in progress

Branch: `feature/rust-backend`. The existing HTML/CSS/browser JavaScript UI and
its `window.edgeline` API are retained. This is a staged migration, **not yet a
fully Rust application**. The working legacy route stays the default.

## First stage implemented

`rust-backend/` builds a native `edgeline-backend` process with:

- authenticated KWin D-Bus window reporting (replaces the Python receiver);
- Edge app activation and output rechecks;
- window fitting, restoration, taskbar/content placement and native selection;
- Steam launch requests and local cached game artwork;
- bounded, same-user, concurrent JSON RPC over a private Unix socket;
- parent-process lifetime tracking and compositor-script cleanup.

Rust talks directly to D-Bus instead of spawning busctl for ordinary operations.
The small KWin JavaScript scripts remain compositor-side glue: KWin's scripting
API is JavaScript, independently of the backend implementation language.

Window reports accept only KWin's current unique D-Bus owner as sender. The
socket and scripts are inside a private 0700 runtime directory, with the socket
and scripts mode 0600. IPC peers must match the effective user ID. Window UUIDs
are validated; text is bounded; arbitrary shell commands are not an RPC method.

## Native Tauri host implemented

`tauri-app/` is a separate Rust/WebKitGTK host. It does not launch Electron or a
Python window bridge. The same UI files are staged unchanged in appearance; only
the staged HTML CSP is adapted for Tauri's local IPC. The compatibility adapter
preserves all 26 frontend methods and existing event payloads. External frames do
not receive the adapter; explicit Tauri permissions allow only the local main,
dashboard and taskbar windows to invoke the host.

Implemented in Rust: window/tray management, separate taskbar/dashboard/control
windows, preference persistence, autostart handling, backend socket transport,
agent event forwarding, and the local no-key Tablet Deck child-process lifecycle.
The hardware agent remains C++ for now. Tablet Deck itself remains its separate
Node application, deliberately in dry-run mode.

A hidden Wayland smoke test loaded all six original page modules, constructed the
Picture page, detected the real Edge, and read a real `state.all` response from
the existing hardware agent. It reported zero JavaScript/CSP errors. This is a
**DOM/API test, not a visual rendering or touch/presentation acceptance test**.
The Electron process remains available for rollback. A subsequent bounded visible
trial minimized only its EdgeLine windows and restored them on trial exit. The
operator reported the native UI appeared to run correctly. Initial mapped
rendering failed on the NVIDIA/AMD system with a Wayland protocol error; a scoped
`WEBKIT_DISABLE_DMABUF_RENDERER=1` workaround kept it running. The native entry
point applies that compatibility path only on NVIDIA/Wayland when no explicit
value is supplied. Set the variable to `0` to opt out. This does not change
session or compositor settings, and is not a performance benchmark.

The native host currently requires an unscaled, unambiguous 2560×720 Edge, to
match the compositor scripts' coordinate contract. It implements hiding its own
windows on disconnect and restoring previously visible windows on reconnect;
physical hotplug acceptance testing is still pending.

The native host reads legacy panel preferences as a fallback, but writes its own
`edgeline-rust` configuration only on explicit settings changes. Smoke mode blocks
host mutations, hardware writes, child-server startup and preference changes.

## Still pending

- C++/Qt hardware agent: HID, DDC, sensors, profiles and its existing RPC calls.
- Remaining native host parity checks: detailed app switching/picker, tray,
  Tablet Deck, physical touch and hotplug. The operator confirmed basic visible
  rendering and Home/Settings operation in a bounded trial. It is not yet the
  permanently deployed app.
- Settings, packaging and startup ownership consolidation.
- Gestures: the unfinished Python prototype is not enabled or bundled. The
  eventual recognizer, touch proxy and portal handling need a Rust implementation.
- Tablet Deck is a separate third-party Node server; it stays in no-key preview.

No claim is made that every operation is now faster. Performance needs measured
end-to-end comparison once the complete migration exists.

## Build / tests

```sh
cargo test --locked --manifest-path rust-backend/Cargo.toml
cargo build --locked --release --manifest-path rust-backend/Cargo.toml
# If rustfmt and clippy are installed:
cargo fmt --manifest-path rust-backend/Cargo.toml -- --check
cargo clippy --locked --manifest-path rust-backend/Cargo.toml --all-targets -- -D warnings
cd ui
npm test
node test/rust-backend.integration.js
```

The integration test needs the current user's KDE session bus and a connected,
unambiguous 2560×720 Edge. It reads actual window state, checks ten concurrent
requests, validates local artwork, rejects invalid/unsupported calls and confirms
foreign D-Bus reports are denied. It **does not** move, resize or activate windows.
No fake window state or hardware readings are substituted.

Read-only feed startup is separate from controller startup: merely inspecting
window state does not install the window-placement controller or move anything.

## Build / check the Tauri host

```sh
npm --prefix ui ci
cargo build --locked --release --manifest-path rust-backend/Cargo.toml
cargo build --locked --release --manifest-path tauri-app/Cargo.toml
cargo test --locked --manifest-path tauri-app/Cargo.toml
cargo clippy --locked --manifest-path tauri-app/Cargo.toml --all-targets -- -D warnings
node tauri-app/test-bridge.js
# Requires the existing hardware agent; hidden, mutation-blocked test:
GDK_BACKEND=wayland tauri-app/target/release/edgeline-desktop --smoke-test
```

WebKitGTK 4.1, GTK3 and the other Tauri Linux development libraries are required.
`ui/node_modules/@fortawesome/fontawesome-free` supplies the already-used UI
icons during staging. Generated web-dist, permissions and build artifacts are
not source files. No system package installation is needed on the development
machine. Native distribution packaging remains pending (`bundle.active=false`).

For an intentional visible trial, quit the old UI first, then run the native
binary without `--smoke-test`. It will use the Rust control core and the C++
compatibility agent. Do not retire the old host until visual/touch parity passes.

## Opt-in legacy UI trial / rollback

The legacy UI can use the new services without changing any renderer files:

```sh
cd ui
EDGELINE_BACKEND=rust npm start
```

Quit any already-running EdgeLine instance first when deliberately doing a GUI
trial, otherwise Electron's single-instance handling activates that existing
instance. The Rust binary is found in the packaged backend resource or the local
Rust release/debug build. `EDGELINE_RUST_BACKEND` can supply an explicit binary.

Without `EDGELINE_BACKEND=rust`, the deployed legacy service path is unchanged.
Do not remove that rollback route until hardware and host parity are verified.
