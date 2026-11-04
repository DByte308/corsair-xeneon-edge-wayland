// SPDX-License-Identifier: GPL-3.0-or-later
// One-shot placement for an app window. Wayland clients cannot position
// themselves, so the core asks the compositor, exactly like the dashboard.
// A new toplevel can be mapped before its caption or class is set, so this
// keeps watching for a few seconds instead of trusting the first event.
(function () {
    var title = __EDGELINE_APP_TITLE__;
    var rect = __EDGELINE_APP_RECT__;
    var placed = null;
    function matches(w) {
        if (!w || String(w.caption) !== title) return false;
        var resourceClass = String(w.resourceClass);
        // An unresolved class is allowed: it can arrive after the caption.
        return !resourceClass || /^edgeline$/i.test(resourceClass);
    }
    // The dashboard is opened always-on-top, so an app launched while Home is
    // up would appear underneath it. Placement is a good moment to drop that:
    // the compositor is the authority on stacking, not the Electron window.
    function lowerDashboard() {
        workspace.windowList().forEach(function (w) {
            if (String(w.caption) === 'EdgeLine dashboard') w.keepAbove = false;
        });
    }
    function place() {
        workspace.windowList().forEach(function (w) {
            if (!matches(w)) return;
            if (w.fullScreen) w.fullScreen = false;
            if (w.maximizeMode) w.setMaximize(false, false);
            if (w.moveable === false || w.resizeable === false) {
                print('EdgeLine: app window refuses placement');
                return;
            }
            var frame = w.frameGeometry;
            if (Math.abs(frame.x - rect.x) < 1 && Math.abs(frame.y - rect.y) < 1
                && Math.abs(frame.width - rect.width) < 1 && Math.abs(frame.height - rect.height) < 1) {
                placed = w;
                return;
            }
            // Place only. An app window behaves like any other window: the rail
            // raises it on demand instead of it floating over a game.
            w.frameGeometry = {x:rect.x, y:rect.y, width:rect.width, height:rect.height};
            lowerDashboard();
            placed = w;
        });
    }
    function watch(w) {
        if (!w) return;
        ['captionChanged', 'windowClassChanged', 'frameGeometryChanged'].forEach(function (name) {
            if (w[name] && typeof w[name].connect === 'function') {
                w[name].connect(function () {
                    // Once placed, later geometry is the user's business.
                    if (placed === w) return;
                    place();
                });
            }
        });
    }
    lowerDashboard();
    workspace.windowList().forEach(watch);
    workspace.windowAdded.connect(function (w) { watch(w); place(); });
    place();
})();
