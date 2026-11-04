// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const list = document.getElementById('list');
const sub = document.getElementById('sub');
let toastTimer = null;

// Each slot is drawn as a diagram of the panel, with the slot filled in, so the
// buttons read without relying on a font that has the right box characters.
const SLOTS = [
  { layout: 'full', rect: [2, 4, 20, 16] },
  { layout: 'left', rect: [2, 4, 9.5, 16] },
  { layout: 'right', rect: [12.5, 4, 9.5, 16] },
  { layout: 'third1', rect: [2, 4, 6, 16] },
  { layout: 'third2', rect: [9, 4, 6, 16] },
  { layout: 'third3', rect: [16, 4, 6, 16] },
];
const SVG_NS = 'http://www.w3.org/2000/svg';
function slotGlyph(rect) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  const frame = document.createElementNS(SVG_NS, 'rect');
  frame.setAttribute('x', '1.5'); frame.setAttribute('y', '3.5');
  frame.setAttribute('width', '21'); frame.setAttribute('height', '17');
  frame.setAttribute('rx', '3');
  frame.setAttribute('fill', 'none');
  frame.setAttribute('stroke', 'currentColor');
  frame.setAttribute('stroke-opacity', '0.45');
  frame.setAttribute('stroke-width', '1.2');
  const fill = document.createElementNS(SVG_NS, 'rect');
  fill.setAttribute('x', String(rect[0])); fill.setAttribute('y', String(rect[1]));
  fill.setAttribute('width', String(rect[2])); fill.setAttribute('height', String(rect[3]));
  fill.setAttribute('rx', '2');
  fill.setAttribute('fill', 'currentColor');
  svg.append(frame, fill);
  return svg;
}
function toast(message) {
  const node = document.getElementById('toast');
  node.textContent = message;
  node.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.remove('show'), 1700);
}
function mark(window) {
  const label = window.own ? window.title : (window.app || window.title || '?');
  return String(label).replace(/^org\.kde\./, '').slice(0, 2).toUpperCase();
}
async function apply(window, layout) {
  try {
    await api.call('windows.fit', { id: window.id, layout });
    toast(`${window.title} → ${layout}`);
  } catch (error) { toast(error.message); }
  setTimeout(refresh, 400);
}
function row(window) {
  const node = document.createElement('div');
  node.className = `win${window.active ? ' active' : ''}`;
  const avatar = document.createElement('span');
  avatar.className = 'avatar';
  avatar.textContent = mark(window);
  const text = document.createElement('span');
  text.className = 'text';
  const title = document.createElement('div');
  title.className = 'title';
  title.textContent = window.title || '(untitled)';
  const app = document.createElement('div');
  app.className = 'app';
  app.textContent = window.own ? 'EdgeLine app' : (window.app || '');
  text.append(title, app);
  const slots = document.createElement('span');
  slots.className = 'slots';
  for (const slot of SLOTS) {
    const button = document.createElement('button');
    button.type = 'button';
    button.title = `Snap to ${slot.layout}`;
    button.setAttribute('aria-label', `Snap ${window.title} to ${slot.layout}`);
    button.append(slotGlyph(slot.rect));
    button.onclick = () => apply(window, slot.layout);
    slots.append(button);
  }
  node.append(avatar, text, slots);
  return node;
}
async function refresh() {
  try {
    const state = await api.call('windows.list');
    const windows = (state.windows || []).filter((window) => window && window.id);
    sub.textContent = windows.length
      ? `${windows.length} window${windows.length === 1 ? '' : 's'} on the Edge`
      : 'Nothing running on the Edge';
    if (!windows.length) {
      list.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = state.error || 'Start an app on the Edge first';
      list.append(empty);
      return;
    }
    list.replaceChildren(...windows.map(row));
  } catch (error) {
    list.replaceChildren();
    const empty = document.createElement('p');
    empty.className = 'empty';
    empty.textContent = error.message;
    list.append(empty);
  }
}
document.getElementById('refresh').onclick = refresh;
document.getElementById('restore').onclick = async () => {
  try { await api.call('windows.restore', {}); toast('Restored the last fitted window'); }
  catch (error) { toast(error.message); }
  setTimeout(refresh, 400);
};
document.getElementById('close').onclick = () => window.close();
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') window.close();
  if (event.key === 'r' || event.key === 'R') refresh();
});
refresh();
setInterval(refresh, 2000);
