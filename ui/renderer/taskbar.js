// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgeline;
// A curated glyph set so a rail tile reads at a glance: an app names one in its
// manifest, and well-known desktop classes get a sensible default.
const GLYPHS = {
  play:       { d: 'M8 5.5v13l11-6.5z', fill: true },
  pulse:      { d: 'M2.5 12h4l2-5.5 3 11 2.5-5.5h7.5' },
  clipboard:  { d: 'M9 4.5h6v2.5H9z M7 7h10v12.5H7z M9.5 11.5h5 M9.5 15h3.5' },
  clock:      { d: 'M12 6.5v6l3.5 2 M3.5 12a8.5 8.5 0 1 0 17 0 8.5 8.5 0 0 0-17 0z' },
  moon:       { d: 'M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z' },
  wrench:     { d: 'M14.5 3.5a5 5 0 0 0-4.4 7.3L4 17l3 3 6.2-6.1a5 5 0 0 0 7.3-4.4l-3 3-2.5-2.5z' },
  grid:       { d: 'M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z' },
  note:       { d: 'M9 17V5.5l9-2V15 M9 17a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z M18 15a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z' },
  terminal:   { d: 'M4 5.5h16v13H4z M7.5 10l2.5 2-2.5 2 M13 14h4' },
  globe:      { d: 'M3.5 12a8.5 8.5 0 1 0 17 0 8.5 8.5 0 0 0-17 0z M3.5 12h17 M12 3.5c2.5 2.4 2.5 14.6 0 17 M12 3.5c-2.5 2.4-2.5 14.6 0 17' },
  chat:       { d: 'M4 5h16v10H10l-6 4.5z M8 9h8 M8 12h5' },
  gamepad:    { d: 'M8 8h8a5 5 0 0 1 4.5 7.2l-.7 1.6a2 2 0 0 1-3.4.5L14.5 15h-5l-1.9 2.3a2 2 0 0 1-3.4-.5l-.7-1.6A5 5 0 0 1 8 8z M9 11v3 M7.5 12.5h3 M15.5 11.5h.01 M17 13.5h.01' },
  camera:     { d: 'M4 8h4l1.5-2h5L16 8h4v11H4z M12 16a3 3 0 1 0 0-6 3 3 0 0 0 0 6z' },
  layers:     { d: 'M12 3.5 3.5 8l8.5 4.5L20.5 8z M3.5 12.5 12 17l8.5-4.5 M3.5 16.5 12 21l8.5-4.5' },
  bolt:       { d: 'M13.5 3 6 13.5h5l-1 7.5 8-11h-5z', fill: true },
  heart:      { d: 'M12 20s-7.5-4.6-7.5-9.4A4.1 4.1 0 0 1 12 7.7a4.1 4.1 0 0 1 7.5 2.9C19.5 15.4 12 20 12 20z' },
};
const CLASS_GLYPHS = [
  [/^steam_app_|^steam$/i, 'gamepad'],
  [/konsole|terminal|kitty|alacritty|xterm/i, 'terminal'],
  [/vivaldi|firefox|chrom|browser/i, 'globe'],
  [/discord|vesktop|telegram|signal/i, 'chat'],
];
let previous = '';
let artwork = null;
let busy = false;
api.launchpadAction('artwork').then(({image}) => { artwork=image; previous=''; }).catch(()=>{});
function glyphName(app) {
  if (app.icon && GLYPHS[app.icon]) return app.icon;
  for (const [pattern, name] of CLASS_GLYPHS) if (pattern.test(String(app.app || ''))) return name;
  return null;
}
function glyphNode(name) {
  const glyph = GLYPHS[name];
  if (!glyph) return null;
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('class', 'app-glyph');
  const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  path.setAttribute('d', glyph.d);
  path.setAttribute('stroke', 'currentColor');
  path.setAttribute('stroke-width', '1.7');
  path.setAttribute('stroke-linecap', 'round');
  path.setAttribute('stroke-linejoin', 'round');
  path.setAttribute('fill', glyph.fill ? 'currentColor' : 'none');
  svg.append(path);
  return svg;
}
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
      const glyph=glyphNode(glyphName(app));
      if(info.game&&artwork&&!app.own){const image=document.createElement('img');image.src=artwork;image.alt='';avatar.append(image);}
      else if(glyph){avatar.classList.add('has-glyph');avatar.append(glyph);}
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
document.getElementById('apps-button').onclick=async()=>{
  try { await api.edgeTaskbarAction('apps'); }
  catch (error) { document.getElementById('connection').textContent=error.message; }
};
document.getElementById('deck').onclick=async()=>{
  await api.dashboardLayout({page:'tablet-deck'});
  await api.edgeTaskbarAction('home');
};
document.getElementById('settings').onclick=()=>api.edgeTaskbarAction('settings');
setInterval(refresh,700);
refresh();
