import { createRingSettings } from './ring-settings';
import { createAppearanceControls } from './appearance';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

type CloseAction = 'ask' | 'tray' | 'exit';

export function createDesktopSettings() {
  const settings = document.createElement('dialog');
  settings.id = 'desktop-settings';
  settings.setAttribute('aria-labelledby', 'desktop-settings-title');
  settings.innerHTML = `<div class="section-heading"><h2 id="desktop-settings-title">系统设置</h2><button class="secondary" data-back>关闭</button></div>
    <div class="settings-tabs" role="group" aria-label="设置分类"><button class="secondary" data-tab="general" aria-pressed="true">常规</button><button class="secondary" data-tab="appearance" aria-pressed="false">主题与配色</button><button class="secondary" data-tab="ring" aria-pressed="false">通用圆盘</button></div>
    <section data-general><p class="field-help">偏好保存在这台电脑，下次启动仍然生效。</p>
    <fieldset class="desktop-options"><legend>点击窗口关闭按钮时</legend>
    <label><input type="radio" name="close-action" value="ask"><span><strong>每次询问</strong><small>关闭前，由你决定是否保留后台运行。</small></span></label>
    <label><input type="radio" name="close-action" value="tray"><span><strong>最小化到系统托盘</strong><small>任务继续处理，Ctrl + Alt 拖动和快捷转换仍可使用。</small></span></label>
    <label><input type="radio" name="close-action" value="exit"><span><strong>退出应用</strong><small>结束 BankFlow；有任务正在处理时会先提醒。</small></span></label></fieldset>
    <p class="field-help">系统托盘位于任务栏右下角，图标可能收在“显示隐藏的图标”中。</p>
    <p data-status role="status"></p><div class="dialog-actions"><button class="primary" data-save>保存设置</button></div></section>`;
  const appearance = createAppearanceControls();
  appearance.hidden = true;
  const ring = createRingSettings(); ring.hidden = true;
  settings.append(appearance, ring);
  settings.querySelectorAll<HTMLButtonElement>('[data-tab]').forEach(button => { button.onclick = () => {
    const isAppearance = button.dataset.tab === 'appearance';
    appearance.hidden = !isAppearance;
    ring.hidden = button.dataset.tab !== 'ring';
    settings.querySelector<HTMLElement>('[data-general]')!.hidden = button.dataset.tab !== 'general';
    settings.querySelectorAll<HTMLButtonElement>('[data-tab]').forEach(tab => tab.setAttribute('aria-pressed', String(tab === button)));
  }; });
  const confirmation = document.createElement('dialog');
  confirmation.id = 'close-confirmation';
  confirmation.setAttribute('aria-labelledby', 'close-confirmation-title');
  confirmation.innerHTML = `<div class="section-heading"><h2 id="close-confirmation-title">关闭窗口</h2><button class="close-dialog-button" data-back aria-label="暂不关闭" title="暂不关闭"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18"/></svg></button></div>
    <p class="field-help">最小化到托盘，转换任务将在后台继续。</p>
    <div class="close-preference-row"><label class="remember-choice"><input type="checkbox" data-remember>记住选择</label><span>可在系统设置中修改</span></div>
    <p data-error role="alert"></p><div class="dialog-actions"><button class="secondary" data-action="exit">退出应用</button><button class="primary" data-action="tray" autofocus>最小化到托盘</button></div>`;
  document.body.append(settings, confirmation);
  for (const dialog of [settings, confirmation]) {
    dialog.querySelector<HTMLButtonElement>('[data-back]')!.onclick = () => dialog.close();
  }
  const status = settings.querySelector<HTMLElement>('[data-status]')!;
  const save = settings.querySelector<HTMLButtonElement>('[data-save]')!;
  async function open() {
    status.textContent = '正在读取设置…';
    save.disabled = true;
    if (!settings.open) settings.showModal();
    try {
      const action = isTauri() ? await invoke<CloseAction>('desktop_preference') : 'ask';
      settings.querySelector<HTMLInputElement>(`input[value="${action}"]`)!.checked = true;
      status.textContent = isTauri() ? '' : '浏览器仅展示设置，保存请使用桌面应用。';
      save.disabled = !isTauri();
    } catch (error) { status.textContent = String(error); }
  }
  settings.onchange = () => { status.textContent = ''; };
  save.onclick = async () => {
    const action = settings.querySelector<HTMLInputElement>('input[name="close-action"]:checked')?.value;
    if (!action) return;
    save.disabled = true;
    try {
      await invoke('save_desktop_preference', { action });
      status.textContent = '设置已保存，下次关闭窗口时生效。';
    } catch (error) { status.textContent = String(error); }
    finally { save.disabled = false; }
  };
  const error = confirmation.querySelector<HTMLElement>('[data-error]')!;
  const buttons = confirmation.querySelectorAll<HTMLButtonElement>('[data-action]');
  buttons.forEach(button => { button.onclick = async () => {
    buttons.forEach(item => { item.disabled = true; });
    error.textContent = '';
    try {
      await invoke('apply_close_choice', {
        action: button.dataset.action,
        remember: confirmation.querySelector<HTMLInputElement>('[data-remember]')!.checked,
      });
      confirmation.close();
    } catch (reason) { error.textContent = String(reason); }
    finally { buttons.forEach(item => { item.disabled = false; }); }
  }; });
  if (isTauri()) {
    void listen('show-settings', () => { void open(); });
    void listen('confirm-close', () => {
      if (confirmation.open) return;
      error.textContent = '';
      confirmation.querySelector<HTMLInputElement>('[data-remember]')!.checked = false;
      confirmation.showModal();
    });
  }
  return { open };
}
