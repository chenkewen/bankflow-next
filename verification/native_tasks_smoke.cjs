// 连接本机诊断版，调用真实命令和冻结引擎；仅处理 build 下的合成样本。
const {chromium}=require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const crypto=require('node:crypto');

(async()=>{
  const root=path.resolve('build/batch-fixtures');
  const sources=['a.pdf','broken.pdf','b.pdf'].map(name=>path.join(root,name));
  const hashes=sources.map(file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex'));
  const browser=await chromium.connectOverCDP('http://127.0.0.1:9230');
  try {
    const pages=browser.contexts()[0].pages();
    const main=pages.find(page=>!page.url().includes('quick'));
    const quick=pages.find(page=>page.url().includes('quick'));
    const invoke=(page,command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
    const waitTask=async(predicate)=>{
      const deadline=Date.now()+60000;
      while(Date.now()<deadline){const state=await invoke(main,'task_snapshot');if(predicate(state))return state;await new Promise(resolve=>setTimeout(resolve,150));}
      throw Error('等待任务状态超时');
    };
    const finish=()=>waitTask(state=>!state.running);
    const closeDialogs=()=>Promise.all(pages.map(page=>page.evaluate(()=>document.querySelectorAll('dialog[open]').forEach(dialog=>dialog.close()))));
    await closeDialogs();
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
    await main.screenshot({path:'build/native-batch-results.png'});
    fs.writeFileSync('build/native-batch-results.json',JSON.stringify(batch,null,2));
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

    const png=path.resolve('build/gesture-fixtures/gesture-sample.png');
    const emit=(event,payload)=>invoke(quick,'plugin:event|emit_to',{target:{kind:'Webview',label:'quick'},event,payload});
    await emit('tauri://drag-enter',{paths:[png],position:{x:180,y:180}});
    await quick.locator('.drag-ring[open]').waitFor();
    const position=await quick.locator('.ring-option').filter({hasText:/^JPG$/}).evaluate(button=>{const r=button.getBoundingClientRect();return{x:(r.left+r.width/2)*devicePixelRatio,y:(r.top+r.height/2)*devicePixelRatio};});
    const before=fs.readdirSync(path.dirname(png)).filter(f=>f.endsWith('.jpg')).length;
    await emit('tauri://drag-drop',{paths:[png],position});
    await emit('tauri://drag-drop',{paths:[png],position});
    await waitTask(s=>s.owner==='quick'&&s.items[0]?.input.endsWith('.png'));
    const ring=await finish();
    assert.equal(ring.items[0].status,'completed');
    assert.equal(fs.readdirSync(path.dirname(png)).filter(f=>f.endsWith('.jpg')).length,before+1);
    assert.equal(path.dirname(ring.items[0].output),path.dirname(png));
    await closeDialogs();
    for(let i=0;i<sources.length;i++) assert.equal(crypto.createHash('sha256').update(fs.readFileSync(sources[i])).digest('hex'),hashes[i]);
    console.log('原生引擎验证通过：2 成功/1 失败且继续执行、图片页数正确、源文件未改动、跨窗口互斥、取消跳过后续、重复圆盘 Drop 只生成 1 个 JPG。');
  } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
