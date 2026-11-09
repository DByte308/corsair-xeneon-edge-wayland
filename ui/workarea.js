// SPDX-License-Identifier: GPL-3.0-or-later
// Lifecycle adapter only. The actual exclusive-zone client is native Rust.
'use strict';
const fs=require('fs');
const path=require('path');
const {spawn}=require('child_process');
let child=null;
let timer=null;
let stopping=false;
let failures=0;
let log=(...parts)=>console.error(...parts);
// A login-time panel can shrink the strip for a moment and the helper then
// rebuilds itself on the next configure. Should it still exit, a short session
// moment must not cost the whole session: back off instead of giving up, and
// keep the retries bounded so a genuinely broken helper is not spawned forever.
const MAX_RESTARTS=8;
const MAX_DELAY_MS=30000;
function setLog(write){ if(typeof write==='function')log=write; }
function executable(){
  const candidates=[process.env.EDGELINE_WORKAREA_HELPER,
    path.join(process.resourcesPath||'', 'backend','edgeline-reserve'),
    path.join(__dirname,'..','rust-backend','target','release','edgeline-reserve')].filter(Boolean);
  for(const file of candidates){try{fs.accessSync(file,fs.constants.X_OK);return file;}catch{}}
  throw new Error('Build the Rust backend to enable the reserved Edge work area');
}
function start(){
  if(child||timer||stopping||process.env.XDG_SESSION_TYPE!=='wayland')return;
  let file;
  try{file=executable();}catch(error){log('[workarea]',error.message);return;}
  const instance=spawn(file,['--parent-pid',String(process.pid),'--wait'],{stdio:['ignore','ignore','pipe']});
  child=instance;
  instance.stderr?.on('data',data=>{const text=data.toString().trim();if(text)log('[workarea]',text);});
  instance.on('error',error=>log('[workarea]',error.message));
  instance.on('close',(code)=>{
    if(child===instance)child=null;
    if(stopping)return;
    failures+=1;
    if(failures>MAX_RESTARTS){
      log(`[workarea] reservation helper stopped after ${MAX_RESTARTS} restarts; restart EdgeLine to retry`);
      return;
    }
    const delay=Math.min(MAX_DELAY_MS,1000*2**(failures-1));
    log(`[workarea] reservation helper exited (${code}); retry ${failures}/${MAX_RESTARTS} in ${delay}ms`);
    timer=setTimeout(()=>{timer=null;start();},delay);
    timer.unref();
  });
}
function stop(){stopping=true;if(timer)clearTimeout(timer);timer=null;if(child)child.kill('SIGTERM');}
function status(){return {running:!!child && !child.killed,pid:child?child.pid:null,failures};}
module.exports={start,stop,status,setLog};
