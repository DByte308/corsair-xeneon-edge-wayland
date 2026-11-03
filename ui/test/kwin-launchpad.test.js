// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const shortcuts = {};
const geometry = { x: 10, y: 20, width: 1000, height: 800 };
const checkOriginal = () => assert.equal(JSON.stringify(target.frameGeometry), JSON.stringify(geometry));
const target = { internalId: 'game', normalWindow: true, resourceClass: 'game', caption: 'Game',
  moveable: true, resizeable: true, frameGeometry: geometry, fullScreen: true,
  maximizeHorizontal: false, maximizeVertical: false,
  setMaximize(h, v) { this.maximizeHorizontal = h; this.maximizeVertical = v; } };
const screen = { geometry: { x: 3000, y: 0, width: 2560, height: 720 } };
let activated, removed;
const workspace = { activeWindow: target, screens: [screen], currentDesktop: {},
  windowAdded: { connect() {} },
  windowActivated: { connect(cb) { activated = cb; } },
  windowRemoved: { connect(cb) { removed = cb; } },
  clientArea() { return screen.geometry; },
  sendClientToScreen() {}, windowList() { return [target]; } };
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../kwin-launchpad.js'), 'utf8'), {
  workspace, KWin: { MaximizeArea: 1 }, print() {},
  registerShortcut(id, _label, _keys, cb) { shortcuts[id] = cb; },
  callDBus(...args) { args.at(-1)({uuid: 'game'}); },
});
activated({ normalWindow: true, resourceClass: 'edgeline-ui', caption: 'EdgeLine dashboard' });
shortcuts.EdgeLineFit();
assert.equal(target.frameGeometry.width, 2560);
assert.equal(target.fullScreen, false);
shortcuts.EdgeLineFit(); // repeated fit must not overwrite saved original geometry
shortcuts.EdgeLineRestore();
checkOriginal();
assert.equal(target.fullScreen, true);
shortcuts.EdgeLinePick();
assert.equal(target.frameGeometry.width, 2560);
shortcuts.EdgeLineRestore();
workspace.screens = [];
shortcuts.EdgeLineFit();
checkOriginal();
workspace.screens = [screen, screen];
shortcuts.EdgeLineFit();
checkOriginal();
removed(target);
shortcuts.EdgeLineRestore();
// Existing control windows are also routed to the Edge on controller startup.
workspace.screens = [screen];
target.caption = 'EdgeLine';
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../kwin-launchpad.js'), 'utf8'), {
  workspace, KWin: { MaximizeArea: 1 }, print() {}, registerShortcut() {}, callDBus() {},
});
assert.equal(target.frameGeometry.x, screen.geometry.x);
assert.equal(target.frameGeometry.width, 2560);
assert.equal(target.frameGeometry.height, 720);
console.log('kwin-launchpad: fit, restore, picker, output guards and Edge-only control placement pass');
