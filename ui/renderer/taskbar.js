// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgeline;
let previous = '';
let artwork = null;
let busy = false;
api.launchpadAction('artwork').then(({image}) => { artwork=image; previous=''; }).catch(()=>{});
function identity(app) {
  // An EdgeLine app window is named by its title, not by the shared class.
  if (app.own) {
    const label = String(app.title || 'App').trim() || 'App';
    const mark = label.split(/\s+/).slice(0, 2).map((word) => word[0]).join('').toUpperCase();
    return {name: label, mark: mark || label.slice(0, 2).toUpperCase(), own: true};
  }
  const name = app.app.toLowerCase();
  if (name === 'steam_app_3471800') return {name:'Vanguard',mark:'VG',game:true};
  if (/steam/.test(name)) return {name:'Steam',mark:'St'};
  if (/konsole|terminal|kitty|alacritty/.test(name)) return {name:'Terminal',mark:'>_'};
  if (/vivaldi|firefox|chrom/.test(name)) return {name:'Browser',mark:'Web'};
  if (/discord|vesktop|telegram/.test(name)) return {name:'Chat',mark:'Chat'};
  const label=app.app.replace(/^org\.kde\./,'') || 'App';
  return {name:label,mark:label.slice(0,2).toUpperCase()};
}
async function refresh() {
  if (busy) return;
  try {
    const state=await api.edgeWindows();
    const signature=JSON.stringify(state);
    if (signature===previous) return;
    previous=signature;
    const focused=document.activeElement?.dataset?.id;
    const buttons=state.windows.map(app=>{
      const info=identity(app);
      const button=document.createElement('button');
      button.dataset.id=app.id;
      button.className=[app.minimized?'app-minimized':'',info.own?'app-own':''].filter(Boolean).join(' ');
      button.title=`${info.name}: ${app.title}`;
      button.setAttribute('aria-label',`Switch to ${app.title}`);
      button.setAttribute('aria-pressed',String(app.active));
      const avatar=document.createElement('span'); avatar.className='app-avatar';
      if(info.game&&artwork){const image=document.createElement('img');image.src=artwork;image.alt='';avatar.append(image);}
      else avatar.textContent=info.mark;
      const label=document.createElement('span');label.className='app-name';label.textContent=info.name;
      if(info.own)button.title=`${info.name} (EdgeLine app)`;
      button.append(avatar,label);
      button.onclick=async()=>{if(busy)return;busy=true;button.disabled=true;
        try{await api.activateEdgeWindow(app.id);}
        catch(error){document.getElementById('connection').textContent=error.message;}
        finally{busy=false;previous='';refresh();}};
      return button;
    });
    document.getElementById('apps').replaceChildren(...buttons);
    if(focused)buttons.find(b=>b.dataset.id===focused)?.focus({preventScroll:true});
    document.getElementById('connection').textContent=state.error || (!state.available?'Edge offline':'');
  } catch { document.getElementById('connection').textContent='App list unavailable'; }
}
document.getElementById('home').onclick=async()=>{
  await api.dashboardLayout({page:'launchpad'});
  await api.edgeTaskbarAction('home');
};
document.getElementById('deck').onclick=async()=>{
  await api.dashboardLayout({page:'tablet-deck'});
  await api.edgeTaskbarAction('home');
};
document.getElementById('settings').onclick=()=>api.edgeTaskbarAction('settings');
setInterval(refresh,700);
refresh();
