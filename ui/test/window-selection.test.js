// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const {selectedWindow} = require('../window-selection');
const id = '{12345678-abcd-1234-1234-123456789abc}';
const entry = {uuid:{type:'s',data:id},caption:{type:'s',data:'Test app'},resourceClass:{type:'s',data:'test'}};
const reply = {type:'a{sv}',data:[entry]};
assert.equal(selectedWindow(JSON.stringify(reply)).id,id);
assert.equal(selectedWindow(reply).suffix,'12345678abcd12341234123456789abc');
assert.equal(selectedWindow({type:'a{sv}',data:[{...entry,uuid:{type:'s',data:[id]}}]}).id,id);
assert.throws(()=>selectedWindow({type:'a{sv}',data:[{}]}));
assert.throws(()=>selectedWindow({type:'s',data:['bad']}));
assert.throws(()=>selectedWindow({type:'a{sv}',data:[{...entry,uuid:{data:'; arbitrary-code'}}]}));
assert.throws(()=>selectedWindow({type:'a{sv}',data:[{...entry,caption:{data:'EdgeLine dashboard'}}]}));
assert.throws(()=>selectedWindow({type:'a{sv}',data:[{...entry,resourceClass:{data:'edgeline'}}]}));
console.log('window-selection: typed D-Bus reply parsing, UUID validation and own-window rejection pass');
