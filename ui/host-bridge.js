// SPDX-License-Identifier: GPL-3.0-or-later
// Host-side methods for the built-in apps, on a socket the native host can call.
//
// The apps ask their host for things that are neither device nor compositor
// concerns: the app registry, the clipboard, MPRIS, telemetry, KDE Connect, window
// decoration and diagnostics. In the Electron host those live in `ui/main.js`
// (`APP_METHODS`) and already have tested modules under `ui/host/`. The Tauri host
// had none of them, so the bundled apps failed with "Unsupported method" in the
// native host regardless of which backend served the panel.
//
// Rather than reimplement all of that in Rust, this is a small JSON-lines server
// that reuses the same modules, so both hosts answer identically and the 22 UI
// suites keep covering the code that runs. Only the facts the host owns (its
// version, pid, session, the display list, whether an app window is open) come
// from the caller, in `params.context`.
//
// Usage: node ui/host-bridge.js [socket-path]
// Default socket: $XDG_RUNTIME_DIR/edgeline-host.sock (0600, private directory).

'use strict';
const net = require('node:net');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const registry = require('./apps/registry.js');
const media = require('./host/media.js');
const telemetry = require('./host/telemetry.js');
const clipboard = require('./host/clipboard.js');
const kdeconnect = require('./host/kdeconnect.js');
const windowDecorate = require('./host/window-decorate.js');
const diagnostics = require('./diagnostics.js');

const MAX_MESSAGE = 4 * 1024 * 1024;
const CORE_CALL_TIMEOUT_MS = 15_000;

function coreSocket() {
  const runtime = process.env.XDG_RUNTIME_DIR || path.join('/run/user', String(process.getuid()));
  return path.join(runtime, 'edgeline-rust', 'backend.sock');
}

/** One request/response against the Rust backend socket, same wire format as `rpc`. */
function coreCall(method, params = {}, timeoutMs = CORE_CALL_TIMEOUT_MS) {
  return new Promise((resolve) => {
    const socket = net.createConnection(coreSocket());
    let done = false;
    const finish = (value) => {
      if (done) return;
      done = true;
      socket.destroy();
      resolve(value);
    };
    const timer = setTimeout(() => finish({ error: 'core request timed out' }), timeoutMs);
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('error', (error) => {
      clearTimeout(timer);
      finish({ error: error.message });
    });
    socket.on('data', (chunk) => {
      buffer += chunk;
      let index;
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (!line) continue;
        let message;
        try { message = JSON.parse(line); } catch { continue; }
        // Events are not replies; ignore them.
        if (message && message.event) continue;
        clearTimeout(timer);
        finish(message || {});
      }
    });
    socket.on('connect', () => {
      socket.write(`${JSON.stringify({ id: 1, method, params })}\n`);
    });
  });
}

/** The registry, in the shape the app drawer and rail expect. */
function appList(openIds) {
  const open = new Set(Array.isArray(openIds) ? openIds.map(String) : []);
  return registry.list()
    .filter((app) => app.compatible && app.enabled)
    .map((app) => ({
      id: app.id,
      name: app.name,
      description: app.description,
      icon: app.icon,
      capabilities: app.capabilities,
      open: open.has(app.id),
    }));
}

async function windowsList(context) {
  const snapshot = await coreCall('window.snapshot');
  // A missing backend is an error, not an empty window list: the two must not look
  // alike to the app that asked.
  if (snapshot.error) throw new Error(`window feed unavailable: ${snapshot.error}`);
  if (snapshot.result && snapshot.result.error) throw new Error(snapshot.result.error);
  const result = snapshot.result || { available: false, windows: [] };
  return { ...result, windows: windowDecorate.decorate(appList(context.openApps), result.windows) };
}

async function diagnosticsRead(context) {
  const snapshot = await coreCall('window.snapshot');
  const compositor = snapshot && snapshot.result ? snapshot.result : {};
  const input = {
    ...(context.diagnostics || {}),
    compositor,
  };
  return diagnostics.collect(input);
}

