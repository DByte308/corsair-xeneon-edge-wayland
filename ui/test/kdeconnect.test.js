// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const kc = require('../host/kdeconnect.js');

// Device ids come back as a busctl string array.
assert.deepEqual(kc.parseIds('as 2 "11942b7881d54587a60ce6b9cd2da270" "97735c6431304f9b9602665849fcc158"'),
  ['11942b7881d54587a60ce6b9cd2da270', '97735c6431304f9b9602665849fcc158']);
assert.deepEqual(kc.parseIds('as 2 "not-a-device" "../../etc"'), [], 'only hex device ids survive');
assert.deepEqual(kc.parseIds(''), []);

// busctl property syntax stays a single source of truth.
assert.equal(kc.parseProperty('b true'), true);
assert.equal(kc.parseProperty('b false'), false);
assert.equal(kc.parseProperty('i 42'), 42);
assert.equal(kc.parseProperty('i -3'), -3);
assert.equal(kc.parseProperty('s "Cachy Laptop"'), 'Cachy Laptop');
assert.equal(kc.parseProperty('s "quote \\" inside"'), 'quote " inside');
assert.equal(kc.parseProperty('s "unterminated'), '');
assert.equal(kc.parseProperty(''), '');
assert.equal(kc.parseProperty(null), '');

// Normalising bounds every field the app renders.
const phone = kc.normalize({ id: 'a'.repeat(32), name: 'Pixel 9', type: 'phone', reachable: true, paired: true, charge: 64 });
assert.deepEqual(phone, { id: 'a'.repeat(32), name: 'Pixel 9', type: 'phone', reachable: true, paired: true, battery: 64 });
assert.equal(kc.normalize({ id: 'x', name: 'y'.repeat(200), type: 'weird type!', charge: 500 }).battery, null);
assert.equal(kc.normalize({ id: 'x', name: 'y'.repeat(200) }).name.length, 60);
assert.equal(kc.normalize({ id: 'x' }).name, 'Unnamed device');
assert.equal(kc.normalize({ id: 'x', type: 'phone<script>' }).type, 'phonescript');
assert.equal(kc.normalize({ id: 'x', reachable: 'yes' }).reachable, false, 'only a real boolean counts as reachable');

(async () => {
  const state = await kc.devices();
  assert.equal(typeof state.available, 'boolean');
  assert.ok(Array.isArray(state.devices));
  for (const device of state.devices) {
    assert.match(device.id, /^[0-9a-f]{32}$/);
    assert.ok(device.name.length <= 60);
    assert.ok(device.battery === null || (Number.isInteger(device.battery) && device.battery >= 0 && device.battery <= 100));
    assert.ok(device.reachable === true || device.reachable === false);
  }
  console.log(`kdeconnect: ids, properties, normalising and live read (${state.devices.length} devices) pass`);
})();
