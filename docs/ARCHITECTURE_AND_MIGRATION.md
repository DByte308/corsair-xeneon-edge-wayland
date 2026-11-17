# ARCHITECTURE & MIGRATION — EdgeLine

Playbook: existing-projects (Phase 4). Current state, target state, gaps, options,
trade-offs, proposed migration. Companion to AUDIT/TARGET_REQUIREMENTS.

## Current architecture (verified)

```
                    ┌──────────────────────────────────────────┐
                    │  ui/  Electron 33 legacy host (DEFAULT)  │
                    │  main.js 1413 loc · renderer · host/     │
                    │  Owns windows/rail/tray. No hardware.    │
                    └───────────────┬──────────────────────────┘
                                    │ window.edgeline API
                                    ▼
                    ┌──────────────────────────────────────────┐
                    │  agent/  C++17 + Qt6 (CURRENT OWNER)     │
                    │  Api.cpp 1505 loc, 41 RPC methods         │
                    │  core/*  ~6162 loc total                  │
                    │    SensorSource 628 · TouchControl 450    │
                    │    TouchProbe 366 · DdcClient 213         │
                    │    Gestures 197 · MprisWatcher 172        │
                    │    NotificationWatcher 193 · Profiles 139 │
                    │    AppRules 85 · ColorProfiles 128 · ...  │
                    │  transport/* HidEnumerator/Recon/Transport│
                    │  x11/*  FocusWatcher/TouchEvent/Probe     │
                    └───────────────┬──────────────────────────┘
                                    │ ddcutil · i2c · hidraw · xinput · D-Bus
                                    ▼
                              hardware (panel/NVIDIA/AMD)
```

The native Tauri host (`tauri-app/`) is a parallel preview that stages the same
web UI but uses **`rust-backend`** as its control core and still shells to the
**C++ agent** for hardware.

```
  tauri-app (native)  →  rust-backend (control core: KWin fit/select/activate,
                         Steam, artwork; 10 methods)  →  C++ agent (hardware owner)
```

### The duplication problem

- **Two hosts** (`ui/` Electron, `tauri-app/` Rust) stage the **same** web UI and
  both own window/tray/rail. Intended for rollback, but it's the single biggest
  maintenance surface and the source of conceptual drift.
- **Two control backends** — rust-backend (control core) and the C++ agent
  (hardware). Only 10 of 41 methods exist in Rust; the rest live in C++.
- The Rust `hardware.rs` is a **pure codec** (no device I/O), so it is a test of
  protocol parity but not an actual device driver.

## Target architecture (single host, hardware in Rust)

```
  tauri-app (native)  ──►  rust-backend (ALL 41 methods; hardware owner)
                             ├─ kwin.rs        (window feed/fit/activate — done)
                             ├─ hardware.rs    (codec — extend to a driver)
                             ├─ ddc.rs         (ddcutil/i2c)
                             ├─ touch.rs       (xinput modes/transform/calib)
                             ├─ gestures.rs    (flag-gated recognizer)
                             ├─ sensors.rs     (network/gpu/power — port from 628loc)
                             ├─ media.rs       (MPRIS), notifications
                             ├─ profiles.rs / rules.rs / colormgr.rs / updates.rs
                             ├─ read*.c        (small DDC/HID syscalls) OR pure Rust
                             └─ rpc (existing multi-tenant socket)
                            └─ (no C++ agent)
  Electron ui/ retired after cutover gate.
```

- **One host** (Tauri), **one backend** (Rust). The web UI renderer is *reused
  unchanged*; only the transport target changes (it already talks to a socket).
- The Rust backend owns all hardware through syscalls/D-Bus, replacing the C++/Qt
  agent. Where a C++ dependency (Qt) provided something, we use the equivalent
  Rust crate or a tiny FFI/command call (ddcutil CLI, xinput CLI, hidraw syscalls).

## Options & trade-offs

### A. In-place parity replacement (recommended)
Port each C++ subsystem to a Rust module under `rust-backend/src/*`, keeping the
exact RPC method names and the existing JSON contract. The frontend and the
renderer stay unchanged. The C++ agent is removed only after the cutover gate.

- **Pros** — frontend untouched (lowest UI risk); every behavior already has a C++
  reference implementation and test fixtures; the socket contract is stable and
  shell-drivable; keeps the "agent/UI separable" design that made testing tractable.
- **Cons** — large port (6,200 loc); must re-derive DDC/touch/gesture behavior from
  C++ fixtures; ddcutil/xinput are external CLIs with version drift; X11 touch
  logic is non-trivial to reproduce.

### B. Fresh Rust-only rewrite
Scrap the routing and design a new architecture from scratch (Rust services +
Tauri, no socket contract, no C++ reference).

- **Pros** — clean design, no legacy vestiges.
- **Cons** — throws away tested behavior and fixtures; much higher risk of subtle
  regressions in picture/touch/calibration; **contradicts the playbook rule to
  preserve established behavior**. Rejected.

### C. Keep both hosts as full citizens
- **Cons** — doubles maintenance; explicitly rejected by operator decision.

## Recommended migration strategy

A hybrid: **Option A for the hardware port** (parity, fixtures-driven) with the
existing Rust **control core kept as-is** for KWin/window work, plus **single-host
consolidation** (Tauri only) once the gate passes. Incremental, reversible, each
step lands behind tests and a rollback point.

## Risks

| Risk | Mitigation |
|---|---|
| Subtle DDC/backlight regressions | Port against C++ fixtures; add Rust parity tests using the same byte fixtures; keep ddcutil CLI path identical. |
| Touch/calibration behavior drift | Preserve the xinput command sequence & matrix math; add Rust tests mirroring Calibration/TouchControl tests. |
| Gestures not reaching panel / grabbing input | Flag-gated; no grab/portal permission during flag+validate; validate on real panel before enabling. |
| X11 touch quirks | Keep X11-only; document; do not attempt Wayland touch rewrite. |
| External CLI drift (ddcutil, xinput) | Version-guard; fail loudly; keep behavior identical to C++ which already shells these. |
| Large port difficulty | Small, ordered steps; each subsystem lands behind its own tests; commit-slice review. |
| Cutover risk | Visible acceptance gate (operator decision); retain a documented rollback point (git tag + old binary) until gate passes. |

## What stays / improves / retired

- **Stays:** web UI renderer, socket RPC design, rail reservation, KWin window
  work, app registry/system, profiles/rules contract, GPL-3.0.
- **Improves:** one backend owns all 41 methods, single host, no dual staging.
- **Retired:** Electron `ui/` host (after gate), C++/Qt agent, `window-bridge.py`,
  the legacy runtime/preferences naming split (consolidated after gate).
