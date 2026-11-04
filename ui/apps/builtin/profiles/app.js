// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const grid = document.getElementById('grid');
const sub = document.getElementById('sub');
let toastTimer = null;

function toast(message) {
  const node = document.getElementById('toast');
  node.textContent = message;
  node.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.remove('show'), 1700);
}
async function apply(name) {
  try {
    await api.call('profiles.apply', { name });
    toast(`${name} applied`);
  } catch (error) { toast(error.message); }
  setTimeout(refresh, 400);
}
async function refresh() {
  try {
    const state = await api.call('profiles.list');
    const profiles = state.profiles || [];
    const active = state.active || '';
    sub.textContent = profiles.length
      ? `${profiles.length} profile${profiles.length === 1 ? '' : 's'}${active ? ` · ${active} active` : ''}`
      : 'No profiles saved yet';
    if (!profiles.length) {
      grid.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = 'Save a profile in EdgeLine settings first; it appears here.';
      grid.append(empty);
      return;
    }
    grid.replaceChildren(...profiles.map((profile) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = `profile${profile.active || profile.name === active ? ' active' : ''}`;
      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = profile.name;
      const label = document.createElement('span');
      label.className = 'state';
      label.textContent = profile.active || profile.name === active ? 'active now' : 'tap to apply';
      button.append(name, label);
      button.onclick = () => apply(profile.name);
      return button;
    }));
  } catch (error) {
    grid.replaceChildren();
    const empty = document.createElement('p');
    empty.className = 'empty';
    empty.textContent = error.message;
    grid.append(empty);
  }
}
document.getElementById('refresh').onclick = refresh;
document.getElementById('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') window.close();
  if (event.key === 'r' || event.key === 'R') refresh();
});
refresh();
setInterval(refresh, 4000);
