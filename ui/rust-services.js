// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const {backend}=require('./rust-backend');
let timer=null;
let polling=false;
let snapshot={available:false,windows:[],error:'Connecting to Rust backend…'};
const launchpad={
  ensureController:()=>backend.call('controller.ensure'),
  action:action=>backend.call('launchpad.action',{action}),
};
async function refresh(){
  if(polling)return;polling=true;
  try{snapshot=await backend.call('window.snapshot');}
  catch(error){snapshot={available:false,windows:[],error:error.message};}
  finally{polling=false;}
}
const windowOverview={
  async start(){await backend.call('window.start');await refresh();if(!timer)timer=setInterval(refresh,700);},
  snapshot:()=>snapshot,
  activate:id=>backend.call('window.activate',{id}),
  stop(){if(timer)clearInterval(timer);timer=null;backend.stop().catch(console.error);},
};
module.exports={launchpad,windowOverview};
