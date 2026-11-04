// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const list = document.getElementById('list');
const sub = document.getElementById('sub');

function deviceTile(device) {
  const node = document.createElement('section');
  node.className = `device${device.reachable ? '' : ' offline'}`;
  const head = document.createElement('div');
  head.className = 'head';
  const name = document.createElement('span');
  name.className = 'name';
  name.textContent = device.name;
  const type = document.createElement('span');
  type.className = 'type';
  type.textContent = device.type;
  head.append(name, type);
  const state = document.createElement('div');
  state.className = 'state';
  const dot = document.createElement('span');
  dot.className = `dot${device.reachable ? ' on' : ''}`;
  const label = document.createElement('span');
  label.textContent = device.reachable ? 'connected' : (device.paired ? 'paired, offline' : 'not paired');
  state.append(dot, label);
  node.append(head, state);
  if (Number.isInteger(device.battery)) {
    const battery = document.createElement('div');
    battery.className = 'battery';
    const bar = document.createElement('div');
    // `is-` prefixes: a bare `empty` class collides with the placeholder style.
    bar.className = `bar${device.battery <= 15 ? ' is-empty' : device.battery <= 35 ? ' is-low' : ''}`;
    const fill = document.createElement('div');
    fill.style.width = `${device.battery}%`;
    bar.append(fill);
    const pct = document.createElement('span');
    pct.className = 'pct';
    pct.textContent = `${device.battery}%`;
    battery.append(bar, pct);
    node.append(battery);
  }
  return node;
}
async function refresh() {
  try {
    const state = await api.call('kdeconnect.devices');
    if (!state.available) {
      list.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = state.error || 'KDE Connect is not running';
      list.append(empty);
      sub.textContent = 'KDE Connect devices';
      return;
    }
    const devices = state.devices || [];
    const online = devices.filter((device) => device.reachable).length;
    sub.textContent = `${devices.length} device${devices.length === 1 ? '' : 's'} · ${online} connected`;
    if (!devices.length) {
      list.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = 'No devices paired yet. Pair one in the KDE Connect settings.';
      list.append(empty);
      return;
    }
    list.replaceChildren(...devices.map(deviceTile));
  } catch (error) {
    list.replaceChildren();
    const empty = document.createElement('p');
    empty.className = 'empty';
    empty.textContent = error.message;
    list.append(empty);
  }
}
document.getElementById('refresh').onclick = refresh;
document.getElementById('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') window.close();
  if (event.key === 'r' || event.key === 'R') refresh();
});
refresh();
setInterval(refresh, 10000);
