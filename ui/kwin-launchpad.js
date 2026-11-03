// SPDX-License-Identifier: GPL-3.0-or-later
// Runs inside KWin, not Electron. State lives only for this script's lifetime.
(function () {
    'use strict';
    var previous = null;
    var lastFitted = null;
    var saved = {};
    function eligible(w) {
        return w && w.normalWindow && !/edgeline|electron/i.test(String(w.resourceClass))
            && !/^EdgeLine/.test(String(w.caption));
    }
    function remember(w) { if (eligible(w)) previous = w; }
    remember(workspace.activeWindow);
    workspace.windowActivated.connect(remember);
    workspace.windowRemoved.connect(function (w) {
        if (previous === w) previous = null;
        if (lastFitted === w) lastFitted = null;
        delete saved[String(w.internalId)];
    });
    function fit(w) {
        if (!eligible(w)) { print('EdgeLine: no eligible target window'); return; }
        var outputs = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        });
        if (outputs.length !== 1) { print('EdgeLine: missing or ambiguous Edge output'); return; }
        if (!w.moveable || !w.resizeable) { print('EdgeLine: window refuses moving/resizing'); return; }
        var id = String(w.internalId);
        if (!saved[id]) {
            var g = w.frameGeometry;
            saved[id] = { window: w, geometry: {x:g.x,y:g.y,width:g.width,height:g.height},
                fullscreen:w.fullScreen, horizontal:!!(w.maximizeMode & 2), vertical:!!(w.maximizeMode & 1) };
        }
        w.fullScreen = false;
        w.setMaximize(false, false);
        var area = workspace.clientArea(KWin.MaximizeArea, outputs[0], workspace.currentDesktop);
        var railRight = outputs[0].geometry.x + 96;
        var contentX = Math.max(area.x, railRight);
        w.frameGeometry = {x:contentX,y:area.y,width:area.x+area.width-contentX,height:area.height};
        workspace.windowList().forEach(function (panel) {
            if (String(panel.caption) === 'EdgeLine dashboard') panel.keepAbove = false;
        });
        previous = w;
        lastFitted = w;
        workspace.activeWindow = w;
        print('EdgeLine: fitted ' + w.caption);
    }
    function placePanel(w) {
        if (!w || !['EdgeLine', 'EdgeLine dashboard', 'EdgeLine taskbar'].includes(String(w.caption))) return;
        var outputs = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        });
        if (outputs.length !== 1) return;
        var g = outputs[0].geometry;
        var isRail = String(w.caption) === 'EdgeLine taskbar';
        w.frameGeometry = {x:g.x+(isRail?0:96),y:g.y,width:isRail?96:g.width-96,height:g.height};
        if (isRail) w.keepAbove = true;
        if (String(w.caption) === 'EdgeLine') {
            workspace.windowList().forEach(function (panel) {
                if (String(panel.caption) === 'EdgeLine dashboard') panel.keepAbove = false;
            });
            w.keepAbove = true;
        }
    }
    workspace.windowActivated.connect(function () {
        workspace.windowList().forEach(function (w) {
            if (String(w.caption) === 'EdgeLine taskbar') {
                w.keepAbove = true;
                if (workspace.raiseWindow) workspace.raiseWindow(w);
            }
        });
    });
    workspace.windowAdded.connect(placePanel);
    workspace.windowList().forEach(placePanel);
    registerShortcut('EdgeLinePanel', 'EdgeLine: place dashboard', '', function () {
        workspace.windowList().forEach(placePanel);
    });
    registerShortcut('EdgeLineVanguardFit', 'EdgeLine: fit Vanguard Galaxy', '', function () {
        var games = workspace.windowList().filter(function (w) {
            return String(w.resourceClass).toLowerCase() === 'steam_app_3471800';
        });
        if (games.length === 1) fit(games[0]);
    });
    registerShortcut('EdgeLineFit', 'EdgeLine: fit last app to Edge', '', function () { fit(previous); });
    registerShortcut('EdgeLineRestore', 'EdgeLine: restore last app', '', function () {
        if (!lastFitted) return;
        var id = String(lastFitted.internalId), old = saved[id];
        if (!old) { print('EdgeLine: no saved placement for target'); return; }
        var w = old.window;
        w.fullScreen = false;
        w.setMaximize(false, false);
        w.frameGeometry = old.geometry;
        w.setMaximize(old.horizontal, old.vertical);
        w.fullScreen = old.fullscreen;
        workspace.activeWindow = w;
        delete saved[id];
        print('EdgeLine: restored ' + w.caption);
    });
    registerShortcut('EdgeLinePick', 'EdgeLine: choose app to fit', '', function () {
        callDBus('org.kde.KWin', '/KWin', 'org.kde.KWin', 'queryWindowInfo', function (info) {
            if (!info || !info.uuid) return;
            var windows = workspace.windowList();
            for (var i = 0; i < windows.length; i++) {
                if (String(windows[i].internalId) === String(info.uuid)) { fit(windows[i]); return; }
            }
        });
    });
    print('EdgeLine: launchpad window controller ready');
})();
