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
  try{file=executable();}catch(error){console.error('[workarea]',error.message);return;}
  const instance=spawn(file,['--parent-pid',String(process.pid),'--wait'],{stdio:['ignore','ignore','pipe']});
  child=instance;
  instance.stderr?.on('data',data=>console.error('[workarea]',data.toString().trim()));
  instance.on('error',error=>console.error('[workarea]',error.message));
  instance.on('close',()=>{
    if(child===instance)child=null;
    if(stopping)return;
    if(++failures<=3){timer=setTimeout(()=>{timer=null;start();},failures*1000);timer.unref();}
    else console.error('[workarea] reservation helper stopped; restart EdgeLine to retry');
  });
}
function stop(){stopping=true;if(timer)clearTimeout(timer);timer=null;if(child)child.kill('SIGTERM');}
function status(){return {running:!!child && !child.killed,pid:child?child.pid:null,failures};}
module.exports={start,stop,status};
