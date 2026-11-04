# Reserve desktop space beside the Edge home panel

The left rail now has a real Wayland exclusive zone, not merely an always-on-top
window or custom fit geometry. KDE's usable work area on the Edge becomes
2464×720, starting 96 pixels to the right of the output's left edge. Normal
window placement and maximization respect it without needing the Move-to-Edge
button. Other monitors and the Edge's physical 2560×720 mode are unchanged.

## Implementation

`rust-backend/src/bin/edgeline-reserve.rs` is a small native Wayland client using
`zwlr_layer_shell_v1`. It creates one transparent ARGB shared-memory surface:

- explicitly attached to the sole unscaled, unrotated 2560×720 output;
- anchored left/top/bottom with an exclusive zone of 96 pixels;
- empty input region and keyboard interactivity None;
- no input-device access, pixel capture, portal consent or compositor settings.

On output removal or ambiguity it releases the reservation. It follows output
changes without assigning the strip to another monitor. A private, per-user
advisory lock allows only one active owner. Additional hosts wait for ownership
rather than doubling the reserved width. Parent process start time is monitored
so a dead, zombie or reused PID cannot leave an orphaned reservation.

Both the Electron rollback host and native Tauri host own this Rust helper.
It stops with the host. A compositor disconnect or process crash also destroys
the Wayland surface, which automatically restores the ordinary work area. No
Plasma panel or persistent display configuration is installed.

## Scope

This is a work-area reservation, not a hardware crop or resolution change.
Applications explicitly forcing fullscreen, restoring their own coordinates or
being manually dragged over panel space may ignore normal work-area hints.
Use windowed mode and the existing fit button for games that do so. Existing fit
logic does not subtract the rail twice once the compositor reserves it.

## Build / check

```sh
cargo build --locked --release --manifest-path rust-backend/Cargo.toml
rust-backend/target/release/edgeline-reserve --check
```

`--check` discovers protocols/output metadata but creates no surface or zone.
The app starts the helper with `--parent-pid <pid> --wait`. The packaged Electron
build includes it under `resources/backend/edgeline-reserve`; the native host
uses that resource location or the source release build. An operator may override
the path with `EDGELINE_WORKAREA_HELPER`.

## Verification

- Rust tests cover output ambiguity, scale/rotation guards and process identity.
- Lifecycle tests cover single helper ownership per host, bounded retries,
  app stop and the parent/wait arguments.
- KWin fit regression verifies no double subtraction when work area is already
  reserved.
- Live KWin query confirmed a 96-pixel reduction only on the Edge. All three
  other output geometries and work areas were unchanged.
- On helper exit, every output work area returned exactly to its baseline.

The current legacy session was enabled without restarting its UI, using a
transient helper tied to that host's PID. Future app launches start their owned
helper directly. This does not make the unfinished Rust hardware/gesture
migration complete.
