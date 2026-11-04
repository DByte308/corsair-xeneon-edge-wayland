// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'edgeline-apps-test-'));
process.env.XDG_DATA_HOME = path.join(sandbox, 'share');
process.env.XDG_CONFIG_HOME = path.join(sandbox, 'config');
const registry = require('../apps/registry.js');

function writeApp(root, folder, manifest, files = { 'index.html': '<!doctype html><title>t</title>' }) {
  const dir = path.join(root, folder);
  fs.mkdirSync(dir, { recursive: true });
  if (manifest !== null) fs.writeFileSync(path.join(dir, 'app.json'), typeof manifest === 'string' ? manifest : JSON.stringify(manifest));
  for (const [name, body] of Object.entries(files)) {
    const target = path.join(dir, name);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, body);
  }
  return dir;
}
const good = { id: 'probe', apiVersion: 1, name: 'Probe', entry: 'index.html', capabilities: ['diagnostics:read'] };

// Builtins ship with the core and are always visible.
const builtin = registry.list().find((app) => app.id === 'edge-doctor');
assert.ok(builtin, 'the shipped Edge Doctor app is discovered');
assert.equal(builtin.source, 'builtin');
assert.equal(builtin.enabled, true);
assert.equal(builtin.compatible, true);
assert.ok(builtin.capabilities.includes('diagnostics:read'));

// User apps are discovered too, and a same-id user app shadows the builtin.
assert.equal(registry.list().some((app) => app.source === 'user'), false, 'no user apps yet');
const builtinIds = registry.list().filter((app) => app.source === 'builtin').map((app) => app.id);
const userRoot = registry.userDirectory();
writeApp(userRoot, 'probe', good);
assert.ok(registry.list().some((app) => app.id === 'probe'), 'a dropped-in app is discovered');
assert.deepEqual(registry.list().filter((app) => app.source === 'builtin').map((app) => app.id).sort(), builtinIds.sort());
const shadow = writeApp(userRoot, 'edge-doctor', { ...good, id: 'edge-doctor', name: 'Shadow doctor' });
assert.equal(registry.list().find((app) => app.id === 'edge-doctor').name, 'Shadow doctor', 'a user app shadows the builtin');
assert.equal(registry.list().find((app) => app.id === 'edge-doctor').source, 'user');
fs.rmSync(shadow, { recursive: true });
assert.equal(registry.list().find((app) => app.id === 'edge-doctor').source, 'builtin', 'deleting the shadow restores the builtin');

// Validation: every refusal names the reason instead of loading half an app.
const cases = [
  ['bad-id', { ...good, id: 'Probe App' }, {}, 'invalid id'],
  ['mismatch', { ...good, id: 'other' }, {}, 'id does not match the directory'],
  ['future', { ...good, id: 'future', apiVersion: 2 }, {}, 'apiVersion 2 is not supported'],
  ['unknown-cap', { ...good, id: 'unknown-cap', capabilities: ['kernel:root'] }, {}, 'unknown capability kernel:root'],
  ['chrome-name', { ...good, id: 'chrome-name', name: 'EdgeLine taskbar' }, {}, 'name is reserved for EdgeLine itself'],
  ['no-entry', { ...good, id: 'no-entry', entry: 'missing.html' }, {}, 'entry is missing or outside the app directory'],
  ['escape', { ...good, id: 'escape', entry: '../probe/index.html' }, {}, 'entry is missing or outside the app directory'],
  ['absolute', { ...good, id: 'absolute', entry: '/etc/passwd' }, {}, 'entry is missing or outside the app directory'],
  ['broken-json', '{ not json', {}, 'unreadable app.json'],
  ['not-object', '[]', {}, 'app.json is not an object'],
];
for (const [folder, manifest, files, reason] of cases) {
  const dir = writeApp(userRoot, folder, manifest, { 'index.html': 'x', ...files });
  const result = registry.readManifest(dir, folder);
  assert.equal(result.ok, false, `${folder} must be refused`);
  assert.equal(result.error, reason, `${folder} refusal reason`);
  fs.rmSync(dir, { recursive: true });
}
assert.equal(registry.readManifest(writeApp(userRoot, 'nojson', null)).error, 'no app.json');

// Two apps may not share a caption: the placement script matches on it.
writeApp(userRoot, 'twin-a', { ...good, id: 'twin-a', name: 'Twins' });
writeApp(userRoot, 'twin-b', { ...good, id: 'twin-b', name: 'Twins' });
const twins = registry.list().filter((app) => app.name === 'Twins');
assert.equal(twins.length, 2);
assert.equal(twins.filter((app) => app.compatible).length, 1, 'one app keeps a duplicated caption');
assert.match(twins.find((app) => !app.compatible).problem, /already uses the name Twins/);
fs.rmSync(path.join(userRoot, 'twin-a'), { recursive: true });
fs.rmSync(path.join(userRoot, 'twin-b'), { recursive: true });

