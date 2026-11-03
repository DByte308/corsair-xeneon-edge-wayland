# Steam launchpad (KDE Wayland)

Choose Dashboard → Launchpad, then enable the dashboard. Buttons are also
available in the control window, including Restore when a game covers the panel.

- Vanguard Galaxy launches `steam://rungameid/3471800` (confirmed against the
  locally installed Steam manifest).
- Steam library opens `steam://open/library`.
- Move app to Edge targets the last normal application activated before using
  EdgeLine; EdgeLine windows are excluded. The app is unfullscreened and
  unmaximized, then fitted to KWin's usable Edge area.
- Move app back restores its first saved frame geometry, maximization and
  fullscreen state. Repeated fitting does not replace the saved original.
- Choose window invokes KWin's native interactive picker; click a window or Esc.

The KWin controller is loaded for the session, without restarting Plasma and
without assigning any keyboard shortcut. Its callbacks are invoked through
KGlobalAccel. Saved placements last only until the controller or KWin restarts.
The script stays loaded if the UI closes so placement state survives UI restarts.

Steam launch and window movement are separate. Games can reject a window size,
need borderless/windowed mode, or letterbox. We fit the frame, not force the
rendering engine's aspect ratio. Requests are labelled as requests, not claimed
successes. The first version identifies a unique 2560×720 logical output; scaled
or ambiguous outputs are refused. Use the picker if focus tracking chose a
different app. Closing a target clears its saved placement.

## Validation

Node tests exercise fit, repeated fit, restore, picker UUID lookup, missing and
ambiguous output, and closed target. Existing display detection and distro tests
also pass. A live disposable Konsole window on KDE Wayland was fitted to KWin's
Edge usable area (2560×676 with the panel reserved) and restored; both geometry
comparisons passed. This does not verify Vanguard Galaxy's own rendering or the
interactive picker's click path; these still need user testing.

The UI now uses Electron 45.2.1: the inherited Electron 33 runtime rendered a grey
window on this machine despite a fully populated DOM. Native Wayland rendering
was confirmed visually by the operator after the runtime update.
