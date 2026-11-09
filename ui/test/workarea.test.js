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
    setTimeout:(callback,delay)=>{const timer={callback,delay,unref(){}};timers.push(timer);return timer;},
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

// The helper's own diagnostics used to reach console.error only, which is lost
// once Electron is up. They have to come out through the injected log.
const logged=fixture();logged.api.setLog((...parts)=>logged.messages.push(parts.join(' ')));logged.api.start();
logged.children[0].child.stderr.emit('data','Error: Compositor assigned unexpected reservation size 96x690\n');
assert.match(logged.messages.join('\n'),/unexpected reservation size 96x690/,'helper stderr is logged');
logged.children[0].child.emit('close',1);
assert.match(logged.messages.join('\n'),/retry 1\/8 in 1000ms/,'the retry says why and when');

// A login-time panel can kill the helper while the session is still settling,
// so the restarts back off instead of burning three attempts in six seconds.
const retry=fixture();retry.api.start();
for(let i=0;i<8;i++){retry.children[i].child.emit('close',1);retry.timers[i].callback();}
assert.equal(retry.children.length,9,'one start plus eight restarts');
assert.equal(retry.timers.length,8);
assert.deepEqual(retry.timers.map(t=>t.delay),[1000,2000,4000,8000,16000,30000,30000,30000],'delays grow and stop at 30s');
retry.children[8].child.emit('close',1);
assert.equal(retry.timers.length,8,'retries are bounded');
assert.match(retry.messages.join('\n'),/restart EdgeLine to retry/,'and say how to recover');
console.log('workarea lifecycle: one owner per host, parent binding, wait/handover, logged backoff and bounded retries pass');
