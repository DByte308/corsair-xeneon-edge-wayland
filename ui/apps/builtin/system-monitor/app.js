// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const grid = document.getElementById('grid');
const foot = document.getElementById('foot');

function card(title, children, options = {}) {
  const section = document.createElement('section');
  section.className = 'card';
  const heading = document.createElement('h2');
  heading.textContent = title;
  section.append(heading);
  if (options.big !== undefined) {
    const big = document.createElement('div');
    big.className = 'big';
    big.append(document.createTextNode(String(options.big)));
    if (options.unit) {
      const unit = document.createElement('span');
      unit.className = 'unit';
      unit.textContent = options.unit;
      big.append(unit);
    }
    section.append(big);
  }
  if (options.meter !== undefined) {
    const meter = document.createElement('div');
    meter.className = `meter${options.meter > 85 ? ' hot' : options.meter > 70 ? ' warm' : ''}`;
    const fill = document.createElement('div');
    fill.style.width = `${Math.max(0, Math.min(100, options.meter))}%`;
    meter.append(fill);
    section.append(meter);
  }
  for (const [label, value] of children) {
    const line = document.createElement('div');
    line.className = 'line';
    const left = document.createElement('span');
    left.className = 'label';
    left.textContent = label;
    const right = document.createElement('span');
    right.className = 'value';
    right.textContent = value;
    line.append(left, right);
    section.append(line);
  }
  return section;
}
const temp = (value) => (Number.isFinite(value) ? `${value.toFixed(1)} °C` : '–');

function render(data) {
  document.getElementById('cpuModel').textContent = data.cpu.name || `${data.cpu.cores} cores`;
  const cards = [];
  cards.push(card('CPU', [['Temp', temp((data.cpu.temps[0] || {}).celsius)]], {
    big: data.cpu.load === null ? '–' : data.cpu.load, unit: '% load',
    meter: data.cpu.load === null ? 0 : data.cpu.load,
  }));
  cards.push(card('Memory', [
    ['Used', `${data.memory.usedGb} of ${data.memory.totalGb} GB`],
    ['Swap', `${data.memory.swap.usedGb} of ${data.memory.swap.totalGb} GB`],
  ], { big: data.memory.percent, unit: '% used', meter: data.memory.percent }));
  for (const gpu of data.gpus) {
    cards.push(card('GPU', [
      ['Chip', gpu.name],
      ...(gpu.label ? [['Sensor', gpu.label]] : []),
      ...(Number.isFinite(gpu.utilisation) ? [['Load', `${gpu.utilisation} %`]] : []),
      ...(Number.isFinite(gpu.memoryPercent) ? [['VRAM', `${gpu.memoryPercent} %`]] : []),
    ], { big: temp(gpu.celsius), meter: Number.isFinite(gpu.utilisation) ? gpu.utilisation : 0 }));
  }
  if (data.fans.length) {
    cards.push(card('Cooling', data.fans.map((fan) => [fan.label,
      Number.isFinite(fan.rpm) ? `${fan.rpm} rpm` : `${fan.watts} W`])));
  }
  if (data.otherTemps.length) {
    cards.push(card('Other sensors', data.otherTemps.map((entry) => [entry.label, temp(entry.celsius)])));
  }
  grid.replaceChildren(...cards);
  foot.textContent = `Read at ${new Date().toLocaleTimeString()} · ${data.cpu.cores} logical cores`;
}
async function refresh() {
  try { render(await api.call('system.telemetry')); }
  catch (error) { grid.replaceChildren(card('Sensors unavailable', [['Reason', error.message]])); }
}
document.getElementById('refresh').onclick = refresh;
document.getElementById('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') window.close();
  if (event.key === 'r' || event.key === 'R') refresh();
});
refresh();
setInterval(refresh, 1500);
