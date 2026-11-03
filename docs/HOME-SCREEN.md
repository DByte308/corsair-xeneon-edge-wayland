# Visual home screen

The launchpad keeps its six large action cards on the left and puts a dedicated
**On this screen** app overview on the right.

- Lists normal application windows assigned to the unique 2560×720 Edge output.
- Excludes EdgeLine itself, desktop panels, skip-taskbar windows and the XWayland
  capture bridge. Apps on other monitors are not shown.
- Shows app/category icons, app name, window title, minimized state and an active
  highlight. Vanguard Galaxy uses its locally cached Steam artwork.
- Selecting an app restores it from minimized state and raises/focuses it on the
  Edge. Its output is checked again before activation; this never moves a window
  back from a different monitor.
- Handles empty, disconnected and backend-error states rather than inventing
  running apps. The renderer polls a cached snapshot every 1.5 seconds; it does
  not spawn a process for each refresh.

## Backend

A session-local KWin script watches window/output/focus changes and sends JSON
to a private D-Bus receiver. A small Python bridge forwards updates over stdout
to the Electron main process. The receiver accepts updates only from the D-Bus
owner of org.kde.KWin. UUIDs and bounded strings are validated before reaching
the sandboxed renderer. Captions are inserted as text, never HTML.

Runtime requirements on KDE Wayland: python3, python3-dbus and python3-gobject
(they are already present on the development Nobara installation). No new npm
runtime dependency or root service is needed. Scripts and the Python receiver
are extracted from app.asar into a private runtime directory before execution.

## Verification

Mocked KWin tests cover Edge-only filtering, own-window exclusion, active and
minimized reporting, missing/ambiguous output and activation rechecks.
The packaged application was tested live: it listed only Vanguard Galaxy on the
Edge, loaded its cached artwork, had no horizontal overflow, and successfully
focused the actual game through its overview entry. KWin and the app feed both
confirmed the game's active state afterward. The home screen was then reopened.
