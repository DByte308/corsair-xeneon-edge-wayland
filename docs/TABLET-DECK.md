# Tablet Deck on the Edge

Open **Tablet Deck** from the home screen's right-hand app box, or choose the
Tablet Deck page in Dashboard settings. A persistent Home button returns to the
launchpad. This is a native ultrawide layout, not the tablet HUD squeezed into a
strip.

## Layout

- Eight columns, up to three rows, with independent section tabs.
- Longer lists split into additional pages; no more than 24 controls per page.
- Button keys, labels, hold/release modes and state semantics are copied without
  alteration from Tablet Deck's existing Star Citizen grid layout.
- Original Tablet Deck manufacturer themes, large text and contrast options.
- EdgeLine reserves a navigation bar; the remaining area is a real browser
  viewport. Fonts and targets are not scaled with CSS transforms.

## Safety / current limitation

**The integration is a no-key preview.** Both `--dry-run` and
`TABLET_DECK_KEYS=none` are enforced when starting the local server. The existing
Linux port only checks whether Star Citizen is running, not that it is focused.
Live key input is intentionally not enabled until that limitation is fixed.
This preview does not change the existing Tablet Deck configuration or the
normal tablet/phone defaults.

The owned server listens only on loopback port 8790. Pairing is still required;
unauthenticated layout requests return 403. The pairing code stays between the
local server and its sandboxed browser frame and is never printed by EdgeLine.
No Electron preload is injected into Tablet Deck, and the frame cannot navigate
the top-level app. No game files are written.

The Tablet Deck source installation must include the Edge changes (native
layout, Edge CSS and `--loopback`). Its location can be overridden with
`EDGELINE_TABLET_DECK_DIR`. The local source changes are on the Tablet Deck
`feature/xeneon-edge` branch; that repository is not bundled into the AppImage.

## Verification

Validated the packaged EdgeLine UI with the local dry-run server:

- All ten pages fit with no horizontal or vertical button overflow.
- At the Edge viewport, controls were approximately 304×149 pixels.
- Large text plus high contrast: all ten pages still fit.
- One synthetic touch tap on Flight Ready produced exactly one dry-run press.
- Server bound to 127.0.0.1 only; unauthenticated layout fetch returned 403.
- Tablet Deck keybind profile remains current; no key/action mappings changed.

In the Tablet Deck source, run `node tools/make-edge-layout.js` after changing
`layouts/star-citizen.json`, then `node tools/check-edge-layout.js` and
`node tools/make-profile.js --check`.
