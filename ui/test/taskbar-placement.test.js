// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert=require('node:assert/strict');
const vm=require('node:vm');
const fs=require('node:fs');
const path=require('node:path');
const edge={geometry:{x:500,y:100,width:2560,height:720}};
function window(caption,output=edge){return {caption,output,normalWindow:true,moveable:true,resizeable:true,
  fullScreen:false,minimized:false,frameGeometry:{...output.geometry},setMaximize(){}};}
const rail=window('EdgeLine taskbar'),home=window('EdgeLine dashboard'),control=window('EdgeLine');
const game=window('Game'),elsewhere=window('Other monitor',{geometry:{x:0,y:0,width:3440,height:1440}});
const originalOther={...elsewhere.frameGeometry};
const windows=[rail,home,control,game,elsewhere];
const workspace={screens:[edge],windowList:()=>windows,raiseWindow(w){this.raised=w;}};
const source=fs.readFileSync(path.join(__dirname,'../kwin-place-panel.js'),'utf8');
vm.runInNewContext(source,{workspace});
assert.equal(rail.frameGeometry.x,500);assert.equal(rail.frameGeometry.width,96);
assert.equal(rail.frameGeometry.height,720);assert.equal(rail.keepAbove,true);assert.equal(workspace.raised,rail);
for(const w of [home,control,game]){assert.equal(w.frameGeometry.x,596);assert.equal(w.frameGeometry.width,2464);}
assert.equal(home.keepAbove,false);assert.equal(control.keepAbove,true);
assert.deepEqual(elsewhere.frameGeometry,originalOther);
workspace.screens=[];rail.frameGeometry={x:1,y:1,width:1,height:1};
vm.runInNewContext(source,{workspace});assert.equal(rail.frameGeometry.x,1);
console.log('taskbar-placement: reserved rail, fitted content, stacking, other-monitor isolation and absent Edge pass');
