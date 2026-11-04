# EdgeLine apps

EdgeLine is a privileged core plus removable apps. The core owns the things
that are the desktop itself: the reserved work area, the sidebar window, the
KWin controller, window fitting, and display matching. Apps are ordinary web
pages the core loads at runtime, and every capability they use is granted by
the core.

An app is a directory. Installing one is copying a folder; removing one is
deleting it or disabling it in config. No rebuild, no register call, no code
change in the core.

## Where apps live

```
ui/apps/builtin/<id>/        shipped with EdgeLine, cannot be removed
~/.local/share/edgeline/apps/<id>/
```

A user app with the same `id` as a builtin shadows it, which is how you try a
modified version of a shipped app without touching the install. Disabling is
reversible and lives in `~/.config/edgeline/apps.json`:

```json
{ "disabled": ["edge-doctor"] }
```

## Manifest

`app.json` in the app directory. Unknown keys are ignored so old cores can load
new apps, but unknown `capabilities` are refused rather than silently dropped.

```json
{
  "id": "edge-doctor",
  "apiVersion": 1,
  "name": "Edge Doctor",
  "description": "What the compositor and the helper processes actually report.",
  "entry": "index.html",
  "capabilities": ["diagnostics:read"],
  "window": { "width": 1180, "height": 660, "resizable": true },
  "network": [],
  "rail": false
}
```

- `id` matches `^[a-z0-9][a-z0-9-]{1,40}$` and must equal the directory name.
- `apiVersion` is the contract version. An app declaring a version the core
  does not implement is listed as incompatible instead of half-loaded.
- `entry` is resolved inside the app directory. `..`, absolute paths and
  symlinks that escape the directory are rejected.
- `network` is a host allow list. Empty means no network at all; the app's
  session partition blocks everything that is not `file:` or IPC.
- `rail` is accepted and ignored. The sidebar lists every window on the Edge,
  so an app needs no registration to appear there. See
  [APP-DEVELOPMENT.md](APP-DEVELOPMENT.md).
- `name` may not start with `EdgeLine`, and no two installed apps may share a
  name: the window caption is the rail label and the compositor placement key.

## Capabilities

The bridge is one method call, `app:call(appId, method, args)`. The core looks
up the calling window, checks the method against that app's granted
capabilities, and refuses anything else. There is no ambient `window.edgeline`
with all 26 host methods in an app window.

| Capability | Methods | Status |
| --- | --- | --- |
| `diagnostics:read` | `diagnostics.read` | implemented |
| `windows:list` | `windows.list` | implemented |
| `windows:activate` | `windows.activate` | implemented |
| `windows:fit` | `windows.fit`, `windows.restore` | reserved |
| `media:playback` | `media.nowPlaying`, `media.command` | implemented |
| `system:telemetry` | `system.telemetry` | implemented |
| `clipboard:read` | `clipboard.read` | implemented |
| `clipboard:write` | `clipboard.write` | implemented |
| `display:control` | `display.ddcGet`, `display.ddcSet` | reserved |
| `notify` | `notify.show` | implemented |

Reserved means the contract names the method and the owning capability, but the
core does not answer it yet. An app that calls one gets a typed refusal, not an
undefined function.

Window control is the privileged one. `windows:*` applies to every normal
window on the Edge, so a tile that can fit windows can also fight a game for
the screen. Treat it like a permission prompt when third-party apps start
asking for it.

## Placement

A Wayland client cannot position its own toplevel, so the core places app
windows through the compositor, the same way it places the dashboard: the app's
manifest gives the size, and the core asks KWin for a rectangle inside the
Edge's usable area, never inside the reserved strip. `window.x/y` in a manifest
is therefore not supported on purpose.

## Opening an app

The tray has an entry per builtin app. From a terminal, a second launch is a
request to the running instance instead of a second copy:

```
electron . --open-app=edge-doctor
```

`EDGELINE_OPEN_APP=<id>` does the same on startup, which is what the local
preview used while building the first app.

## Built-in apps

| App | Capabilities | What it does |
| --- | --- | --- |
| `edge-doctor` | `diagnostics:read` | work area, sidebar frame, controller and helper state |
| `now-playing` | `media:playback` | MPRIS artwork, transport controls, progress |
| `system-monitor` | `system:telemetry` | hwmon temperatures, fans, load, memory, NVIDIA/AMD GPUs |
| `clipboard` | `clipboard:read`, `clipboard:write` | Klipper history as touch targets |
| `ambient` | `media:playback` | clock and now-playing strip for an idle panel |

## Isolation

Every app window gets `sandbox: true`, `contextIsolation: true`,
`nodeIntegration: false`, a preload generated from the manifest capability list,
and its own `persist:edgeline-app-<id>` session partition so one app cannot read
another app's storage. The window is a plain `file:` page with a CSP that
forbids inline script and remote script.

A crashing or hung app must never take the sidebar with it: app windows are
created lazily, destroyed when hidden, and the core runs with zero user apps
whenever an app fails to load. `EDGELINE_SAFE_MODE=1` starts the core with no
user apps at all.

## Native helpers

Apps that need a process (gestures, dictation) do not get to spawn one
directly. They ask the core, which runs the helper as a `systemd-run --user
--scope` unit with `NoNewPrivileges=yes`, `ProtectSystem=strict`,
`ProtectHome=read-only`, `PrivateTmp=yes` and no network unless the manifest
declares it. This is the Linux-native equivalent of a container, and it keeps
the one guarantee that matters: a bad app cannot silently gain hardware access.

## Contract versioning

`apiVersion` is bumped only for breaking changes to the manifest or method
shapes. Additive methods and capabilities do not bump it. The core keeps
support for the previous version for one release cycle, and lists incompatible
apps as disabled rather than skipping them silently.

## Migration order

The core and the apps are separated before anything is rewritten, so no step
can leave the desktop without a working sidebar.

1. Contract, registry, capability bridge, Edge Doctor (this step). No existing
   page is touched.
2. Settings becomes an app. The page keeps its markup; only its host calls move
   behind capabilities.
3. Touch, then Home, then the Tablet Deck preview.
4. New apps on the proven contract: now playing, system telemetry, ambient
   mode, launch profiles, snap zones. The rail already lists every running Edge
   window, EdgeLine apps included, so switching works for all of them.
5. Native-helper apps last, behind the sandboxed scope runner.

## Known core-only behaviour

These are not apps and must never become apps:

- reserving the Edge work area (`edgeline-reserve`)
- keeping the sidebar at the physical left edge
- the KWin controller and its shortcut namespace
- fitting and restoring windows
- choosing the Edge output and the DMA-BUF compatibility re-exec

## Parity with the native host

The registry, manifest validation and capability table are host-side and shared.
The Tauri host implements the same three entry points: `apps.list`,
`apps.readManifest` and `app:call`. The Electron host is the reference
implementation until the native host reaches feature parity, and the Rust
compositor feed still needs the screens and panel geometry that the Electron
feed carries today.
