// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const telemetry = require('../host/telemetry.js');

// hwmon shaping: millidegrees become celsius, unreadable sensors are absent.
const shaped = telemetry.readHwmonTree([
  { name: 'k10temp', temps: [42.5, 38.2], fans: [], power: [] },
  { name: 'amdgpu', temps: [55.1], fans: [1200], power: [102.4] },
  { name: 'nvme', temps: [39.9], fans: [], power: [] },
]);
assert.deepEqual(shaped.temps.map((entry) => [entry.label, entry.celsius]),
  [['CPU', 42.5], ['CPU 2', 38.2], ['GPU', 55.1], ['NVMe', 39.9]]);
assert.deepEqual(shaped.fans.map((entry) => [entry.label, entry.rpm]), [['GPU fan', 1200]]);
assert.deepEqual(shaped.power.map((entry) => [entry.label, entry.watts]), [['GPU power', 102.4]]);

// Memory percentages come from MemAvailable, never from a hardcoded guess.
const mem = telemetry.parseMeminfo(`MemTotal:       32000000 kB
MemFree:         2000000 kB
MemAvailable:   16000000 kB
SwapTotal:       8000000 kB
SwapFree:        6000000 kB
`);
assert.equal(mem.totalGb, 30.5);
assert.equal(mem.usedGb, 15.3);
assert.equal(mem.percent, 50);
assert.equal(mem.swap.usedGb, 1.9);
assert.deepEqual(telemetry.parseMeminfo(''), { totalGb: 0, usedGb: 0, percent: 0, swap: { totalGb: 0, usedGb: 0 } });

// /proc/stat deltas, including the first call that has no baseline.
const stat = (busy, idle) => `cpu  ${busy} 0 ${busy} ${idle} ${idle} 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n`;
const load = telemetry.createLoadReader();
assert.equal(load(stat(100, 100)), null, 'the first sample only establishes a baseline');
assert.equal(load(stat(150, 150)), 50, 'half of the delta busy is 50 percent');
assert.equal(load(stat(160, 240)), 10, 'idle delta dominates, so the load is low');
assert.equal(load(stat(400, 250)), 96);
assert.equal(load(stat(50, 500)), null, 'a counter reset yields no reading instead of a negative load');
assert.equal(load('nonsense'), null);
assert.equal(telemetry.parseProcStat('cpu0 1 2 3 4'), null);

// nvidia-smi CSV, with a broken line that must be dropped.
const gpus = telemetry.parseNvidia(`NVIDIA GeForce RTX 4090, 51, 7, 1024, 24564
"NVIDIA RTX A4000", 44, 0, 200, 16376
broken line
, 12, 3, 4, 5
`);
assert.equal(gpus.length, 2);
assert.equal(gpus[0].celsius, 51);
assert.equal(gpus[0].memoryPercent ?? undefined, undefined);
assert.equal(gpus[1].name, 'NVIDIA RTX A4000', 'quoted names are unwrapped');
assert.deepEqual(telemetry.parseNvidia(''), []);

// Bounded numbers: the app must never be handed NaN or Infinity.
(async () => {
  const data = await telemetry.collect();
  assert.equal(data.available, true);
  assert.ok(Number.isInteger(data.cpu.cores));
  for (const gpu of data.gpus) assert.ok(Number.isFinite(gpu.celsius));
  for (const entry of data.otherTemps) assert.ok(Number.isFinite(entry.celsius));
  assert.ok(data.memory.percent >= 0 && data.memory.percent <= 100);
  assert.equal(typeof data.memory.usedGb, 'number');
  console.log('telemetry: hwmon shaping, memory, load deltas, nvidia parsing and bounds pass');
})();
