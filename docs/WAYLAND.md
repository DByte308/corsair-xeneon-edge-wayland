# KDE Wayland fork — initial implementation

Goal: an Edge app/widget surface, while keeping normal windows usable on the
output. This first increment adds the native touch backend, not a finished shell.

## Implemented

- Native Electron Wayland selection when XDG_SESSION_TYPE=wayland.
- KWin D-Bus discovery of the 27c0:0859 wch.cn TouchScreen digitizer.
- Touch enable/disable through KWin, never xinput under XWayland.
- Output mapping through KWin outputName, using kscreen-doctor JSON and the
  current physical 2560x720 mode. Multiple matching outputs are refused.
- Mapping is checked before enabling touch in Main cursor mode.
- Unsupported independent/ripple modes are disabled in the Touch page.
- X11 global touch capture and focus watching are refused on Wayland.
- Updates point to this fork, not upstream releases.

## Limitations / pending verification

Requires KDE Plasma's org.kde.KWin.InputDevice D-Bus API and kscreen-doctor.
Other compositors are not supported yet. KWin's API is compositor-specific.
Five-point calibration and per-app rules have not been ported. Matrix writes
return failure rather than silently applying X11 coordinates to KWin.

Native Wayland window placement/fullscreen targeting still needs end-to-end
verification; Electron cannot assume X11-style absolute window positioning.
Hotplug/persistence and capability reporting need further work.

The full Release build succeeds on the Nobara development machine. All 14
CTest suites and both UI test scripts pass. The read-only probe finds the real
digitizer and DP-1 output. Both the unpacked UI and AppImage have been built in
ui/dist. These are local-host builds, not portable release packages: the agent
uses system Qt and HID libraries. Missing hidapi-devel headers were extracted
locally under ~/.cache/edgeline-build-deps without installing system packages.
No full app runtime claim is made; display placement remains unverified.
No display layout or touch settings were changed during the read-only probe.

The inherited npm dependency tree reports 24 vulnerabilities (including one
critical). Dependency updates and security review are required before treating
this experimental build as a production release.

## Read-only backend probe

From the repository root:

```sh
c++ -std=c++17 -fPIC $(pkg-config --cflags Qt6Core Qt6DBus) -Iagent/src \
  agent/src/core/WaylandTouch.cpp agent/tests/probe_wayland.cpp \
  $(pkg-config --libs Qt6Core Qt6DBus) -o /tmp/edgeline-wayland-probe
/tmp/edgeline-wayland-probe
```
