// SPDX-License-Identifier: GPL-3.0-or-later
(function () {
    var id = __EDGELINE_WINDOW_ID__;
    var windows = workspace.windowList();
    var selected = windows.filter(function (w) { return String(w.internalId) === id; })[0];
    if (!selected) return;
    // Recheck output at execution time: never pull an app from another screen.
    if (!selected.output || selected.output.geometry.width !== 2560 || selected.output.geometry.height !== 720) return;
    windows.forEach(function (w) {
        if (String(w.caption) === 'EdgeLine' || String(w.caption) === 'EdgeLine dashboard') w.keepAbove = false;
    });
    selected.minimized = false;
    workspace.activeWindow = selected;
    workspace.raiseWindow(selected);
})();
