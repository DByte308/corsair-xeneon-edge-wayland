// SPDX-License-Identifier: GPL-3.0-or-later
'use strict';
// busctl JSON keeps variant values typed; unwrap those explicitly rather than
// relying on KWin's JavaScript conversion of a delayed a{sv} reply.
function selectedWindow(reply) {
  const data = typeof reply === 'string' ? JSON.parse(reply) : reply;
  const properties = data?.type === 'a{sv}' && data.data?.[0];
  if (!properties) throw new Error('KWin returned an invalid window selection');
  const value = (key) => {
    const raw = properties[key]?.data;
    return Array.isArray(raw) ? raw[0] : raw;
  };
  const id = value('uuid');
  if (typeof id !== 'string' || !/^\{?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}?$/i.test(id)) {
    throw new Error('No application window was selected');
  }
  if (/^EdgeLine/.test(String(value('caption'))) || /^edgeline$/i.test(String(value('resourceClass')))) {
    throw new Error('Choose the app you want to move, not an EdgeLine window');
  }
  return { id, suffix:id.replace(/[^a-z0-9]/gi,'') };
}
module.exports = { selectedWindow };
