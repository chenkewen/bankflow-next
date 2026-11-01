const {build}=require('esbuild');
const {chromium}=require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');
const assert=require('node:assert/strict');
(async()=>{
  const bundle=await build({stdin:{contents:`import {createRingSettings,generalOptions} from './frontend/src/ring-settings'; import {quickOptions} from './frontend/src/quick-options'; import {createDragRing} from './frontend/src/drag-ring'; window.generalOptions=generalOptions; window.quickOptions=quickOptions; document.body.append(createRingSettings()); window.calls=[]; window.ring=createDragRing(async(p,o)=>calls.push(o),()=>true,()=>{});`,resolveDir:process.cwd()},bundle:true,write:false,format:'iife'});
  const browser=await chromium.launch({channel:'msedge',headless:true});
  try {
    const context=await browser.newContext({viewport:{width:700,height:850}});
    await context.route('http://ring.test/**',route=>route.fulfill({body:'<html><body></body></html>',contentType:'text/html'}));
    const page=await context.newPage(); await page.goto('http://ring.test');
    await page.addScriptTag({content:bundle.outputFiles[0].text});
    assert.deepEqual(await page.evaluate(()=>generalOptions(['a.pdf']).map(o=>o.mode)),['pdf_images','pdf_images','pdf_split','pdf_rotate']);
    assert.equal(await page.evaluate(()=>generalOptions(['a.pdf','b.pdf']).filter(o=>o.separate).length),4);
    assert.equal(await page.evaluate(()=>generalOptions(['a.pdf','b.pdf']).find(o=>o.mode==='pdf_merge').separate),false);
    assert.equal(await page.evaluate(()=>generalOptions(['a.pdf','b.png']).length),0);
    await page.getByRole('button',{name:'JPG上移',exact:true}).click();
    await page.locator('[name="ring-angle"][value="180"]').check();
    await page.locator('[data-save-ring]').click();
    assert.equal(await page.evaluate(()=>generalOptions(['a.pdf'])[0].format),'jpg');
    assert.equal(await page.evaluate(()=>generalOptions(['a.pdf']).find(o=>o.mode==='pdf_rotate').angle),180);
    await page.locator('[data-kind="image"]').click();
    await page.locator('[data-quality]').fill('70'); await page.locator('[data-width]').fill('800');
    await page.locator('[data-save-ring]').click();
    const other=await context.newPage();await other.goto('http://ring.test');await other.addScriptTag({content:bundle.outputFiles[0].text});
    const options=await other.evaluate(()=>generalOptions(['a.png']));assert.equal(options[0].width,800);assert.equal(options[1].quality,70);
    assert.equal(await page.evaluate(()=>quickOptions(['a.pdf'])[0].mode),'pdf');
    const profiles=await page.evaluate(()=>{
      const result=[];
      for(const profile of ['bank','general','bank']) {
        ring.prepareDrag(profile);ring.handle({type:'enter',paths:['a.pdf'],position:{x:180,y:180}});
        result.push({profile:document.querySelector('.drag-ring').dataset.profile,first:document.querySelector('.ring-option').textContent});ring.cancelDrag();
      }return result;
    });
    assert.deepEqual(profiles,[{profile:'bank',first:'Excel'},{profile:'general',first:'JPG'},{profile:'bank',first:'Excel'}]);
    await page.locator('[data-quality]').fill('0');await page.locator('[data-save-ring]').click();
    assert.match(await page.locator('[data-ring-status]').textContent(),/1–100/);
    assert.equal(await page.evaluate(()=>generalOptions(['a.png'])[0].quality),70);
    console.log('双圆盘通过：入口隔离、按类型过滤、PDF 批量/合并、排序与参数跨窗口保存、非法参数拒绝。');
  } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
