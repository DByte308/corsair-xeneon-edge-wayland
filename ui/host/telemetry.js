// SPDX-License-Identifier: GPL-3.0-or-later
// Hardware readings for the system app. Every reader takes its input as data so
// the shaping can be tested without this machine's sensors.
'use strict';
const fs = require('fs');
const os = require('os');
const { execFile } = require('child_process');
const { promisify } = require('util');

const run = promisify(execFile);
const HWMON = '/sys/class/hwmon';

// hwmon exposes millidegrees and rpm as text files; anything unreadable is
// simply absent rather than a zero.
function readNumber(file) {
  try {
    const value = Number.parseFloat(fs.readFileSync(file, 'utf8').trim());
    return Number.isFinite(value) ? value : null;
  } catch { return null; }
}
function sensorLabel(name, index) {
  const labels = { k10temp: 'CPU', amdgpu: 'GPU', nvme: 'NVMe', asus: 'Board', spd5118: 'RAM', mt7921_phy0: 'Wi-Fi' };
  const base = labels[name] || name;
  return index > 1 ? `${base} ${index}` : base;
}
function readHwmonTree(sensors) {
  // sensors: [{name, temps:[c...], fans:[rpm...], power:[w...]}]
  const temps = [], fans = [], power = [];
  for (const sensor of sensors) {
    sensor.temps.forEach((value, i) => temps.push({ source: sensor.name, label: sensorLabel(sensor.name, i + 1), celsius: value }));
    sensor.fans.forEach((value, i) => fans.push({ source: sensor.name, label: `${sensorLabel(sensor.name, i + 1)} fan`, rpm: value }));
    sensor.power.forEach((value) => power.push({ source: sensor.name, label: `${sensorLabel(sensor.name, 1)} power`, watts: value }));
  }
  return { temps, fans, power };
}
function scanHwmon(root = HWMON) {
  let entries = [];
  try { entries = fs.readdirSync(root); } catch { return []; }
  const sensors = [];
  for (const entry of entries) {
    if (!/^hwmon\d+$/.test(entry)) continue;
    const dir = `${root}/${entry}`;
    let name = '';
    try { name = fs.readFileSync(`${dir}/name`, 'utf8').trim().slice(0, 32); } catch { continue; }
    if (!name) continue;
    const list = (pattern) => {
      let files = [];
      try { files = fs.readdirSync(dir); } catch { return []; }
      return files.filter((file) => pattern.test(file)).sort()
        .map((file) => readNumber(`${dir}/${file}`)).filter((value) => value !== null);
    };
    sensors.push({
      name,
      temps: list(/^temp\d+_input$/).map((milli) => Math.round(milli / 100) / 10),
      fans: list(/^fan\d+_input$/).map(Math.round),
      power: list(/^power\d+_average$/).map((micro) => Math.round(micro / 10000) / 100),
    });
  }
  return sensors;
}
function parseMeminfo(text) {
  const field = (name) => {
    const match = new RegExp(`^${name}:\\s+(\\d+) kB`, 'm').exec(String(text));
    return match ? Number(match[1]) : 0;
  };
  const total = field('MemTotal'), available = field('MemAvailable');
  const swapTotal = field('SwapTotal'), swapFree = field('SwapFree');
  const gb = (kb) => Math.round(kb / 1048576 * 10) / 10;
  return {
    totalGb: gb(total), usedGb: gb(Math.max(0, total - available)),
    percent: total ? Math.round((total - available) / total * 100) : 0,
    swap: { totalGb: gb(swapTotal), usedGb: gb(Math.max(0, swapTotal - swapFree)) },
  };
}
function parseProcStat(text) {
  const line = String(text).split('\n').find((entry) => entry.startsWith('cpu '));
  if (!line) return null;
  const parts = line.trim().split(/\s+/).slice(1).map(Number);
  const idle = (parts[3] || 0) + (parts[4] || 0);
  return { total: parts.reduce((sum, value) => sum + (Number.isFinite(value) ? value : 0), 0), idle };
}
// Load is a delta, so the previous sample has to persist between calls.
function createLoadReader() {
  let previous = null;
  return (text) => {
    const sample = parseProcStat(text);
    if (!sample) return null;
    const last = previous;
    previous = sample;
    if (!last) return null;
    const total = sample.total - last.total, idle = sample.idle - last.idle;
    if (total <= 0) return null;
    return Math.max(0, Math.min(100, Math.round((1 - idle / total) * 100)));
  };
}
const readLoad = createLoadReader();

function parseNvidia(text) {
  return String(text || '').split('\n').map((line) => line.trim()).filter(Boolean).map((line) => {
    const [name, temp, util, used, total] = line.split(',').map((part) => part.trim());
    return {
      name: String(name || '').replace(/^"|"$/g, '').trim().slice(0, 60), celsius: Number(temp),
      utilisation: Number(util), usedMb: Number(used), totalMb: Number(total),
    };
  }).filter((gpu) => gpu.name && Number.isFinite(gpu.celsius) && Number.isFinite(gpu.totalMb) && gpu.totalMb > 0);
}

async function collect() {
  const sensors = scanHwmon();
  const { temps, fans, power } = readHwmonTree(sensors);
  let memory = { totalGb: 0, usedGb: 0, percent: 0, swap: { totalGb: 0, usedGb: 0 } };
  try { memory = parseMeminfo(fs.readFileSync('/proc/meminfo', 'utf8')); } catch { /* keep zeros */ }
  let load = null;
  try { load = readLoad(fs.readFileSync('/proc/stat', 'utf8')); } catch { /* first call has no baseline */ }
  let nvidia = [];
  try {
    const { stdout } = await run('nvidia-smi', ['--query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total',
      '--format=csv,noheader,nounits'], { timeout: 2500 });
    nvidia = parseNvidia(stdout);
  } catch { /* no NVIDIA GPU, or the driver is busy */ }
  const amd = temps.filter((entry) => entry.source === 'amdgpu');
  return {
    available: true,
    cpu: {
      cores: os.cpus().length,
      load,
      name: (os.cpus()[0] && os.cpus()[0].model || '').trim().slice(0, 80),
      temps: temps.filter((entry) => entry.source === 'k10temp' || entry.source === 'coretemp'),
    },
    gpus: [
      ...amd.map((entry) => ({ name: 'AMD GPU', label: entry.label, celsius: entry.celsius })),
      ...nvidia.map((gpu) => ({ name: gpu.name, celsius: gpu.celsius, utilisation: gpu.utilisation,
        memoryPercent: Math.round(gpu.usedMb / gpu.totalMb * 100) })),
    ],
    memory,
    fans: fans.concat(power.map((entry) => ({ label: entry.label, watts: entry.watts }))),
    otherTemps: temps.filter((entry) => !['k10temp', 'coretemp', 'amdgpu'].includes(entry.source)),
  };
}

module.exports = { scanHwmon, readHwmonTree, parseMeminfo, parseProcStat, createLoadReader, parseNvidia, collect };
