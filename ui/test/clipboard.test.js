// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const clipboard = require('../host/clipboard.js');

// busctl prints a string array on one line: "as 3 a b "with space" "quote \" inside"".
assert.deepEqual(clipboard.parseStringArray('as 3 alpha beta "with space"'),
  ['alpha', 'beta', 'with space']);
assert.deepEqual(clipboard.parseStringArray('as 1 "line\\nbreak"'), ['line\nbreak']);
assert.deepEqual(clipboard.parseStringArray('as 2 "say \\"hi\\"" plain'), ['say "hi"', 'plain']);
assert.deepEqual(clipboard.parseStringArray('as 0'), []);
assert.deepEqual(clipboard.parseStringArray(''), []);
assert.deepEqual(clipboard.parseStringArray(null), []);

// Sanitising: empties dropped, duplicates collapsed, long text bounded.
const items = clipboard.sanitize([
  'one', 'one', '', '   ', 'two\nlines', 'x'.repeat(1000),
]);
assert.equal(items.length, 3, 'duplicates and blanks are dropped');
assert.equal(items[0].text, 'one');
assert.equal(items[1].lines, 2);
assert.equal(items[2].truncated, true);
assert.equal(items[2].text.length, clipboard.MAX_TEXT);
assert.equal(clipboard.sanitize([]).length, 0);
assert.equal(clipboard.sanitize(['\u0000\u0000']).length, 0, 'null bytes do not become entries');

// A history larger than the cap is bounded, not passed through.
const many = Array.from({ length: 100 }, (_, i) => `entry ${i}`);
assert.equal(clipboard.sanitize(many).length, clipboard.MAX_ITEMS);
assert.equal(clipboard.sanitize(many)[0].text, 'entry 0');

(async () => {
  await assert.rejects(clipboard.copy(''), /Nothing to copy/);
  await assert.rejects(clipboard.copy(null), /Nothing to copy/);
  const state = await clipboard.history();
  assert.equal(typeof state.available, 'boolean');
  assert.ok(Array.isArray(state.items));
  console.log('clipboard: klipper array parsing, sanitising, caps and empty-copy refusal pass');
})();
