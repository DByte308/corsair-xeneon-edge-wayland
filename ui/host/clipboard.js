// SPDX-License-Identifier: GPL-3.0-or-later
// Clipboard shelf backed by Klipper's session-bus API. Klipper is the
// clipboard owner inside Plasma, so this reads history without inventing a
// second clipboard of our own.
'use strict';
const { execFile } = require('child_process');
const { promisify } = require('util');

const run = promisify(execFile);
const BUS = 'org.kde.klipper';
const PATH = '/klipper';
const IFACE = 'org.kde.klipper.klipper';
const MAX_ITEMS = 24;
const MAX_TEXT = 400;

// busctl prints arrays as "as N item item ..." on one line; items are quoted
// when they contain spaces.
function parseStringArray(stdout) {
  const line = String(stdout || '').trim().split('\n').pop() || '';
  const body = line.replace(/^as\s+\d+\s*/, '');
  const items = [];
  const pattern = /"((?:[^"\\]|\\.)*)"|(\S+)/g;
  let match;
  while ((match = pattern.exec(body))) {
    const raw = match[1] !== undefined ? match[1] : match[2];
    items.push(raw.replace(/\\"/g, '"').replace(/\\n/g, '\n').replace(/\\\\/g, '\\'));
  }
  return items;
}

function sanitize(items) {
  const seen = new Set();
  const out = [];
  for (const item of items) {
    const text = String(item || '').replace(/\u0000/g, '').trim();
    if (!text) continue;
    const key = text.slice(0, MAX_TEXT);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ text: key, truncated: text.length > MAX_TEXT, lines: key.split('\n').length });
    if (out.length >= MAX_ITEMS) break;
  }
  return out;
}

async function history() {
  try {
    const { stdout } = await run('busctl', ['--user', 'call', BUS, PATH, IFACE, 'getClipboardHistoryMenu'], { timeout: 2500 });
    const items = sanitize(parseStringArray(stdout));
    return { available: true, items, error: '' };
  } catch (error) {
    return { available: false, items: [], error: /not found|No such/i.test(error.message) ? 'Klipper is not running' : error.message };
  }
}

async function copy(text) {
  const value = String(text || '').slice(0, 100000);
  if (!value) throw new Error('Nothing to copy');
  await run('busctl', ['--user', 'call', BUS, PATH, IFACE, 'setClipboardContents', 's', value], { timeout: 2500 });
  return { ok: true, length: value.length };
}

module.exports = { parseStringArray, sanitize, history, copy, MAX_ITEMS, MAX_TEXT };
