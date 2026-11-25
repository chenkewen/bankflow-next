// 使用真实 Edge 渲染主窗口与悬浮窗，模拟原生对话框和任务事件。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { build } = require('esbuild');
const { chromium } = require(process.env.BANKFLOW_PLAYWRIGHT_PATH || 'playwright');

async function run() {
  const app = await build({ entryPoints: ['frontend/src/main.ts'], bundle: true,
    write: false, format: 'iife', loader: { '.css': 'empty' } });
  const mocks = await build({ stdin: { resolveDir: process.cwd(), contents: `
    import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
    import { emit } from '@tauri-apps/api/event';
    import { quickOptions } from './frontend/src/quick-options';
    window.isTauri = true;
    const owner = location.search.includes('quick') ? 'quick' : 'main';
    mockWindows(owner);
    window.calls = [];
    window.options = quickOptions;
    window.emitTask = event => emit('conversion', event);
    window.emitDropEvent = (type, payload) => emit('tauri://drag-' + type, payload);
    mockIPC(async (command, args) => {
      window.calls.push([command, args]);
      if (command === 'task_snapshot') {
        if (location.search.includes('race')) await new Promise(r => window.resumeSnapshot = r);
        return {running:false,owner,items:[{input:'A.pdf',status:'completed',output:'A.xlsx',message:null,elapsed_ms:18}],
          logs:['开始转换 A.pdf','已导出 A.xlsx'],elapsed_ms:21};
      }
      if (command === 'plugin:dialog|open') return window.pick;
      if (command === 'clear_task') {
        const snapshot = {running:false, owner:'', items:[], logs:[], elapsed_ms:null};
        await window.emitTask({kind:'cleared', snapshot});
        return snapshot;
      }
      if (command === 'quick_selection') return [];
      return null;
    }, {shouldMockEvents:true});` }, bundle: true, write: false, format: 'iife' });

  const browser = await chromium.launch({ channel: 'msedge', headless: true });
  async function open(query = '') {
    const page = await browser.newPage({ viewport: query.includes('quick')
      ? { width: 360, height: 400 } : { width: 1120, height: 820 } });
    await page.route('http://workbench.test/**', route => route.fulfill({
      contentType: 'text/html', body: '<html><body><div id="app"></div></body></html>' }));
    await page.goto('http://workbench.test/' + query);
    for (const css of ['style', 'theme', 'appearance']) {
      await page.addStyleTag({ content: fs.readFileSync('frontend/src/' + css + '.css', 'utf8') });
    }
    await page.addScriptTag({ content: mocks.outputFiles[0].text });
    await page.addScriptTag({ content: app.outputFiles[0].text });
    return page;
  }
  try {
    const page = await open();
    await page.locator('#result-file').waitFor();
    assert.equal(await page.locator('#status').textContent(), '已导出');
    assert.match(await page.locator('#detail').textContent(), /耗时 21 毫秒/);
    await page.click('#show-log');
    assert.equal(await page.locator('#log-content').textContent(), '开始转换 A.pdf\n已导出 A.xlsx');
    await page.click('#close-log');
    await page.click('#clear-current');
    await page.waitForFunction(() => document.querySelector('#status').textContent === '等待添加文件');
    assert.equal(await page.locator('#result-file').isHidden(), true);
    await page.waitForFunction(() => calls.some(([command]) => command === 'drag_status'));
    const position = await page.locator('#pick').evaluate(button => {
      const r = button.getBoundingClientRect();
      return {x:(r.left + r.width / 2) * devicePixelRatio, y:(r.top + r.height / 2) * devicePixelRatio};
    });
    const pdf = {paths:['dragged.pdf'], position};
    const drag = (type, payload) => page.evaluate(([type, payload]) => emitDropEvent(type, payload), [type, payload]);
    await drag('enter', pdf);
    await drag('over', {position});
    await drag('leave', {});
    await drag('enter', pdf);
    assert.equal(await page.locator('.drag-ring').isVisible(), false);
    assert.notEqual(await page.locator('#filename').textContent(), 'dragged.pdf');
    await drag('drop', pdf);
    assert.equal(await page.locator('#filename').textContent(), 'dragged.pdf');
    assert.equal(await page.locator('.drag-ring').isVisible(), false);
    assert.equal(await page.evaluate(() => calls.some(([c]) => c === 'start_conversion')), false);
    await page.fill('#password', 'temporary');
    await page.click('#clear-current');
    await page.waitForFunction(() => document.querySelector('#filepath').textContent === '');
    assert.equal(await page.locator('#start').isDisabled(), true);
    assert.equal(await page.locator('#password').inputValue(), '');
    await page.evaluate(() => { window.pick = 'B.webp'; });
    await page.click('#pick');
    assert.equal(await page.locator('#filename').textContent(), 'B.webp');
    assert.equal(await page.locator('#result-file').isHidden(), true);
    assert.equal(await page.locator('#result-folder').isHidden(), true);
    assert.equal(await page.locator('#result-path').textContent(), '');
    assert.equal(await page.locator('input[name="mode"][value="ocr"]').isChecked(), true);
    const types = await page.evaluate(() => calls.find(([c]) => c === 'plugin:dialog|open')[1].options.filters[0].extensions);
    assert.ok(types.includes('webp'));
    assert.deepEqual(await page.evaluate(() => options(['A.WEBP','B.png']).find(o => o.mode === 'ocr')),
      {label:'分别 OCR',mode:'ocr',separate:true});
    assert.equal(await page.evaluate(() => options(['A.psd']).some(o => o.mode === 'ocr')), false);
    assert.ok(await page.evaluate(() => options(['A.ppm','B.ico','C.tga','D.im']).length > 0));

    await page.click('#image-tools');
    await page.locator('input[name="image-format"][value="jpg"]').check();
    await page.locator('#image-quality').fill('0');
    await page.evaluate(() => { window.pick = ['C.png']; });
    await page.click('#image-pick');
    await page.click('#image-run');
    assert.match(await page.locator('#image-error').textContent(), /质量/);
    assert.equal(await page.locator('#image-quality').evaluate(el => el === document.activeElement), true);
    await page.locator('input[name="image-format"][value="png"]').check();
    await page.click('#image-run');
    const request = await page.evaluate(() => calls.find(([c]) => c === 'start_conversion')[1].request);
    assert.equal(request.quality, 90);
    assert.equal(request.format, 'png');

    const quick = await open('?quick=1');
    await quick.waitForFunction(() => calls.some(([c]) => c === 'quick_selection'));
    await quick.evaluate(() => emitTask({kind:'completed',output:'B.xlsx',snapshot:{running:false,owner:'quick',
      items:[{input:'B.pdf',status:'completed',output:'B.xlsx',message:null,elapsed_ms:1199}],
      elapsed_ms:1200,logs:['开始转换 B.pdf','已导出 B.xlsx']}}));
    await quick.locator('#completion').waitFor({state:'visible'});
    assert.match(await quick.locator('#completion').textContent(), /耗时 1\.2 秒/);
    await quick.click('#open-folder');
    assert.deepEqual(await quick.evaluate(() => calls.find(([c]) => c === 'open_result')[1]),
      {path:'B.xlsx', folder:true});

    for (const [statuses, title, detail] of [
      [['error', 'error'], '全部任务失败', '共 2 份，成功 0 份，失败 2 份'],
      [['completed', 'error'], '部分任务失败', '共 2 份，成功 1 份，失败 1 份'],
      [['completed', 'completed'], '已完成', '共 2 份，成功 2 份，失败 0 份'],
      [['completed', 'cancelled', 'skipped'], '已取消', '共 3 份，成功 1 份，失败 0 份，已取消 1 份，未执行 1 份'],
      [['error', 'skipped'], '部分任务失败', '共 2 份，成功 0 份，失败 1 份，未执行 1 份'],
    ]) {
      const items = statuses.map((status, index) => ({input: `${index}.pdf`, status, output: null, message: null}));
      const snapshot = {running: false, owner: 'quick', items, logs: ['本次批量任务结束'], elapsed_ms: 1075};
      for (const target of [page, quick]) {
        await target.evaluate(snapshot => emitTask({kind: 'batchcompleted', snapshot}), snapshot);
        assert.equal(await target.locator('#status').textContent(), title);
        assert.equal(await target.locator('#result-file').isHidden(), true);
        assert.equal(await target.locator('#tasks-dialog [data-summary]').textContent(), `${title} · ${detail} · 耗时 1.075 秒`);
        assert.match(await target.locator('#detail').textContent(), /耗时 1\.075 秒/);
        assert.equal(await target.locator('#tasks-dialog').isVisible(), target === page);
      }
    }

    await page.locator('#tasks-dialog [data-close]').click();
    await page.click('#show-log');
    for (const [kind, title, message] of [['error','转换失败','输入文件不存在'], ['cancelled','已取消','任务已取消']]) {
      await page.evaluate(({kind, message}) => emitTask({kind,message,snapshot:{running:false,owner:'main',elapsed_ms:0,
        items:[{input:'sample.pdf',status:kind,output:null,message,elapsed_ms:0}],logs:['开始转换', message]}}), {kind,message});
      assert.equal(await page.locator('#status').textContent(), title);
      assert.equal(await page.locator('#detail').textContent(), `${message} · 耗时 0 毫秒`);
      assert.equal(await page.locator('#log-content').textContent(), `开始转换\n${message}`);
    }
    await page.click('#close-log');

    const race = await open('?race=1');
    await race.waitForFunction(() => typeof window.resumeSnapshot === 'function');
    await race.evaluate(async () => {
      await emitTask({kind:'started',snapshot:{running:true,owner:'quick',items:[],logs:[]}});
      window.resumeSnapshot();
    });
    await race.waitForFunction(() => calls.some(([c]) => c === 'shortcut_status'));
    assert.equal(await race.locator('#pick').isDisabled(), true);
    assert.equal(await race.locator('#result-file').isHidden(), true);
    assert.ok(await race.locator('#clear-current').isHidden() || await race.locator('#clear-current').isDisabled());
    console.log('主窗口、快捷窗回归通过：结果归属、启动竞态、图片 OCR、格式过滤、质量校验、快捷结果及批量任务状态。');
  } finally {
    await browser.close();
  }
}
run().catch(error => { console.error(error); process.exitCode = 1; });
