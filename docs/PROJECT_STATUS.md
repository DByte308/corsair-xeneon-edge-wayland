# PROJECT STATUS — EdgeLine Rust migration

Updated 2026-11-18 (end of A0–A7 port). Branch `feature/rust-backend`.

## Playbook run: existing-project rewrite → complete the Rust migration, single host

Operator decisions: (1) finish the Rust migration, (2) make Tauri the single
deployed host (retire Electron), (3) cutover gate = a visible acceptance run,
(4) gestures flag-gated and validated first.

### Done (verified)

- **Audit + inventory + requirements + architecture + plan** committed in
  `docs/`: AUDIT, RESOURCE_INVENTORY, TARGET_REQUIREMENTS,
  ARCHITECTURE_AND_MIGRATION, DEVELOPMENT_PLAN.
- **A0 HID driver** `rust-backend/src/hid.rs` — real driver for the Edge Bragi
  channel (`1b1c:1d0d`): sysfs discovery, open `/dev/hidrawN` as the user
  (no root, O_NONBLOCK), gated exchange (writes require a `Confirmation`
  reason; reads approved by construction), every exchange logged to
  `~/.local/share/edgeline/hid.log`. **Validated live against the panel**:
  brightness 50 / backlight 95 / contrast 50 / RGB 128.
- **A1 DDC/CI** `ddc.rs` — ddcutil-backed, real reported maxima, RGB gain 0..100
  vs reported 0..255 conversion, restoreDefaults by scope, refuses unadvertised
  and unsafe writes (0x60 input, 0xD6 power).
- **A2 touch** `touch/` — modes off/main-cursor/own-pointer/ripple,
  transformation matrix, applyOutputMapping, on-panel calibration
  (residual-before-write), config validation.
- **A3 sensors** `sensors.rs` — network/GPU/power/load/mem/disk, nvidia-smi +
  amdgpu sysfs (both vendors), interface pinning, gpu selection.
