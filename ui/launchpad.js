// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const { execFile } = require('child_process');
const { promisify } = require('util');
const path = require('path');
const run = promisify(execFile);
const ACTIONS = { fit: 'EdgeLineFit', restore: 'EdgeLineRestore', pick: 'EdgeLinePick',
  panel: 'EdgeLinePanel', 'fit-vanguard': 'EdgeLineVanguardFit' };
const plugin = 'edgeline-launchpad';
async function dbus(...args) {
  const { stdout } = await run('busctl', ['--user', 'call', ...args], { timeout: 5000 });
  return stdout.trim();
}
async function ensureController() {
  const loaded = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', plugin);
  if (loaded === 'b true') return;
  const answer = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss',
    path.join(__dirname, 'kwin-launchpad.js'), plugin);
  const match = /^i (\d+)$/.exec(answer);
  if (!match) throw new Error('KWin refused the launchpad controller');
  await dbus('org.kde.KWin', `/Scripting/Script${match[1]}`, 'org.kde.kwin.Script', 'run');
}
async function action(name, shell) {
  if (name === 'vanguard') {
    await shell.openExternal('steam://rungameid/3471800');
    return { message: 'Launch request sent to Steam' };
  }
  if (name === 'steam') {
    await shell.openExternal('steam://open/library');
    return { message: 'Steam library requested' };
  }
  if (name === 'panel') {
    const tag = `edgeline-panel-${process.pid}-${Date.now()}`;
    const answer = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss',
      path.join(__dirname, 'kwin-place-panel.js'), tag);
    const match = /^i (\d+)$/.exec(answer);
    if (!match) throw new Error('KWin refused dashboard placement');
    await dbus('org.kde.KWin', `/Scripting/Script${match[1]}`, 'org.kde.kwin.Script', 'run');
    // run() queues execution in KWin. Immediate unloading can cancel it.
    setTimeout(() => {
      dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', tag).catch(() => {});
    }, 5000).unref();
    return { message: 'Dashboard placement requested directly from KWin' };
  }
  if (!Object.hasOwn(ACTIONS, name)) throw new Error('Unknown launchpad action');
  if (process.env.XDG_SESSION_TYPE !== 'wayland') throw new Error('Window controls require KDE Wayland');
  await ensureController();
  await dbus('org.kde.kglobalaccel', '/component/kwin', 'org.kde.kglobalaccel.Component', 'invokeShortcut', 's', ACTIONS[name]);
  // Shortcut delivery is not proof a game accepted the requested geometry.
  return { message: name === 'pick' ? 'Click the window to fit; Esc cancels' : 'Window request sent to KWin; check the target window' };
}
module.exports = { action, ensureController };
