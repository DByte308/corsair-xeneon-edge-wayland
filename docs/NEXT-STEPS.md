# Rust migration checkpoint

## Saved and tested

- Rust control backend: native KWin reporting, window fitting/selection,
  activation, placement, Steam requests and local artwork.
- Native Tauri/WebKitGTK host, using the existing web UI rather than Electron.
- All 26 existing frontend API methods retained by the compatibility adapter.
- Separate native taskbar/dashboard/control windows and tray implementation.
- Local-only command permissions; external frames do not receive the host API.
- Rust preferences separated from the existing installation for rollback.
- Release binary builds; Rust tests and Clippy pass.
- Frontend/API/staging checks pass.
- Hidden Wayland smoke test passes: all six page modules load, the Picture page
  is constructed, the real Edge is detected and real compatibility-agent state
  is read with no JavaScript/CSP errors.
- Read-only Rust D-Bus integration passes, including rejecting foreign window
  reports. The working Electron installation remains running unchanged.

## Not finished / not enabled

1. Visible Tauri acceptance: rendering, tray, app switching, taskbar, touch,
   Tablet Deck embedding and physical display hotplug. Hidden DOM/API tests do
   not establish visible presentation or touch behavior.
2. Replace the C++/Qt hardware compatibility agent with Rust services, retaining
   HID/DDC/sensors/profiles behavior and error reporting.
3. Implement and safely validate the two-finger gesture bridge in Rust. It is
   not enabled; the migration does not grab touch devices or request portal
   input permissions.
4. Complete native distribution packaging and controlled cutover. Keep the
   working build available until acceptance succeeds.

Tablet Deck remains a separate third-party Node application, in no-key preview
mode. Do not silently enable its weaker Linux process-running input gate.

See [RUST-MIGRATION.md](RUST-MIGRATION.md) for build/test commands. The native
release executable is `tauri-app/target/release/edgeline-desktop`; use
`--smoke-test` for the hidden, mutation-blocked test. A visible trial should be a
controlled switch, not an unannounced replacement of the current app.
