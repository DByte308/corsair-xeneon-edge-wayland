// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const { decorate } = require('../host/window-decorate.js');

const installed = [
  { id: 'now-playing', name: 'Now Playing', icon: 'play', compatible: true },
  { id: 'app-drawer', name: 'Apps', icon: 'grid', compatible: true, rail: false },
  { id: 'edge-doctor', name: 'Edge Doctor', icon: 'wrench', compatible: true },
  { id: 'broken', name: 'Broken App', icon: 'bolt', compatible: false },
  { id: 'iconless', name: 'No Glyph', icon: null, compatible: true },
];
const windows = [
  { id: 'a', title: 'Now Playing', app: 'edgeline', own: true },
  { id: 'b', title: 'Edge Doctor', app: 'edgeline', own: true, icon: 'stale' },
  { id: 'c', title: 'Broken App', app: 'edgeline', own: true },
  { id: 'd', title: 'No Glyph', app: 'edgeline', own: true },
  { id: 'e', title: 'VanguardGalaxy', app: 'steam_app_3471800', own: false },
  { id: 'f', title: 'Renamed Window', app: 'edgeline', own: true },
  { id: 'g', title: 'Apps', app: 'edgeline', own: true },
];
const out = decorate(installed, windows);
assert.deepEqual(out[0], { ...windows[0], appId: 'now-playing', icon: 'play' });
assert.deepEqual(out[1], { ...windows[1], appId: 'edge-doctor', icon: 'wrench' }, 'the registry wins over a stale icon on the window');
assert.equal(out[2].appId, undefined, 'an incompatible app gets no icon');
assert.equal(out[3].icon, null, 'an app without a glyph keeps the initials fallback');
assert.deepEqual(out[4], windows[4], 'foreign windows are untouched');
assert.deepEqual(out[5], windows[5], 'a caption that matches nothing is left alone');
assert.deepEqual(out.map((w) => w.id), ['a', 'b', 'c', 'd', 'e', 'f'],
  'order is preserved and a launcher window is dropped from the rail');
assert.equal(out.some((w) => w.title === 'Apps'), false, 'the drawer never appears as its own tile');
assert.deepEqual(decorate([], windows).map((w) => w.id), ['a', 'b', 'c', 'd', 'e', 'f', 'g'],
  'without a registry nothing is dropped');
assert.deepEqual(decorate(installed, []), []);
assert.deepEqual(decorate(installed, null), []);
assert.deepEqual(decorate(installed, [null, undefined]), [null, undefined]);
assert.deepEqual(decorate(null, windows), windows, 'no registry means no decoration');
assert.deepEqual(decorate([{ id: 'x', name: 'Apps', rail: false }], [{ id: 'g', title: 'Apps', own: true }]), [],
  'rail:false alone is enough to hide a launcher');
console.log('window-decorate: own windows get their app icon, foreign and unknown captions untouched');
