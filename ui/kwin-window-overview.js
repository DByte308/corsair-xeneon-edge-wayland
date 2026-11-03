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
                if (!w.normalWindow || w.deleted || w.skipTaskbar
                    || /^EdgeLine/.test(String(w.caption))
                    || /^(edgeline|xwaylandvideobridge)$/.test(String(w.resourceClass))) return;
                if (w.output !== edge) return;
                windows.push({id:String(w.internalId), title:String(w.caption),
                    app:String(w.resourceClass), active:!!w.active, minimized:!!w.minimized});
            });
        }
        callDBus(__EDGELINE_BUS__, '/Windows', 'dev.edgeline.Windows', 'Update',
            JSON.stringify({available:outputs.length === 1, windows:windows}));
    }
    function track(w) {
        w.frameGeometryChanged.connect(publish);
        w.captionChanged.connect(publish);
        w.minimizedChanged.connect(publish);
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
