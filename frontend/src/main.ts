import { version as appVersion } from '../../package.json';
import { imageInputExtensions, imageOutputFormats, ocrImageExtensions } from './quick-options';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { cancelConversion, onConversion, startConversion, type TaskSnapshot } from './api';
import { createTaskPanel, summarizeTasks, withElapsed } from './task-panel';
import { createDesktopSettings } from './desktop-settings';
import './style.css';
import './theme.css';
import './appearance.css';
import { initializeAppearance } from './appearance';
initializeAppearance();
import { createDragRing } from './drag-ring';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { listen } from '@tauri-apps/api/event';
const quickWindow = new URLSearchParams(location.search).has('quick');

const icons = {
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6M8 13h8M8 17h5"/>',
  image: '<rect x="3" y="3" width="18" height="18" rx="3"/><circle cx="8" cy="8" r="1.5"/><path d="m21 15-5-5L5 21"/>',
  shield: '<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z"/><path d="m8 12 3 3 5-6"/>',
  folder: '<path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  arrow: '<path d="M5 12h14m-5-5 5 5-5 5"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  settings: '<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3"/><circle cx="15" cy="17" r="3"/>',
  logout: '<path d="M9 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4M14 8l4 4-4 4m-7-4h11"/>',
  lock: '<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3m-4 4v3"/>',
};
const icon = (name: keyof typeof icons) => `<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name]}</svg>`;

