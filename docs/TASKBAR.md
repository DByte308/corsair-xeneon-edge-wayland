# Persistent Edge taskbar

A dedicated 96-pixel window runs down the left edge of the Edge display. It is
not part of the dashboard, so covering the dashboard with another app does not
remove the taskbar.

- Home returns to the launchpad and hides the control/settings window.
- Deck opens Tablet Deck directly (still the no-key preview).
- App icons list normal windows assigned to the Edge, including minimized apps.
  Selecting one restores/raises it and highlights its active state.
- Settings opens the EdgeLine controls.
- The app list scrolls when there are more icons than fit vertically.

KWin positions the taskbar at the physical Edge's left edge and keeps it above
windowed/fitted apps. A native Rust layer-shell helper now reserves a real
96-pixel exclusive zone, so normal desktop placement and maximization use the
remaining work area instead of going under the rail. EdgeLine's controls and
dashboard occupy the remaining 2464×720 region. Move-to-Edge actions use the usable work area to the right of
the rail; existing windowed Edge apps overlapping the rail are moved/shrunk into
that region. Other monitors are untouched. Restore retains the previous saved
geometry as before. Apps insisting on exclusive fullscreen are not covered by
this windowed-mode guarantee.

The reservation surface is transparent, has an empty input region and requests
no keyboard focus. It therefore cannot cover the rail visually or intercept its
taps. Its lifetime is tied to the app; closing/crashing the owner removes the
reservation automatically. A shared lock prevents multiple app hosts reserving
the same strip twice. See [WORKAREA.md](WORKAREA.md) for details.

No Plasma panel is installed, and no global display layout is changed. The rail
is hidden when the Edge disconnects. Like the current backend, detection requires
one unambiguous 2560×720 logical output.

## Validation

Unit tests verify rail geometry, content reservation, window stacking, isolation
of other monitors and absent-display behavior. Live packaged-app checks verified:

- rail geometry 96×720 at the Edge's left edge;
- game and controls start 96 pixels to its right;
- selecting Vanguard from the rail activates the game while the rail stays above;
- Home raises the home screen without removing the rail;
- Deck opens the embedded Tablet Deck page directly.
