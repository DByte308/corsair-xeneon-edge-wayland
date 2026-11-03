// SPDX-License-Identifier: GPL-3.0-or-later
// One-shot compositor placement; no global-shortcut routing.
(function () {
    var outputs = workspace.screens.filter(function (s) {
        return s.geometry.width === 2560 && s.geometry.height === 720;
    });
    if (outputs.length !== 1) return;
    var g = outputs[0].geometry;
    workspace.windowList().forEach(function (w) {
        if (String(w.caption) !== 'EdgeLine dashboard') return;
        w.fullScreen = false;
        w.setMaximize(false, false);
        w.frameGeometry = {x:g.x,y:g.y,width:g.width,height:g.height};
        w.keepAbove = true;
    });
})();
