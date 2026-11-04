# Writing an EdgeLine app

Everything an app has to get right, in the order it bites. The contract itself
is in [APPS.md](APPS.md); this file is the practical list.

## The rail is the app switcher

Every normal window on the Edge appears in the left sidebar, including
EdgeLine's own app windows. There is no separate "register with the rail" step,
and nothing to enable: a window shows up because it is on the Edge.

That has consequences:

- **Your window title is the rail label.** Set `document.title` to the app name
  you want a person to read at 96 px wide. Keep it short; two words is already
  long. "Edge Doctor" fits, "Diagnostics and Health Console" does not.
- **Names must be unique.** Two apps that share a caption are ambiguous to the
  placement script and to the rail, so the registry refuses the second one.
- **Names may not start with `EdgeLine`.** That prefix belongs to the sidebar,
  the dashboard and the settings window, which the rail deliberately hides.
- **One window per app.** `openApp` reuses the existing window. Do not open
  additional top-level windows from your page; they are denied.
- **Launchers set `"rail": false`** so they do not appear twice: once as a
  fixed rail button and once as a window tile. Ordinary apps leave it unset and
  show up in the rail while they run.
- **Your manifest is your drawer entry.** The Apps app lists every installed,
  enabled app from `app.json`: name, description, icon. Nothing to register, and
  a disable in `~/.config/edgeline/apps.json` removes the tile.
- **Minimized is visible, closed is gone.** The rail dims a minimized tile and
  clicking it restores the window. Anything you were doing in the background
  stops when the window is closed, because the window is destroyed, not hidden.
- **EdgeLine app windows are marked in the rail** (slightly different avatar
  tint) so a user can tell them from their own applications.

The rail refreshes every 700 ms from the compositor feed. You do not need to
announce yourself or push state; just keep the window's title accurate.

## Window sizing and placement

- A Wayland client **cannot position itself**. `x`/`y` in the manifest are not
  supported and are ignored. The core asks KWin for a rectangle inside the
  Edge's usable area, which starts 96 px in from the physical left edge and is
  2464×720 on a 2560×720 Edge.
- Declare a size that fits that area with a margin. The core clamps to the work
  area, but a 2400×700 app on a 2464×720 work area has no breathing room.
  `1180×680` and `900×600` both feel right in practice.
- Do not set `alwaysOnTop`, do not fullscreen, do not maximize. An app window
  behaves like a normal window, so the rail can raise it and a game can win the
  screen back.
- Do not draw your own window chrome beyond what the design language wants; the
  window is frameless. A visible Close button is expected, and `window.close()`
  is allowed.

## Sandbox and the bridge

- Your page runs with `sandbox: true`, `contextIsolation: true`, no Node, no
  `require`, no filesystem, no `webview`.
- The only host surface is `window.edgelineApp`:
  `call(method, args)`, `info()`, `close()`, `onClosed(cb)`.
- `call` succeeds only for methods your manifest's `capabilities` grant, and
  only for methods the core implements. Unknown method, missing grant and
  "reserved for a later build" are three distinct, readable refusals.
- Do not reach for `window.edgeline`. That is the core's own host API and it is
  not exposed to apps, by design.
- Your page may only ever show its own entry file. Navigating anywhere else
  (including another file in your own directory) is blocked, as is opening a
  child window. Build the UI as one document, or ask for a capability instead
  of a navigation.

## Network, storage, assets

- No network at all unless the manifest lists hosts, and only `https` is
  allowed even then. Requests are checked per hop and re-read from the manifest,
  so a stale allow list cannot linger.
- `localStorage`/`indexedDB` are per app, in a `persist:edgeline-app-<id>`
  partition. Another app cannot read your data, and you cannot read theirs.
- Ship your CSS, fonts and images inside the app directory. A CSP meta tag with
  `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:`
  is the expected baseline: no inline script, no inline handlers, no eval, no
  remote assets.

## Touch and readability

- The Edge is a 2560×720 strip viewed from ~60 cm and touched with a finger.
  Minimum hit target 44×44 CSS px; the rail uses 74 px tiles as a reference.
- Never hide a function behind hover. There is no pointer.
- No dropdowns that open off-screen, no modal chains, no tiny close targets.
- Keep contrast high; the panel sits next to a bright ultrawide.

## Structure that keeps it testable

- Put the logic that computes what to show in a separate module with no DOM
  access, and export it in a way that works in both Node and the sandboxed page:

  ```js
  if (typeof module !== 'undefined' && module.exports) module.exports = { buildReport };
  ```

  `ui/apps/builtin/edge-doctor/model.js` is the reference: the checks are tested
  in `ui/test/doctor.test.js` with fixtures and hostile input, while the page
  only renders the result.
- Add a fixture preview: a tiny Electron script that loads your `index.html`
  with a stub `window.edgelineApp` returning canned data and captures
  `capturePage()`. That is how the doctor's layout was checked without touching
  the desktop.
- Add your suite to `ui/package.json` `scripts.test`. There is no separate
  test runner.

## Capabilities

Ask for the smallest thing that works. Each capability names the methods it
owns:

| Capability | Methods |
| --- | --- |
| `diagnostics:read` | `diagnostics.read` |
| `windows:list` | `windows.list` |
| `windows:activate` | `windows.activate` |
| `windows:fit` | `windows.fit`, `windows.restore` |
| `media:playback` | `media.nowPlaying`, `media.command` |
| `system:telemetry` | `system.telemetry` |
| `display:control` | `display.ddcGet`, `display.ddcSet` |
| `notify` | `notify.show` |

`windows:*` is the privileged one: window fitting applies to every app on the
Edge, so a tile that fits windows can fight a game for the screen. Everything
else is read-only or scoped.

Adding a capability means three edits in the same commit: the method map in
`ui/apps/registry.js`, the handler in `ui/main.js`, and the table above. Until
the handler exists, `IMPLEMENTED` keeps callers from getting a half-answer.

## What the core already gives you

- The reserved work area and the pinned rail. Never reserve space yourself.
- App placement inside the work area.
- Window switching from the rail, including restoring a minimized window.
- Safe mode (`EDGELINE_SAFE_MODE=1`) which loads no user apps at all, and a
  hard failure per app: a page that fails to load is destroyed and logged, and
  the rail keeps working.

## Checklist before calling an app done

1. `app.json` valid, `id` equals the directory, `entry` inside it.
2. Window title short, unique, not starting with `EdgeLine`.
3. Size fits 2464×720 with a margin; no self-positioning, no always-on-top.
4. Only the capabilities you actually use, and every call handles a refusal.
5. CSP meta tag present; no inline script; assets local.
6. `document.title` never changes to something else while running, or the rail
   label changes with it.
7. Close button works, and closing leaves nothing running.
8. Pure model module plus a fixture preview plus a suite in `npm test`.
9. Works with the app disabled and re-enabled from `~/.config/edgeline/apps.json`.
10. Read once on a 96 px wide strip: is the rail label still readable?

## Known gaps for app authors

- The Rust backend feed does not yet carry `screens`/`panel`, so
  `diagnostics.read` under `EDGELINE_BACKEND=rust` reports the compositor as
  unavailable. Do not depend on those fields being present.
- Icons are a curated set (`play`, `pulse`, `clipboard`, `clock`, `moon`,
  `wrench`, `grid`, `note`, `terminal`, `globe`, `chat`, `gamepad`, `camera`,
  `layers`, `bolt`, `heart`). Name one in `app.json`; an unknown name is
  ignored and the rail falls back to your title's initials.
- There is no packaging step for third-party apps. A directory is the unit, and
  installing means copying it to `~/.local/share/edgeline/apps/<id>/`.
