// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');
const readline = require('readline');
let child = null;
let pending = null;
let connection = null;
function open() {
  if (connection && child && child.exitCode === null) return Promise.resolve(connection);
  if (pending) return pending;
  pending = new Promise((resolve, reject) => {
    const directory = process.env.EDGELINE_TABLET_DECK_DIR || path.join(os.homedir(), 'Dokumente', 'tablet-deck');
    if (!fs.existsSync(path.join(directory, 'layouts', 'edge.json'))) {
      reject(new Error('Tablet Deck with the Edge layout is not installed. Set EDGELINE_TABLET_DECK_DIR to its folder.'));
      return;
    }
    // This integration is deliberately dry-run until true Wayland game-focus
    // gating is implemented. Never enable the weaker process-running gate.
    const instance = spawn(process.execPath,
      [path.join(directory, 'server.js'), '--dry-run', '--loopback', '--port', '8790', '--ui'],
      { cwd:directory, env:{...process.env, ELECTRON_RUN_AS_NODE:'1', TABLET_DECK_KEYS:'none'}, stdio:['pipe','pipe','pipe'] });
    child = instance;
    let settled = false;
    const fail = (message) => { if (!settled) { settled = true; clearTimeout(timer); reject(new Error(message)); } };
    const timer = setTimeout(() => { fail('Tablet Deck did not start within 10 seconds'); instance.kill(); }, 10000);
    instance.on('error', () => fail('Could not start the local Tablet Deck server'));
    instance.on('exit', () => {
      if (child === instance) { child = null; connection = null; }
      fail('Tablet Deck stopped before it was ready (port 8790 may already be in use)');
    });
    // Never forward server output: it contains the private pairing code.
    instance.stderr.resume();
    readline.createInterface({input:instance.stdout}).on('line', (line) => {
      if (!line.startsWith('@ui ')) return;
      try {
        const event = JSON.parse(line.slice(4));
        if (!settled && event.t === 'ready' && event.dryRun === true && event.port === 8790) {
          settled = true; clearTimeout(timer);
          connection = {url:`http://127.0.0.1:8790/?device=edge&t=${encodeURIComponent(event.token)}`, dryRun:true};
          resolve(connection);
        }
      } catch { /* ignore non-protocol console messages */ }
    });
  }).finally(() => { pending = null; });
  return pending;
}
function stop() {
  if (child && child.stdin.writable) child.stdin.end('quit\n');
  connection = null;
}
module.exports = {open, stop};
