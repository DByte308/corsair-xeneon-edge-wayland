// SPDX-License-Identifier: GPL-3.0-or-later
// Slot geometry for the Edge. Pure: a work area in, a rectangle out.
'use strict';

const GAP = 6;
const MIN_WIDTH = 200;
const MIN_HEIGHT = 160;

const LAYOUTS = {
  full: (a) => ({ x: a.x, y: a.y, width: a.width, height: a.height }),
  left: (a) => ({ x: a.x, y: a.y, width: Math.round(a.width / 2) - GAP, height: a.height }),
  right: (a) => ({
    x: a.x + Math.round(a.width / 2) + GAP, y: a.y,
    width: a.width - Math.round(a.width / 2) - GAP, height: a.height,
  }),
  top: (a) => ({ x: a.x, y: a.y, width: a.width, height: Math.round(a.height / 2) - GAP }),
  bottom: (a) => ({
    x: a.x, y: a.y + Math.round(a.height / 2) + GAP,
    width: a.width, height: a.height - Math.round(a.height / 2) - GAP,
  }),
  third1: (a) => ({ x: a.x, y: a.y, width: Math.round(a.width / 3) - GAP, height: a.height }),
  third2: (a) => ({
    x: a.x + Math.round(a.width / 3) + GAP, y: a.y,
    width: Math.round(a.width / 3) - GAP * 2, height: a.height,
  }),
  third3: (a) => {
    const x = a.x + 2 * Math.round(a.width / 3) + GAP;
    return { x, y: a.y, width: a.x + a.width - x, height: a.height };
  },
};

function isLayout(name) {
  return typeof name === 'string' && Object.hasOwn(LAYOUTS, name);
}

// Returns a clamped rectangle, or null when the layout or the area is unusable.
function slot(area, layout) {
  if (!area || !isLayout(layout)) return null;
  const numbers = ['x', 'y', 'width', 'height'].map((key) => area[key]);
  if (!numbers.every((value) => Number.isInteger(value))) return null;
  if (area.width < MIN_WIDTH || area.height < MIN_HEIGHT) return null;
  const rect = LAYOUTS[layout](area);
  const width = Math.max(MIN_WIDTH, Math.min(rect.width, area.x + area.width - rect.x));
  const height = Math.max(MIN_HEIGHT, Math.min(rect.height, area.y + area.height - rect.y));
  return { x: rect.x, y: rect.y, width, height };
}

module.exports = { slot, isLayout, LAYOUTS: Object.keys(LAYOUTS), GAP, MIN_WIDTH, MIN_HEIGHT };
