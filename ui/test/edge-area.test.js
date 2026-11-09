// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { RAIL_WIDTH, railSafeArea, compositorArea, appRect } = require('../host/edge-area');

const edge = { bounds: { x: 467, y: 2880, width: 2560, height: 720 } };
const geometry = { x: 467, y: 2880, width: 2560, height: 720 };

// With the reservation in place the compositor already reports the rail as
// taken, so clamping changes nothing.
const reserved = [{ geometry, workarea: { x: 467 + RAIL_WIDTH, y: 2880, width: 2560 - RAIL_WIDTH, height: 720 } }];
let area = railSafeArea(edge, compositorArea(edge, reserved));
assert.equal(area.x, 563, 'a reserved Edge keeps the compositor work area');
assert.equal(area.width, 2464);

// The regression: the helper exited at login, so the work area covered the rail
// and every window placed from it slid under the sidebar.
const unreserved = [{ geometry, workarea: { x: 467, y: 2880, width: 2560, height: 720 } }];
area = railSafeArea(edge, compositorArea(edge, unreserved));
assert.equal(area.x, 563, 'an unreserved Edge still starts right of the rail');
assert.equal(area.width, 2464, 'and loses exactly the rail column');

// No compositor report at all falls back to the output, still clamped.
assert.equal(railSafeArea(edge, compositorArea(edge, [])).x, 563);
assert.equal(railSafeArea(edge, compositorArea(edge, null)).width, 2464);
// A work area that starts left of the output cannot drag the rail with it.
assert.equal(railSafeArea(edge, { x: 0, y: 0, width: 3440, height: 1440 }).x, 563);
// A work area wider than the output is cut at the output's right edge.
assert.equal(railSafeArea(edge, { x: 563, y: 2880, width: 4000, height: 720 }).width, 2464);

// Apps keep their manifest size while they fit, and stay inside the content
// area when they do not.
const drawer = { window: { width: 1560, height: 660 } };
const centred = appRect(drawer, edge, reserved);
assert.equal(centred.width, 1560);
assert.equal(centred.height, 660);
assert.equal(centred.x, 563 + Math.round((2464 - 1560) / 2));
assert.equal(centred.y, 2880 + Math.round((720 - 660) / 2));
const same = appRect(drawer, edge, unreserved);
assert.deepEqual(same, centred, 'a missing reservation does not move the window');
const huge = appRect({ window: { width: 4000, height: 2000 } }, edge, unreserved);
assert.equal(huge.width, 2464 - 24);
assert.equal(huge.height, 720 - 24);
assert.ok(huge.x >= 563, 'a window that fills the Edge still starts right of the rail');
assert.ok(huge.x + huge.width <= edge.bounds.x + edge.bounds.width);

// The compositor-side placement enforces the same column, and keeps the rail on
// top of what it places.
const place = fs.readFileSync(path.join(__dirname, '..', 'kwin-place-app.js'), 'utf8');
assert.match(place, /function railRight\(\)/, 'placement knows where the rail ends');
assert.match(place, /target\.x < right/, 'and shifts a window that would start under it');
assert.match(place, /function keepRailAbove\(\)/, 'the rail is kept on top');
assert.match(place, /'EdgeLine taskbar'[\s\S]{0,160}w\.keepAbove = true/, 'the rail is raised above app windows');
assert.match(place, /if \(w\.keepAbove\) w\.keepAbove = false/, 'an app window is never above the rail');
assert.match(place, /placed !== w \|\| !w\.fullScreen/, 'a fullscreen app does not take the rail with it');

// The host places from the clamped area only.
const main = fs.readFileSync(path.join(__dirname, '..', 'main.js'), 'utf8');
assert.match(main, /edgeArea\.appRect\(app, edge/, 'app placement uses the rail-safe rectangle');
assert.match(main, /edgeArea\.railSafeArea\(edge, edgeArea\.compositorArea/, 'snap slots use it too');
assert.doesNotMatch(main, /match \? match\.workarea : \{/, 'the compositor work area is never used raw');
console.log('edge area: the rail column holds with and without a compositor reservation');
