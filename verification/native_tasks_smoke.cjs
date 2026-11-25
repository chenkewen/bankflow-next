// 连接本次测试启动的独立 WebView2，调用真实 Tauri 命令和 Rust 引擎。
const {chromium}=require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const crypto=require('node:crypto');

(async()=>{
  const root=path.resolve(process.env.BANKFLOW_SMOKE_DIR);
  const sources=['a.pdf','broken.pdf','b.pdf'].map(name=>path.join(root,name));
  const hashes=sources.map(file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex'));
  const browser=await chromium.connectOverCDP(process.env.BANKFLOW_SMOKE_CDP);
  try {
    const pages=browser.contexts()[0].pages();
    const main=pages.find(page=>!page.url().includes('quick'));
    const quick=pages.find(page=>page.url().includes('quick'));
    const invoke=(page,command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
    const emit=(page,label,event,payload)=>invoke(page,'plugin:event|emit_to',{
      target:{kind:'Webview',label},event,payload,
    });
    const waitTask=async(predicate)=>{
      const deadline=Date.now()+60000;
      while(Date.now()<deadline){const state=await invoke(main,'task_snapshot');if(predicate(state))return state;await new Promise(resolve=>setTimeout(resolve,150));}
      throw Error('等待任务状态超时');
    };
    const finish=()=>waitTask(state=>!state.running);
    const closeDialogs=()=>Promise.all(pages.map(page=>page.evaluate(()=>document.querySelectorAll('dialog[open]').forEach(dialog=>dialog.close()))));
    await main.waitForFunction(()=>window.__TAURI_INTERNALS__ && document.querySelector('.drag-ring'));
    await quick.waitForFunction(()=>window.__TAURI_INTERNALS__ && document.querySelector('.drag-ring'));
    await closeDialogs();
    const pickerCenter=await main.locator('#pick').evaluate(button=>{
      const r=button.getBoundingClientRect();
      return {x:(r.left+r.width/2)*devicePixelRatio,y:(r.top+r.height/2)*devicePixelRatio};
    });
    const dragged={paths:[sources[0]],position:pickerCenter};
    const beforeDrop=await invoke(main,'task_snapshot');
    await emit(main,'main','tauri://drag-enter',dragged);
    await emit(main,'main','tauri://drag-over',{position:pickerCenter});
    assert.equal(await main.locator('.drag-ring').isVisible(),false);
    await emit(main,'main','tauri://drag-drop',dragged);
    await main.waitForFunction(name=>document.querySelector('#filename').textContent===name,path.basename(sources[0]));
    assert.equal(await main.locator('.drag-ring').isVisible(),false);
    assert.deepEqual(await invoke(main,'task_snapshot'),beforeDrop);
    await invoke(main,'start_conversion',{request:{input:sources[0],inputs:sources,mode:'pdf_images',format:'png',password:'',separate:true}});
    const batch=await finish();
    assert.deepEqual(batch.items.map(item=>item.status),['completed','error','completed']);
    for(const item of batch.items.filter(item=>item.output)) {
      assert.equal(path.dirname(item.output).toLowerCase(),root.toLowerCase());
      assert.ok(fs.existsSync(item.output));
    }
    assert.equal(fs.readdirSync(batch.items[0].output).filter(f=>f.endsWith('.png')).length,2);
    assert.equal(fs.readdirSync(batch.items[2].output).filter(f=>f.endsWith('.png')).length,1);
    await main.locator('#tasks-dialog').waitFor({state:'visible'});
    await closeDialogs();

    const cancelSource=path.join(root,'cancel-source.pdf');
    await invoke(main,'start_conversion',{request:{input:cancelSource,inputs:[cancelSource,sources[2]],mode:'pdf_images',format:'png',password:'',separate:true}});
    await assert.rejects(invoke(main,'exit_application'),error=>String(error).includes('任务仍在处理中'));
    const preference=await invoke(main,'desktop_preference');
    await assert.rejects(invoke(main,'apply_close_choice',{action:'exit',remember:true}),error=>String(error).includes('任务仍在处理中'));
    assert.equal(await invoke(main,'desktop_preference'),preference);
    await assert.rejects(invoke(quick,'start_conversion',{request:{input:sources[2],mode:'pdf_rotate',password:''}}),error=>String(error).includes('已有任务'));
    await invoke(main,'cancel_conversion');
    const cancelled=await finish();
    assert.equal(cancelled.items[1].status,'skipped');
    assert.ok(['cancelled','skipped'].includes(cancelled.items[0].status));
    await closeDialogs();

    const png=path.join(root,'gesture-sample.png');
    const quickEvent=(event,payload)=>emit(quick,'quick',event,payload);
    await quickEvent('quick-drag-begin',{id:1,profile:'general'});
    await quickEvent('tauri://drag-enter',{paths:[sources[0]],position:{x:180,y:180}});
    await quick.locator('.drag-ring[data-profile="general"][open] .ring-option').first().waitFor();
    await quickEvent('quick-drag-end',1);
    await quick.locator('.drag-ring').waitFor({state:'hidden'});
    await quickEvent('quick-drag-begin',{id:2,profile:'bank'});
    await quickEvent('tauri://drag-enter',{paths:[png],position:{x:180,y:180}});
    await quick.locator('.drag-ring[open]').waitFor();
    const position=await quick.locator('.ring-option').filter({hasText:/^JPG$/}).evaluate(button=>{const r=button.getBoundingClientRect();return{x:(r.left+r.width/2)*devicePixelRatio,y:(r.top+r.height/2)*devicePixelRatio};});
    const before=fs.readdirSync(path.dirname(png)).filter(f=>f.endsWith('.jpg')).length;
    await quickEvent('tauri://drag-drop',{paths:[png],position});
    await quickEvent('tauri://drag-drop',{paths:[png],position});
    await waitTask(s=>s.owner==='quick'&&s.items[0]?.input.endsWith('.png'));
    const ring=await finish();
    assert.equal(ring.items[0].status,'completed');
    assert.equal(fs.readdirSync(path.dirname(png)).filter(f=>f.endsWith('.jpg')).length,before+1);
    assert.equal(path.dirname(ring.items[0].output),path.dirname(png));
    await closeDialogs();
    for(let i=0;i<sources.length;i++) assert.equal(crypto.createHash('sha256').update(fs.readFileSync(sources[i])).digest('hex'),hashes[i]);
    console.log('真实桌面回归通过。普通拖入只选择流水，两个快捷手势各自打开对应圆盘，重复投放只生成一个 JPG。批量任务、取消及跨窗口互斥通过。');
  } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
