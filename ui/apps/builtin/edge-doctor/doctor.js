// SPDX-License-Identifier: GPL-3.0-or-later
// The whole surface this app has: one read-only diagnostics call.
'use strict';

const api = window.edgelineApp;
const reportEl = document.getElementById('report');
const subtitleEl = document.getElementById('subtitle');
const generatedEl = document.getElementById('generated');
const hintEl = document.getElementById('hint');
const rawPane = document.getElementById('rawPane');
const rawText = document.getElementById('rawText');
let latest = null;

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function renderRow(entry) {
  const row = el('div', 'row');
  row.dataset.status = entry.status;
  row.append(el('span', 'dot'), el('span', 'label', entry.label), el('span', 'value', entry.actual),
    el('span', 'expected', entry.expected ? `expected ${entry.expected}` : ''));
  return row;
}

function renderReport(report) {
  reportEl.replaceChildren();
  for (const section of report.sections) {
    const card = el('section', 'card');
    card.append(el('h2', null, section.title));
    for (const entry of section.rows) card.append(renderRow(entry));
    reportEl.append(card);
  }
  document.body.dataset.verdict = report.verdict;
  const { ok, warn, bad } = report.counts;
  subtitleEl.textContent = bad ? `${bad} problem${bad === 1 ? '' : 's'} found` :
    warn ? `${warn} warning${warn === 1 ? '' : 's'}, nothing broken` : 'everything lines up';
  generatedEl.textContent = `Read at ${new Date(report.generatedAt || Date.now()).toLocaleTimeString()}`;
  hintEl.textContent = 'Read-only. Press R to refresh.';
}

async function refresh() {
  subtitleEl.textContent = 'Reading the desktop…';
  try {
    const diagnostics = await api.call('diagnostics.read');
    latest = diagnostics;
    rawText.textContent = JSON.stringify(diagnostics, null, 2);
    renderReport(buildReport(diagnostics));
    const summary = buildReport(diagnostics);
    console.info(`[doctor] verdict=${summary.verdict} ok=${summary.counts.ok} warn=${summary.counts.warn} bad=${summary.counts.bad}`);
  } catch (error) {
    reportEl.replaceChildren();
    const card = el('section', 'card');
    card.append(el('h2', null, 'Diagnostics unavailable'), renderRow({
      label: 'Host refused diagnostics.read', actual: String(error && error.message || error),
      expected: 'granted capability diagnostics:read', status: 'bad',
    }));
    reportEl.append(card);
    document.body.dataset.verdict = 'bad';
    subtitleEl.textContent = 'diagnostics.read failed';
  }
}

document.getElementById('refresh').addEventListener('click', refresh);
document.getElementById('raw').addEventListener('click', () => { rawPane.hidden = !rawPane.hidden; });
document.getElementById('close').addEventListener('click', () => window.close());
window.addEventListener('keydown', (event) => {
  if (event.key === 'r' || event.key === 'R') refresh();
  else if (event.key === 'Escape') window.close();
});
refresh();
setInterval(refresh, 5000);
