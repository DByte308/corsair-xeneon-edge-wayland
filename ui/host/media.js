// SPDX-License-Identifier: GPL-3.0-or-later
// MPRIS playback for apps. Parsing is pure; the bus calls live in one place so
// the payloads can be tested from fixtures.
'use strict';
const { execFile } = require('child_process');
const fs = require('fs');
const { promisify } = require('util');

const run = promisify(execFile);
const PREFIX = 'org.mpris.MediaPlayer2.';
const MAX_ART = 4 * 1024 * 1024;

function parsePlayers(stdout) {
  return String(stdout || '').split('\n')
    .map((line) => line.trim().split(/\s+/)[0])
    .filter((name) => name && name.startsWith(PREFIX) && /^[\w.-]+$/.test(name))
    .slice(0, 16);
}

// busctl --json=short get-property returns {type, data} where data is the
// variant payload. Property lists arrive as {type:'a{sv}', data:{key:{type,data}}}.
function unwrap(raw) {
  if (!raw || typeof raw !== 'object') return undefined;
  return Object.hasOwn(raw, 'data') ? raw.data : raw;
}
const EMPTY_TRACK = { title: '', artist: '', album: '', artUrl: '', length: 0 };
function parseMetadata(raw) {
  const data = unwrap(raw);
  if (!data || typeof data !== 'object') return { ...EMPTY_TRACK };
  const get = (key) => unwrap(data[key]);
  const artists = Array.isArray(get('xesam:artist')) ? get('xesam:artist') : [];
  const length = Number(get('mpris:length'));
  return {
    title: String(get('xesam:title') || '').slice(0, 200),
    artist: String(artists[0] || '').slice(0, 120),
    album: String(get('xesam:album') || '').slice(0, 120),
    artUrl: String(get('mpris:artUrl') || '').slice(0, 400),
    length: Number.isFinite(length) && length > 0 ? Math.round(length / 1000) : 0,
  };
}
function artDataUrl(artUrl) {
  try {
    if (!/^file:\/\//.test(artUrl)) return '';
    const path = decodeURIComponent(artUrl.replace('file://', ''));
    const stat = fs.statSync(path);
    if (!stat.isFile() || stat.size > MAX_ART) return '';
    const head = fs.readFileSync(path).subarray(0, 4);
    const mime = head[0] === 0xff && head[1] === 0xd8 ? 'image/jpeg'
      : head.subarray(1, 4).toString() === 'PNG' ? 'image/png'
        : head.subarray(0, 3).toString() === 'GIF' ? 'image/gif' : '';
    if (!mime) return '';
    return `data:${mime};base64,${fs.readFileSync(path).toString('base64')}`;
  } catch { return ''; }
}
async function property(player, iface, name) {
  const { stdout } = await run('busctl', ['--user', '--json=short', 'get-property', player,
    '/org/mpris/MediaPlayer2', iface, name], { timeout: 2500 });
  return JSON.parse(stdout);
}
async function call(player, method) {
  await run('busctl', ['--user', 'call', player, '/org/mpris/MediaPlayer2',
    'org.mpris.MediaPlayer2.Player', method], { timeout: 2500 });
}
async function nowPlaying() {
  let players = [];
  try {
    const { stdout } = await run('busctl', ['--user', 'list', '--no-legend'], { timeout: 2500 });
    players = parsePlayers(stdout);
  } catch { return { available: false, players: [], error: 'MPRIS unavailable' }; }
  if (!players.length) return { available: false, players: [], error: 'No media player running' };
  const entries = [];
  for (const player of players) {
    try {
      const [metadata, status, position] = await Promise.all([
        property(player, 'org.mpris.MediaPlayer2.Player', 'Metadata'),
        property(player, 'org.mpris.MediaPlayer2.Player', 'PlaybackStatus'),
        property(player, 'org.mpris.MediaPlayer2.Player', 'Position').catch(() => null),
      ]);
      const info = parseMetadata(metadata);
      if (!info.title) continue;
      const micros = Number(unwrap(position));
      entries.push({ player, status: String(unwrap(status) || 'Stopped'), position: Number.isFinite(micros) ? Math.round(micros / 1e6) : 0, ...info });
    } catch { /* a player can vanish between list and read */ }
  }
  if (!entries.length) return { available: false, players: [], error: 'No track playing' };
  entries.sort((a, b) => (a.status === 'Playing' ? 0 : 1) - (b.status === 'Playing' ? 0 : 1));
  const active = entries[0];
  return {
    available: true, players: entries.map((entry) => entry.player), error: '',
    player: active.player, status: active.status, title: active.title, artist: active.artist,
    album: active.album, length: active.length, position: active.position, art: artDataUrl(active.artUrl),
  };
}
const COMMANDS = { playpause: 'PlayPause', play: 'Play', pause: 'Pause', next: 'Next', previous: 'Previous' };
function commandName(action) {
  return Object.hasOwn(COMMANDS, action) ? COMMANDS[action] : null;
}
async function command(action) {
  const method = commandName(action);
  if (!method) throw new Error(`Unknown playback command ${String(action).slice(0, 20)}`);
  const state = await nowPlaying();
  if (!state.available) throw new Error(state.error || 'No media player running');
  await call(state.player, method);
  return { ok: true, player: state.player, action };
}

module.exports = { parsePlayers, parseMetadata, artDataUrl, commandName, nowPlaying, command, unwrap };
