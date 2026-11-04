// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const { execFile } = require('child_process');
const { promisify } = require('util');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { createHash } = require('crypto');
const { selectedWindow } = require('./window-selection');
let pickerInProgress = false;
// KGlobalAccel retains action names after scripts stop. Version the names by
// controller content so a rebuilt UI never invokes a stale script callback.
const controllerVersion = createHash('sha256')
  .update(fs.readFileSync(path.join(__dirname, 'kwin-launchpad.js'))).digest('hex').slice(0, 12);
const shortcutPrefix = `EdgeLine${controllerVersion}`;
let scriptDirectory;
function scriptPath(name) {
  // KWin is outside Electron and cannot read virtual app.asar paths.
  if (!scriptDirectory) {
    scriptDirectory = fs.mkdtempSync(path.join(process.env.XDG_RUNTIME_DIR || os.tmpdir(), 'edgeline-kwin-'));
    fs.chmodSync(scriptDirectory, 0o700);
  }
  const destination = path.join(scriptDirectory, name);
  let source = fs.readFileSync(path.join(__dirname, name), 'utf8');
  if (name === 'kwin-launchpad.js') {
    source = source.replaceAll("registerShortcut('EdgeLine", `registerShortcut('${shortcutPrefix}`);
  }
  fs.writeFileSync(destination, source, { mode: 0o600 });
  return destination;
}
const run = promisify(execFile);
const ACTIONS = { fit: `${shortcutPrefix}Fit`, restore: `${shortcutPrefix}Restore`, pick: `${shortcutPrefix}Pick`,
  panel: `${shortcutPrefix}Panel`, 'fit-vanguard': `${shortcutPrefix}VanguardFit` };
const plugin = `edgeline-launchpad-${controllerVersion}`;
// KWin keeps a loaded script until it is unloaded or the compositor restarts.
// Every rebuild changes the plugin name, so the names actually loaded are
// recorded and stale copies are unloaded instead of piling up and fighting
// over the same windows.
const controllerStatePath = path.join(
  process.env.XDG_RUNTIME_DIR || os.tmpdir(), 'edgeline-kwin-controllers.json');