// Symlinks that escape the directory are refused, not followed.
const outside = path.join(sandbox, 'outside.html');
fs.writeFileSync(outside, 'secret');
const linked = writeApp(userRoot, 'linked', { ...good, id: 'linked', entry: 'page.html' }, {});
fs.symlinkSync(outside, path.join(linked, 'page.html'));
assert.equal(registry.readManifest(linked, 'linked').error, 'entry is missing or outside the app directory');

// Disable state is reversible and survives a reload.
assert.equal(registry.resolve('probe').ok, true);
registry.setEnabled('probe', false);
assert.equal(registry.resolve('probe').ok, false);
assert.equal(registry.resolve('probe').error, 'app is disabled');
assert.equal(registry.list().find((app) => app.id === 'probe').enabled, false);
assert.ok(JSON.parse(fs.readFileSync(registry.configPath(), 'utf8')).disabled.includes('probe'));
registry.setEnabled('probe', true);
assert.equal(registry.resolve('probe').ok, true);
assert.throws(() => registry.setEnabled('nope', true), /unknown app/);
assert.throws(() => registry.setEnabled('../etc', true), /invalid app id/);

// A corrupt config must not disable everything.
fs.writeFileSync(registry.configPath(), '{ broken');
assert.equal(registry.readDisabled().length, 0);
assert.equal(registry.list().filter((app) => app.compatible).every((app) => app.enabled), true);
assert.ok(registry.list().some((app) => !app.compatible), 'unusable directories are listed, not hidden');
assert.equal(registry.list().filter((app) => !app.compatible).every((app) => app.enabled === false), true);

// Capability gate: the app gets only what the manifest asked for.
const probe = registry.resolve('probe').app;
assert.deepEqual(registry.authorize(probe, 'diagnostics.read'), { ok: true, method: 'diagnostics.read', capability: 'diagnostics:read' });
assert.equal(registry.authorize(probe, 'windows.activate').ok, false);
assert.match(registry.authorize(probe, 'windows.activate').error, /not granted windows:activate/);
assert.equal(registry.authorize(probe, 'display.ddcSet').ok, false);
assert.match(registry.authorize(probe, 'constructor').error, /unknown app method/);
assert.match(registry.authorize(probe, '__proto__').error, /unknown app method/);
assert.match(registry.authorize(probe, '').error, /unknown app method/);
const granted = registry.resolve('probe').app;
granted.capabilities.push('display:control');
assert.equal(registry.authorize(granted, 'display.ddcSet').ok, false, 'reserved capabilities are refused until implemented');
assert.match(registry.authorize(granted, 'display.ddcSet').error, /not implemented/);

// Icons are cosmetic: a known name is kept, an unknown one is ignored so an app
// newer than the core still runs with its initials.
const icons = registry.readManifest(writeApp(userRoot, 'iconned', { ...good, id: 'iconned', icon: 'play' }), 'iconned').app;
assert.equal(icons.icon, 'play');
assert.equal(registry.readManifest(writeApp(userRoot, 'iconless', { ...good, id: 'iconless' }), 'iconless').app.icon, null);
assert.equal(registry.readManifest(writeApp(userRoot, 'iconnope', { ...good, id: 'iconnope', icon: 'from-the-future' }), 'iconnope').app.icon, null);
assert.equal(registry.readManifest(writeApp(userRoot, 'iconnum', { ...good, id: 'iconnum', icon: 42 }), 'iconnum').app.icon, null);
assert.ok(registry.ICONS.has('wrench'));
for (const app of registry.list().filter((entry) => entry.source === 'builtin')) {
  assert.ok(app.icon, `${app.id} ships with an icon`);
  assert.ok(registry.ICONS.has(app.icon), `${app.id} icon is one the rail can draw`);
}

// Window defaults are bounded so a manifest cannot create a 40000px window.
assert.deepEqual(registry.readManifest(writeApp(userRoot, 'sized', { ...good, id: 'sized', window: { width: 99999, height: -4 } }), 'sized').app.window,
  { width: 1180, height: 660, resizable: true });
assert.deepEqual(registry.readManifest(writeApp(userRoot, 'sized2', { ...good, id: 'sized2', window: { width: 800, height: 400, resizable: false } }), 'sized2').app.window,
  { width: 800, height: 400, resizable: false });
console.log('apps-registry: discovery, shadowing, manifest refusals, symlink escape, enable/disable and capability gate pass');