document.querySelector<HTMLDivElement>('#app')!.innerHTML = `
  <a class="skip-link" href="#workspace">跳转到工作区</a>
  <aside class="sidebar">
    <div class="brand"><img class="brand-mark" src="/brand/app_icon.png" alt="" width="52" height="52" /><div>BankFlow<span>第二代</span></div></div>
    <nav aria-label="工具导航"><p class="nav-label">我的工作台</p><a class="nav-item selected" href="#workspace" aria-current="page" aria-label="银行流水">${icon('file')}<span>银行流水</span><span class="nav-dot"></span></a><p class="nav-label next-label">更多工具</p><button id="pdf-tools" aria-label="PDF 工具" class="nav-item tool-nav">${icon('folder')}<span>PDF 工具</span></button><button id="image-tools" aria-label="图片转换" class="nav-item tool-nav">${icon('image')}<span>图片转换</span></button><button id="tasks-tools" aria-label="转换任务" class="nav-item tool-nav">${icon('clock')}<span>转换任务</span></button></nav>
    <div class="sidebar-bottom"><div class="local-card"><div class="privacy-heading">${icon('shield')}<strong>文件不上传</strong></div><p>离线处理，原文件保留。</p></div><button id="system-settings" class="nav-item system-settings" aria-label="系统设置">${icon('settings')}<span>系统设置</span></button><div class="sidebar-utilities"><span class="version-tag" aria-label="当前版本 ${appVersion}">桌面版 <span>${appVersion}</span></span><button id="quit-app" class="quit-app" title="退出应用；有任务时会先提示">${icon('logout')}<span>退出</span></button></div></div>
  </aside>
  <div class="main-shell">
    <div class="topbar"><span>文件工作台 <span class="separator">/</span> <strong>银行流水</strong></span><button id="quick-pick" class="secondary">快捷转换 · 拖入文件</button><span class="offline-badge"><span></span> 本地离线处理</span></div>
    <main id="workspace" tabindex="-1">
      <header><div><p class="eyebrow">BANKFLOW / DOCUMENT WORKSPACE</p><h1>银行流水，轻松转<span>Excel</span></h1><p class="subtitle">导入一份流水，得到一份清晰、可编辑的表格。</p></div></header>
      <ol class="steps" aria-label="转换步骤"><li class="current"><span>1</span>选择文件</li><li><span>2</span>确认设置</li><li><span>3</span>获取 Excel</li></ol>
      <div class="workspace-grid">
        <section class="panel upload-panel" aria-labelledby="upload-title"><div class="section-heading"><h2 id="upload-title">添加银行流水</h2><span class="section-meta">每次一个文件</span></div>
          <button id="pick" class="file-picker" aria-describedby="file-support"><img class="document-illustration" src="/brand/document-flow.svg" alt="" aria-hidden="true" width="240" height="160" /><strong id="filename">选择银行流水，开始整理</strong><span class="picker-description">支持电子 PDF、扫描件和流水图片</span><span class="choose-file">${icon('folder')} 选择文件</span></button>
          <p id="filepath" class="path"></p><div id="file-support" class="file-support"><span>支持格式</span><span>PDF</span><span>PNG / JPG / WebP</span><span>BMP / TIFF</span></div><div class="file-assurance">${icon('shield')}<span>原文件保留不变，安心处理每一份流水。</span></div>
        </section>
        <section class="panel settings-panel" aria-labelledby="settings-title"><div class="section-heading"><h2 id="settings-title">转换设置</h2><span class="section-meta">按需调整</span></div>
          <div class="fields"><fieldset id="mode" class="mode-options" aria-describedby="mode-help"><legend>选择文件类型</legend><label class="mode-option"><input type="radio" name="mode" value="pdf" checked /><span class="mode-symbol">${icon('file')}</span><span><strong>电子 PDF</strong><small>可选中文字，直接提取</small></span><span class="radio-indicator"></span></label><label class="mode-option"><input type="radio" name="mode" value="ocr" /><span class="mode-symbol">${icon('image')}</span><span><strong>扫描件 / 图片</strong><small>识别照片与扫描文字</small></span><span class="radio-indicator"></span></label></fieldset><p id="mode-help" class="field-help">能选中文字的 PDF，通常选择直接提取。</p><label for="password">PDF 密码 <span class="optional">可选</span><input id="password" type="password" autocomplete="off" placeholder="文件加密时填写" aria-describedby="password-help" /></label><p id="password-help" class="field-help">${icon('lock')} 仅用于本次打开文件，不保存密码。</p></div>
          <div class="output-row"><div><strong>保存到</strong><p id="output-label">原文件所在目录</p></div><button id="output" class="secondary">更改</button></div>
        </section>
      </div>
      <section class="panel result" aria-labelledby="task-title" data-state="idle"><div class="task-icon">${icon('clock')}</div><div class="task-content"><div class="result-top"><h2 id="task-title">转换任务</h2><div class="result-actions"><button id="result-file" class="secondary" hidden>打开文件</button><button id="result-folder" class="secondary" hidden>打开目录</button><button id="show-log" class="secondary">查看日志</button><button id="clear-current" class="secondary" title="清除当前选中的流水和本次任务记录，保留原文件及导出文件" disabled>清除本次</button><span id="status">等待添加文件</span></div></div><p id="detail" role="status">准备好文件后，就可以开始了。</p><progress id="progress" max="100" value="0" aria-label="转换进度"></progress><p id="result-path" class="path"></p></div></section>

    </main>
      <footer><p id="environment">${icon('lock')} 文件不上传，处理更安心</p><div class="footer-actions"><button id="cancel" class="secondary" hidden>取消转换</button><button id="start" class="primary" disabled><span id="start-label">开始转换</span>${icon('arrow')}</button></div></footer>
  </div>
  <dialog id="pdf-dialog" aria-labelledby="pdf-title"><h2 id="pdf-title">PDF 工具</h2><p>本地处理，保留原文件。结果默认保存到第一个文件旁。</p>
  <fieldset id="pdf-controls"><legend>操作</legend><div class="pdf-operations">
  <label><input type="radio" name="pdf-operation" value="merge" checked>合并</label><label><input type="radio" name="pdf-operation" value="split">逐页拆分</label><label><input type="radio" name="pdf-operation" value="extract">提取页面</label><label><input type="radio" name="pdf-operation" value="rotate">旋转</label></div>
  <button id="pdf-pick" class="secondary">选择 PDF</button><ol id="pdf-files" aria-label="待处理文件顺序"></ol>
  <label id="pdf-pages-label">页码范围（留空表示全部）<input id="pdf-pages" placeholder="例如：1-3,5" /></label>
  <fieldset id="pdf-angle-label" class="format-picker" hidden><legend>顺时针旋转角度</legend><div class="angle-options">${[90,180,270].map(angle=>`<label class="format-card"><input type="radio" name="pdf-angle" value="${angle}" ${angle===90?'checked':''}><span class="format-card-title">${angle}°</span></label>`).join('')}</div></fieldset>
  <p class="field-help">页面工具不保留原文件加密或数字签名。</p><label>PDF 密码（可选，合并时用于所有文件）<input id="pdf-password" type="password" autocomplete="off" /></label></fieldset>
  <p id="pdf-error" role="status"></p><div class="dialog-actions"><button id="pdf-run" class="primary">开始处理</button><button id="pdf-close" class="secondary">返回</button></div></dialog>
  <dialog id="image-dialog" aria-labelledby="image-title"><h2 id="image-title">图片转换</h2><p>离线处理，保留原文件；批量结果保存到新目录。</p>
  <fieldset class="image-fields"><legend>操作</legend><div class="pdf-operations"><label><input type="radio" name="image-mode" value="image_convert" checked>图片格式转换</label><label><input type="radio" name="image-mode" value="images_pdf">图片合并 PDF</label><label><input type="radio" name="image-mode" value="pdf_images">PDF 转图片</label></div>
  <button id="image-pick" class="secondary">选择文件</button><ol id="image-files" aria-label="图片顺序"></ol>
  <fieldset id="image-format-label" class="format-picker"><legend>输出格式</legend><div class="format-grid">${imageOutputFormats.map(({format:value,description})=>`<label class="format-card"><input type="radio" name="image-format" value="${value}" ${value==='png'?'checked':''}><span class="format-card-title">${value.toUpperCase()}<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5 10 3 3 7-7"/></svg></span><span class="format-card-description">${description}</span></label>`).join('')}</div><p id="image-format-help" class="field-help">选择适合用途的格式；PNG 可保留透明背景。</p></fieldset>
  <label>输出宽度（像素，0 表示默认，等比例缩放）<input id="image-width" aria-describedby="image-error" type="number" min="0" max="16000" value="0"></label>
  <label id="image-quality-label">JPG / WEBP 质量（1–100）<input id="image-quality" aria-describedby="image-error" type="number" min="1" max="100" value="90"></label>
  <label id="image-password-label" hidden>PDF 密码（可选）<input id="image-password" type="password" autocomplete="off"></label>
  <p class="field-help">PDF 默认按 144 DPI 转图片。图片转 PDF 时透明区域使用白色背景；暂不支持动画和多帧图片。</p></fieldset>
  <p id="image-error" role="status"></p><div class="dialog-actions"><button id="image-run" class="primary">开始处理</button><button id="image-close" class="secondary">返回</button></div></dialog>
  <dialog id="completion" aria-labelledby="completion-title"><h2 id="completion-title">转换完成</h2><p>结果已保存，你想如何查看？</p><p id="open-error" role="status"></p><div class="dialog-actions"><button id="open-file" class="primary">打开文件</button><button id="open-folder" class="secondary">打开目录</button><button id="close-completion" class="secondary">稍后查看</button></div></dialog><dialog id="log-dialog" aria-labelledby="log-title"><div class="section-heading"><h2 id="log-title">转换日志</h2><button id="close-log" class="secondary">关闭</button></div><pre id="log-content" aria-live="polite">暂无日志</pre></dialog>`;

const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const start = el<HTMLButtonElement>('start');
const cancel = el<HTMLButtonElement>('cancel');
const clearCurrent = el<HTMLButtonElement>('clear-current');
const progress = el<HTMLProgressElement>('progress');
const mode = el<HTMLFieldSetElement>('mode');
const selectedMode = () => document.querySelector<HTMLInputElement>('input[name="mode"]:checked')!.value as 'pdf' | 'ocr';
const password = el<HTMLInputElement>('password');
let input = '';
let outputDir: string | null = null;
let busy = false;
let ready = false;
let resultPath = '';
let taskRevision = 0;
let taskSnapshot: TaskSnapshot = {running:false,owner:'',items:[],logs:[],elapsed_ms:null};
const logDialog=el<HTMLDialogElement>('log-dialog');
clearCurrent.onclick = async () => {
  clearCurrent.disabled = true;
  try {
    await invoke('clear_task');
  } catch (error) {
    clearCurrent.disabled = busy || !ready || (!input && !taskSnapshot.items.length);
    showError(error);
  }
};
function renderLogs() { el('log-content').textContent=taskSnapshot.logs.length ? taskSnapshot.logs.join('\n') : '暂无日志'; }
function displayResult(path = '') {
  resultPath = path;
  el('result-path').textContent = path;
  for (const id of ['result-file','result-folder']) el(id).hidden = !path;
}
el('show-log').onclick=()=>{renderLogs();logDialog.showModal();};
el('close-log').onclick=()=>logDialog.close();
for(const [id,folder] of [['result-file',false],['result-folder',true]] as const) {
  el(id).onclick=async()=>{if (!resultPath) return; try {await invoke('open_result',{path:resultPath,folder});} catch(error) {showError(error);}};
}
const taskPanel=createTaskPanel();
const desktopSettings=quickWindow?null:createDesktopSettings();
el('system-settings').onclick=()=>{void desktopSettings?.open();};
el('quit-app').onclick=async()=>{try{await invoke('exit_application');}catch(error){showError(error);}};
el('tasks-tools').onclick=()=>taskPanel.open();
const completion = el<HTMLDialogElement>('completion');
if (quickWindow) document.body.append(completion);
el('close-completion').onclick = () => completion.close();
completion.addEventListener('close',()=>{if(quickWindow) void getCurrentWindow().hide();});
for (const [id, folder] of [['open-file', false], ['open-folder', true]] as const) {
  el(id).onclick = async () => {
    if (!resultPath) return;
    try { await invoke('open_result', { path: resultPath, folder }); completion.close(); }
    catch (error) { el('open-error').textContent = String(error); }
  };
}


function setStep(step: number) {
  document.querySelectorAll('.steps li').forEach((item, index) => {
    item.classList.toggle('current', index === step - 1);
    if (index === step - 1) item.setAttribute('aria-current', 'step');
    else item.removeAttribute('aria-current');
  });
}

function setBusy(value: boolean) {
  if (value && !busy) displayResult();
  busy = value;
  for (const id of ['pick', 'output', 'mode', 'password']) {
    (el(id) as HTMLButtonElement | HTMLInputElement | HTMLFieldSetElement).disabled = value;
  }
  start.disabled = value || !input || !ready;
  clearCurrent.disabled = value || !ready || (!input && !taskSnapshot.items.length);
  cancel.hidden = !value;
  cancel.disabled = false;
  el('start-label').textContent = value ? '转换中…' : '开始转换';
  document.querySelector<HTMLElement>('.result')!.dataset.state = value ? 'processing' : 'idle';
  setStep(input ? 2 : 1);
}

function showError(error: unknown) {
  document.querySelector<HTMLElement>('.result')!.dataset.state = 'error';
  el('status').textContent = '操作失败';
  el('detail').textContent = String(error);
  if(quickWindow) {
    el('quick-error').textContent=String(error);
    const notice=el<HTMLDialogElement>('quick-notice');if(!notice.open) notice.showModal();
    void invoke('quick_present').catch(console.error);
  }
}

function selectStatement(path: string) {
  const extension = path.match(/\.([^.\\/]+)$/)?.[1]?.toLowerCase();
  if (extension !== 'pdf' && !ocrImageExtensions.includes(extension ?? '')) {
    showError('请选择 PDF 或支持的流水图片。');
    return;
  }
  input = path;
  el('filename').textContent = path.split(/[\\/]/).pop()!;
  el('filepath').textContent = path;
  if (extension !== 'pdf') document.querySelector<HTMLInputElement>('input[name="mode"][value="ocr"]')!.checked = true;
  mode.dispatchEvent(new Event('change'));
  el('task-title').textContent = '转换任务';
  el('status').textContent = '准备就绪';
  el('detail').textContent = '可以开始转换。扫描版 PDF 请手动选择 OCR。';
  displayResult();
  progress.value = 0;
  setBusy(false);
}

el('pick').onclick = async () => {
  try {
    const revision = taskRevision;
    const path = await open({ multiple: false, filters: [{ name: '流水文件', extensions: ['pdf', ...ocrImageExtensions] }] });
    if (path && !busy && taskRevision === revision) selectStatement(path);
  } catch (error) { showError(error); }
};

mode.onchange = () => {
  el('mode-help').textContent = selectedMode() === 'ocr'
    ? '适合扫描件和照片，识别耗时通常比直接提取更长。'
    : '能选中文字的 PDF，通常选择直接提取。';
};

el('output').onclick = async () => {
  try {
    const path = await open({ directory: true, multiple: false });
    if (path) { outputDir = path; el('output-label').textContent = path; }
  } catch (error) { showError(error); }
};

start.onclick = async () => {
  if (busy || !input) return;
  setBusy(true);
  el('task-title').textContent = '转换任务';
  el('status').textContent = '正在启动';
  el('detail').textContent = '正在加载解析引擎，首次 OCR 加载可能需要稍候。';
  progress.removeAttribute('value');
  try {
    await startConversion({ input, output_dir: outputDir, mode: selectedMode(), password: password.value });
  } catch (error) { setBusy(taskSnapshot.running); progress.value = 0; showError(error); }
  finally { password.value = ''; }
};

cancel.onclick = async () => {
  cancel.disabled = true;
  try {
    await cancelConversion();
    el('status').textContent = '正在取消';
    el('detail').textContent = '等待当前解析步骤结束，不导出未完成结果。';
  } catch (error) { cancel.disabled = false; showError(error); }
};

function renderTask(snapshot: TaskSnapshot) {
  taskPanel.update(snapshot);
  setBusy(snapshot.running);
  if (logDialog.open) renderLogs();
  if (!snapshot.items.length) return;
  const batch = snapshot.items.length > 1;
  el('task-title').textContent = batch ? '批量转换' : '转换任务';
  if (snapshot.running) {
    const active = snapshot.items.findIndex(item => item.status === 'running');
    el('status').textContent = '正在处理';
    el('detail').textContent = active < 0 ? '正在准备任务' :
      `正在处理 ${active + 1} / ${snapshot.items.length}：${snapshot.items[active].input.split(/[\\/]/).pop()}`;
    return;
  }
  const { title, detail, state } = summarizeTasks(snapshot);
  const item = snapshot.items[0];
  const success = state === 'completed';
  displayResult(!batch && success ? item.output ?? '' : '');
  el('status').textContent = !batch && success ? '已导出' : title;
  const message = batch ? detail : item.message ?? (success ? '结果已保存，可打开文件或所在目录查看。' : title);
  el('detail').textContent = withElapsed(message, snapshot.elapsed_ms);
  document.querySelector<HTMLElement>('.result')!.dataset.state = state;
  if (success) setStep(3);
  progress.value = batch || success ? 100 : 0;
}

function resetCurrentTask() {
  input = '';
  password.value = '';
  el('filename').textContent = '选择银行流水，开始整理';
  el('filepath').textContent = '';
  document.querySelector<HTMLInputElement>('input[name="mode"][value="pdf"]')!.checked = true;
  mode.dispatchEvent(new Event('change'));
  displayResult();
  el('open-error').textContent = '';
  el('task-title').textContent = '转换任务';
  el('status').textContent = '等待添加文件';
  el('detail').textContent = '准备好文件后，就可以开始了。';
  progress.value = 0;
  renderLogs();
  if (completion.open) completion.close();
  if (quickWindow) {
    el<HTMLDialogElement>('quick-notice').close();
    el('quick-error').textContent = '';
  }
}

async function initialize() {
  if (!isTauri()) {
    el('environment').textContent = '界面预览 · 转换功能请使用桌面版';
    el<HTMLButtonElement>('pick').disabled = true;
    el<HTMLButtonElement>('output').disabled = true;
    return;
  }
  try {
    await onConversion(event => {
      if(event.kind==='log') { taskSnapshot.logs.push(event.message ?? ''); if(taskSnapshot.logs.length>1000) taskSnapshot.logs.shift(); if(logDialog.open) renderLogs(); return; }
      if(event.snapshot) {
        taskRevision++;
        taskSnapshot=event.snapshot;
        if (event.kind === 'cleared') {
          resetCurrentTask();
          renderTask(taskSnapshot);
          if (!quickWindow && !document.querySelector('dialog[open]')) el('pick').focus();
          return;
        }
        renderTask(taskSnapshot);
        const ownTask = taskSnapshot.owner === (quickWindow ? 'quick' : 'main');
        if(event.kind==='started') {
          if(completion.open) completion.close();
          displayResult();
          if(taskSnapshot.items.length>1 && quickWindow && ownTask) void invoke('show_tasks').catch(showError);
        }
        if(event.kind==='batchcompleted' && !quickWindow) taskPanel.open();
        if (event.kind === 'completed' && ownTask) {
          completion.querySelector('p')!.textContent = withElapsed('结果已保存', taskSnapshot.elapsed_ms);
          el('open-error').textContent = '';
          completion.showModal();
          if (quickWindow) void invoke('quick_present').catch(showError);
        }
        if (event.kind === 'error' && quickWindow && ownTask) {
          showError(withElapsed(event.message ?? '转换失败', taskSnapshot.elapsed_ms));
        }
        return;
      }
      if (event.kind === 'progress') {
        el('status').textContent = '正在解析';
        el('detail').textContent = `${taskSnapshot.items.length>1 ? `文件 ${taskSnapshot.items.findIndex(item=>item.status==='running')+1}/${taskSnapshot.items.length} · ` : ''}第 ${event.page} / ${event.total} 页`;
        progress.value = event.total ? event.page! / event.total * 100 : 0;
        return;
      }
    });
    await listen('show-tasks',()=>{if(!quickWindow) taskPanel.open();});
    const revision = taskRevision;
    const snapshot = await invoke<TaskSnapshot>('task_snapshot');
    ready = true;
    if (taskRevision === revision) {
      taskSnapshot = snapshot;
    }
    renderTask(taskSnapshot);
    await listen<string>('shortcut-error', event=>{ if (!quickWindow) showError(event.payload); });
    await listen('drag-enabled',()=>{el('environment').textContent='Ctrl 拖动：银行流水 · Ctrl + Alt 拖动：通用转换';});
    if(quickWindow) {
      let gestureId=0;
      await listen<{id:number;profile:'bank'|'general'}>('quick-drag-begin',async event=>{
        gestureId=event.payload.id;
        if(busy) return;
        const previous=document.querySelectorAll<HTMLDialogElement>('dialog[open]:not(.drag-ring)');
        previous.forEach(dialog=>dialog.close());
        if(previous.length) await new Promise(resolve=>setTimeout(resolve,0));
        if(gestureId!==event.payload.id) return;
        if(dragRing.prepareDrag(event.payload.profile)) void invoke('drag_present',{id:event.payload.id}).catch(showError);
      });
      await listen<number>('quick-drag-end',event=>{
        if(gestureId===event.payload) {gestureId=0;dragRing.cancelDrag();}
      });
      const present = async (paths: string[]) => {
        // 上次错误或结果弹窗不能阻挡下一次明确的快捷转换请求。
        const previous = document.querySelectorAll<HTMLDialogElement>('dialog[open]:not(.drag-ring)');
        previous.forEach(dialog => dialog.close());
        if (previous.length) await new Promise(resolve => setTimeout(resolve, 0));
        if (dragRing.show(paths)) {
          await invoke('quick_present');
        }
      };
      await listen<string[]>('quick-selection',event=>{ void present(event.payload).catch(showError); });
      const selected=await invoke<string[]>('quick_selection');
      if(selected.length) await present(selected);
      else void getCurrentWindow().hide();
    }
    const shortcutReady=await invoke<boolean>('shortcut_status');
    el('environment').textContent=shortcutReady?'Ctrl + Alt + Space 呼出快捷转换 · 本地处理':'系统快捷键注册失败，可能被占用；可用上方快捷转换按钮。';
    try {
      await getCurrentWebview().onDragDropEvent(({ payload }) => {
        if (quickWindow) {
          dragRing.handle(payload);
          return;
        }
        if (payload.type !== 'drop' || busy || document.querySelector('dialog[open]')) return;
        if (payload.paths.length !== 1) {
          showError('银行流水每次处理一个文件，请拖入一份 PDF 或流水图片。');
          return;
        }
        selectStatement(payload.paths[0]);
      });
    } catch { el('environment').textContent = '拖放暂不可用，可使用快捷转换选择文件。'; }
    if(quickWindow) await invoke('drag_ready');
    if(await invoke<boolean>('drag_status')) el('environment').textContent='Ctrl 拖动：银行流水 · Ctrl + Alt 拖动：通用转换';
  } catch (error) { showError(error); }
}
void initialize();

const pdfDialog = el<HTMLDialogElement>('pdf-dialog');
let pdfFiles: string[] = [];
const pdfOperation = () => document.querySelector<HTMLInputElement>('input[name="pdf-operation"]:checked')!.value;
function renderFiles(files: string[], id: string) {
  const list = el(id); list.replaceChildren();
  files.forEach((path, index) => {
    const row = document.createElement('li');
    const name = document.createElement('span'); name.textContent = path.split(/[\\/]/).pop()!; name.title = path; row.append(name);
    for (const [text, delta] of [['上移', -1], ['下移', 1], ['移除', 0]] as const) {
      const button = document.createElement('button'); button.className = 'secondary'; button.textContent = text;
      button.setAttribute('aria-label', `${text}第 ${index + 1} 个文件`);
      button.disabled = delta !== 0 && (index + delta < 0 || index + delta >= files.length);
      button.onclick = () => { if (!delta) files.splice(index, 1); else [files[index], files[index+delta]] = [files[index+delta], files[index]]; renderFiles(files, id); };
      row.append(button);
    }
    list.append(row);
  });
}
const renderPdfFiles = () => renderFiles(pdfFiles, 'pdf-files');
el('pdf-tools').onclick = () => {
  if (busy) { showError('请先等待当前任务完成或取消。'); return; }
  el('pdf-error').textContent = ''; pdfDialog.showModal();
};
el('pdf-close').onclick = () => pdfDialog.close();
function updatePdfOperation() {
  el('pdf-pages-label').hidden = pdfOperation() === 'merge';
  el('pdf-angle-label').hidden = pdfOperation() !== 'rotate';
}
document.querySelectorAll('input[name="pdf-operation"]').forEach(radio => radio.addEventListener('change', updatePdfOperation));
updatePdfOperation();
el('pdf-pick').onclick = async () => {
  try {
    if (!isTauri()) throw new Error('选择文件请使用桌面版');
    const paths = await open({ multiple: true, filters: [{ name: 'PDF', extensions: ['pdf'] }] });
    if (paths) { pdfFiles.push(...paths); renderPdfFiles(); }
  } catch (error) { el('pdf-error').textContent = String(error); }
};
el('pdf-run').onclick = async () => {
  const operation = pdfOperation();
  if (busy || !ready) { el('pdf-error').textContent = '请使用桌面版，并等待当前任务结束。'; return; }
  if ((operation === 'merge' && pdfFiles.length < 2) || (operation !== 'merge' && pdfFiles.length !== 1)) {
    el('pdf-error').textContent = '合并至少选择两个文件；其他操作请选择一个文件。'; return;
  }
  const pages = el<HTMLInputElement>('pdf-pages').value;
  if (operation !== 'merge' && pages.trim() && !/^\s*\d+\s*(?:-\s*\d+\s*)?(?:[,，]\s*\d+\s*(?:-\s*\d+\s*)?)*$/.test(pages)) {
    el('pdf-error').textContent = '页码格式应为 1-3,5。'; return;
  }
  const secret = el<HTMLInputElement>('pdf-password');
  setBusy(true); pdfDialog.close();
  el('task-title').textContent = `PDF ${{merge:'合并',split:'拆分',extract:'提取',rotate:'旋转'}[operation]}`;
  el('status').textContent = '正在处理'; el('detail').textContent = '正在加载 PDF，请稍候。';
  progress.removeAttribute('value');
  try { await startConversion({input:pdfFiles[0], inputs:[...pdfFiles], mode:`pdf_${operation}` as 'pdf_merge'|'pdf_split'|'pdf_extract'|'pdf_rotate', pages, angle:Number(document.querySelector<HTMLInputElement>('input[name="pdf-angle"]:checked')!.value), password:secret.value, output_dir:null}); }
  catch(error) { setBusy(taskSnapshot.running); showError(error); }
  finally { secret.value = ''; }
};

const imageDialog = el<HTMLDialogElement>('image-dialog');
let imageFiles: string[] = [];
const imageFormat = () => document.querySelector<HTMLInputElement>('input[name="image-format"]:checked')!.value;
const imageMode = () => document.querySelector<HTMLInputElement>('input[name="image-mode"]:checked')!.value as 'image_convert'|'images_pdf'|'pdf_images';
el('image-tools').onclick = () => { if(busy) {showError('请先完成或取消当前任务。');return;} el('image-error').textContent='';imageDialog.showModal(); };
el('image-close').onclick = () => imageDialog.close();
function updateImageFields() {
  el('image-error').textContent='';
  const mode = imageMode();
  el('image-format-label').hidden = mode === 'images_pdf';

  el('image-password-label').hidden = mode !== 'pdf_images';
  const formats=Array.from(document.querySelectorAll<HTMLInputElement>('input[name="image-format"]'));
  for(const option of formats) option.disabled=mode==='pdf_images' && !['png','jpg'].includes(option.value);
  if(mode==='pdf_images' && !['png','jpg'].includes(imageFormat())) formats[0].checked=true;
  const usesQuality = mode !== 'images_pdf' && ['jpg','webp'].includes(imageFormat());
  el('image-quality-label').hidden = !usesQuality;
  el<HTMLInputElement>('image-quality').disabled = !usesQuality;
  el('image-format-help').textContent=mode==='pdf_images'?'PDF 页面支持导出 PNG 或 JPG。':'选择适合用途的格式；PNG 可保留透明背景。';

}
document.querySelectorAll('input[name="image-mode"]').forEach(radio=>radio.addEventListener('change',()=>{imageFiles=[];renderFiles(imageFiles,'image-files');updateImageFields();}));
updateImageFields();
document.querySelectorAll('input[name="image-format"]').forEach(radio=>radio.addEventListener('change',updateImageFields));
el('image-pick').onclick=async()=>{
  try {
    if(!isTauri()) throw new Error('选择文件请使用桌面版');
    const mode=imageMode();
    const filters=mode==='pdf_images' ? [{name:'PDF',extensions:['pdf']}] : [{name:'图片',extensions:imageInputExtensions},{name:'所有文件',extensions:['*']}];
    const paths=await open({multiple:mode!=='pdf_images',filters});
    if(paths){
      if(mode==='pdf_images') imageFiles=[];
      imageFiles.push(...(Array.isArray(paths)?paths:[paths]));
      renderFiles(imageFiles,'image-files');
    }
  } catch(error){el('image-error').textContent=String(error);}
};
el('image-run').onclick=async()=>{
  if(busy || !ready){el('image-error').textContent='请使用桌面版，并等待当前任务结束。';return;}
  const mode=imageMode(), widthField=el<HTMLInputElement>('image-width'), qualityField=el<HTMLInputElement>('image-quality');
  const width=Number(widthField.value), quality=qualityField.disabled ? 90 : Number(qualityField.value);
  if(!imageFiles.length || (mode==='pdf_images' && imageFiles.length!==1)){el('image-error').textContent='请选择文件；PDF 转图片一次选择一个 PDF。';return;}
  if(!Number.isInteger(width)||width<0||width>16000){el('image-error').textContent='宽度须为 0–16000 的整数。';widthField.focus();return;}
  if(!Number.isInteger(quality)||quality<1||quality>100){el('image-error').textContent='质量须为 1–100 的整数。';qualityField.focus();return;}
  const secret=el<HTMLInputElement>('image-password');
  setBusy(true);imageDialog.close();el('task-title').textContent='图片转换';el('status').textContent='正在处理';el('detail').textContent='正在加载文件，请稍候。';progress.removeAttribute('value');
  try{await startConversion({input:imageFiles[0],inputs:[...imageFiles],mode,format:imageFormat(),width,quality,password:secret.value,output_dir:null});}
  catch(error){setBusy(taskSnapshot.running);showError(error);}
  finally{secret.value='';}
};

const dragRing=createDragRing(async(paths,option)=>{
  setBusy(true);el('task-title').textContent=`快捷转换 · ${option.label}`;
  el('status').textContent='正在处理';el('detail').textContent='正在加载文件，请稍候。';progress.removeAttribute('value');
  try{await startConversion({input:paths[0],inputs:paths,mode:option.mode,separate:option.separate??false,format:option.format??'png',width:option.width??0,quality:option.quality??90,angle:option.angle??90,pages:'',password:'',output_dir:null});}
  catch(error){setBusy(taskSnapshot.running);showError(error);}
},()=>ready&&!busy,showError,()=>{if(quickWindow) void getCurrentWindow().hide();});
el('quick-pick').onclick=async()=>{
  if(busy){showError('请先完成或取消当前任务。');return;}
  try{
    if(!isTauri()) throw new Error('快捷转换请使用桌面版。');
    const paths=await open({multiple:true,filters:[{name:'可转换文件',extensions:['pdf',...imageInputExtensions]}]});
    if(paths) dragRing.show(paths);
  }catch(error){showError(error);}
};

if(quickWindow) {
  document.documentElement.classList.add('quick-root');
  document.body.classList.add('quick-window');
  const notice=document.createElement('dialog');notice.id='quick-notice';
  notice.innerHTML='<h2>操作未完成</h2><p id="quick-error" role="status"></p><button id="quick-error-close" class="secondary">关闭</button>';
  document.body.append(notice);
  notice.addEventListener('close',()=>void getCurrentWindow().hide());
  el('quick-error-close').onclick=()=>notice.close();
  document.addEventListener('keydown',event=>{if(event.key==='Escape'&&!document.querySelector('dialog[open]')) void getCurrentWindow().hide();});
}
