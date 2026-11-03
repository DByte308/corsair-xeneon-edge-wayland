// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const signal = () => ({connect() {}});
const edge = {geometry:{width:2560,height:720}};
const other = {geometry:{width:3440,height:1440}};
const id = '{12345678-1234-1234-1234-123456789abc}';
function window(caption, output, app='test') {
  return {internalId:id,caption,output,resourceClass:app,normalWindow:true,active:false,minimized:false,
    frameGeometryChanged:signal(),captionChanged:signal(),minimizedChanged:signal(),outputChanged:signal()};
}
const game = window('VanguardGalaxy',edge,'steam_app_3471800');
const elsewhere = window('Private document',other);
const control = window('EdgeLine',edge,'edgeline');
const dashboard = window('EdgeLine dashboard',edge,'edgeline');
const windows = [game,elsewhere,control,dashboard];
let latest;
const workspace = {screens:[edge,other],windowList:()=>windows,windowAdded:signal(),windowRemoved:signal(),
  windowActivated:signal(),screensChanged:signal(),raiseWindow(w){this.raised=w;}};
const source=fs.readFileSync(path.join(__dirname,'../kwin-window-overview.js'),'utf8').replace('__EDGELINE_BUS__','"dev.test"');
function report() {
  vm.runInNewContext(source,{workspace,callDBus(...args){latest=JSON.parse(args.at(-1));}});
}
report();
assert.equal(latest.windows.length,1);
assert.equal(latest.windows[0].app,'steam_app_3471800');
assert.equal(latest.available,true);
game.minimized=true; game.active=true;
report();assert.equal(latest.windows[0].minimized,true);assert.equal(latest.windows[0].active,true);
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
console.log('window-overview: Edge-only filtering, own-window exclusion, active/minimized state, ambiguity and activation guards pass');
