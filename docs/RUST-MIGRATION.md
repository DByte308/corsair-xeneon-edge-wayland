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

## Still pending

- C++/Qt hardware agent: HID, DDC, sensors, profiles and its existing RPC calls.
- Electron window/tray host: replace with Tauri/Rust after API parity testing.
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

## Opt-in UI trial / rollback

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
