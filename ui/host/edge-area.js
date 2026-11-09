// SPDX-License-Identifier: GPL-3.0-or-later
// Where an app window may go on the Edge. Pure: geometry in, rectangle out.
'use strict';

// The rail is a fixed column on the Edge's left edge. The compositor reports it
// as taken only while the reservation helper holds its exclusive zone, so the
// column is enforced here too: placement must not depend on a helper that may
// have exited (a panel settling at login used to kill it), or the app window
// ends up underneath the sidebar.
const RAIL_WIDTH = 96;
const MIN_WIDTH = 320;
const MIN_HEIGHT = 200;
const MARGIN = 24;

// The compositor's usable rectangle, clamped so it starts at the rail's right
// edge and never leaves the output.
function railSafeArea(edge, area, railWidth = RAIL_WIDTH) {
  const left = edge.bounds.x + railWidth;
  const right = edge.bounds.x + edge.bounds.width;
  const x = Math.max(area.x, left);
  return {
    x,
    y: area.y,
    width: Math.max(0, Math.min(area.x + area.width, right) - x),
    height: area.height,
  };
}

// What the compositor currently calls usable on the Edge, before the rail is
// taken out of it. Falls back to the whole output when it has not reported a
// work area for this screen.
function compositorArea(edge, screens) {
  const list = screens || [];
  const match = list.find((entry) => entry && entry.geometry && entry.workarea
    && entry.geometry.x === edge.bounds.x
    && entry.geometry.y === edge.bounds.y
    && entry.geometry.width === edge.bounds.width);
  return match ? match.workarea : {
    x: edge.bounds.x, y: edge.bounds.y,
    width: edge.bounds.width, height: edge.bounds.height,
  };
}

// The rectangle an app window of this manifest wants, centred in the part of
// the Edge that the rail does not own.
function appRect(manifest, edge, screens) {
  const area = railSafeArea(edge, compositorArea(edge, screens));
  const width = Math.min(manifest.window.width, Math.max(MIN_WIDTH, area.width - MARGIN));
  const height = Math.min(manifest.window.height, Math.max(MIN_HEIGHT, area.height - MARGIN));
  return {
    x: Math.round(area.x + (area.width - width) / 2),
    y: Math.round(area.y + (area.height - height) / 2),
    width, height,
  };
}

module.exports = { RAIL_WIDTH, railSafeArea, compositorArea, appRect };
