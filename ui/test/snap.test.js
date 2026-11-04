// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const { slot, isLayout, LAYOUTS } = require('../host/snap.js');

// The real Edge work area.
const area = { x: 563, y: 2880, width: 2464, height: 720 };
assert.deepEqual(slot(area, 'full'), area);
assert.deepEqual(slot(area, 'left'), { x: 563, y: 2880, width: 1226, height: 720 });
const right = slot(area, 'right');
assert.deepEqual(right, { x: 1801, y: 2880, width: 1226, height: 720 });
assert.equal(right.x + right.width, area.x + area.width, 'the right half ends at the area edge');
assert.equal(slot(area, 'left').x + slot(area, 'left').width <= area.x + area.width, true);
assert.ok(right.x - (slot(area, 'left').x + slot(area, 'left').width) <= 14, 'halves leave one gap, not two');

// Thirds tile the row exactly, with the gap between them.
const thirds = ['third1', 'third2', 'third3'].map((layout) => slot(area, layout));
assert.equal(thirds[0].x, 563);
assert.equal(thirds[2].x + thirds[2].width, area.x + area.width);
for (let i = 1; i < thirds.length; i += 1) {
  const previous = thirds[i - 1];
  assert.ok(thirds[i].x > previous.x + previous.width, 'thirds do not overlap');
  assert.ok(thirds[i].x - (previous.x + previous.width) <= 14, 'and stay close together');
}
// Vertical halves stay inside the area.
for (const layout of ['top', 'bottom']) {
  const rect = slot(area, layout);
  assert.ok(rect.y >= area.y);
  assert.ok(rect.y + rect.height <= area.y + area.height);
}

// Refusals: unknown layout, malformed area, tiny area.
assert.equal(slot(area, 'diagonal'), null);
assert.equal(slot(area, ''), null);
assert.equal(slot(area, '__proto__'), null);
assert.equal(slot(area, 'constructor'), null);
assert.equal(slot(null, 'full'), null);
assert.equal(slot({ x: 0, y: 0, width: 100, height: 100 }, 'full'), null);
assert.equal(slot({ x: 0, y: 0, width: '2464', height: 720 }, 'full'), null);
assert.equal(slot({ x: 0.5, y: 0, width: 2464, height: 720 }, 'full'), null);
assert.equal(isLayout('full'), true);
assert.equal(isLayout('FULL'), false);
assert.equal(LAYOUTS.length, 8);

// Every layout stays inside the area for an awkward, non-divisible width.
const odd = { x: 100, y: 50, width: 1001, height: 601 };
for (const layout of LAYOUTS) {
  const rect = slot(odd, layout);
  assert.ok(rect.x >= odd.x && rect.y >= odd.y, `${layout} starts inside`);
  assert.ok(rect.x + rect.width <= odd.x + odd.width, `${layout} ends inside`);
  assert.ok(rect.y + rect.height <= odd.y + odd.height, `${layout} ends inside vertically`);
  assert.ok(rect.width >= 200 && rect.height >= 160, `${layout} respects the minimums`);
}
console.log('snap: full, halves, thirds, bounds, minimums and layout refusals pass');
