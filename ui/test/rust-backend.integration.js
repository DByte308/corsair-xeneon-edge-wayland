// SPDX-License-Identifier: GPL-3.0-or-later
// Explicit desktop integration test. Reads KWin state; never moves/activates apps.
'use strict';
const assert=require('node:assert/strict');
const {execFileSync}=require('node:child_process');
const {RustBackend}=require('../rust-backend');
(async()=>{
  const client=new RustBackend();
  try {
    const info=await client.call('backend.info');
    assert.equal(info.language,'Rust');
    await client.call('window.start');
    let snapshot;
    for(let i=0;i<30;i++) {
      snapshot=await client.call('window.snapshot');
      if(snapshot.available)break;
      await new Promise(resolve=>setTimeout(resolve,100));
    }
    assert.equal(snapshot.available,true,'Connect a unique 2560×720 Edge for this integration test');
    const replies=await Promise.all(Array.from({length:10},()=>client.call('backend.info')));
    assert(replies.every(reply=>reply.language==='Rust'));
    await assert.rejects(client.call('window.activate',{id:'not-a-uuid'}));
    await assert.rejects(client.call('run-arbitrary-command'));
    let denied=false;
    try {
      execFileSync('busctl',['--user','call',`dev.edgeline.Windows.rust.p${client.child.pid}`,
        '/Windows','dev.edgeline.Windows','Update','s','{"available":false,"windows":[]}'],{stdio:'pipe'});
    } catch(error) { denied=/Only KWin|Access.?denied/i.test(error.stderr.toString()); }
    assert(denied,'Only KWin may supply window reports');
    const art=await client.call('launchpad.action',{action:'artwork'});
    assert(art.image===null||art.image.startsWith('data:image/jpeg;base64,'));
    console.log(JSON.stringify({rustBackend:true,edgeAvailable:snapshot.available,
      actualEdgeWindows:snapshot.windows.length,concurrentRequests:replies.length,
      untrustedDbusDenied:denied,windowMutations:false}));
  } finally { await client.stop(); }
})().catch(error=>{console.error(error);process.exitCode=1;});
