# TARGET REQUIREMENTS — EdgeLine Rust migration cutover

Playbook: existing-projects (Phase 3). Owner decisions recorded here override any
prior "not enabled" framing. Baseline: branch `feature/rust-backend` @ `4bbc087`.

## Goal (operator decision)

Complete the staged Rust migration and make the **native Tauri host the single,
deployed host**, retiring the Electron `ui/` legacy host. This is not a full
architecture rewrite; it is completing and consolidating the in-flight migration.

## Scope

### In scope

1. **Port the C++/Qt hardware agent to Rust services** with equivalent
   behavior and error reporting for: HID transport, DDC/CI, screen/backlight,
   touch modes + transformation matrix + calibration, gestures (state machine),
   sensors (network/GPU/power), profiles, per-app rules, MPRIS media,
   notifications, colour management, updates. Retain the same RPC method surface
   the frontend already uses.
2. **Complete the rust control core** to cover all 41 current agent methods (or a
   documented, approved subset) through the same `window.edgeline` API contract.
3. **Gesture bridge in Rust**, kept **behind a flag**, validated safely on the
   real panel before any enabling. No digitizer grab or portal input permission
   until a verified path exists.
4. **Native distribution packaging + controlled cutover** (single host).
5. **Retire the Electron host** only after the cutover gate passes.

### Out of scope (explicitly)

- Wayland touch stack rewrite; touch modes/transform remain X11/XInput2 (README
  documents this as a permanent limitation). Gesture *recognition* may become
  Wayland-aware later, but the touch/texture path stays X11 for now.
- A widget SDK, iCUE import, a D-Bus interface (`dev.edgeline.Ctl1`), and
  Flatpak/rpm/AUR packaging — all remain out of scope.
- No new features beyond parity + the single-host consolidation unless separately
  approved.

## What must keep working (non-negotiable)

- Picture control (DDC via ddcutil + brightness over HID), with the panel's real
  reported maxima respected (sharpness max 4, 7 presets, RGB gain only under
  "User 1", input list as reported).
- Four touch modes (off / main-cursor / own-pointer / ripple), panel-local
  calibration with residual-error-before-write.
- Dashboard tiles (clock, cpu, gpu, memory, disk, network, now-playing, power,
  notifications), both NVIDIA (`nvidia-smi`) and AMD (`amdgpu` sysfs).
- Profiles and per-app focus rules (WM_CLASS / `_NET_WM_STATE_FULLSCREEN`).
- The rail: 96px reserved Wayland exclusive zone, 2464×720 work area.
- All 41 RPC methods reachable by the same contract; new Rust backend must not
  break the API surface.

## Constraints

- Must remain **no kernel module, no root daemon**; runs as the user.
- ddcutil + i2c group; udev rule for HID; no secret values in repo.
- Gestures and any input-grabbing behavior must **not** be silently enabled; the
  migration does not grab touch devices or request portal input permissions
  during the "flag + validate" phase.
- Tablet Deck stays a separate third-party Node server, in no-key preview; do not
  silently enable its weaker Linux process-running input gate.

## Acceptance criteria (cutover gate — all must be true)

1. **All automated tests green**: agent suite (replaced by Rust equivalents),
   rust backend (≥ current 13), tauri host, UI suites (19), plus a Rust-side
   integration that replaces `ui/test/rust-backend.integration.js`.
2. **Visible acceptance run passes** on the real Edge: Home/Settings navigation,
   physical touch, app switching + window picker, tray, display hotplug, Tablet
   Deck embedding (no-key), and each dashboard tile shows real values.
3. **No regression** in picture control, touch modes, calibration, profiles/rules.
4. **Single host**: Tauri build is the default, packaged (`bundle` enabled),
   and the Electron host is removed/retired with a documented rollback point.
5. **Gestures**: Rust recognizer validated behind a flag on the real panel with
   no input-grab side effects; enabling is a separate, approved decision.

## Deliverables this playbook run produces

- `docs/AUDIT.md` (done)
- `docs/RESOURCE_INVENTORY.md` (done)
- `docs/TARGET_REQUIREMENTS.md` (this)
- `docs/ARCHITECTURE_AND_MIGRATION.md`
- `docs/DEVELOPMENT_PLAN.md`
- `docs/TESTING.md`, `docs/SECURITY.md`, `docs/DECISIONS.md`, `docs/PROJECT_STATUS.md`
- Rust services + tests implementing the in-scope ports.

## Open questions / assumptions (recorded)

- A1: We assume the existing C++ RPC method set is the contract to preserve.
  If some methods were only for the (now-redundant) Electron host, this may shrink
  — to be confirmed during porting.
- A2: Native host requires unscaled 2560×720; multi/fractional-scaled Edge setups
  are not part of this acceptance gate.
- A3: `cargo clippy`/`cargo fmt` noted as possibly absent on this host; will be
  run/installed as part of the work or documented if unavailable.
