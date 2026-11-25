const assert = require('node:assert/strict');
const {createHash} = require('node:crypto');
const fs = require('node:fs');
const {chromium} = require('playwright');

async function run() {
  const browser = await chromium.connectOverCDP(process.env.BANKFLOW_SMOKE_CDP);
  let main;
  try {
    const context = browser.contexts()[0];
    const localWindow = page => {
      const url = new URL(page.url());
      return url.hostname === 'tauri.localhost' && !url.searchParams.has('quick');
    };
    // WebView2 刚就绪时页面可能仍为 about:blank，等待首页导航后再检查实际渲染。
    const deadline = Date.now() + 10000;
    while (!(main = context.pages().find(localWindow))) {
      if (Date.now() >= deadline) {
        throw new Error('未加载内置工作台，实际页面为 ' + context.pages().map(p => p.url()).join(', '));
      }
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    await main.getByRole('heading', {name: /银行流水.*Excel/}).waitFor();
    assert.equal(await main.locator('.brand span').innerText(), '第二代');
    assert.equal(await main.locator('.version-tag span').innerText(), require('../package.json').version);
    await main.getByRole('button', {name: 'PDF 工具'}).click();
    await main.locator('#pdf-dialog').waitFor({state: 'visible'});
    assert.ok(await main.evaluate(() => window.__TAURI_INTERNALS__.invoke('desktop_preference')));
    await main.locator('#pdf-close').click();
    const source = process.env.BANKFLOW_SMOKE_STATEMENT;
    const drop = type => main.evaluate(({source, type}) =>
      window.__TAURI_INTERNALS__.invoke('plugin:event|emit_to', {
        target: {kind: 'Webview', label: 'main'}, event: 'tauri://drag-' + type,
        payload: {paths: [source], position: {x: 420, y: 320}},
      }), {source, type});
    await drop('enter');
    assert.equal(await main.locator('.drag-ring').isVisible(), false);
    await drop('drop');
    await main.waitForFunction(source => document.querySelector('#filepath')?.textContent === source, source);
    const snapshot = () => main.evaluate(() => window.__TAURI_INTERNALS__.invoke('task_snapshot'));
    const idle = await snapshot();
    assert.equal(idle.items.length, 0);
    await main.locator('#start').click();
    await main.locator('#completion').waitFor({state: 'visible'});
    await main.locator('#close-completion').click();
    const task = await snapshot();
    assert.equal(task.items[0].status, 'completed');
    assert.ok(fs.existsSync(task.items[0].output));
    const quick = context.pages().find(page => new URL(page.url()).searchParams.has('quick'));
    assert.ok(quick);
    await quick.waitForFunction(output => document.querySelector('#result-path')?.textContent === output, task.items[0].output, {polling: 100});
    assert.ok(Number.isInteger(task.elapsed_ms) && task.elapsed_ms >= 0);
    assert.match(await main.locator('#detail').innerText(), /耗时/);
    await main.locator('#show-log').click();
    const log = await main.locator('#log-content').innerText();
    assert.equal(log, task.logs.join('\n'));
    assert.match(log, /开始/);
    assert.match(log, /耗时/);
    await main.locator('#close-log').click();
    const files = [source, task.items[0].output];
    const digests = () => files.map(file => createHash('sha256').update(fs.readFileSync(file)).digest('hex'));
    const beforeClear = digests();
    await main.getByRole('button', {name: '清除本次', exact: true}).click();
    await main.waitForFunction(() => document.querySelector('#filepath')?.textContent === '');
    await quick.waitForFunction(() => document.querySelector('#result-path')?.textContent === '', null, {polling: 100});
    assert.deepEqual(await snapshot(), idle);
    assert.equal(await main.locator('#status').innerText(), '等待添加文件');
    assert.equal(await main.locator('#progress').evaluate(bar => bar.value), 0);
    assert.equal(await main.locator('#start').isDisabled(), true);
    assert.equal(await main.locator('#result-file').isHidden(), true);
    assert.equal(await main.locator('#result-folder').isHidden(), true);
    assert.equal(await main.locator('#result-path').textContent(), '');
    assert.doesNotMatch(await main.locator('#detail').innerText(), /耗时/);
    assert.equal(await main.locator('#log-content').textContent(), '暂无日志');
    assert.deepEqual(digests(), beforeClear);
    await drop('enter');
    await drop('drop');
    await main.waitForFunction(source => document.querySelector('#filepath')?.textContent === source, source);
    assert.equal(await main.locator('#start').isEnabled(), true);
    console.log(`发布版通过：导出 Excel，日志 ${task.logs.length} 行，耗时 ${task.elapsed_ms} 毫秒。清除本次后已复位、原件和成品校验一致，可再次选入。`);
  } finally {
    if (main) {
      await main.evaluate(() => {
        setTimeout(() => window.__TAURI_INTERNALS__?.invoke('exit_application'), 0);
      }).catch(() => {});
    }
    await browser.close();
  }
}

run().catch(error => { console.error(error); process.exitCode = 1; });
