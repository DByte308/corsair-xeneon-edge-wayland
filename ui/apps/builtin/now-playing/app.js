// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const el = (id) => document.getElementById(id);
const artEl = el('art');
let current = null;
let toastTimer = null;

function clock(seconds) {
  const value = Math.max(0, Math.round(seconds || 0));
  return `${Math.floor(value / 60)}:${String(value % 60).padStart(2, '0')}`;
}
function toast(message) {
  const node = el('toast');
  node.textContent = message;
  node.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.remove('show'), 1600);
}
function setArt(url) {
  artEl.replaceChildren();
  if (!url) { const note = document.createElement('span'); note.className = 'note'; note.textContent = '♪'; artEl.append(note); return; }
  const image = document.createElement('img');
  image.src = url;
  image.alt = '';
  artEl.append(image);
}
function render(state) {
  current = state;
  const available = state && state.available;
  el('player').textContent = available ? `${state.player.replace('org.mpris.MediaPlayer2.', '')} · ${state.status}` : (state && state.error) || 'No player';
  el('title').textContent = available ? state.title : 'Nothing playing';
  el('artist').textContent = available ? state.artist : '';
  el('album').textContent = available ? state.album : '';
  el('playpause').disabled = !available;
  el('next').disabled = !available;
  el('previous').disabled = !available;
  el('playpause').textContent = available && state.status === 'Playing' ? '⏸' : '▶';
  const length = available ? state.length : 0;
  const position = available ? state.position : 0;
  el('position').textContent = clock(position);
  el('length').textContent = clock(length);
  el('bar').style.width = length ? `${Math.min(100, position / length * 100)}%` : '0';
  setArt(available ? state.art : '');
}
async function refresh() {
  try { render(await api.call('media.nowPlaying')); }
  catch (error) { render({ available: false, error: error.message }); }
}
async function send(action) {
  try { await api.call('media.command', { action }); toast(action === 'playpause' ? 'Toggled playback' : `${action} requested`); }
  catch (error) { toast(error.message); }
  setTimeout(refresh, 250);
}
el('playpause').onclick = () => send('playpause');
el('next').onclick = () => send('next');
el('previous').onclick = () => send('previous');
el('refresh').onclick = refresh;
el('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => {
  if (event.key === ' ') { event.preventDefault(); send('playpause'); }
  else if (event.key === 'ArrowRight') send('next');
  else if (event.key === 'ArrowLeft') send('previous');
  else if (event.key === 'Escape') window.close();
  else if (event.key === 'r' || event.key === 'R') refresh();
});
refresh();
setInterval(refresh, 2000);
