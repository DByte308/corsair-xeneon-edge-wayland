// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const { spawn } = require('child_process');
const readline = require('readline');
const fs = require('fs');
const { scriptPath, dbus } = require('./launchpad');
let child = null;
let tag = null;
let state = { available: false, windows: [], error: 'Connecting to KWin…' };
const uuid = /^\{?[0-9a-f]{8}-[0-9a-f-]{27}\}?$/i;
const integer = (value, min, max) => (Number.isInteger(value) && value >= min && value <= max ? value : null);
function readRect(raw) {
  if (!raw || typeof raw !== 'object') return null;
  const x = integer(raw.x, -100000, 100000), y = integer(raw.y, -100000, 100000);
  const width = integer(raw.width, 1, 100000), height = integer(raw.height, 1, 100000);
  return x === null || y === null || width === null || height === null ? null : { x, y, width, height };
}
function readScreens(raw) {
  if (!Array.isArray(raw)) return [];
  return raw.slice(0, 16).flatMap((entry) => {
    const geometry = readRect(entry && entry.geometry);
    const workarea = readRect(entry && entry.workarea);
    if (!geometry || !workarea) return [];
    return [{ name: String(entry.name || '').slice(0, 64), geometry, workarea }];
  });
}
function readPanel(raw) {
  if (!Array.isArray(raw)) return [];
  return raw.slice(0, 16).flatMap((entry) => {
    const rect = readRect(entry);
    if (!rect || typeof entry.title !== 'string') return [];
    return [{ title: entry.title.slice(0, 128), ...rect, keepAbove: !!entry.keepAbove, minimized: !!entry.minimized }];
  });
}
async function load(file, name) {
  const result = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss', file, name);
  const match = /^i (\d+)$/.exec(result);
  if (!match) throw new Error('KWin refused the window overview script');
  await dbus('org.kde.KWin', `/Scripting/Script${match[1]}`, 'org.kde.kwin.Script', 'run');
}
async function start() {
  if (child) return;
  if (process.env.XDG_SESSION_TYPE !== 'wayland') return;
  const busName = `dev.edgeline.Windows.p${process.pid}`;
  child = spawn('python3', ['-u', scriptPath('window-bridge.py'), busName], { stdio: ['ignore', 'pipe', 'pipe'] });
  child.on('error', (error) => { state = { available:false, windows:[], error:error.message }; });
  child.on('exit', () => { child = null; state = { available:false, windows:[], error:'Window overview bridge stopped' }; });
  child.stderr.on('data', (data) => console.error('[window-overview]', data.toString().trim()));
  readline.createInterface({ input: child.stdout }).on('line', (line) => {
    try {
      const data = JSON.parse(line);
      if (data.ready) {
        const file = scriptPath('kwin-window-overview.js');
        fs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('__EDGELINE_BUS__', JSON.stringify(busName)));
        tag = `edgeline-window-overview-${process.pid}`;
        load(file, tag).catch((error) => { state = { available:false, windows:[], error:error.message }; });
      } else if (Array.isArray(data.windows)) {
        state = { available:!!data.available, error:'', windows:data.windows.slice(0, 100)
          .filter(w => typeof w.id === 'string' && uuid.test(w.id))
          .map(w => ({id:w.id, title:String(w.title).slice(0,256), app:String(w.app).slice(0,128),
            active:!!w.active, minimized:!!w.minimized})),
          screens:readScreens(data.screens), panel:readPanel(data.panel) };
      }
    } catch (error) { console.error('[window-overview]', error.message); }
  });
}
async function activate(id) {
  if (!uuid.test(id) || !state.windows.some(w => w.id === id)) throw new Error('That app is no longer on the Edge');
  const file = scriptPath('kwin-activate-window.js');
  fs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('__EDGELINE_WINDOW_ID__', JSON.stringify(id)));
  const name = `edgeline-activate-${process.pid}-${Date.now()}`;
  await load(file, name);
  setTimeout(() => dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', name).catch(() => {}), 5000).unref();
  return { message:'App selected on the Edge' };
}
function stop() {
  if (tag) dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', tag).catch(() => {});
  if (child) child.kill();
}
module.exports = { start, stop, snapshot:() => state, activate };
