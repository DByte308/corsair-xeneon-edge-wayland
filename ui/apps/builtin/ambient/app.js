// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const el = (id) => document.getElementById(id);
const art = el('art');
let artUrl = '';

function tick() {
  const now = new Date();
  el('time').textContent = now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  el('date').textContent = now.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'long' });
}
function setArt(url) {
  const next = url || '';
  if (next === artUrl) return;
  artUrl = next;
  art.replaceChildren();
  if (!next) return;
  const image = document.createElement('img');
  image.src = next;
  image.alt = '';
  art.append(image);
}
async function refresh() {
  try {
    const state = await api.call('media.nowPlaying');
    if (!state.available) {
      el('state').textContent = 'Nothing playing';
      el('track').textContent = '';
      el('by').textContent = '';
      setArt('');
      return;
    }
    el('state').textContent = state.status === 'Playing' ? 'Now playing' : state.status;
    el('track').textContent = state.title;
    el('by').textContent = [state.artist, state.album].filter(Boolean).join(' · ');
    setArt(state.art);
  } catch { /* ambient mode must never shout */ }
}
el('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => { if (event.key === 'Escape') window.close(); });
tick();
setInterval(tick, 15000);
refresh();
setInterval(refresh, 8000);
