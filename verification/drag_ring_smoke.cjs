// 验证拖放事件的取消与去重，不调用引擎或读取真实文件。
const { build } = require('esbuild');
const { chromium } = require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');
const assert = require('node:assert/strict');

(async () => {
  const bundle = await build({stdin: {contents: `
    import {createDragRing} from './frontend/src/drag-ring';
    import {quickOptions} from './frontend/src/quick-options';
    window.quickOptions=quickOptions;
    window.calls=[]; window.errors=[];
    window.ring=createDragRing(async(files,option)=>window.calls.push({files,option}),()=>true,message=>window.errors.push(message));
  `, resolveDir: process.cwd()}, bundle:true, write:false, format:'iife'});
  const browser=await chromium.launch({channel:'msedge',headless:true});
  try {
    for (const deviceScaleFactor of [1,2]) {
    const page=await browser.newPage({viewport:{width:360,height:400},deviceScaleFactor});
    await page.setContent('<html><body></body></html>');
    await page.addStyleTag({path:'frontend/src/style.css'});
    await page.addStyleTag({path:'frontend/src/theme.css'});
    await page.addStyleTag({path:'frontend/src/appearance.css'});
    await page.addScriptTag({content:bundle.outputFiles[0].text});
    const pdfOptions=await page.evaluate(()=>quickOptions(['a.pdf','b.pdf']));
    assert.equal(pdfOptions.length,6);
    assert.equal(pdfOptions.filter(option=>option.separate).length,5);
    assert.equal(pdfOptions.find(option=>option.mode==='pdf_merge').separate,undefined);
    const result=await page.evaluate(async()=>{
      const files=['C:\\test\\sample.png'];
      const position=()=>{const r=document.querySelector('.ring-option').getBoundingClientRect();return {x:(r.left+r.width/2)*devicePixelRatio,y:(r.top+r.height/2)*devicePixelRatio};};
      ring.prepareDrag();ring.handle({type:'enter',paths:files,position:{x:180,y:180}});
      const before=document.querySelectorAll('.ring-option').length;
      const drop={type:'drop',paths:files,position:position()};
      ring.handle(drop);ring.handle(drop);ring.cancelDrag();
      const once=calls.length;
      ring.prepareDrag();ring.handle({type:'enter',paths:files,position:{x:180,y:180}});ring.cancelDrag();ring.handle({...drop,position:position()});
      const cancelled=calls.length;
      ring.prepareDrag();ring.handle({type:'enter',paths:files,position:{x:180,y:180}});
      const r=document.querySelector('.drag-ring').getBoundingClientRect();
      ring.handle({type:'drop',paths:files,position:{x:(r.left+4)*devicePixelRatio,y:(r.top+4)*devicePixelRatio}});
      const centerClosed=!document.querySelector('.drag-ring').open;
      ring.prepareDrag();ring.handle({type:'enter',paths:['C:\\test\\unsupported.txt'],position:{x:180,y:180}});
      const unsupportedClosed=!document.querySelector('.drag-ring').open;
      ring.prepareDrag();ring.handle({type:'enter',paths:files,position:{x:180,y:180}});ring.handle({type:'leave'});ring.handle({...drop,position:position()});
      return {before,once,cancelled,centerClosed,unsupportedClosed,final:calls.length};
    });
    assert.deepEqual(result,{before:6,once:1,cancelled:1,centerClosed:true,unsupportedClosed:true,final:1});
    await page.evaluate(()=>ring.prepareDrag());
    assert.equal(await page.locator('.drag-ring').evaluate(dialog=>{
      dialog.focus();return getComputedStyle(dialog).outlineStyle;
    }),'none');
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('.drag-ring').evaluate(dialog=>dialog.open),false);
    await page.close();
    console.log(`缩放 ${deviceScaleFactor}x 通过：6 个格式、重复 Drop 只提交一次、Esc/取消/菜单空白区/不支持类型/移出不提交。`);
    }
  } finally { await browser.close(); }
})().catch(error=>{console.error(error);process.exitCode=1;});
