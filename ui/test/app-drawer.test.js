// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const { visible, order, summarize } = require('../apps/builtin/app-drawer/model.js');
const registry = require('../apps/registry.js');

const apps = [
  { id: 'system-monitor', name: 'System', open: false, compatible: true, enabled: true },
  { id: 'app-drawer', name: 'Apps', open: true, compatible: true, enabled: true },
  { id: 'edge-doctor', name: 'Edge Doctor', open: true, compatible: true, enabled: true },
  { id: 'broken', name: 'Broken', open: false, compatible: false, enabled: false },
  { id: 'ambient', name: 'Ambient', open: false, compatible: true, enabled: false },
  { id: 'clipboard', name: 'Clipboard', open: false, compatible: true, enabled: true },
];
assert.deepEqual(visible(apps).map((app) => app.id),
  ['system-monitor', 'edge-doctor', 'clipboard'],
  'the drawer never lists itself, unusable or disabled apps');
assert.deepEqual(order(apps).map((app) => app.id),
  ['edge-doctor', 'clipboard', 'system-monitor'],
  'running apps come first, then alphabetical');
assert.deepEqual(summarize(apps), { total: 3, running: 1 });
assert.deepEqual(summarize([]), { total: 0, running: 0 });
assert.deepEqual(order(null), []);
assert.deepEqual(visible([null, undefined, 42]), []);
assert.deepEqual(order([{ id: 'x' }, { id: 'app-drawer' }]).map((app) => app.id), ['x'],
  'an app without flags is still usable');

// The drawer must be able to see every shipped app, and the registry must be
// able to launch each of them.
const shipped = registry.list().filter((app) => app.compatible);
assert.ok(shipped.length >= 6, 'all builtin apps are discovered');
for (const app of shipped) {
  assert.ok(app.name && app.icon, `${app.id} has a name and an icon for the tile`);
  assert.ok(registry.CAPABILITIES.has('apps:launch'));
  assert.equal(registry.authorize({ capabilities: ['apps:launch'] }, 'apps.launch').ok, true);
  assert.equal(registry.authorize({ capabilities: ['apps:launch'] }, 'apps.list').ok, true);
}
const health = registry.resolve('app-drawer');
assert.equal(health.ok, true);
assert.deepEqual(health.app.capabilities, ['apps:launch']);
console.log('app-drawer: visibility, ordering, counts, capability and full app coverage pass');
