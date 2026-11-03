// SPDX-License-Identifier: GPL-3.0-or-later
// Migration-only transport. The web UI and preload API do not change.
'use strict';
const fs = require('fs');
const path = require('path');
const net = require('net');
const readline = require('readline');
const {spawn} = require('child_process');
class RustBackend {
  constructor() { this.child=null; this.socket=null; this.starting=null; this.pending=new Map(); this.next=1; }
  executable() {
    const candidates=[process.env.EDGELINE_RUST_BACKEND,
      path.join(process.resourcesPath || '', 'backend', 'edgeline-backend'),
      path.join(__dirname,'..','rust-backend','target','release','edgeline-backend'),
      path.join(__dirname,'..','rust-backend','target','debug','edgeline-backend')].filter(Boolean);
    for(const candidate of candidates) {try {fs.accessSync(candidate,fs.constants.X_OK);return candidate;}catch{}}
    throw new Error('Rust backend binary is missing; build rust-backend with cargo build --release');
  }
  start() {
    if(this.socket&&!this.socket.destroyed)return Promise.resolve();
    if(this.starting)return this.starting;
    this.starting=new Promise((resolve,reject)=>{
      let settled=false;
      const child=spawn(this.executable(),['--parent-pid',String(process.pid)],{stdio:['ignore','pipe','pipe']});
      this.child=child;
      const fail=error=>{if(!settled){settled=true;clearTimeout(timer);reject(error);}};
      const timer=setTimeout(()=>{fail(new Error('Rust backend did not become ready'));child.kill();},10000);
      child.stderr.on('data',data=>console.error('[rust-backend]',data.toString().trim()));
      child.on('error',fail);
      child.on('exit',()=>{
        if(this.child===child)this.child=null;
        this.socket?.destroy();this.socket=null;
        fail(new Error('Rust backend exited before startup completed'));
        for(const pending of this.pending.values()){clearTimeout(pending.timer);pending.reject(new Error('Rust backend exited'));}
        this.pending.clear();
      });
      readline.createInterface({input:child.stdout}).on('line',line=>{
        let ready;try{ready=JSON.parse(line);}catch{return;}
        if(ready.event!=='ready'||typeof ready.socket!=='string'||settled)return;
        const socket=net.createConnection(ready.socket);
        this.socket=socket;
        socket.setEncoding('utf8');
        let buffer='';
        socket.on('connect',()=>{settled=true;clearTimeout(timer);resolve();});
        socket.on('error',fail);
        socket.on('data',data=>{
          buffer+=data;
          if(buffer.length>8*1024*1024){socket.destroy(new Error('Rust response exceeds 8 MiB'));return;}
          let end;
          while((end=buffer.indexOf('\n'))>=0){
            const line=buffer.slice(0,end);buffer=buffer.slice(end+1);
            let response;try{response=JSON.parse(line);}catch{continue;}
            const pending=this.pending.get(response.id);if(!pending)continue;
            clearTimeout(pending.timer);this.pending.delete(response.id);
            if(response.error)pending.reject(new Error(response.error.message||'Rust backend error'));
            else pending.resolve(response.result);
          }
        });
        socket.on('close',()=>{
          for(const pending of this.pending.values()){clearTimeout(pending.timer);pending.reject(new Error('Rust backend connection closed'));}
          this.pending.clear();
        });
      });
    }).finally(()=>{this.starting=null;});
    return this.starting;
  }
  async call(method,params={}) {
    await this.start();
    const id=this.next++;
    return new Promise((resolve,reject)=>{
      const timeout=method==='launchpad.action'&&params.action==='pick'?130000:17000;
      const timer=setTimeout(()=>{this.pending.delete(id);reject(new Error('Rust request timed out'));},timeout);
      this.pending.set(id,{resolve,reject,timer});
      this.socket.write(JSON.stringify({id,method,params})+'\n',error=>{
        if(error){clearTimeout(timer);this.pending.delete(id);reject(error);}
      });
    });
  }
  async stop() {
    const child=this.child;if(!child)return;
    const deadline=setTimeout(()=>child.kill('SIGTERM'),3000);deadline.unref();
    child.once('exit',()=>clearTimeout(deadline));
    try{await this.call('backend.stop');}catch{child.kill('SIGTERM');}
  }
}
module.exports={RustBackend,backend:new RustBackend()};