/**
 * What an app must be granted to call `method`, straight from the registry — the
 * single source of truth for that mapping. The native host decides *whether* the
 * app has it (the verdict belongs next to the window identity); this only says
 * what to compare against.
 */
function requiredCapability(method) {
  const capability = registry.METHODS[method];
  if (typeof capability !== 'string') throw new Error(`unknown method: ${method}`);
  // Ask the registry whether the capability alone would pass, which covers both
  // "does any app implement this" and the implemented-method filter, instead of
  // reaching into its internals.
  const verdict = registry.authorize({ capabilities: [capability] }, method);
  return { method, capability, implemented: verdict.ok === true };
}

// One entry per method the bundled apps call. Everything here is a host
// responsibility: none of it belongs in the device backend.
const METHODS = {
  // Registry lookups the native host needs before it can create a window; this is
  // discovery, not a security decision.
  'apps.resolve': (args) => {
    const resolved = registry.resolve(String((args && args.id) || ''));
    if (!resolved.ok) throw new Error(resolved.error);
    const app = resolved.app;
    return {
      id: app.id,
      name: app.name,
      entry: app.entry,
      directory: app.directory,
      capabilities: app.capabilities,
      window: app.window,
      apiVersion: app.apiVersion,
    };
  },
  'apps.requiredCapability': (args) => requiredCapability(String((args && args.method) || '')),
  'apps.list': (args, context) => appList(context.openApps || args.openApps),
  'clipboard.read': () => clipboard.history(),
  'media.nowPlaying': () => media.nowPlaying(),
  'system.telemetry': () => telemetry.collect(),
  'kdeconnect.devices': () => kdeconnect.devices(),
  'windows.list': (args, context) => windowsList(context),
  'diagnostics.read': (args, context) => diagnosticsRead(context),
};

async function dispatch(request) {
  const method = request && typeof request.method === 'string' ? request.method : '';
  const handler = Object.hasOwn(METHODS, method) ? METHODS[method] : null;
  try {
    if (!handler) {
      return { id: request.id, error: { message: `Unsupported host method: ${method}` } };
    }
    const args = request.params && typeof request.params === 'object' ? request.params : {};
    const context = request.context && typeof request.context === 'object' ? request.context : {};
    const result = await handler(args, context);
    return { id: request.id, result: result === undefined ? null : result };
  } catch (error) {
    return { id: request.id, error: { message: String((error && error.message) || error) } };
  }
}

function serve(socketPath) {
  const directory = path.dirname(socketPath);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  try { fs.unlinkSync(socketPath); } catch { /* not there */ }
  const server = net.createServer((socket) => {
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('error', () => {});
    socket.on('data', (chunk) => {
      buffer += chunk;
      if (buffer.length > MAX_MESSAGE) { socket.destroy(); return; }
      let index;
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (!line) continue;
        let request;
        try {
          request = JSON.parse(line);
        } catch {
          socket.write(`${JSON.stringify({ id: null, error: { message: 'Invalid JSON request' } })}\n`);
          continue;
        }
        dispatch(request).then((reply) => {
          if (!socket.destroyed) socket.write(`${JSON.stringify(reply)}\n`);
        });
      }
    });
  });
  server.listen(socketPath, () => {
    fs.chmodSync(socketPath, 0o600);
    // The host waits for this line before routing app methods here.
    console.error(`host bridge ready: ${socketPath}`);
  });
  return server;
}

if (require.main === module) {
  const runtime = process.env.XDG_RUNTIME_DIR || path.join('/run/user', String(process.getuid()));
  const socketPath = process.argv[2] || path.join(runtime, 'edgeline-host.sock');
  serve(socketPath);
}

module.exports = {
  dispatch, appList, windowsList, diagnosticsRead, requiredCapability, METHODS, serve,
};
