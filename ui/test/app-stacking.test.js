// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.join(__dirname, '..');
const main = fs.readFileSync(path.join(root, 'main.js'), 'utf8');
const place = fs.readFileSync(path.join(root, 'kwin-place-app.js'), 'utf8');

// The dashboard is opened always-on-top. An app started while Home is up must
// not end up underneath it, which is what "Apps does nothing" looked like.
assert.match(place, /function lowerDashboard\(\)/, 'placement can lower the dashboard');
assert.match(place, /'EdgeLine dashboard'\)\s*w\.keepAbove = false/, 'and it does so through the compositor');
assert.ok(place.indexOf('lowerDashboard();') < place.indexOf('lowerDashboard();\n    workspace.windowList()'),
  'it runs before the placement watchers are installed');
assert.match(place, /lowerDashboard\(\);\s*\n\s*placed = w;/, 'and again once the window has been placed');
assert.match(main, /function lowerDashboard\(\)/, 'the host also drops its own always-on-top flag');
assert.match(main, /const app = resolved\.app;\s*\n\s*lowerDashboard\(\);/, 'opening any app lowers the dashboard first');

// Reusing an open window has to raise it, not just focus it: Wayland ignores
// focus-only raises.
const reused = main.indexOf('existing.show();');
assert.ok(reused > 0, 'reuse path shows the window');
assert.ok(main.indexOf('existing.setAlwaysOnTop(false);', reused) > reused, 'reuse drops any stale always-on-top');
assert.ok(main.indexOf('existing.moveTop();', reused) > reused, 'reuse raises the window');
assert.ok(main.indexOf('existing.focus();', reused) > main.indexOf('existing.moveTop();', reused), 'and then focuses it');
const created = main.indexOf('window.once(\'ready-to-show\'');
assert.ok(main.indexOf('window.moveTop();', created) > created, 'a new window is raised on show');

// Activating an app from the rail already lowers the panels compositor-side.
const activate = fs.readFileSync(path.join(root, 'kwin-activate-window.js'), 'utf8');
assert.match(activate, /'EdgeLine dashboard'\)\s*w\.keepAbove = false/, 'activation lowers the dashboard too');
console.log('app-stacking: dashboard is lowered for new and reused app windows, and both are raised');
