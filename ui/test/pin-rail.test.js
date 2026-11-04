// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
function signal(){const handlers=[];return {connect:f=>handlers.push(f),emit(...args){for(const f of [...handlers])f(...args);}};}
const screen={geometry:{x:500,y:100,width:2560,height:720}};
function window(id,caption,resourceClass){return {internalId:id,caption,resourceClass,normalWindow:true,
  minimized:false,fullScreen:false,maximizeMode:0,moveable:true,resizeable:true,
  frameGeometryChanged:signal(),captionChanged:signal(),windowClassChanged:signal(),outputChanged:signal(),minimizedChanged:signal(),fullScreenChanged:signal(),maximizedChanged:signal(),
  setMaximize(h,v){this.maximizeMode=(h?2:0)|(v?1:0);this.maximizedChanged.emit();}};}
const rail=window('rail','EdgeLine taskbar','edgeline');
let geometry={x:596,y:100,width:96,height:720};let writes=0;
Object.defineProperty(rail,'frameGeometry',{get:()=>geometry,set:value=>{
  if(++writes>40)throw new Error('Geometry callback feedback loop');geometry={...value};rail.frameGeometryChanged.emit();
}});
const game=window('game','Game','game');game.frameGeometry={x:900,y:100,width:900,height:600};
const settings=window('settings','EdgeLine','edgeline');settings.frameGeometry={x:596,y:100,width:2464,height:720};
const impostor=window('browser','EdgeLine taskbar','browser');impostor.frameGeometry={x:900,y:100,width:200,height:200};
let windows=[rail,game,settings,impostor];
const workspace={screens:[screen],activeWindow:game,currentDesktop:{},
  windowActivated:signal(),windowRemoved:signal(),windowAdded:signal(),screensChanged:signal(),
  windowList:()=>windows,clientArea:()=>({...screen.geometry,x:596,width:2464}),raiseWindow(){}};
vm.runInNewContext(fs.readFileSync(path.join(__dirname,'../kwin-launchpad.js'),'utf8'),{
  workspace,KWin:{MaximizeArea:1},print(){},registerShortcut(){},callDBus(){},
});
assert.equal(geometry.x,500);assert.equal(writes,1);
rail.frameGeometryChanged.emit();assert.equal(writes,1,'Already pinned must not write/recurse');
geometry={...geometry,x:596,width:80};rail.frameGeometryChanged.emit();
assert.equal(geometry.x,500);assert.equal(geometry.width,96);assert.equal(writes,2,'A constrain push converges in one write');
rail.minimized=true;rail.minimizedChanged.emit();geometry={...geometry,x:596};rail.frameGeometryChanged.emit();
assert.equal(geometry.x,596,'Do not manipulate minimized rollback windows');
rail.minimized=false;rail.minimizedChanged.emit();assert.equal(geometry.x,500);
rail.fullScreen=true;rail.maximizeMode=3;rail.maximizedChanged.emit();assert.equal(rail.fullScreen,false);assert.equal(rail.maximizeMode,0);
settings.frameGeometry={x:900,y:200,width:1000,height:620};settings.frameGeometryChanged.emit();assert.equal(settings.frameGeometry.x,900,'Only the rail is pinned');
assert.equal(game.frameGeometry.x,900);assert.equal(impostor.frameGeometry.x,900,'Unrelated window with matching title is untouched');
screen.geometry.x=700;workspace.screensChanged.emit();assert.equal(geometry.x,700);
workspace.screens=[];geometry={...geometry,x:12};rail.frameGeometryChanged.emit();assert.equal(geometry.x,12,'No fallback onto another monitor');
workspace.screens=[screen];rail.outputChanged.emit();assert.equal(geometry.x,700);
rail.resizeable=false;geometry={x:796,y:100,width:120,height:700};rail.frameGeometryChanged.emit();assert.equal(geometry.x,700);assert.equal(geometry.width,120);
const fixedWrites=writes;rail.frameGeometryChanged.emit();assert.equal(writes,fixedWrites,'Fixed-size clients must not trigger size feedback loops');
rail.resizeable=true;rail.frameGeometryChanged.emit();assert.equal(geometry.width,96);
rail.moveable=false;geometry={...geometry,x:796};rail.frameGeometryChanged.emit();assert.equal(geometry.x,796);
rail.moveable=true;rail.frameGeometryChanged.emit();assert.equal(geometry.x,700);
workspace.windowRemoved.emit(rail);windows=windows.filter(w=>w!==rail);geometry={...geometry,x:12};rail.frameGeometryChanged.emit();assert.equal(geometry.x,12,'Removed windows must not retain live pin callbacks');
const late=window('late','','');late.frameGeometry={x:796,y:100,width:96,height:720};windows.push(late);workspace.windowAdded.emit(late);
late.caption='EdgeLine taskbar';late.captionChanged.emit();assert.equal(late.frameGeometry.x,700,'An unresolved class still permits the exact-title rail');
late.resourceClass='edgeline';late.windowClassChanged.emit();assert.equal(late.frameGeometry.x,700,'Late class assignment must keep the pin');
const spoof=window('spoof','EdgeLine taskbar','electron');spoof.frameGeometry={x:900,y:100,width:300,height:300};windows.push(spoof);workspace.windowAdded.emit(spoof);
assert.equal(spoof.frameGeometry.x,900,'A generic electron window must not be pinned by a spoofed title');
spoof.frameGeometryChanged.emit();assert.equal(spoof.frameGeometry.x,900);
console.log('pin-rail: constrain correction, idempotence, restore, maximize guards, output movement, late class, spoofed title and rail-only scope pass');
