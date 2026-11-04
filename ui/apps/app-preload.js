// SPDX-License-Identifier: GPL-3.0-or-later
// Preload for app windows. Deliberately one method: every app call is checked
// against that app's granted capabilities in the main process, and the app id
// is taken from the window, never from the page.
'use strict';

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('edgelineApp', {
  call: (method, args) => ipcRenderer.invoke('app-call', method, args === undefined ? {} : args),
  info: () => ipcRenderer.invoke('app-window-info'),
  close: () => ipcRenderer.invoke('app-close'),
  onClosed: (cb) => ipcRenderer.on('app-closed', () => cb()),
});
