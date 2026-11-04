// SPDX-License-Identifier: GPL-3.0-or-later
// The compositor feed knows a caption; the registry knows which caption is one
// of our own apps and which glyph it wants. This joins the two.
'use strict';

// windows: [{id, title, app, active, minimized, own}]
// installed: [{id, name, icon, compatible, rail}]
// A window is returned with its app id and icon; windows belonging to apps with
// `rail: false` (launchers such as the app drawer) are dropped, because they are
// opened from a fixed rail button rather than switched to like an app.
function decorate(installed, windows) {
  const byName = new Map();
  for (const app of Array.isArray(installed) ? installed : []) {
    if (!app || app.compatible === false || typeof app.name !== 'string') continue;
    byName.set(app.name, app);
  }
  return (Array.isArray(windows) ? windows : []).flatMap((window) => {
    if (!window || !window.own) return [window];
    const app = byName.get(window.title);
    if (!app) return [window];
    if (app.rail === false) return [];
    return [{ ...window, appId: app.id, icon: app.icon || null }];
  });
}

module.exports = { decorate };