- **A4 media+notifications** `media.rs` — MPRIS now-playing + transport
  allow-list, artwork inlined (file:// <=4MiB), monitor-only notification
  D-Bus; additive `media.snapshot` (read-only).
- **A5 profiles/rules/settings/colour** `profiles.rs` — profiles CRUD, focus
  rules (WM_CLASS / fullscreen), settings, colormgr profile default. The Rust
  backend has no X11 focus watcher yet, so `rules.get` reports `watching:false`.
- **A6 updates** `updates.rs` — once/day GitHub check, per-version skip,
  on/off, no download, exit 10 on update.
- **A7 gestures** `gestures.rs` — two-finger/state-machine recognizer,
  **flag-gated and disabled by default** (`GESTURES_ENABLED=false`), pure state
  machine, no device grab / no portal permission. Exposed via read-only
  `gestures.config`.

### Verified at baseline

- `cargo test --locked` (rust-backend): **147 passed + 3 (edgeline-reserve)**; 0
  warnings; release build clean.
- Flaky disk-usage sensor test de-flaked (pure `disk_usage_from` + tolerance).
  Sensor `temp_dir()` hardened with a monotonic counter (parallel-test
  shared-state race removed).
- **B1 done**: `rust-backend/tests/b1_integration.rs` replaces
  `ui/test/rust-backend.integration.js`. Spawns the real `edgeline-backend` on a
  private socket and asserts the read-only contract (Rust `backend.info`, ten
  concurrent requests, artwork null-or-JPEG-data-URL, rejects invalid UUIDs and
  unsupported methods). Live half gated behind `EDGELINE_INTEGRATION_LIVE=1`
  reads the real KWin window snapshot and asserts an untrusted DBus `Update` is
  denied. Live-parallel verified green x6.
- UI: 19 suites pass. Tauri host: 2 pass. C++ agent: 14 pass.
- Live RPC probe against real panel + session bus: screen.state (bright 50),
  media.snapshot (Vivaldi playing), updates.settings, profiles.list,
  rules.get, gestures.config all respond correctly.

### Remaining (not done)

Downstream Phase B–D from the plan, each gated by the cutover decision:
- **B1 done** (committed d58ddbe + 909f1a2 + fixup).
- **B2** Enable Tauri `bundle.active=true` + native packaging (deb/AppImage).
  MUST NOT disturb the legacy rollback route (EDGELINE_BACKEND unset counts as
  the Electron path) until gate C.
- **B** Cutover blockers (must land before B3/B4):
  - \`state.all\` / \`device.state\` / \`system.info\` served by the Rust backend. ✅ **DONE**
    (cutover blocker #16; ported to rust-backend/src/state.rs + main.rs dispatch
    arms; live-verified on a real session bus; field-level parity vs `Api.cpp`
    `systemSnapshot()` and `state.all`).
  - event-broadcast channel (per-connection writer registry) so the Rust backend
    can push the events the dashboard relies on. ✅ **DONE** (task #17): a
    process-wide `tokio::sync::broadcast` fan-out on `Backend`, one forwarder per
    connection, and `{event,data}` lines on the same sockets. Emitted today:
    `sensors` (streamed at the client's interval), `screen`, `touch`,
    `touchConfig`, `profiles` (`{active}`/`{changed}`), `rules`, `update`,
    `ddc`, `ddcLog`. Still missing, each needing a watcher the Rust backend does
    not have: `device` (hotplug), `focus` (rules arming) and
    `touch.point`/`gesture` (XInput2 stream). `ui.action` is now emitted for the
    dashboard vocabulary (`dashboard-toggle`, `dashboard-page-next`): those live
    in the UI process, so `launchpad.action` relays them instead of failing as an
    unknown KWin shortcut, matching `Api::runGestureAction`. The remaining
    gesture actions that C++ performs itself (`brightness-up`/`-down`,
    `blank-toggle`, `profile-next`/`-previous`) belong to the gesture bridge and
    stay unimplemented with gestures disabled. The `sensors` event carries the
    merged media fields: `state.all` and the event share `with_media`, so the
    dashboard's now-playing tile and notification badge survive each tick.
- **B3** Make Tauri the default build / entry.
- **B4** Remove the Electron \`ui/\` host + \`window-bridge.py\` + legacy runtime
  split — only after the visible acceptance gate (C) passes.
- **C** Visible acceptance run on the real Edge (physical touch, app switching,
  tray, hotplug, Tablet Deck, dashboard tiles) — operator-hosted.
- **D** Enable gestures behind the strict opt-in flag, validated on the real
  panel, with a separate approval.

### Honest gaps / limitations

- **`state.all` / `device.state` / `system.info` are now served by the Rust
  backend** (cutover blocker #16, landed). The UI calls `state.all` on every
  connect (`app.js refreshAll`, `dashboard.js start`, `main.js primeTrayState`),
  and today it is still masked by the C++ agent owning the generic socket
  (`edgeline.sock`). With the event-broadcast channel landed, the Rust backend
  is end-to-end parity for the state snapshots and the events listed below.
  `state.all`
  reproduces the nine C++ keys in order (`system/device/ddc/screen/touch/sensors/
  rules/color/touchConfig`).
- **`system.info` reports discovered tools, not a guess.** The Rust backend used
  to hardcode `/usr/bin/ddcutil`, `/usr/bin/xinput` and an invented `xinput`
  version of `1.4`; on a non-FHS distribution (NixOS, Guix) that named tools
  that were not there, and on this host `xinput` is not installed at all, so the
  old value was fiction. `rust-backend/src/tools.rs` now resolves both through
  `PATH` and asks each tool for its version, once at startup (the C++ agent
  re-runs four processes per `system.info`). Empty means "not found" and the
  Settings pane already renders that as `not found` with a note, so the honest
  answer reaches the operator instead of a plausible one.
- **One version number, seven carriers.** `rust-backend` was `0.1.0` while
  everything else was `0.7.0`, and `page-settings.js` warns whenever the agent's
  `system.version` differs from the interface's — so cutover would have shown
  the orange "the agent serving the panel is another version" banner on every
  Settings pane. The backend is `0.7.0` now. The carriers, which still move by
  hand (bump them together): `rust-backend/Cargo.toml`,
  `tauri-app/Cargo.toml`, `tauri-app/tauri.conf.json`, `ui/package.json`,
  `packaging/dev.edgeline.Ctl.metainfo.xml`, and `agent/CMakeLists.txt`
  (`project(edgeline-agent VERSION …)`). The seventh, `updates.rs`
  `EDGELINE_VERSION`, which drives the release-tag comparison and the
  User-Agent, now takes `env!("CARGO_PKG_VERSION")` and is pinned to it by
  `updates::tests::current_version_is_parseable` — it used to be a literal, so a
  crate bump alone made the updater nag an install that was already current.
- **Event broadcast landed** (task #17, cutover blocker). The UI's
  `api.onEvent` paths now have a Rust source: `sensors` streaming (interval
  clamped to >= 250 ms, interruptible so a re-time or disable applies at once,
  and served while the flag is set even with no client, like the C++ agent),
  plus `screen`, `touch`, `touchConfig`, `profiles`, `rules`, `update`, `ddc`
  and `ddcLog` pushed from the dispatch arms or the DDC worker. Two defects the
  audit caught in the first cut are fixed and regression-tested: the `sensors`
  event used to carry placeholder media (it blanked the dashboard's now-playing
  tile and notification badge every second), and a disconnected client leaked
  one fd because the event forwarder stayed parked owning the write half
  (`b1_sensors_stream_broadcasts_merged_snapshots`,
  `b1_closed_connections_release_their_sockets`).
- **No event-broadcast channel for `device`, `focus`, `touch.point`/`gesture`.**
  Those three need machinery the Rust backend does not have yet: a hotplug
  watcher (udev/HID), the X11 focus watcher (so `rules.get` still reports
  `watching:false`), and the XInput2 touch stream (the `touch.stream` arm turns
  on nothing). `ui.action` **is** emitted now: `launchpad.action` relays the
  dashboard vocabulary (`dashboard-toggle`, `dashboard-page-next`) to the UI
  process that owns the dashboard, as `Api::runGestureAction` does, instead of
  failing as an unknown KWin shortcut; the window vocabulary is validated up
  front so a typo cannot load the KWin controller either. Dashboard tiles that
  follow sensors, DDC, profiles and rules update live; anything driven by the
  three watchers above does not.
- Rust backend still reports `hardware: "legacy C++ agent (not migrated)"` in
  `backend.info` — the C++ agent is still the running hardware owner for the
  Electron host; the Rust backend is invoked alongside it during dev. The
  "complete migration" goal means the Rust backend must own hardware directly
  (ddcutil/HID/xinput paths are ported; the running service wiring + cutover is
  what remains).
- `rules.get` returns `watching:false` (no X11 focus watcher in Rust yet).
- `profile.save/apply` write brightness/contrast over the A0 HID path; preset
  (0x14), 0x87 and RGB-gain-over-DDC are not yet driven from the Rust backend
  (they are still DDC-side; ddc.rs has them but the profile apply path only
  writes HID-exposed fields). Documented in ARCHITECTURE_AND_MIGRATION.
- Gestures deliberately disabled; no claim of gesture functionality.
- `cargo clippy` / `cargo fmt` are not installed on this host; not claimed.

> Audit note (2026-11-18): method counts drifting across docs. The Rust backend
> serves **44** dispatch arms; the C++ agent registers **39** calls in Api.cpp;
> earlier docs said 41. The authoritative live count for the Rust backend is the
> `match` arms in `rust-backend/src/main.rs` (44). The two additional arms vs the
> frontend contract are read-only diagnostics `media.snapshot` and
> `gestures.config`. `agent.quit` is obsolete (superseded by `backend.stop`).

### Next recommended step

Proceed to Phase B3/B4 only after the **visible acceptance gate (C)**. The
single-host decision is safe, but the playbook requires the visible run to pass
before retiring the Electron host. If you want to continue now, the next
concrete deliverable is **B1** (Rust-side integration test), which is low-risk
and unblocks the rest of B.
