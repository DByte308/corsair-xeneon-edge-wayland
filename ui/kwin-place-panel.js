// SPDX-License-Identifier: GPL-3.0-or-later
// One-shot compositor placement; no global-shortcut routing.
(function () {
    var outputs = workspace.screens.filter(function (s) {
        return s.geometry.width === 2560 && s.geometry.height === 720;
    });
    if (outputs.length !== 1) return;
    var g = outputs[0].geometry;
    var windows = workspace.windowList();
    var controlVisible = windows.some(function (w) { return String(w.caption) === 'EdgeLine' && !w.minimized; });
    windows.forEach(function (w) {
        var title = String(w.caption);
        if (!['EdgeLine dashboard','EdgeLine','EdgeLine taskbar'].includes(title)) {
            // Make room for the rail in existing windowed Edge apps too.
            if (w.normalWindow && w.output === outputs[0] && !w.fullScreen && w.moveable && w.resizeable) {
                var old = w.frameGeometry;
                if (old.x < g.x + 96) {
                    var x = g.x + 96;
                    w.frameGeometry = {x:x,y:old.y,width:Math.min(old.width,g.x+g.width-x),height:old.height};
                }
            }
            return;
        }
        w.fullScreen = false;
        w.setMaximize(false, false);
        var isRail = title === 'EdgeLine taskbar';
        w.frameGeometry = {x:g.x+(isRail?0:96),y:g.y,width:isRail?96:g.width-96,height:g.height};
        w.keepAbove = isRail || title === 'EdgeLine' || !controlVisible;
    });
    windows.forEach(function (w) {
        if (String(w.caption) === 'EdgeLine taskbar') workspace.raiseWindow(w);
    });
})();
