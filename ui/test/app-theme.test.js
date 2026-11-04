// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const theme = fs.readFileSync(path.join(__dirname, '..', 'apps', 'app-theme.css'), 'utf8');

// The sky is drawn, not downloaded: no remote assets, no fonts, no images.
assert.equal(/https?:\/\//.test(theme), false, 'the theme must not reference a remote URL');
assert.equal(/url\(/.test(theme), false, 'the theme must not load an asset at all');
assert.match(theme, /body::before/, 'a star field layer exists');
assert.match(theme, /body::after/, 'a haze layer exists');
assert.match(theme, /pointer-events: none/, 'sky layers never eat touches');
assert.match(theme, /body > \*/, 'content is lifted above the sky');
assert.match(theme, /prefers-reduced-motion/, 'the drift respects reduced motion');
assert.ok(theme.includes('z-index: 0'), 'sky sits behind content');

// Every app window gets it, and the injection is wired in one place.
const main = fs.readFileSync(path.join(__dirname, '..', 'main.js'), 'utf8');
assert.match(main, /app-theme\.css/, 'the host reads the theme file');
assert.match(main, /insertCSS/, 'the host injects it into app windows');
assert.match(main, /applyAppTheme\(window, app\.id\);/, 'every opened app window goes through it');
// insertCSS before the first frame exists fails, so it must run after load.
const loadIndex = main.indexOf("on('did-finish-load'");
const themeIndex = main.indexOf('applyAppTheme(window, app.id)');
assert.ok(loadIndex > 0 && themeIndex > loadIndex, 'the theme is injected after the document loads, not at window creation');
assert.match(main, /render-process-gone/, 'a crashed app renderer is logged and closed');
assert.match(main, /LOG_FILE/, 'crash-relevant logs also go to a file, because main-process stdout stops reaching journald');
assert.match(main, /uncaughtException/, 'an uncaught error is logged instead of vanishing');
console.log('app-theme: no remote assets, sky layers, content lift, reduced motion and host wiring pass');
