// SPDX-License-Identifier: GPL-3.0-or-later
(async () => {
  try {
    const info = await edgeline.appInfo();
    const status = await edgeline.connected();
    const deadline = Date.now() + 12000;
    while ((document.getElementById('page-picture')?.textContent.length || 0) < 100 && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    const agent = status.connected ? await edgeline.call('state.all') : null;
    await window.__TAURI__.core.invoke('edgeline', {method:'host.test-report', params:{
      api:typeof edgeline.call === 'function', runtime:info.runtime, edgePresent:info.edgePresent,
      bodyLength:document.body.innerText.length,
      pictureLength:document.getElementById('page-picture')?.textContent.length || 0,
      connected:status.connected, agentState:!!(agent?.system?.version && typeof agent?.device?.present === 'boolean'),
      bootFunction:typeof window.boot, renderFunction:typeof window.render,
      ready:document.readyState, errors:window.__edgeSmokeErrors || [], apiTrace:window.__edgeApiTrace || [],
      appLoaded:typeof state!=='undefined'&&!!state.app.runtime, tab:typeof state!=='undefined'?state.tab:null,
      pageCount:typeof PAGES!=='undefined'?Object.keys(PAGES).length:0,
      pictureHtmlLength:document.getElementById('page-picture')?.innerHTML.length || 0,
      pictureTextLength:document.getElementById('page-picture')?.textContent.length || 0,
    }});
  } catch (error) {
    await window.__TAURI__.core.invoke('edgeline', {method:'host.test-report', params:{error:String(error)}});
  }
})();
