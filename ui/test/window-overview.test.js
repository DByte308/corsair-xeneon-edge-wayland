// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const signal = () => ({connect() {}});
const edge = {name:'DP-1',geometry:{x:467,y:2880,width:2560,height:720}};
const other = {name:'HDMI-A-2',geometry:{x:0,y:0,width:3440,height:1440}};
const areas = new Map([[edge,{x:563,y:2880,width:2464,height:720}],[other,{x:0,y:0,width:3440,height:1394}]]);
const id = '{12345678-1234-1234-1234-123456789abc}';
function window(caption, output, app='test', skipTaskbar=false) {
  return {internalId:id,caption,output,resourceClass:app,normalWindow:true,active:false,minimized:false,skipTaskbar,
    frameGeometry:{x:467,y:2880,width:96,height:720},keepAbove:false,
    frameGeometryChanged:signal(),captionChanged:signal(),minimizedChanged:signal(),outputChanged:signal()};
}
const game = window('VanguardGalaxy',edge,'steam_app_3471800');
const elsewhere = window('Private document',other);
const control = window('EdgeLine',edge,'edgeline');
const dashboard = window('EdgeLine dashboard',edge,'edgeline');
const doctor = window('Edge Doctor',edge,'edgeline',true);
const hiddenHelper = window('Helper panel',edge,'test',true);
const windows = [game,elsewhere,control,dashboard,doctor,hiddenHelper];
let latest;
const workspace = {screens:[edge,other],windowList:()=>windows,windowAdded:signal(),windowRemoved:signal(),
  windowActivated:signal(),screensChanged:signal(),currentDesktop:{},
  clientArea(_option,screen){return areas.get(screen);},raiseWindow(w){this.raised=w;}};
const source=fs.readFileSync(path.join(__dirname,'../kwin-window-overview.js'),'utf8').replace('__EDGELINE_BUS__','"dev.test"');
function report() {
  vm.runInNewContext(source,{workspace,KWin:{MaximizeArea:1},callDBus(...args){latest=JSON.parse(args.at(-1));}});
}
report();
assert.deepEqual(latest.windows.map((w)=>[w.app,w.title,w.own]).sort(),
  [['edgeline','Edge Doctor',true],['steam_app_3471800','VanguardGalaxy',false]],
  'Edge apps include EdgeLine app windows but never its chrome, and skip-taskbar apps stay hidden');
assert.equal(latest.available,true);
assert.deepEqual(latest.screens.map((s)=>[s.name,s.workarea.x,s.workarea.width]),[['DP-1',563,2464],['HDMI-A-2',0,3440]],
  'the feed reports compositor work areas, not only app windows');
assert.deepEqual(latest.panel.map((p)=>[p.title,p.x,p.width,p.keepAbove]),[['EdgeLine',467,96,false],['EdgeLine dashboard',467,96,false]],
  'panel windows are reported so a displaced rail is visible');
control.frameGeometry={x:563,y:2880,width:96,height:720};control.keepAbove=true;report();
assert.equal(latest.panel[0].x,563,'a displaced rail is reported as it really is');
const gameEntry = () => latest.windows.find((w) => w.app === 'steam_app_3471800');
game.minimized=true; game.active=true;
report();assert.equal(gameEntry().minimized,true);assert.equal(gameEntry().active,true);
workspace.screens=[edge,edge];report();assert.equal(latest.available,false);assert.equal(latest.windows.length,0);
workspace.screens=[other];report();assert.equal(latest.windows.length,0);
workspace.screens=[edge,other];
const activate=fs.readFileSync(path.join(__dirname,'../kwin-activate-window.js'),'utf8').replace('__EDGELINE_WINDOW_ID__',JSON.stringify(id));
control.keepAbove=true;dashboard.keepAbove=true;
vm.runInNewContext(activate,{workspace});
assert.equal(workspace.activeWindow,game);assert.equal(workspace.raised,game);
assert.equal(game.minimized,false);assert.equal(control.keepAbove,false);assert.equal(dashboard.keepAbove,false);
workspace.activeWindow=null;game.output=other;
vm.runInNewContext(activate,{workspace});assert.equal(workspace.activeWindow,null);
console.log('window-overview: Edge-only filtering, EdgeLine apps in the rail, chrome and skip-taskbar exclusion, state, ambiguity and activation guards pass');
