// SPDX-License-Identifier: GPL-3.0-or-later
// One-shot: put a specific window into a slot on the Edge. The rectangle is
// computed by the core; this only asks the compositor to apply it.
(function () {
    var id = __EDGELINE_WINDOW_ID__;
    var rect = __EDGELINE_RECT__;
    var windows = workspace.windowList();
    var target = windows.filter(function (w) { return String(w.internalId) === id; })[0];
    if (!target) return;
    if (!target.output || target.output.geometry.width !== 2560 || target.output.geometry.height !== 720) {
        print('EdgeLine: target left the Edge, not snapping it');
        return;
    }
    if (target.fullScreen) target.fullScreen = false;
    if (target.maximizeMode) target.setMaximize(false, false);
    if (target.moveable === false || target.resizeable === false) {
        print('EdgeLine: target refuses moving or resizing');
        return;
    }
    target.frameGeometry = {x:rect.x, y:rect.y, width:rect.width, height:rect.height};
    // Snapping implies wanting to look at it.
    target.minimized = false;
    workspace.activeWindow = target;
    workspace.raiseWindow(target);
})();
