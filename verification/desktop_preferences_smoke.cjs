// 使用本次运行专属的诊断应用和设置目录，验证关闭窗口和托盘偏好。
const {chromium}=require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');
const assert=require('node:assert/strict');
(async()=>{
  const browser=await chromium.connectOverCDP(process.env.BANKFLOW_SMOKE_CDP);
  const page=browser.contexts()[0].pages().find(p=>!p.url().includes('quick'));
  const invoke=(command,args={})=>page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});
  assert.match(process.env.BANKFLOW_SMOKE_APP_ID, /^local\.bankflow\.smoke\.t[0-9a-f]{32}$/);
  assert.equal(await invoke('plugin:app|identifier'),process.env.BANKFLOW_SMOKE_APP_ID);
  const previous=await invoke('desktop_preference');
  try {
    await invoke('save_desktop_preference',{action:'ask'});
    await invoke('plugin:event|emit_to',{target:{kind:'Webview',label:'main'},event:'confirm-close',payload:null});
    await page.locator('#close-confirmation').waitFor({state:'visible'});
    assert.equal(await page.locator('[data-remember]').isChecked(),false);
    await page.locator('#close-confirmation [data-action="tray"]').click();
    await page.locator('#close-confirmation').waitFor({state:'hidden'});
    assert.equal(await invoke('plugin:window|is_visible',{label:'main'}),false);
    assert.equal(await invoke('desktop_preference'),'ask');
    await invoke('show_tasks');
    await page.evaluate(()=>document.querySelectorAll('dialog[open]').forEach(d=>d.close()));
    await page.locator('#system-settings').click();
    await page.locator('input[name="close-action"][value="tray"]').check();
    await page.locator('#desktop-settings [data-save]').click();
    await page.getByText('设置已保存，下次关闭窗口时生效。').waitFor();
    assert.equal(await invoke('desktop_preference'),'tray');
    await page.locator('#desktop-settings [data-back]').click();
    await invoke('apply_close_choice',{action:'tray',remember:false});
    assert.equal(await invoke('plugin:window|is_visible',{label:'main'}),false);
    await invoke('show_tasks');
    await page.evaluate(()=>document.querySelectorAll('dialog[open]').forEach(d=>d.close()));
    await invoke('save_desktop_preference',{action:'ask'});
    await invoke('plugin:event|emit_to',{target:{kind:'Webview',label:'main'},event:'confirm-close',payload:null});
    await page.locator('#close-confirmation').waitFor({state:'visible'});
    await page.keyboard.press('Escape');
    await page.locator('#close-confirmation').waitFor({state:'hidden'});
    assert.equal(await invoke('plugin:window|is_visible',{label:'main'}),true);
    await assert.rejects(invoke('save_desktop_preference',{action:'invalid'}));
    console.log('通过：关闭询问事件、托盘隐藏/恢复、不记忆、保存设置、托盘操作隐藏、Esc 保留窗口、非法值拒绝。');
  } finally {
    await invoke('save_desktop_preference',{action:previous});
    // 先返回调试调用，再让应用正常退出并释放 WebView2 配置目录。
    await page.evaluate(() => {
      setTimeout(() => window.__TAURI_INTERNALS__.invoke('exit_application'), 0);
    });
    await browser.close();
  }
})().catch(e=>{console.error(e);process.exit(1);});
