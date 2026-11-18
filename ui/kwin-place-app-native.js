// edgeline: place one app window by its compositor id.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Wayland clients cannot position themselves, so the host asks the compositor.
// This is the native host's counterpart to `kwin-place-app.js`, with two
// differences the audit insisted on:
//
//   1. Identity is the window's `internalId` (the same UUID `window.snapshot`
//      reports), not its caption. Matching on a title falls apart the moment an
//      app writes a document name into it.
//   2. It waits for the window to exist — checking `windowList()` first and then
//      `windowAdded` — instead of betting on a fixed delay after mapping.
//
// It places once and remembers that it did, so a later event cannot move a window
// the user has since dragged.
(function () {
    // Identity: the compositor id when the host already knows it, otherwise the
    // window title plus a class that belongs to our host. The id is more precise but
    // only exists once the window is in the feed, and a Wayland window that has not
    // been placed yet may sit on another output and so not be in it at all.
    var target = __EDGELINE_WINDOW_ID__;
    var title = __EDGELINE_WINDOW_TITLE__;
    var rect = __EDGELINE_APP_RECT__;
    var placed = false;

    function matches(w) {
        if (!w) return false;
        if (target) return String(w.internalId) === target;
        if (String(w.caption) !== title) return false;
        var resourceClass = String(w.resourceClass);
        // An unresolved class is allowed: it can arrive after the caption.
        return !resourceClass || /^edgeline/i.test(resourceClass);
    }

    function apply(w) {
        if (placed || !matches(w)) return;
        placed = true;
        // Nothing the host places may sit in front of the rail or tuck under it.
        workspace.windowList().forEach(function (other) {
            if (String(other.caption) !== 'EdgeLine taskbar' || other.minimized) return;
            if (!other.keepAbove) other.keepAbove = true;
        });
        workspace.windowList().forEach(function (other) {
            if (String(other.caption) === 'EdgeLine dashboard') other.keepAbove = false;
        });
        // Raise it above the panel: `workspace.activeWindow` alone did not bring it
        // to the front on this compositor (measured: the window appeared in the feed
        // with active=false), which is the same reason the Electron host asks for the
        // top explicitly after show().
        w.keepAbove = true;
        // A fullscreen window sits in a higher layer than keepAbove, so it has to
        // leave fullscreen before the geometry sticks.
        if (w.fullScreen) w.fullScreen = false;
        w.frameGeometry = {
            x: Math.round(rect.x),
            y: Math.round(rect.y),
            width: Math.round(rect.width),
            height: Math.round(rect.height),
        };
        // Bring it to the front: the host shows the window before placing it, so
        // without this it opens behind the panel (Wayland will not raise on a focus
        // request alone, which is why the Electron host calls moveTop()).
        workspace.activeWindow = w;
    }

    function scan() {
        workspace.windowList().forEach(apply);
    }

    scan();
    if (!placed) {
        var handler = workspace.windowAdded.connect(function (w) {
            apply(w);
            if (placed) workspace.windowAdded.disconnect(handler);
        });
    }
})();
