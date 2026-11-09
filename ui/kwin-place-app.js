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
    // The rail is pinned as its own column. Nothing the core places may sit in
    // front of it, and nothing may tuck under it: with the reservation gone the
    // work area covers the rail, so placement keeps to the right of it as well.
    function keepRailAbove() {
        workspace.windowList().forEach(function (w) {
            if (String(w.caption) !== 'EdgeLine taskbar' || w.minimized) return;
            if (!w.keepAbove) w.keepAbove = true;
            if (workspace.raiseWindow) workspace.raiseWindow(w);
        });
    }
    function railRight() {
        // The rail's own frame is authoritative: it is pinned to the Edge's left
        // edge, so its right side is where an app window may start.
        var rail = null;
        workspace.windowList().forEach(function (w) {
            if (String(w.caption) === 'EdgeLine taskbar' && !w.minimized) rail = w;
        });
        if (rail) return rail.frameGeometry.x + rail.frameGeometry.width;
        var edge = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        })[0];
        return edge ? edge.geometry.x + 96 : null;
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
            // An app window belongs under the rail, never above it.
            if (w.keepAbove) w.keepAbove = false;
            var target = {x:rect.x, y:rect.y, width:rect.width, height:rect.height};
            var right = railRight();
            if (right !== null && target.x < right) {
                var width = target.x + target.width - right;
                target = {x:right, y:target.y, width:width > 0 ? width : target.width, height:target.height};
            }
            var frame = w.frameGeometry;
            if (Math.abs(frame.x - target.x) < 1 && Math.abs(frame.y - target.y) < 1
                && Math.abs(frame.width - target.width) < 1 && Math.abs(frame.height - target.height) < 1) {
                placed = w;
                return;
            }
            // Place only. An app window behaves like any other window: the rail
            // raises it on demand instead of it floating over a game.
            w.frameGeometry = target;
            lowerDashboard();
            keepRailAbove();
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
        // KWin puts a fullscreen window in a layer above an always-on-top one,
        // so a fullscreen Edge app would cover the rail. It is a panel app, not
        // a game: keep it in its window.
        if (w.fullScreenChanged && typeof w.fullScreenChanged.connect === 'function') {
            w.fullScreenChanged.connect(function () {
                if (placed !== w || !w.fullScreen) return;
                w.fullScreen = false;
                keepRailAbove();
            });
        }
    }
    lowerDashboard();
    keepRailAbove();
    workspace.windowList().forEach(watch);
    workspace.windowAdded.connect(function (w) { watch(w); place(); });
    place();
    keepRailAbove();
})();
