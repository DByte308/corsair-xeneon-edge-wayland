// SPDX-License-Identifier: GPL-3.0-or-later
// One-shot placement for an app window. Wayland clients cannot position
// themselves, so the core asks the compositor, exactly like the dashboard.
(function () {
    var title = __EDGELINE_APP_TITLE__;
    var rect = __EDGELINE_APP_RECT__;
    function place() {
        workspace.windowList().forEach(function (w) {
            if (String(w.caption) !== title) return;
            if (w.fullScreen) w.fullScreen = false;
            if (w.maximizeMode) w.setMaximize(false, false);
            if (w.moveable === false || w.resizeable === false) {
                print('EdgeLine: app window refuses placement');
                return;
            }
            w.frameGeometry = {x:rect.x, y:rect.y, width:rect.width, height:rect.height};
            w.keepAbove = true;
        });
    }
    place();
    workspace.windowAdded.connect(place);
})();
