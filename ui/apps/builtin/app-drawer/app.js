// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
const api = window.edgelineApp;
const grid = document.getElementById('grid');
const sub = document.getElementById('sub');
let toastTimer = null;

// Same curated set the rail draws, kept in one place per app bundle.
const GLYPHS = {
  play: { d: 'M8 5.5v13l11-6.5z', fill: true },
  pulse: { d: 'M2.5 12h4l2-5.5 3 11 2.5-5.5h7.5' },
  clipboard: { d: 'M9 4.5h6v2.5H9z M7 7h10v12.5H7z M9.5 11.5h5 M9.5 15h3.5' },
  clock: { d: 'M12 6.5v6l3.5 2 M3.5 12a8.5 8.5 0 1 0 17 0 8.5 8.5 0 0 0-17 0z' },
  moon: { d: 'M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z' },
  wrench: { d: 'M14.5 3.5a5 5 0 0 0-4.4 7.3L4 17l3 3 6.2-6.1a5 5 0 0 0 7.3-4.4l-3 3-2.5-2.5z' },
  grid: { d: 'M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z' },
  note: { d: 'M9 17V5.5l9-2V15 M9 17a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z M18 15a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z' },
  terminal: { d: 'M4 5.5h16v13H4z M7.5 10l2.5 2-2.5 2 M13 14h4' },
  globe: { d: 'M3.5 12a8.5 8.5 0 1 0 17 0 8.5 8.5 0 0 0-17 0z M3.5 12h17 M12 3.5c2.5 2.4 2.5 14.6 0 17 M12 3.5c-2.5 2.4-2.5 14.6 0 17' },
  chat: { d: 'M4 5h16v10H10l-6 4.5z M8 9h8 M8 12h5' },
  gamepad: { d: 'M8 8h8a5 5 0 0 1 4.5 7.2l-.7 1.6a2 2 0 0 1-3.4.5L14.5 15h-5l-1.9 2.3a2 2 0 0 1-3.4-.5l-.7-1.6A5 5 0 0 1 8 8z' },
  camera: { d: 'M4 8h4l1.5-2h5L16 8h4v11H4z M12 16a3 3 0 1 0 0-6 3 3 0 0 0 0 6z' },
  layers: { d: 'M12 3.5 3.5 8l8.5 4.5L20.5 8z M3.5 12.5 12 17l8.5-4.5 M3.5 16.5 12 21l8.5-4.5' },
  bolt: { d: 'M13.5 3 6 13.5h5l-1 7.5 8-11h-5z', fill: true },
  heart: { d: 'M12 20s-7.5-4.6-7.5-9.4A4.1 4.1 0 0 1 12 7.7a4.1 4.1 0 0 1 7.5 2.9C19.5 15.4 12 20 12 20z' },
};
function glyphNode(name) {
  const glyph = GLYPHS[name];
  if (!glyph) return null;
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
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
function toast(message) {
  const node = document.getElementById('toast');
  node.textContent = message;
  node.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.remove('show'), 1600);
}
async function launch(app) {
  try {
    const result = await api.call('apps.launch', { id: app.id });
    toast(`${app.name} ${result.reused ? 'is open' : 'opened'}`);
  } catch (error) { toast(error.message); }
  setTimeout(refresh, 300);
}
function tile(app) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'tile';
  button.title = app.description || app.name;
  const glyph = document.createElement('span');
  glyph.className = 'glyph';
  const svg = glyphNode(app.icon);
  if (svg) glyph.append(svg);
  else {
    const mark = document.createElement('span');
    mark.className = 'mark';
    mark.textContent = String(app.name || '?').slice(0, 2).toUpperCase();
    glyph.append(mark);
  }
  const name = document.createElement('span');
  name.className = 'name';
  name.textContent = app.name;
  const desc = document.createElement('span');
  desc.className = 'desc';
  desc.textContent = app.description || '';
  button.append(glyph, name, desc);
  if (app.open) {
    const badge = document.createElement('span');
    badge.className = 'badge';
    badge.textContent = 'open';
    button.append(badge);
  }
  button.onclick = () => launch(app);
  return button;
}
async function refresh() {
  try {
    const apps = order(await api.call('apps.list'));
    const { total, running } = summarize(apps);
    sub.textContent = `${total} app${total === 1 ? '' : 's'}${running ? ` · ${running} open` : ''}`;
    if (!apps.length) {
      grid.replaceChildren();
      const empty = document.createElement('p');
      empty.className = 'empty';
      empty.textContent = 'No apps installed';
      grid.append(empty);
      return;
    }
    grid.replaceChildren(...apps.map(tile));
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
setInterval(refresh, 2500);
