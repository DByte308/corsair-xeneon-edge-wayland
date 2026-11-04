// SPDX-License-Identifier: GPL-3.0-or-later
// What the core knows about the desktop, assembled for the Edge Doctor app.
// Deliberately free of Electron imports: the payload is plain JSON so it can be
// tested without a display, and the native host can produce the same shape.
'use strict';

const SCHEMA = 1;
const MAX = 64;

function clampString(value, limit = 200) {
  return typeof value === 'string' ? value.slice(0, limit) : '';
}
function rect(raw) {
  if (!raw || typeof raw !== 'object') return null;
  const pick = (key, min, max) => (Number.isInteger(raw[key]) && raw[key] >= min && raw[key] <= max ? raw[key] : null);
  const x = pick('x', -100000, 100000), y = pick('y', -100000, 100000);
  const width = pick('width', 1, 100000), height = pick('height', 1, 100000);
  return x === null || y === null || width === null || height === null ? null : { x, y, width, height };
}
function normalizeDisplay(raw, edgeId) {
  if (!raw || typeof raw !== 'object') return null;
  // Electron says bounds/workArea, the compositor feed says geometry/workarea.
  const geometry = rect(raw.geometry || raw.bounds || raw);
  if (!geometry) return null;
  const workarea = rect(raw.workarea || raw.workArea) || geometry;
  const scale = typeof raw.scale === 'number' && raw.scale > 0 && raw.scale <= 10 ? Math.round(raw.scale * 100) / 100 : 1;
  return {
    id: clampString(raw.id, 80), name: clampString(raw.name, 64), label: clampString(raw.label, 80),
    geometry, workarea, scale, primary: raw.primary === true, edge: raw.id === edgeId,
  };
}
function normalizePanel(raw) {
  if (!Array.isArray(raw)) return [];
  return raw.slice(0, 16).flatMap((entry) => {
    const frame = rect(entry);
    if (!frame || typeof entry.title !== 'string') return [];
    return [{ title: clampString(entry.title, 100), ...frame, keepAbove: !!entry.keepAbove, minimized: !!entry.minimized }];
  });
}

// input: { generatedAt, host, railWidth, displays[], edgeId, compositor:{screens[],panel[]},
//          overview:{available,count,error}, controller, reservation, agent }
function collect(input = {}) {
  const displays = (Array.isArray(input.displays) ? input.displays : [])
    .slice(0, 16).map((display) => normalizeDisplay(display, input.edgeId)).filter(Boolean);
  const compositor = input.compositor && typeof input.compositor === 'object' ? input.compositor : {};
  const screens = (Array.isArray(compositor.screens) ? compositor.screens : []).slice(0, 16).flatMap((screen) => {
    const geometry = rect(screen && screen.geometry);
    const workarea = rect(screen && screen.workarea);
    if (!geometry || !workarea) return [];
    return [{ name: clampString(screen.name, 64), geometry, workarea }];
  });
  const edgeDisplay = displays.find((display) => display.edge) || null;
  // The compositor reports the work area the desktop actually gives us; Electron
  // can lag behind an exclusive-zone change, so it is only a fallback.
  const match = edgeDisplay && screens.find((screen) => screen.geometry.x === edgeDisplay.geometry.x
    && screen.geometry.y === edgeDisplay.geometry.y
    && screen.geometry.width === edgeDisplay.geometry.width);
  const railWidth = Number.isInteger(input.railWidth) && input.railWidth > 0 && input.railWidth <= 512 ? input.railWidth : 96;
  const host = input.host && typeof input.host === 'object' ? input.host : {};
  const agent = input.agent && typeof input.agent === 'object' ? input.agent : {};
  const reservation = input.reservation && typeof input.reservation === 'object' ? input.reservation : {};
  const controller = input.controller && typeof input.controller === 'object' ? input.controller : {};
  const overview = input.overview && typeof input.overview === 'object' ? input.overview : {};
  return {
    schema: SCHEMA,
    generatedAt: clampString(input.generatedAt || new Date().toISOString(), 40),
    railWidth,
    host: {
      runtime: clampString(host.runtime, 40), version: clampString(host.version, 40),
      pid: Number.isInteger(host.pid) ? host.pid : null,
      session: clampString(host.session, 20), desktop: clampString(host.desktop, 40),
      backend: clampString(host.backend, 20), safeMode: host.safeMode === true,
      rendering: clampString(host.rendering, 60),
    },
    displays,
    edge: edgeDisplay ? {
      id: edgeDisplay.id, label: edgeDisplay.label,
      // Electron has no connector name on Wayland; the compositor feed does.
      name: (match && match.name) || edgeDisplay.name || edgeDisplay.label || edgeDisplay.id,
      geometry: edgeDisplay.geometry,
      workarea: match ? match.workarea : edgeDisplay.workarea,
      workareaSource: match ? 'compositor' : 'electron',
    } : null,
    panel: normalizePanel(compositor.panel),
    compositor: {
      available: screens.length > 0,
      screens: screens.length,
      windows: Number.isInteger(overview.count) ? overview.count : 0,
      error: clampString(overview.error, 200),
    },
    controller: {
      plugin: clampString(controller.plugin, 60),
      loaded: controller.loaded === true,
      recorded: Array.isArray(controller.recorded) ? controller.recorded.map((name) => clampString(name, 60)).slice(0, 16) : [],
      stale: Number.isInteger(controller.stale) ? controller.stale : 0,
    },
    reservation: {
      running: reservation.running === true,
      pid: Number.isInteger(reservation.pid) ? reservation.pid : null,
      failures: Number.isInteger(reservation.failures) ? reservation.failures : 0,
    },
    agent: {
      socket: clampString(agent.socket, 200),
      connected: agent.connected === true,
    },
    limits: { displays: 16, panelWindows: 16, screens: 16, recordedControllers: 16, maxString: MAX },
  };
}

module.exports = { SCHEMA, collect, rect };
