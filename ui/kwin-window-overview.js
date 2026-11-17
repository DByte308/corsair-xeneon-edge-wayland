// SPDX-License-Identifier: GPL-3.0-or-later
// Session-local read-only feed to EdgeLine's private D-Bus bridge.
(function () {
    function publish() {
        var outputs = workspace.screens.filter(function (s) {
            return s.geometry.width === 2560 && s.geometry.height === 720;
        });
        var windows = [];
        if (outputs.length === 1) {
            var edge = outputs[0];
            workspace.windowList().forEach(function (w) {
                var caption = String(w.caption);
                var resourceClass = String(w.resourceClass);
                // EdgeLine's own app windows are real Edge apps and belong in
                // the rail; its chrome (sidebar, dashboard, settings) does not.
                var ownApp = /^edgeline$/i.test(resourceClass)
                    && caption.length > 0 && !/^EdgeLine/.test(caption);
                if (!w.normalWindow || w.deleted) return;
                if (/^xwaylandvideobridge$/i.test(resourceClass)) return;
                if (!ownApp && (/^EdgeLine/.test(caption) || w.skipTaskbar)) return;
                if (w.output !== edge) return;
                windows.push({id:String(w.internalId), title:caption,
                    app:resourceClass, active:!!w.active, minimized:!!w.minimized, own:ownApp});
            });
        }
        var panel = [];
        var screens = workspace.screens.map(function (s) {
            var area = workspace.clientArea(KWin.MaximizeArea, s, workspace.currentDesktop);
            return {name:String(s.name),
                geometry:{x:s.geometry.x,y:s.geometry.y,width:s.geometry.width,height:s.geometry.height},
                workarea:{x:area.x,y:area.y,width:area.width,height:area.height}};
        });
        workspace.windowList().forEach(function (w) {
            if (!/^EdgeLine/.test(String(w.caption))) return;
            panel.push({title:String(w.caption), x:w.frameGeometry.x, y:w.frameGeometry.y,
                width:w.frameGeometry.width, height:w.frameGeometry.height,
                keepAbove:!!w.keepAbove, minimized:!!w.minimized});
        });
        // The active window, for per-app rules. It is global, not per-output: a
        // rule fires on what the user is working in, which may well be on another
        // screen. State names are the _NET_WM_STATE ones rules.json matches on.
        var aw = workspace.activeWindow;
        var focus = {valid: false, wmInstance: '', wmClass: '', states: []};
        if (aw) {
            var states = [];
            if (aw.fullScreen) states.push('_NET_WM_STATE_FULLSCREEN');
            var mode = Number(aw.maximizeMode);
            if (mode === 1 || mode === 3) states.push('_NET_WM_STATE_MAXIMIZED_VERT');
            if (mode === 2 || mode === 3) states.push('_NET_WM_STATE_MAXIMIZED_HORZ');
            if (aw.keepAbove) states.push('_NET_WM_STATE_ABOVE');
            if (aw.keepBelow) states.push('_NET_WM_STATE_BELOW');
            if (aw.skipTaskbar) states.push('_NET_WM_STATE_SKIP_TASKBAR');
            if (aw.modal) states.push('_NET_WM_STATE_MODAL');
            if (aw.minimized) states.push('_NET_WM_STATE_HIDDEN');
            focus = {valid: true, wmInstance: String(aw.resourceName || ''),
                wmClass: String(aw.resourceClass || ''), states: states};
        }
        callDBus(__EDGELINE_BUS__, '/Windows', 'dev.edgeline.Windows', 'Update',
            JSON.stringify({available:outputs.length === 1, windows:windows, screens:screens,
                panel:panel, focus:focus}));
    }
    function track(w) {
        w.frameGeometryChanged.connect(publish);
        w.captionChanged.connect(publish);
        w.minimizedChanged.connect(publish);
        // Per-app rules can match a window state (fullscreen), so a state change
        // has to reach the feed without waiting for an activation. Guarded: a KWin
        // release without one of these signals must not take the whole feed down.
        ['fullScreenChanged', 'keepAboveChanged'].forEach(function (name) {
            if (w[name] && w[name].connect) w[name].connect(publish);
        });
        w.outputChanged.connect(publish);
        publish();
    }
    workspace.windowList().forEach(track);
    workspace.windowAdded.connect(track);
    workspace.windowRemoved.connect(publish);
    workspace.windowActivated.connect(publish);
    workspace.screensChanged.connect(publish);
    publish();
})();
