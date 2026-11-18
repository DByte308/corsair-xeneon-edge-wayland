// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
// The Electron preload that used to be the API surface is gone (B4). The contract it
// stood for is frozen here instead: every method the renderer calls by name. A frozen
// list can fail when the bridge loses one, which a comparison against deleted code
// could no longer do.
const legacy=Object.fromEntries([
  'activateEdgeWindow','appInfo','autostartGet','autostartSet','call','closeCalibration',
  'connected','dashboard','dashboardLayout','dashboardState','deviceLabelGet','deviceLabelSet',
  'edgeDisplay','edgeTaskbarAction','edgeWindows','launchpadAction','onDashboardStatus',
  'onDisplaysChanged','onEvent','onStatus','onSystemTheme','openCalibration','openExternal',
  'openTabletDeck','ripple','windowAction',
].map((name)=>[name,true]));
const calls=[];const events={};
const window={__TAURI__:{core:{invoke:async(command,args)=>{calls.push({command,args});return {ok:true};}},event:{listen:(name,callback)=>{events[name]=callback;}}}};
window.top=window;
const source=fs.readFileSync(path.join(__dirname,'frontend-bridge.js'),'utf8');
vm.runInNewContext(source,{window});
const api=window.edgeline;
// Staging may change the IPC policy, never the visual UI or its application JS.
const renderer=path.join(__dirname,'../ui/renderer');
const staged=path.join(__dirname,'web-dist/renderer');
if(fs.existsSync(staged)){
  for(const entry of fs.readdirSync(renderer,{withFileTypes:true})){
    if(!entry.isFile())continue;
    const original=fs.readFileSync(path.join(renderer,entry.name),'utf8');
    const expected=entry.name.endsWith('.html')?original.replaceAll("default-src 'none';", "default-src 'none'; connect-src ipc: http://ipc.localhost;"):original;
    assert.equal(fs.readFileSync(path.join(staged,entry.name),'utf8'),expected,`${entry.name} must remain visually unchanged`);
  }
}
const capability=JSON.parse(fs.readFileSync(path.join(__dirname,'capabilities/local.json'),'utf8'));
assert.equal(capability.local,true);assert.equal(capability.remote,undefined);
assert(capability.permissions.includes('allow-edgeline'));
assert.deepEqual(Object.keys(api).sort(),Object.keys(legacy).sort(),'Every existing web API must remain available');
assert(Object.isFrozen(api));
(async()=>{
  await api.call('state.all',{});
  assert.equal(calls.at(-1).command,'edgeline');
  assert.equal(calls.at(-1).args.method,'rpc');
  assert.equal(calls.at(-1).args.params.method,'state.all');
  await api.edgeTaskbarAction('home');assert.equal(calls.at(-1).args.method,'edge-taskbar-action');
  await api.launchpadAction('pick');assert.equal(calls.at(-1).args.params.action,'pick');
  await api.dashboardLayout({page:'tablet-deck'});assert.equal(calls.at(-1).args.params.layout.page,'tablet-deck');
  let payload;api.onEvent(event=>{payload=event;});events['agent-event']({payload:{event:'sensors'}});assert.equal(payload.event,'sensors');
  const remote={top:{}};vm.runInNewContext(source,{window:remote});assert.equal(remote.edgeline,undefined,'External frames must never receive host API');
  console.log(`Tauri bridge: ${Object.keys(api).length} existing API methods preserved, payloads/events compatible, remote-frame guard passes`);
})().catch(error=>{console.error(error);process.exitCode=1;});
