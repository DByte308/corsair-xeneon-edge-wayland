// SPDX-License-Identifier: GPL-3.0-or-later
// KDE Connect devices for the phone app. Read-only: names, reachability and
// battery, straight from kdeconnectd's own session-bus objects.
'use strict';
const { execFile } = require('child_process');
const { promisify } = require('util');
const { parseStringArray } = require('./clipboard');

const run = promisify(execFile);
const SERVICE = 'org.kde.kdeconnect';
const DEVICES = '/modules/kdeconnect';
const DEVICE_IFACE = 'org.kde.kdeconnect.device';
const BATTERY_IFACE = 'org.kde.kdeconnect.device.battery';
const DEVICE_ID = /^[0-9a-f]{32}$/;

function parseIds(stdout) {
  return parseStringArray(stdout).filter((id) => DEVICE_ID.test(id));
}

// busctl get-property prints `b true`, `s "Name"`, `i 3` on one line.
function parseProperty(stdout) {
  const value = String(stdout || '').trim();
  if (value === 'b true') return true;
  if (value === 'b false') return false;
  if (/^i -?\d+$/.test(value)) return Number(value.split(/\s+/)[1]);
  const text = /^s "([\s\S]*)"$/.exec(value);
  return text ? text[1].replace(/\\"/g, '"') : '';
}

async function property(path, iface, name) {
  const { stdout } = await run('busctl', ['--user', 'get-property', SERVICE, path, iface, name], { timeout: 2000 });
  return parseProperty(stdout);
}

function normalize(raw) {
  const type = String(raw.type || 'unknown').replace(/[^a-z]/gi, '').slice(0, 20);
  const charge = Number.isInteger(raw.charge) && raw.charge >= 0 && raw.charge <= 100 ? raw.charge : null;
  return {
    id: raw.id,
    name: String(raw.name || 'Unnamed device').slice(0, 60),
    type: type || 'unknown',
    reachable: raw.reachable === true,
    paired: raw.paired !== false,
    battery: charge,
  };
}

async function devices() {
  let ids = [];
  try {
    const { stdout } = await run('busctl', ['--user', 'call', SERVICE, DEVICES,
      'org.kde.kdeconnect.daemon', 'devices', 'bb', 'false', 'false'], { timeout: 2500 });
    ids = parseIds(stdout);
  } catch (error) {
    return { available: false, devices: [], error: /not found|No such/i.test(error.message) ? 'KDE Connect is not running' : error.message };
  }
  const list = [];
  for (const id of ids.slice(0, 16)) {
    const path = `${DEVICES}/devices/${id}`;
    try {
      const [name, type, reachable, paired] = await Promise.all([
        property(path, DEVICE_IFACE, 'name').catch(() => ''),
        property(path, DEVICE_IFACE, 'type').catch(() => ''),
        property(path, DEVICE_IFACE, 'isReachable').catch(() => false),
        property(path, DEVICE_IFACE, 'isPaired').catch(() => true),
      ]);
      let charge = null;
      if (reachable === true) {
        // The battery object only exists while the plugin is loaded.
        const value = await property(`${path}/battery`, BATTERY_IFACE, 'charge').catch(() => null);
        if (Number.isInteger(value)) charge = value;
      }
      list.push(normalize({ id, name, type, reachable, paired, charge }));
    } catch { /* a device can disappear between list and read */ }
  }
  list.sort((a, b) => Number(b.reachable) - Number(a.reachable) || a.name.localeCompare(b.name));
  return { available: true, devices: list, error: '' };
}

module.exports = { parseIds, parseProperty, normalize, devices };
