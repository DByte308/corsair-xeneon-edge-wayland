// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const runtime = fs.mkdtempSync(path.join(os.tmpdir(), 'edgeline-controller-test-'));
process.env.XDG_RUNTIME_DIR = runtime;
const launchpad = require('../launchpad.js');
assert.match(launchpad.plugin, /^edgeline-launchpad-[0-9a-f]{12}$/, 'plugin names stay versioned');
assert.deepEqual(launchpad.recordedControllers(), [], 'a missing record means nothing to unload');
launchpad.recordController('edgeline-launchpad-aaaaaaaaaaaa');
launchpad.recordController('edgeline-launchpad-aaaaaaaaaaaa');
assert.deepEqual(launchpad.recordedControllers(), ['edgeline-launchpad-aaaaaaaaaaaa'], 'recording the same controller is idempotent');
launchpad.recordController('edgeline-launchpad-bbbbbbbbbbbb');
assert.deepEqual(launchpad.recordedControllers(),
  ['edgeline-launchpad-bbbbbbbbbbbb', 'edgeline-launchpad-aaaaaaaaaaaa'],
  'every loaded controller stays recorded so it can be unloaded later');
assert.deepEqual(launchpad.staleControllerNames(
  ['edgeline-launchpad-bbbbbbbbbbbb', 'edgeline-launchpad-aaaaaaaaaaaa'], 'edgeline-launchpad-bbbbbbbbbbbb'),
  ['edgeline-launchpad-aaaaaaaaaaaa'], 'only the current controller survives cleanup');
assert.deepEqual(launchpad.staleControllerNames(
  ['edgeline-launchpad-aaaaaaaaaaaa', 'edgeline-launchpad-aaaaaaaaaaaa'], 'edgeline-launchpad-bbbbbbbbbbbb'),
  ['edgeline-launchpad-aaaaaaaaaaaa'], 'duplicates collapse to one unload');
assert.deepEqual(launchpad.staleControllerNames(
  ['not-a-controller', '../evil', ''], 'edgeline-launchpad-bbbbbbbbbbbb'), [],
  'tampered entries never reach the compositor call');
fs.writeFileSync(launchpad.controllerStatePath, '{not json');
assert.deepEqual(launchpad.recordedControllers(), [], 'a corrupt record degrades to no cleanup');
launchpad.recordController('edgeline-launchpad-cccccccccccc');
assert.deepEqual(launchpad.recordedControllers(), ['edgeline-launchpad-cccccccccccc'], 'recording repairs a corrupt record');
console.log('kwin-controller: versioned plugin names, recorded loads and stale-controller cleanup pass');
