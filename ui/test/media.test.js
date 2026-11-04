// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const media = require('../host/media.js');

// busctl list output, including the players we must ignore.
const list = [
  'org.freedesktop.DBus 1 system d :1.0 - -',
  'org.mpris.MediaPlayer2.vivaldi.instance2 3699 xdg-dbus-proxy d :1.137 user@1000.service - -',
  'org.mpris.MediaPlayer2.spotify 4001 spotify d :1.150 user@1000.service - -',
  'org.mpris.MediaPlayer2.evil/../x 1 x d :1.9 - -',
  'org.mpris.OtherApp.service 2 x d :1.10 - -',
  '',
].join('\n');
assert.deepEqual(media.parsePlayers(list),
  ['org.mpris.MediaPlayer2.vivaldi.instance2', 'org.mpris.MediaPlayer2.spotify'],
  'only real MPRIS player names survive');
assert.deepEqual(media.parsePlayers(''), []);
assert.deepEqual(media.parsePlayers(null), []);

// A real Metadata property as busctl --json=short prints it.
const metadata = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixtures', 'mpris-metadata.json'), 'utf8'));
const track = media.parseMetadata(metadata);
assert.equal(track.title, '🛡️ Generations Under One Guardian');
assert.equal(track.artist, 'Royalix');
assert.equal(track.length, 59061, "length stays in seconds as MPRIS reports it");
assert.equal(media.parseMetadata({}).title, '');
assert.equal(media.parseMetadata(null).title, '');

// Tags, missing fields and wrong types must not throw or leak through.
const odd = media.parseMetadata({ type: 'a{sv}', data: {
  'xesam:title': { type: 's', data: 'x'.repeat(500) },
  'xesam:artist': { type: 'as', data: [] },
  'mpris:length': { type: 'x', data: -5 },
  'mpris:artUrl': { type: 's', data: 'http://example.invalid/cover.png' },
} });
assert.equal(odd.title.length, 200);
assert.equal(odd.artist, '');
assert.equal(odd.length, 0);
assert.equal(media.artDataUrl(odd.artUrl), '', 'only local artwork is inlined');
assert.equal(media.artDataUrl(''), '');
assert.equal(media.artDataUrl('file:///etc/shadow'), '');
assert.equal(media.artDataUrl('file:///nonexistent.png'), '');

// Real artwork is inlined only when it is a small image.
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'edgeline-media-'));
const png = path.join(temp, 'cover.png');
fs.writeFileSync(png, Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]));
const dataUrl = media.artDataUrl(`file://${png}`);
assert.match(dataUrl, /^data:image\/png;base64,[A-Za-z0-9+/=]+$/);
const jpeg = path.join(temp, 'cover.jpg');
fs.writeFileSync(jpeg, Buffer.from([0xff, 0xd8, 0xff, 0xe0, 1, 2, 3]));
assert.match(media.artDataUrl(`file://${jpeg}`), /^data:image\/jpeg;base64,/);
const text = path.join(temp, 'cover.txt');
fs.writeFileSync(text, 'not an image');
assert.equal(media.artDataUrl(`file://${text}`), '', 'a non-image file is never inlined');

// Commands are an allow list, never free-form method names.
assert.equal(media.commandName('playpause'), 'PlayPause');
assert.equal(media.commandName('next'), 'Next');
assert.equal(media.commandName('previous'), 'Previous');
assert.equal(media.commandName(''), null);
assert.equal(media.commandName('Seek'), null);
assert.equal(media.commandName('__proto__'), null);
assert.equal(media.commandName('constructor'), null);
assert.equal(media.commandName('play; rm -rf /'), null);
assert.equal(media.commandName(undefined), null);

(async () => {
  await assert.rejects(media.command('Seek'), /Unknown playback command/);
  await assert.rejects(media.command('../../bin/sh'), /Unknown playback command/);
  console.log('media: player discovery, metadata shaping, artwork inlining and command allow list pass');
})();
