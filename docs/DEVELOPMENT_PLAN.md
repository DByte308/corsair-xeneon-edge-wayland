# DEVELOPMENT PLAN — EdgeLine Rust migration + single host

Playbook: existing-projects (Phase 5). Scope = finish the Rust migration and make
the native Tauri host the single deployed host, retiring Electron. Approved
operator decisions: cutover gate = visible acceptance; gestures = flag + validate
first. Each task is small, ordered, independently reviewable, and lands behind its
own tests. Git slice per task; rollback = `git revert` + retained old binary.

> **Workflow note:** implementation is delegated to parallel subagents, one per
> Rust module (see "Execution model"). The tasks below define the contract each
> subagent must satisfy; the parent reviews each slice and runs the gate.

## Success criteria (from TARGET_REQUIREMENTS)

- All 41 agent RPC methods served by the Rust backend; frontend contract unchanged.
- All automated tests green (Rust backend ≥ current 13 + new module tests; tauri
  2; UI 19; one Rust integration replacing `ui/test/rust-backend.integration.js`).
- Visible acceptance run passes on the real Edge.
- Tauri = single host, `bundle` enabled, Electron removed after gate.
- Rust gesture recognizer validated behind a flag; no grab/portal input.

## Execution model

Subagents do the porting, one Rust module (task) at a time, isolated reads/writes
(disjoint `src` files, no two agents touch the same file). Each returns a self-
contained Rust module + tests + a short DIFF-REVIEW note. The parent:
- reviews each slice for API parity + no UI contract drift;
- runs the relevant `cargo test`/`clippy`/`fmt`;
- merges onto `feature/rust-backend` with a descriptive commit;
- maintains DECISIONS.md / PROJECT_STATUS.md.

## Phase A — Port the hardware/core to Rust (the big one)

Ordered so each step is testable and reversible. Sizes approximate C++ LOC to
port, from the audit.

| # | Task (module) | C++ source to port | Scope | Acceptance | Tests to add |
|---|---|---|---|---|---|
| A1 | `delayed/ddc.rs` | DdcClient(213), DdcCapabilities(117), EdgeDevice(36), WriteGate(80) | DDC/CI via ddcutil + i2c; backlight/brightness/contrast/preset/gain/input/blank; strict maxima from panel; write-gate for destructive 0x60/0xD6 | `ddc.set/get/state/restoreDefaults` match C++ fixtures; respects reported maxima | Fixture parity from `test/test_ddcclient`, `test_capabilities` |
| A2 | `delayed/touch.rs` | TouchControl(450), TouchConfig(112), Calibration(137), XinputParse(53) | xinput transformer, mode + matrix, on-panel calibration (residual-before-write) | `touch.*` methods same xinput sequence as C++; matrix math parity | `test/test_touchconfig`, `test_xinput`, `test_calibration` |
| A3 | `delayed/sensors.rs` | SensorSource(628), AmdGpu(103), Gpus(90) | network/GPU/power/load/disk/mem; nvidia-smi + amdgpu sysfs; both vendors read every poll | dashboard tiles real values; `sensors.*` parity | `test/test_amdgpu`, `test_gpus` (+ new sensor tests) |
| A4 | `delayed/media_notify.rs` | MprisWatcher(172), NotificationWatcher(193) | MPRIS now-playing + control; DBus notification monitor (monitor-only) | `media.control`, `notifications.clear`, events | mirror C++ tests where they exist |
| A5 | `delayed/profiles_rules.rs` | Profiles(139), RulesStore(85), AppRules(85), AppSettings(150), ColorProfiles(128), Colormgr(30) | profiles/apply/delete/rename; focus rules; settings; colormgr | `profiles.*`, `rules.*`, `settings.*`, `color.*` parity | `test/test_rules`, `test_colord`, `test_colormgr` |
| A6 | `delayed/updates.rs` | UpdateChecker(216) | once/day GitHub update check; skipVersion; no download-in-app | `updates.check`, `updates.settings`, `update` event | new |
| A7 | `delayed/gestures.rs` | Gestures(197), WaylandTouch(85) | two-finger recognizer state machine, **flag-gated** | builds, disabled by default, no grab/portal; validated on real panel separately | `test/test_gestures` (reuse state-machine fixtures) |

### A0 (foundation, do first)
Convert `rust-backend/src/hardware.rs` from pure codec to an **actual driver**:
open the hidraw device (udev, no root), open/reuse the socket safely, and wire
`ddc.rs`/`touch.rs` onto it. Add an `EDGELINE_HARDWARE=rust` opt-in and a
**compatibility shim** so the UI keeps talking to the same socket. Keep the C++
agent available for rollback.

**Gate after A0:** rung `cargo test`, `cargo clippy`, `cargo fmt`; rust backend
builds with hardware driver, UI smoke via `EDGELINE_BACKEND=rust` still works.

## Phase B — Native host packaging + cutover

| # | Task | Scope | Acceptance |
|---|---|---|---|
| B1 | Rust integration test | replace `ui/test/rust-backend.integration.js` with a Rust-side equivalent (reads real window state, 10 concurrent calls, artwork, rejects invalid/foreign) | green; no fake data |
| B2 | Tauri bundle | set `bundle.active=true`, minimal `.deb`/AppImage packaging; keep old binary until gate | `cargo build --release` produces package |
| B3 | Make Tauri default | native binary is the launched entry; default service path points at Rust backend | default = Tauri |
| B4 | Remove Electron host | delete `ui/` Electron host + `window-bridge.py` + legacy runtime naming after gate passes | committed removal; rollback tag retained |

## Phase C — Acceptance gate (visible, on real Edge)

| # | Check | How | Pass |
|---|---|---|---|
| C1 | Automated suites | all above green | yes |
| C2 | Home/Settings nav | native visible run | yes |
| C3 | Physical touch | Edge touch modes + calibration | yes |
| C4 | App switching + window picker | native | yes |
| C5 | Tray | native | yes |
| C6 | Display hotplug | reconnect Edge | yes (no orphan reservation) |
| C7 | Tablet Deck (no-key) | embed | yes |
| C8 | Dashboard tiles real | cpu/gpu/mem/disk/net/power/media/notify | yes |

Gate must pass **before B4**. If C fails, roll back to C++ agent, fix, retry.

## Phase D — Gestures (behind flag)

Only after A7 + C-gate: enable a strict opt-in flag, validate on the real panel,
write into DECISIONS.md with a separate approval before any default enabling.

## Risks / rollback

- Each phase lands as a separate commit; `git revert` per slice.
- Retained old binary + C++ agent until C-gate passes.
- ddcutil/xinput version drift — surface as errors, never silent.
- Gestures / input-grab — not enabled during migration.
- Clippy/fmt on host — install or document gap; do not claim green if unrun.

## Sequencing / dependencies

A0 → (A1..A7 in parallel via subagents, each on its own file set) → B1 → B2 → B3
→ C-gate → B4 → D. A1..A7 are largely independent (disjoint modules), so they can
be built concurrently by subagents; A0 must land first because later modules build
on the driver. `gestures.rs` (A7) is independent but must stay flag-gated.
