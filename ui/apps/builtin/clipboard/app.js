// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const list = document.getElementById('list');
const sub = document.getElementById('sub');
let toastTimer = null;

function toast(message) {
  const node = document.getElementById('toast');
  node.textContent = message;
  node.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.remove('show'), 1600);
}
function preview(text) {
  const lines = text.split('\n');
  return lines.slice(0, 2).join('\n') + (lines.length > 2 ? ' …' : '');
}
async function copy(text) {
  try {
    await api.call('clipboard.write', { text });
    toast(`Copied ${text.length} characters`);
  } catch (error) { toast(error.message); }
}
async function refresh() {
  try {
    const state = await api.call('clipboard.read');
    if (!state.available) {
      list.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = state.error || 'Clipboard history unavailable';
      list.append(empty);
      sub.textContent = 'Klipper history';
      return;
    }
    sub.textContent = `${state.items.length} entr${state.items.length === 1 ? 'y' : 'ies'} · tap to copy`;
    if (!state.items.length) {
      list.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = 'Nothing copied yet';
      list.append(empty);
      return;
    }
    list.replaceChildren(...state.items.map((item) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'item';
      const text = document.createElement('span');
      text.className = 'text';
      text.textContent = preview(item.text);
      const meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = `${item.text.length} chars${item.lines > 1 ? ` · ${item.lines} lines` : ''}${item.truncated ? ' · long' : ''}`;
      button.append(text, meta);
      button.onclick = () => copy(item.text);
      return button;
    }));
  } catch (error) {
    list.replaceChildren();
    const empty = document.createElement('p');
    empty.className = 'empty';
    empty.textContent = error.message;
    list.append(empty);
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
