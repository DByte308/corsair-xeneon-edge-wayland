// SPDX-License-Identifier: GPL-3.0-or-later
// The host bridge is what makes the bundled apps work in the native host: the
// methods they call are host responsibilities, implemented once in ui/host/* and
// reached over a socket instead of being reimplemented in Rust. These tests pin
// the dispatch contract, not the platform modules (those have their own suites).
'use strict';
const assert = require('node:assert/strict');
const bridge = require('../host-bridge.js');

(async () => {
  // apps.list: registry shape, and `open` comes from the host's window list.
  const list = await bridge.dispatch({ id: 1, method: 'apps.list', context: { openApps: [] } });
  assert.deepEqual(list.result.filter((app) => app.id === 'ambient').map((app) => app.name), ['Ambient'],
    'built-in apps come from the registry');
  for (const app of list.result) {
    assert.equal(typeof app.id, 'string');
    assert.equal(typeof app.name, 'string');
    assert.ok(Array.isArray(app.capabilities), `${app.id} lists capabilities`);
    assert.equal(app.open, false, 'no app windows are open in this context');
  }
  const opened = await bridge.dispatch({ id: 2, method: 'apps.list', context: { openApps: ['ambient'] } });
  assert.equal(opened.result.find((app) => app.id === 'ambient').open, true,
    'the host says which apps are open, the bridge does not guess');

  // An unknown method is refused by name rather than answered with null.
  const unknown = await bridge.dispatch({ id: 3, method: 'apps.teleport' });
  assert.match(unknown.error.message, /Unsupported host method: apps\.teleport/);

  // apps.launch belongs to the host (it creates a window), so this bridge must keep
  // refusing it: a wrong route has to fail loudly instead of pretending the bridge
  // could open an app.
  const launch = await bridge.dispatch({ id: 4, method: 'apps.launch', params: { id: 'ambient' } });
  assert.match(launch.error.message, /Unsupported host method: apps\.launch/,
    'apps.launch is the host\'s job; the bridge must not silently answer it');

  // The registry mapping the host authorizes against: a known method names its
  // capability, an unknown one is an error rather than a default allow.
  const required = await bridge.dispatch({ id: 7, method: 'apps.requiredCapability', params: { method: 'diagnostics.read' } });
  assert.deepEqual(required.result, { method: 'diagnostics.read', capability: 'diagnostics:read', implemented: true });
  const reserved = await bridge.dispatch({ id: 8, method: 'apps.requiredCapability', params: { method: 'display.ddcSet' } });
  assert.equal(reserved.result.implemented, false, 'a reserved capability is reported as not implemented');
  const bogus = await bridge.dispatch({ id: 9, method: 'apps.requiredCapability', params: { method: 'nope.nope' } });
  assert.match(bogus.error.message, /unknown method: nope\.nope/);

  // diagnostics: the host's own facts are merged in, and the schema is the same
  // one the doctor app reads in the Electron host.
  const diagnostics = await bridge.dispatch({
    id: 5,
    method: 'diagnostics.read',
    context: { diagnostics: { railWidth: 96, host: { runtime: 'Tauri', version: '0.7.0' } } },
  });
  assert.equal(diagnostics.result.schema, 1);
  assert.equal(diagnostics.result.host.runtime, 'Tauri', 'the host block comes from the caller');
  assert.equal(diagnostics.result.host.version, '0.7.0');
  assert.equal(diagnostics.result.railWidth, 96);

  // A missing core socket is reported as an error, never as an empty window list:
  // "no windows" and "the backend is not running" must not look alike.
  const previous = process.env.XDG_RUNTIME_DIR;
  process.env.XDG_RUNTIME_DIR = '/nonexistent-for-this-test';
  const windows = await bridge.dispatch({ id: 6, method: 'windows.list', context: { openApps: [] } });
  assert.ok(windows.error, `windows.list must fail loudly, got ${JSON.stringify(windows)}`);
  if (previous === undefined) delete process.env.XDG_RUNTIME_DIR;
  else process.env.XDG_RUNTIME_DIR = previous;

  console.log('host-bridge: dispatch contract, open-state from the host, and the apps.launch refusal pass');
})().catch((error) => {
  console.error(`host-bridge: ${error.message}`);
  process.exit(1);
});
