// SPDX-License-Identifier: GPL-3.0-or-later
// Compatibility API for the existing HTML/CSS/JS. No Node or Electron required.
(() => {
  if (window.top !== window) return; // Never give Tablet Deck's iframe host privileges.
  window.__edgeSmokeErrors = [];
  if (window.__EDGE_SMOKE__) {
    window.addEventListener('error', event => window.__edgeSmokeErrors.push(event.message));
    window.addEventListener('unhandledrejection', event => window.__edgeSmokeErrors.push(String(event.reason)));
    window.addEventListener('securitypolicyviolation', event => window.__edgeSmokeErrors.push(`CSP: ${event.violatedDirective}`));
  }
  window.__edgeApiTrace=[];
  const invoke = (method, params = {}) => {
    const trace=stage=>{if(window.__EDGE_SMOKE__){window.__edgeApiTrace.push(`${method}:${stage}`);if(window.__edgeApiTrace.length>100)window.__edgeApiTrace.shift();}};
    trace('start');
    return window.__TAURI__.core.invoke('edgeline', {method, params}).then(value=>{trace('done');return value;},error=>{trace('error');throw error;});
  };
  const listen = (name, callback) => window.__TAURI__.event.listen(name, event => callback(event.payload));
  Object.defineProperty(window, 'edgeline', {value:Object.freeze({
    call:(method,params={})=>invoke('rpc',{method,params}),
    connected:()=>invoke('agent-connected'),
    onStatus:callback=>listen('agent-status',callback),
    onEvent:callback=>listen('agent-event',callback),
    windowAction:action=>invoke('window-action',{action}),
    openExternal:url=>invoke('open-external',{url}),
    autostartGet:()=>invoke('autostart-get'),
    autostartSet:on=>invoke('autostart-set',{on}),
    deviceLabelGet:()=>invoke('device-label-get'),
    deviceLabelSet:label=>invoke('device-label-set',{label}),
    openTabletDeck:()=>invoke('tablet-deck-open'),
    edgeTaskbarAction:action=>invoke('edge-taskbar-action',{action}),
    edgeWindows:()=>invoke('edge-windows'),
    activateEdgeWindow:id=>invoke('edge-window-activate',{id}),
    launchpadAction:action=>invoke('launchpad-action',{action}),
    dashboard:on=>invoke('dashboard',{on}),
    dashboardState:()=>invoke('dashboard-state'),
    dashboardLayout:layout=>invoke('dashboard-layout',{layout}),
    openCalibration:()=>invoke('open-calibration'),
    closeCalibration:()=>invoke('close-calibration'),
    ripple:on=>invoke('ripple',{on}),
    edgeDisplay:()=>invoke('edge-display'),
    appInfo:()=>invoke('app-info'),
    onSystemTheme:callback=>listen('system-theme',callback),
    onDisplaysChanged:callback=>listen('displays-changed',callback),
    onDashboardStatus:callback=>listen('dashboard-status',callback),
  }), writable:false, configurable:false});
})();
