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

## Visible trial checkpoint

The operator confirmed the basic native UI appears to run correctly after a
controlled visible trial (Home/Settings navigation). KWin verified a 96-pixel
left taskbar and 2464-pixel content area. The first mapped launch hit a Wayland
protocol error; an app-local WebKit DMA-BUF compatibility path kept the trial
running. This is applied only on NVIDIA/Wayland unless the caller explicitly
sets `WEBKIT_DISABLE_DMABUF_RENDERER` (including `0` to opt out). No compositor
or session-wide configuration was changed. The old UI was retained for automatic
rollback.

## Not finished / not enabled

1. Remaining native acceptance: detailed tray/app-switching tests, window picker,
   Tablet Deck embedding, physical touch coverage and display hotplug. Basic
   rendering confirmation is not complete feature parity.
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
