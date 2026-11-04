// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const {EventEmitter}=require('node:events');
const source=fs.readFileSync(path.join(__dirname,'../workarea.js'),'utf8');
function fixture(session='wayland'){
  const children=[],timers=[],messages=[];
  const module={exports:{}};
  vm.runInNewContext(source,{module,__dirname:path.join(__dirname,'..'),
    process:{pid:1234,env:{XDG_SESSION_TYPE:session}},
    console:{error:(...args)=>messages.push(args.join(' '))},
    setTimeout:callback=>{const timer={callback,unref(){}};timers.push(timer);return timer;},
    clearTimeout:timer=>{timer.cancelled=true;},
    require:name=>{
      if(name==='fs')return {constants:{X_OK:1},accessSync(){}};
      if(name==='path')return path;
      if(name==='child_process')return {spawn:(file,args,options)=>{
        const child=new EventEmitter();child.stderr=new EventEmitter();child.kills=[];
        child.kill=signal=>child.kills.push(signal);children.push({file,args,options,child});return child;
      }};
      throw new Error(name);
    },
  });
  return {api:module.exports,children,timers,messages};
}
const live=fixture();live.api.start();live.api.start();assert.equal(live.children.length,1);
assert.deepEqual(Array.from(live.children[0].args),['--parent-pid','1234','--wait']);
assert(live.children[0].file.endsWith('edgeline-reserve'));
live.api.stop();assert.deepEqual(live.children[0].child.kills,['SIGTERM']);
live.children[0].child.emit('close',0);assert.equal(live.timers.length,0);
const other=fixture('x11');other.api.start();assert.equal(other.children.length,0);
const retry=fixture();retry.api.start();
for(let i=0;i<3;i++){retry.children[i].child.emit('close',1);retry.timers[i].callback();}
assert.equal(retry.children.length,4);retry.children[3].child.emit('close',1);assert.equal(retry.timers.length,3);
console.log('workarea lifecycle: one owner per host, parent binding, wait/handover, stop and bounded retries pass');
