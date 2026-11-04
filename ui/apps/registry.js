// SPDX-License-Identifier: GPL-3.0-or-later
// App bundles: discovery, manifest validation and capability grants.
// The core loads apps at runtime, so every field that reaches the filesystem
// or the bridge is validated here and nowhere else.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');

const API_VERSION = 1;
const SUPPORTED_API = new Set([1]);
const ID = /^[a-z0-9][a-z0-9-]{1,40}$/;
const CAPABILITIES = new Set([
  'diagnostics:read', 'windows:list', 'windows:activate', 'windows:fit',
  'media:playback', 'system:telemetry', 'display:control', 'notify',
  'clipboard:read', 'clipboard:write',
]);
const METHODS = {
  'diagnostics.read': 'diagnostics:read',
  'windows.list': 'windows:list',
  'windows.activate': 'windows:activate',
  'windows.fit': 'windows:fit',
  'windows.restore': 'windows:fit',
  'media.nowPlaying': 'media:playback',
  'media.command': 'media:playback',
  'system.telemetry': 'system:telemetry',
  'display.ddcGet': 'display:control',
  'display.ddcSet': 'display:control',
  'notify.show': 'notify',
  'clipboard.read': 'clipboard:read',
  'clipboard.write': 'clipboard:write',
};
// A small curated set, so the rail can draw a crisp themed glyph instead of
// two letters. An unknown name is ignored rather than refused: an icon is
// cosmetic, and an app that ships one newer than the core should still run.
const ICONS = new Set([
  'play', 'pulse', 'clipboard', 'clock', 'moon', 'wrench', 'grid', 'note',
  'terminal', 'globe', 'chat', 'gamepad', 'camera', 'layers', 'bolt', 'heart',
]);
const IMPLEMENTED = new Set([
  'diagnostics.read',
  'media.nowPlaying', 'media.command',
  'system.telemetry',
  'clipboard.read', 'clipboard.write',
  'notify.show',
  'windows.list', 'windows.activate',
]);

function builtinDirectory() { return path.join(__dirname, 'builtin'); }
function userDirectory() {
  const dataHome = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
  return path.join(dataHome, 'edgeline', 'apps');
}
function configPath() {
  const configHome = process.env.XDG_CONFIG_HOME || path.join(os.homedir(), '.config');
  return path.join(configHome, 'edgeline', 'apps.json');
}

function readDisabled() {
  try {
    const parsed = JSON.parse(fs.readFileSync(configPath(), 'utf8'));
    return Array.isArray(parsed.disabled) ? parsed.disabled.filter((id) => typeof id === 'string' && ID.test(id)) : [];
  } catch { return []; }
}
function writeDisabled(ids) {
  const file = configPath();
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, JSON.stringify({ disabled: [...new Set(ids)].sort() }, null, 2) + '\n', { mode: 0o600 });
}

// A manifest may point at a file inside its own directory and nowhere else.
function containedEntry(directory, entry) {
  if (typeof entry !== 'string' || !entry || entry.length > 200) return null;
  if (path.isAbsolute(entry) || entry.includes('\0')) return null;
  const resolved = path.resolve(directory, entry);
  const prefix = path.resolve(directory) + path.sep;
  if (!resolved.startsWith(prefix)) return null;
  try {
    const real = fs.realpathSync(resolved);
    if (!real.startsWith(fs.realpathSync(directory) + path.sep)) return null;
    if (!fs.statSync(real).isFile()) return null;
    return real;
  } catch { return null; }
}

function normalizeWindow(raw) {
  const source = raw && typeof raw === 'object' ? raw : {};
  const number = (value, fallback, min, max) =>
    (Number.isInteger(value) && value >= min && value <= max ? value : fallback);
  return {
    width: number(source.width, 1180, 320, 4000),
    height: number(source.height, 660, 200, 2200),
    resizable: source.resizable !== false,
  };
}

// Returns { ok, app } or { ok:false, error }. Never throws on bad input.
function readManifest(directory, folderName) {
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(path.join(directory, 'app.json'), 'utf8'));
  } catch (error) {
    return { ok: false, error: /ENOENT/.test(error.message) ? 'no app.json' : 'unreadable app.json' };
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return { ok: false, error: 'app.json is not an object' };
  const id = parsed.id;
  if (typeof id !== 'string' || !ID.test(id)) return { ok: false, error: 'invalid id' };
  if (folderName && id !== folderName) return { ok: false, error: 'id does not match the directory' };
  if (!SUPPORTED_API.has(parsed.apiVersion)) return { ok: false, error: `apiVersion ${parsed.apiVersion} is not supported` };
  const name = typeof parsed.name === 'string' && parsed.name.trim() ? parsed.name.trim().slice(0, 60) : id;
  // Window captions are what the rail and the placement script match on, so
  // the core's own window names are not available to apps.
  if (/^EdgeLine/i.test(name)) return { ok: false, error: 'name is reserved for EdgeLine itself' };
  const description = typeof parsed.description === 'string' ? parsed.description.slice(0, 300) : '';
  const requested = Array.isArray(parsed.capabilities) ? parsed.capabilities : [];
  const unknown = requested.filter((capability) => !CAPABILITIES.has(capability));
  if (unknown.length) return { ok: false, error: `unknown capability ${unknown[0]}` };
  const capabilities = [...new Set(requested)];
  const entry = containedEntry(directory, parsed.entry);
  if (!entry) return { ok: false, error: 'entry is missing or outside the app directory' };
  const network = Array.isArray(parsed.network)
    ? [...new Set(parsed.network.filter((host) => typeof host === 'string' && /^[a-z0-9.*-]{1,120}$/i.test(host)))]
    : [];
  return { ok: true, app: {
    id, apiVersion: parsed.apiVersion, name, description, capabilities,
    directory: path.resolve(directory), entry, network,
    icon: typeof parsed.icon === 'string' && ICONS.has(parsed.icon) ? parsed.icon : null,
    window: normalizeWindow(parsed.window), rail: parsed.rail === true,
  } };
}

