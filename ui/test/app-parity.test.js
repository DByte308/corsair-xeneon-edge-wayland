// SPDX-License-Identifier: GPL-3.0-or-later
// The "is this method usable" answer lives in three places by design:
// the registry (what may be granted), the host dispatch table (what runs) and
// the contract doc (what a reader is told). This test keeps them agreeing.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const registry = require('../apps/registry.js');

const root = path.join(__dirname, '..');
const registrySource = fs.readFileSync(path.join(root, 'apps', 'registry.js'), 'utf8');
const mainSource = fs.readFileSync(path.join(root, 'main.js'), 'utf8');
const docSource = fs.readFileSync(path.join(root, '..', 'docs', 'APPS.md'), 'utf8');

function block(source, pattern, label) {
  const match = pattern.exec(source);
  assert.ok(match, `${label} not found in the sources`);
  return match[1];
}
const implemented = new Set([...block(registrySource, /const IMPLEMENTED = new Set\(\[([\s\S]*?)\]\);/g, 'IMPLEMENTED')
  .matchAll(/'([a-z]+\.[A-Za-z]+)'/g)].map((m) => m[1]));
const methods = new Set([...block(registrySource, /const METHODS = \{([\s\S]*?)\n\};/g, 'METHODS')
  .matchAll(/'([a-z]+\.[A-Za-z]+)':/g)].map((m) => m[1]));
const handlers = new Set([...block(mainSource, /const APP_METHODS = \{([\s\S]*?)\n\};/g, 'APP_METHODS')
  .matchAll(/'([a-z]+\.[A-Za-z]+)':/g)].map((m) => m[1]));

// Every implemented method actually has a handler, and no handler is reachable
// without being declared implemented.
assert.deepEqual([...implemented].filter((m) => !handlers.has(m)), [],
  'every implemented method needs an entry in APP_METHODS');
assert.deepEqual([...handlers].filter((m) => !implemented.has(m)), [],
  'a handler without IMPLEMENTED would be refused before it can run');
// Implemented methods are always declared methods; reserved ones are not.
assert.deepEqual([...implemented].filter((m) => !methods.has(m)), [], 'IMPLEMENTED must name declared methods');
assert.deepEqual([...handlers].filter((m) => !methods.has(m)), [], 'a handler must name a declared method');

// authorize() must agree with the table for both a granted and an ungranted app.
const app = { capabilities: [...registry.CAPABILITIES] };
for (const method of methods) {
  const verdict = registry.authorize(app, method);
  if (implemented.has(method)) assert.equal(verdict.ok, true, `${method} should be callable when granted`);
  else assert.match(verdict.error, /not implemented/, `${method} should be refused as not implemented`);
}
assert.equal(registry.authorize({ capabilities: [] }, 'diagnostics.read').ok, false);

// The capability table in the docs matches the code, row by row.
const rows = [...docSource.matchAll(/^\| `([a-z:-]+)` \| (.*?) \| (implemented|reserved) \|$/gm)];
assert.ok(rows.length >= 8, 'the capability table is still in the contract doc');
for (const [, capability, methodList, status] of rows) {
  assert.ok(registry.CAPABILITIES.has(capability), `${capability} in docs but not in the registry`);
  const named = [...methodList.matchAll(/`([a-z]+\.[A-Za-z]+)`/g)].map((m) => m[1]);
  assert.ok(named.length > 0, `${capability} lists no methods`);
  for (const method of named) {
    assert.ok(methods.has(method), `${method} in docs but not in the registry`);
    assert.equal(implemented.has(method), status === 'implemented',
      `${method} is documented as ${status} but the code says otherwise`);
  }
}
for (const capability of registry.CAPABILITIES) {
  assert.ok(rows.some((row) => row[1] === capability), `capability ${capability} is missing from the contract doc`);
}

// Every shipped app asks only for capabilities the core can actually answer.
for (const entry of registry.list()) {
  if (!entry.compatible) continue;
  for (const capability of entry.capabilities) {
    assert.ok(registry.CAPABILITIES.has(capability), `${entry.id} asks for unknown ${capability}`);
    const owned = [...methods].filter((method) => registry.METHODS[method] === capability);
    assert.ok(owned.some((method) => implemented.has(method)),
      `${entry.id} asks for ${capability} but nothing implements it`);
  }
}
console.log('app-parity: registry, dispatch table and contract doc agree on every method');
