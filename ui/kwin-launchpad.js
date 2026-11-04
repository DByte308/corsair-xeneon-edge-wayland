// SPDX-License-Identifier: GPL-3.0-or-later
// Runs inside KWin, not Electron. State lives only for this script's lifetime.
(function () {
    'use strict';
    var previous = null;
    var lastFitted = null;
    var saved = {};
    var watchedRailWindows = {};
    var railWarnings = {};
    var pinningRail = false;
    var railClassPattern = /^(edgeline|edgeline-ui|edgeline-desktop|dev\.edgeline\.rust|electron|edge)$/i;
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
        delete watchedRailWindows[String(w.internalId)];
        delete railWarnings[String(w.internalId)];
        delete railWarnings['size:' + String(w.internalId)];
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
    function ownedRail(w) {
        return w && String(w.caption) === 'EdgeLine taskbar'
            && railClassPattern.test(String(w.resourceClass));
    }
    function pinRail(w) {
        if (!ownedRail(w) || w.minimized || pinningRail) return;
        var outputs = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        });
        if (outputs.length !== 1) return;
        var g = outputs[0].geometry;
        pinningRail = true;
        try {
            if (w.fullScreen) w.fullScreen = false;
            if (w.maximizeMode) w.setMaximize(false, false);
            if (w.moveable === false) {
                var id = String(w.internalId);
                if (!railWarnings[id]) print('EdgeLine: cannot pin an immovable taskbar window');
                railWarnings[id] = true;
                return;
            }
            delete railWarnings[String(w.internalId)];
            var current = w.frameGeometry;
            var width = w.resizeable === false ? current.width : 96;
            var height = w.resizeable === false ? current.height : g.height;
            var sizeWarning = 'size:' + String(w.internalId);
            if (w.resizeable === false && (Math.abs(current.width-96)>0.5 || Math.abs(current.height-g.height)>0.5)) {
                if (!railWarnings[sizeWarning]) print('EdgeLine: taskbar client refuses resizing; pinning its position only');
                railWarnings[sizeWarning] = true;
            } else { delete railWarnings[sizeWarning]; }
            // Own writes emit frameGeometryChanged too. Only correct real drift.
            if (Math.abs(current.x-g.x)>0.5 || Math.abs(current.y-g.y)>0.5
                || Math.abs(current.width-width)>0.5 || Math.abs(current.height-height)>0.5) {
                w.frameGeometry = {x:g.x,y:g.y,width:width,height:height};
            }
            w.keepAbove = true;
        } finally { pinningRail = false; }
    }
    function watchRail(w) {
        if (!w) return;
        var id = String(w.internalId);
        if (watchedRailWindows[id]) return;
        watchedRailWindows[id] = true;
        // A title/class can arrive after windowAdded. Observers are inert until
        // ownedRail matches; ordinary app and settings windows are never pinned.
        if (String(w.caption) === 'EdgeLine taskbar' && !railClassPattern.test(String(w.resourceClass))) {
            print('EdgeLine: taskbar class is not recognized yet: ' + String(w.resourceClass));
        }
        ['frameGeometryChanged','captionChanged','windowClassChanged','outputChanged','minimizedChanged','fullScreenChanged','maximizedChanged'].forEach(function (name) {
            if (w[name] && typeof w[name].connect === 'function') w[name].connect(function () {
                if (watchedRailWindows[id]) pinRail(w);
            });
        });
        pinRail(w);
    }
    function placePanel(w) {
        if (!w || !['EdgeLine', 'EdgeLine dashboard', 'EdgeLine taskbar'].includes(String(w.caption))) return;
        if (String(w.caption) === 'EdgeLine taskbar') { pinRail(w); return; }
        var outputs = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        });
        if (outputs.length !== 1) return;
        var g = outputs[0].geometry;
        w.frameGeometry = {x:g.x+96,y:g.y,width:g.width-96,height:g.height};
        if (String(w.caption) === 'EdgeLine') {
            workspace.windowList().forEach(function (panel) {
                if (String(panel.caption) === 'EdgeLine dashboard') panel.keepAbove = false;
            });
            w.keepAbove = true;
        }
    }
    workspace.windowActivated.connect(function () {
        workspace.windowList().forEach(function (w) {
            if (ownedRail(w)) {
                pinRail(w);
                w.keepAbove = true;
                if (workspace.raiseWindow) workspace.raiseWindow(w);
            }
        });
    });
    workspace.windowAdded.connect(placePanel);
    workspace.windowAdded.connect(watchRail);
    workspace.windowList().forEach(placePanel);
    workspace.windowList().forEach(watchRail);
    if (workspace.screensChanged && typeof workspace.screensChanged.connect === 'function') {
        workspace.screensChanged.connect(function () { workspace.windowList().forEach(pinRail); });
    }
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
    function registerWindowFit(w) {
        if (!eligible(w)) return;
        var suffix = String(w.internalId).replace(/[^a-z0-9]/gi, '');
        registerShortcut('EdgeLineWindow' + suffix, 'EdgeLine: fit selected window', '', function () {
            // The selected window may have closed while the picker was active.
            if (workspace.windowList().indexOf(w) >= 0) fit(w);
        });
    }
    workspace.windowList().forEach(registerWindowFit);
    workspace.windowAdded.connect(registerWindowFit);
    print('EdgeLine: launchpad window controller ready');
})();