function scanRoot(root, source) {
  let entries;
  try { entries = fs.readdirSync(root, { withFileTypes: true }); }
  catch { return []; }
  const found = [];
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isDirectory() || !ID.test(entry.name)) continue;
    const directory = path.join(root, entry.name);
    const result = readManifest(directory, entry.name);
    if (!result.ok) { found.push({ id: entry.name, source, problem: result.error }); continue; }
    found.push({ ...result.app, source });
  }
  return found;
}

// Builtins first, then user apps; a user app shadows a builtin with the same id.
function discover() {
  const apps = new Map();
  for (const app of scanRoot(builtinDirectory(), 'builtin')) apps.set(app.id, app);
  for (const app of scanRoot(userDirectory(), 'user')) apps.set(app.id, app);
  // Captions are matched by the compositor placement script, so two apps may
  // not share one. Builtins keep their name; the rest are refused loudly
  // rather than silently stealing each other's windows.
  const byName = new Map();
  for (const app of apps.values()) {
    if (app.problem) continue;
    byName.set(app.name, [...(byName.get(app.name) || []), app]);
  }
  for (const [name, group] of byName) {
    if (group.length < 2) continue;
    const keep = group.find((app) => app.source === 'builtin') || group[0];
    for (const app of group) {
      if (app !== keep) app.problem = `another app already uses the name ${name}`;
    }
  }
  return apps;
}

function list() {
  const disabled = new Set(readDisabled());
  return [...discover().values()].map((app) => {
    if (app.problem) return {
      id: app.id, apiVersion: app.apiVersion ?? null, name: app.name || app.id,
      description: app.description || '', capabilities: app.capabilities || [],
      source: app.source, rail: false, enabled: false, compatible: false,
      problem: app.problem, unimplemented: [],
    };
    const loaded = !disabled.has(app.id);
    return {
      id: app.id, apiVersion: app.apiVersion, name: app.name, description: app.description,
      capabilities: app.capabilities, source: app.source, rail: app.rail, icon: app.icon,
      enabled: loaded, compatible: true,
      unimplemented: app.capabilities.filter((capability) => !implementedCapability(capability)),
    };
  }).sort((a, b) => a.name.localeCompare(b.name));
}

function implementedCapability(capability) {
  return Object.entries(METHODS).some(([method, owner]) => owner === capability && IMPLEMENTED.has(method));
}

// Resolves one app for loading. Disabled, incompatible and unknown ids are
// distinct refusals so the UI can say which one happened.
function resolve(id) {
  if (typeof id !== 'string' || !ID.test(id)) return { ok: false, error: 'invalid app id' };
  const app = discover().get(id);
  if (!app) return { ok: false, error: 'unknown app' };
  if (app.problem) return { ok: false, error: `app is unusable: ${app.problem}` };
  if (readDisabled().includes(id)) return { ok: false, error: 'app is disabled' };
  return { ok: true, app };
}

function setEnabled(id, enabled) {
  if (typeof id !== 'string' || !ID.test(id)) throw new Error('invalid app id');
  const app = discover().get(id);
  if (!app) throw new Error('unknown app');
  const disabled = new Set(readDisabled());
  if (enabled) disabled.delete(id); else disabled.add(id);
  writeDisabled([...disabled]);
  return { id, enabled: !!enabled };
}

// Method -> capability check for the bridge. Unknown methods are refused even
// when the app holds a capability that will own them later.
function authorize(app, method) {
  if (typeof method !== 'string' || !Object.hasOwn(METHODS, method)) {
    return { ok: false, error: `unknown app method ${String(method).slice(0, 40)}` };
  }
  const capability = METHODS[method];
  if (!app.capabilities.includes(capability)) return { ok: false, error: `app was not granted ${capability}` };
  if (!IMPLEMENTED.has(method)) return { ok: false, error: `${method} is not implemented in this build` };
  return { ok: true, method, capability };
}

module.exports = {
  API_VERSION, CAPABILITIES, METHODS, ID, ICONS,
  builtinDirectory, userDirectory, configPath,
  readManifest, discover, list, resolve, setEnabled, authorize,
  containedEntry, readDisabled, implementedCapability,
};