// Recording demands the current name format. Unloading accepts any earlier
// format too, so a future naming change cannot strand an old controller.
const CONTROLLER_NAME = /^edgeline-launchpad-[0-9a-f]{12}$/;
const CONTROLLER_PREFIX = /^edgeline-launchpad-[a-z0-9]{4,}$/i;
function staleControllerNames(recorded, current) {
  return [...new Set(recorded)]
    .filter((name) => CONTROLLER_PREFIX.test(name) && name !== current);
}
function recordedControllers() {
  try {
    const parsed = JSON.parse(fs.readFileSync(controllerStatePath, 'utf8'));
    return Array.isArray(parsed) ? staleControllerNames(parsed, null) : [];
  } catch { return []; }
}
async function controllerStatus() {
  const recorded = recordedControllers();
  let currentLoaded = false;
  try {
    currentLoaded = (await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', plugin)) === 'b true';
  } catch { /* KWin may be restarting; report the recorded state instead */ }
  return { plugin, loaded: currentLoaded, recorded, stale: recorded.filter((name) => name !== plugin).length };
}
function recordController(name) {
  if (!CONTROLLER_NAME.test(name)) return;
  // Only this controller remains loaded: everything else recorded was just
  // unloaded, so a merged list would grow without bound.
  try { fs.writeFileSync(controllerStatePath, JSON.stringify([name]), { mode: 0o600 }); }
  catch { /* the controller is live either way; recording is best effort */ }
}
async function dbus(...args) {
  const { stdout } = await run('busctl', ['--user', 'call', ...args], { timeout: 5000 });
  return stdout.trim();
}
async function loadController() {
  // Also runs when this controller is already loaded: a crash restart with an
  // older controller still recorded must clear it, not just re-record itself.
  for (const stale of recordedControllers().filter((name) => name !== plugin)) {
    try {
      await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', stale);
    } catch { /* an already-gone controller needs no cleanup */ }
  }
  const loaded = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', plugin);
  if (loaded === 'b true') { recordController(plugin); return; }
  const answer = await dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss',
    scriptPath('kwin-launchpad.js'), plugin);
  const match = /^i (\d+)$/.exec(answer);
  if (!match) throw new Error('KWin refused the launchpad controller');
  await dbus('org.kde.KWin', `/Scripting/Script${match[1]}`, 'org.kde.kwin.Script', 'run');
  recordController(plugin);
}
let controllerLoad = null;
function ensureController() {
  // Concurrent callers share one load instead of racing two loadScript calls.
  if (!controllerLoad) {
    controllerLoad = loadController().finally(() => { controllerLoad = null; });
  }
  return controllerLoad;
}
function localGameArtwork() {
  const roots = [path.join(os.homedir(), '.local/share/Steam'), path.join(os.homedir(), '.steam/steam')];
  for (const root of roots) {
    const cache = path.join(root, 'appcache/librarycache/3471800');
    try {
      for (const entry of fs.readdirSync(cache, { withFileTypes: true })) {
        if (!entry.isDirectory()) continue;
        const image = path.join(cache, entry.name, 'library_hero.jpg');
        try {
          if (fs.statSync(image).size > 4 * 1024 * 1024) continue;
          return `data:image/jpeg;base64,${fs.readFileSync(image).toString('base64')}`;
        } catch { /* another cache entry may contain the hero */ }
      }
    } catch { /* no local Steam artwork; gradient fallback remains */ }
  }
  return null;
}
async function action(name, shell) {
  if (name === 'artwork') return { image: localGameArtwork() };
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
      scriptPath('kwin-place-panel.js'), tag);
    const match = /^i (\d+)$/.exec(answer);
    if (!match) throw new Error('KWin refused dashboard placement');
    await dbus('org.kde.KWin', `/Scripting/Script${match[1]}`, 'org.kde.kwin.Script', 'run');
    // run() queues execution in KWin. Immediate unloading can cancel it.
    setTimeout(() => {
      dbus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', tag).catch(() => {});
    }, 5000).unref();
    return { message: 'Dashboard placement requested directly from KWin' };
  }
  if (name === 'pick') {
    if (pickerInProgress) throw new Error('Window selection is already active; click an app or press Esc');
    pickerInProgress = true;
    try {
      await ensureController();
      const {stdout} = await run('busctl', ['--user', '--timeout=120', '--json=short', 'call',
        'org.kde.KWin', '/KWin', 'org.kde.KWin', 'queryWindowInfo'], {timeout:125000});
      const selected = selectedWindow(stdout);
      await dbus('org.kde.kglobalaccel', '/component/kwin', 'org.kde.kglobalaccel.Component',
        'invokeShortcut', 's', `${shortcutPrefix}Window${selected.suffix}`);
      return {message:'Selected app sent to the Edge'};
    } catch (error) {
      if (/UserCancel|User cancelled/i.test(error.stderr || error.message)) return {message:'Window selection cancelled',cancelled:true};
      throw new Error(`Could not select that app. Press Esc if the crosshair is still active. ${error.message}`);
    } finally { pickerInProgress = false; }
  }
  if (!Object.hasOwn(ACTIONS, name)) throw new Error('Unknown launchpad action');
  if (process.env.XDG_SESSION_TYPE !== 'wayland') throw new Error('Window controls require KDE Wayland');
  await ensureController();
  await dbus('org.kde.kglobalaccel', '/component/kwin', 'org.kde.kglobalaccel.Component', 'invokeShortcut', 's', ACTIONS[name]);
  // Shortcut delivery is not proof a game accepted the requested geometry.
  return { message: name === 'pick' ? 'Click the window to fit; Esc cancels' : 'Window request sent to KWin; check the target window' };
}
module.exports = { action, ensureController, scriptPath, dbus, plugin, controllerStatePath,
  controllerStatus, recordedControllers, recordController, staleControllerNames };
