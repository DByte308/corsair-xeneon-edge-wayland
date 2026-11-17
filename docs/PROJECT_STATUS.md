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
  - \`state.all\` / \`device.state\` / \`system.info\` served by the Rust backend.
  - event-broadcast channel (per-connection writer registry) so the Rust backend
    can push the events the dashboard relies on.
- **B3** Make Tauri the default build / entry.
- **B4** Remove the Electron \`ui/\` host + \`window-bridge.py\` + legacy runtime
  split — only after the visible acceptance gate (C) passes.
- **C** Visible acceptance run on the real Edge (physical touch, app switching,
  tray, hotplug, Tablet Deck, dashboard tiles) — operator-hosted.
- **D** Enable gestures behind the strict opt-in flag, validated on the real
  panel, with a separate approval.

### Honest gaps / limitations

- **`state.all` is not served by the Rust backend.** The UI calls it on every
  connect (`app.js refreshAll`, `dashboard.js start`, `main.js primeTrayState`).
  Today this is masked because both hosts send generic RPC to the C++ agent
  socket (`edgeline.sock`); the Rust backend socket (`edgeline-rust/backend.sock`)
  only serves core/launchpad/window methods. **Cutover-critical: must be ported
  before B3/B4** (when the C++ agent is retired).
- **No event-broadcast channel.** The C++ agent pushes `ddc`, `screen`, `touch`,
  `sensors`, `ddcLog`, `device`, `rules`, `touchConfig` events; the Rust backend
  is request/response only and has no per-connection writer registry to push to.
  Dashboard tiles and live updates depend on `api.onEvent`. Also masked today by
  the C++ agent, and cutover-critical.
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
