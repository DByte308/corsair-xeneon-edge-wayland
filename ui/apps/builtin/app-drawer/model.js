// SPDX-License-Identifier: GPL-3.0-or-later
// Drawer layout rules, kept pure so they can be tested without a browser.
'use strict';

const SELF = 'app-drawer';

// Apps the drawer may show: usable, enabled, and not the drawer itself.
function visible(apps, self = SELF) {
  return (Array.isArray(apps) ? apps : [])
    .filter((app) => app && typeof app.id === 'string' && app.id !== self)
    .filter((app) => app.enabled !== false && app.compatible !== false);
}

// Running apps first, then alphabetical. Stable for equal sort keys so the
// grid does not shuffle while the user is reaching for a tile.
function order(apps) {
  return visible(apps).map((app, index) => ({ app, index }))
    .sort((a, b) => {
      const running = Number(b.app.open === true) - Number(a.app.open === true);
      if (running) return running;
      const byName = String(a.app.name || '').localeCompare(String(b.app.name || ''));
      return byName || a.index - b.index;
    })
    .map((entry) => entry.app);
}

function summarize(apps) {
  const list = visible(apps);
  return {
    total: list.length,
    running: list.filter((app) => app.open === true).length,
  };
}

if (typeof module !== 'undefined' && module.exports) module.exports = { visible, order, summarize, SELF };
