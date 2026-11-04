// SPDX-License-Identifier: GPL-3.0-or-later
// Turns the core's diagnostics payload into rows a human can scan.
// Pure: no DOM, no host calls, so the checks themselves are testable.
'use strict';

const STATUS_ORDER = { bad: 0, warn: 1, ok: 2, info: 3 };

function row(label, actual, expected, status) {
  return { label, actual: actual === null || actual === undefined ? 'unknown' : String(actual),
    expected: expected === null || expected === undefined ? '' : String(expected), status };
}
function rectText(rect) {
  return rect ? `${rect.width}×${rect.height} at ${rect.x},${rect.y}` : 'missing';
}
function check(label, actual, expected, ok, options = {}) {
  const status = ok ? 'ok' : (options.warnOnly ? 'warn' : 'bad');
  return row(label, actual, expected, status);
}

function edgeRows(diag) {
  const rail = Number.isInteger(diag.railWidth) ? diag.railWidth : 96;
  if (!diag.edge) {
    return [row('Edge display', 'not found', 'one 2560×720 output', 'bad'),
      row('Displays seen', String(diag.displays.length), 'at least one', 'info')];
  }
  const g = diag.edge.geometry;
  const work = diag.edge.workarea;
  const offset = work.x - g.x;
  const rows = [
    check('Output', `${diag.edge.name || diag.edge.id} ${rectText(g)}`, 'the physical Edge', true),
    check('Reserved strip', `${offset} px at the left edge`, `${rail} px`,
      offset === rail, { warnOnly: true }),
    check('Usable area origin', `${work.x},${work.y}`, `${g.x + rail},${g.y}`, work.x === g.x + rail),
    check('Usable area size', `${work.width}×${work.height}`, `${g.width - rail}×${g.height}`,
      work.width === g.width - rail && work.height === g.height),
    row('Work area source', diag.edge.workareaSource, 'compositor', diag.edge.workareaSource === 'compositor' ? 'ok' : 'warn'),
  ];
  if (offset === 0) rows[1] = row('Reserved strip', '0 px, nothing reserved', `${rail} px`, 'warn');
  return rows;
}

function railRows(diag) {
  const rail = Number.isInteger(diag.railWidth) ? diag.railWidth : 96;
  const bars = (diag.panel || []).filter((entry) => entry.title === 'EdgeLine taskbar');
  if (!bars.length) return [row('Sidebar window', 'not found', 'EdgeLine taskbar', 'bad')];
  const bar = bars[0];
  if (!diag.edge) return [row('Sidebar window', rectText(bar), 'Edge display missing', 'warn')];
  const g = diag.edge.geometry;
  return [
    check('Sidebar position', `${bar.x},${bar.y}`, `${g.x},${g.y}`, bar.x === g.x && bar.y === g.y),
    check('Sidebar size', `${bar.width}×${bar.height}`, `${rail}×${g.height}`, bar.width === rail && bar.height === g.height),
    check('Sidebar stacking', bar.keepAbove ? 'keep above' : 'normal', 'keep above', bar.keepAbove === true, { warnOnly: true }),
    check('Sidebar visible', bar.minimized ? 'minimized' : 'shown', 'shown', bar.minimized !== true, { warnOnly: true }),
  ];
}

function controllerRows(diag) {
  const controller = diag.controller || {};
  const recorded = Array.isArray(controller.recorded) ? controller.recorded : [];
  return [
    check('KWin controller', controller.loaded ? 'loaded' : 'not loaded', 'one loaded instance', controller.loaded === true),
    check('Recorded controllers', recorded.length ? recorded.join(', ') : 'none', 'exactly one',
      recorded.length === 1, { warnOnly: true }),
    check('Stale controllers', String(controller.stale || 0), '0', (controller.stale || 0) === 0, { warnOnly: true }),
  ];
}

function serviceRows(diag) {
  const reservation = diag.reservation || {};
  const agent = diag.agent || {};
  const compositor = diag.compositor || {};
  return [
    check('Reservation helper', reservation.running ? `running, pid ${reservation.pid}` : 'not running',
      'running', reservation.running === true, { warnOnly: true }),
    check('Helper restarts', String(reservation.failures || 0), '0', (reservation.failures || 0) === 0, { warnOnly: true }),
    check('Hardware agent', agent.connected ? 'connected' : 'not connected', 'connected', agent.connected === true),
    check('Compositor feed', compositor.available ? `${compositor.windows} windows on the Edge` : (compositor.error || 'unavailable'),
      'available', compositor.available === true, { warnOnly: true }),
    check('Apps loaded', String((diag.host || {}).safeMode ? 'safe mode' : 'normal'), 'normal', !(diag.host || {}).safeMode,
      { warnOnly: true }),
  ];
}

function hostRows(diag) {
  const host = diag.host || {};
  return [
    row('Host', `${host.runtime || 'unknown'} ${host.version || ''}`.trim(), '', 'info'),
    row('Process', host.pid ? `pid ${host.pid}` : 'unknown', '', 'info'),
    row('Session', `${host.session || 'unknown'} / ${host.desktop || 'unknown'}`, '', 'info'),
    row('Backend', host.backend || 'unknown', '', 'info'),
    row('Rendering', host.rendering || 'default', '', 'info'),
    row('Report generated', diag.generatedAt || '', '', 'info'),
  ];
}

function otherDisplayRows(diag) {
  const others = (diag.displays || []).filter((display) => !display.edge);
  if (!others.length) return [row('Other outputs', 'none', 'left untouched', 'info')];
  return others.map((display) => {
    const offset = display.workarea.x - display.geometry.x;
    return row(`${display.name || display.label || display.id}`,
      `${rectText(display.geometry)} workarea offset ${offset}`,
      'not touched by EdgeLine', 'info');
  });
}

function buildReport(diag) {
  const sections = [
    { title: 'Edge output', rows: edgeRows(diag) },
    { title: 'Sidebar', rows: railRows(diag) },
    { title: 'KWin controller', rows: controllerRows(diag) },
    { title: 'Services', rows: serviceRows(diag) },
    { title: 'Host', rows: hostRows(diag) },
    { title: 'Other outputs', rows: otherDisplayRows(diag) },
  ];
  const counts = { ok: 0, warn: 0, bad: 0, info: 0 };
  for (const section of sections) for (const entry of section.rows) counts[entry.status] += 1;
  const verdict = counts.bad ? 'bad' : counts.warn ? 'warn' : 'ok';
  return { verdict, counts, sections, generatedAt: diag.generatedAt || '' };
}

// Node (tests) and the sandboxed renderer (no module) share this one file.
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { buildReport, edgeRows, railRows, controllerRows, serviceRows, STATUS_ORDER };
}
