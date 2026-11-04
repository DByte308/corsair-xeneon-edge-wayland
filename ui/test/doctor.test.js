// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const { collect } = require('../diagnostics.js');
const { buildReport } = require('../apps/builtin/edge-doctor/model.js');

const displays = [
  { id: '1', label: 'XENEON EDGE', size: { width: 2560, height: 720 },
    geometry: { x: 467, y: 2880, width: 2560, height: 720 },
    workarea: { x: 467, y: 2880, width: 2560, height: 720 }, scale: 1 },
  { id: '2', label: '49in', size: { width: 3440, height: 1440 },
    geometry: { x: 0, y: 0, width: 3440, height: 1440 },
    workarea: { x: 0, y: 0, width: 3440, height: 1394 }, scale: 1 },
];
const healthy = collect({
  generatedAt: '2026-01-01T00:00:00.000Z', railWidth: 96, edgeId: '1',
  host: { runtime: 'Electron', version: '45.2.1', pid: 4242, session: 'wayland', desktop: 'KDE', backend: 'electron' },
  displays,
  compositor: {
    screens: [
      { name: 'DP-1', geometry: { x: 467, y: 2880, width: 2560, height: 720 }, workarea: { x: 563, y: 2880, width: 2464, height: 720 } },
      { name: 'HDMI-A-2', geometry: { x: 0, y: 0, width: 3440, height: 1440 }, workarea: { x: 0, y: 0, width: 3440, height: 1394 } },
    ],
    panel: [{ title: 'EdgeLine taskbar', x: 467, y: 2880, width: 96, height: 720, keepAbove: true, minimized: false }],
  },
  overview: { available: true, count: 3, error: '' },
  controller: { plugin: 'edgeline-launchpad-aaaaaaaaaaaa', loaded: true, recorded: ['edgeline-launchpad-aaaaaaaaaaaa'], stale: 0 },
  reservation: { running: true, pid: 555, failures: 0 },
  agent: { socket: '/run/user/1000/edgeline.sock', connected: true },
});

// The compositor work area wins over Electron's view of it.
assert.equal(healthy.edge.workarea.x, 563);
assert.equal(healthy.edge.workareaSource, 'compositor');
assert.equal(healthy.edge.geometry.width, 2560);
assert.equal(healthy.panel[0].title, 'EdgeLine taskbar');
const good = buildReport(healthy);
assert.equal(good.verdict, 'ok', `healthy layout must be ok, got ${JSON.stringify(good.counts)}`);
assert.equal(good.counts.bad, 0);
assert.equal(good.counts.warn, 0);

// A displaced sidebar and a missing reservation are what the last two bugs
// looked like; both must be flagged, and by the right section.
const displaced = JSON.parse(JSON.stringify(healthy));
displaced.panel[0].x = 563;
displaced.panel[0].width = 96;
const displacedReport = buildReport(displaced);
assert.equal(displacedReport.verdict, 'bad');
const sidebar = displacedReport.sections.find((section) => section.title === 'Sidebar');
assert.equal(sidebar.rows[0].status, 'bad');
assert.match(sidebar.rows[0].actual, /563/);
assert.match(sidebar.rows[0].expected, /467/);

const unreserved = JSON.parse(JSON.stringify(healthy));
unreserved.edge.workarea = { x: 467, y: 2880, width: 2560, height: 720 };
unreserved.reservation.running = false;
const unreservedReport = buildReport(unreserved);
assert.equal(unreservedReport.verdict, 'bad', 'an unreserved work area cannot pass');
const edgeSection = unreservedReport.sections.find((section) => section.title === 'Edge output');
assert.equal(edgeSection.rows[1].status, 'warn');
assert.match(edgeSection.rows[1].actual, /nothing reserved/);

// Stale controllers and a broken controller are warnings/bad respectively.
const stacked = JSON.parse(JSON.stringify(healthy));
stacked.controller.stale = 2;
stacked.controller.recorded = ['edgeline-launchpad-aaaaaaaaaaaa', 'edgeline-launchpad-bbbbbbbbbbbb'];
const stackedReport = buildReport(stacked);
assert.equal(stackedReport.sections.find((s) => s.title === 'KWin controller').rows[2].status, 'warn');
const noController = JSON.parse(JSON.stringify(healthy));
noController.controller = { plugin: '', loaded: false, recorded: [], stale: 0 };
assert.equal(buildReport(noController).verdict, 'bad');

// A missing Edge is a hard failure, not a crash.
const noEdge = collect({ railWidth: 96, edgeId: null, displays: [displays[1]], compositor: {}, host: {} });
assert.equal(noEdge.edge, null);
const noEdgeReport = buildReport(noEdge);
assert.equal(noEdgeReport.verdict, 'bad');
assert.ok(noEdgeReport.sections.some((s) => s.title === 'Edge output'));
assert.equal(buildReport(collect({})).verdict, 'bad', 'an empty payload still renders a report');

// Hostile input is bounded, never echoed raw.
const hostile = collect({
  railWidth: 100000, edgeId: '1',
  displays: [1, null, { geometry: { x: 'NaN', y: 0, width: 10, height: 10 } }, { ...displays[0], label: 'x'.repeat(5000) }],
  compositor: { screens: [null, { geometry: { x: 0, y: 0, width: -1, height: 0 } }, { geometry: { x: 467, y: 2880, width: 2560, height: 720 }, workarea: { x: 563, y: 2880, width: 2464, height: 720 } }],
    panel: [{ title: 'y'.repeat(5000), x: 0, y: 0, width: 10, height: 10 }, null, { title: 'no frame' }] },
  host: { pid: 'not-a-pid', runtime: 'z'.repeat(400) },
  controller: { recorded: Array.from({ length: 100 }, (_, i) => `edgeline-launchpad-${i}`) },
  reservation: { running: 'yes' },
});
assert.equal(hostile.railWidth, 96, 'an absurd rail width falls back to the documented default');
assert.equal(hostile.displays.length, 1, 'only displays with a full rectangle survive');
assert.ok(hostile.displays[0].label.length <= 80);
assert.equal(hostile.host.pid, null);
assert.ok(hostile.host.runtime.length <= 40);
assert.equal(hostile.controller.recorded.length, 16);
assert.equal(hostile.panel.length, 1);
assert.ok(hostile.panel[0].title.length <= 100);
assert.equal(hostile.reservation.running, false);
const hostileReport = buildReport(hostile);
assert.ok(hostileReport.sections.every((section) => section.rows.every((row) => typeof row.actual === 'string')));

// Other outputs are reported as untouched, so a stray strut is visible.
const reportWithOthers = buildReport(healthy);
const others = reportWithOthers.sections.find((section) => section.title === 'Other outputs');
assert.equal(others.rows.length, 1);
assert.match(others.rows[0].actual, /3440×1440/);
assert.match(others.rows[0].expected, /not touched/);

// The model file stays a pure module (no DOM, no Electron) so it can be tested.
const modelSource = require('node:fs').readFileSync(path.join(__dirname, '../apps/builtin/edge-doctor/model.js'), 'utf8');
assert.equal(/document\.|require\('electron'\)/.test(modelSource), false);
console.log('edge-doctor: healthy layout, displaced rail, missing reservation, stale controllers, missing Edge and hostile payload pass');
