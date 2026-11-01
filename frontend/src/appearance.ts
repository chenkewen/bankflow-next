type Mode = 'light' | 'dark' | 'system';
type Theme = { mode: Mode; color: string };
const key = 'bankflow.appearance.v1';
const defaults: Theme = { mode: 'light', color: '#087f68' };
const system = matchMedia('(prefers-color-scheme: dark)');
const presets = [['松石绿', '#087f68'], ['海蓝', '#2563eb'], ['鸢尾紫', '#7755bb'], ['玫瑰', '#b83d66'], ['琥珀', '#a46b12']];
const validColor = (value: string) => /^#[0-9a-f]{6}$/i.test(value);
function read(): Theme {
  try {
    const value = JSON.parse(localStorage.getItem(key) || 'null');
    if (value && ['light', 'dark', 'system'].includes(value.mode) && validColor(value.color)) return value;
  } catch { /* 损坏或不可用的本地偏好不阻止界面启动。 */ }
  return { ...defaults };
}
let theme = read();
const rgb = (hex: string) => [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16));
const mix = (a: string, b: string, amount: number) => '#' + rgb(a).map((v, i) => Math.round(v * (1 - amount) + rgb(b)[i] * amount).toString(16).padStart(2, '0')).join('');
function luminance(hex: string) {
  const channels = rgb(hex).map(v => { const n = v / 255; return n <= .04045 ? n / 12.92 : ((n + .055) / 1.055) ** 2.4; });
  return channels[0] * .2126 + channels[1] * .7152 + channels[2] * .0722;
}
const contrast = (a: string, b: string) => (Math.max(luminance(a), luminance(b)) + .05) / (Math.min(luminance(a), luminance(b)) + .05);
function readable(color: string, background: string, target: string) {
  for (let i = 0; i <= 100; i++) {
    const candidate = mix(color, target, i / 100);
    if (contrast(candidate, background) >= 4.5) return candidate;
  }
  return target;
}
function apply() {
  const dark = theme.mode === 'dark' || (theme.mode === 'system' && system.matches);
  const surface = dark ? '#20272f' : '#ffffff';
  const accent = readable(theme.color, '#ffffff', '#000000');
  const ink = readable(theme.color, surface, dark ? '#ffffff' : '#000000');
  const values: Record<string, string> = {
    canvas: dark ? '#161c23' : '#f5f8f8', surface, text: dark ? '#e7edf4' : '#122b35',
    muted: dark ? '#b0bdca' : '#647680', border: dark ? '#45515d' : '#dde7e6',
    'control-border': dark ? '#687685' : '#aabfbd', sidebar: mix(theme.color, '#08121a', .8),
    accent, 'accent-hover': mix(accent, '#000000', .16), 'accent-ink': ink,
    'accent-soft': mix(theme.color, surface, dark ? .85 : .94), focus: ink,
    'sidebar-selected': mix(theme.color, '#18242c', .6), 'sidebar-highlight': mix(theme.color, '#ffffff', .6),
    error: dark ? '#ffada7' : '#a73932', 'error-soft': dark ? '#42292c' : '#fff5f3',
  };
  const root = document.documentElement;
  root.dataset.appearance = dark ? 'dark' : 'light';
  root.style.colorScheme = dark ? 'dark' : 'light';
  for (const [name, value] of Object.entries(values)) root.style.setProperty(`--${name}`, value);
  document.dispatchEvent(new Event('appearance-changed'));
}
export function initializeAppearance() {
  apply();
  system.addEventListener('change', apply);
  // 同一应用的主窗口和快捷圆盘共享本地存储，同步设置但不抢焦点。
  window.addEventListener('storage', event => { if (event.key === key || event.key === null) { theme = read(); apply(); } });
}
export function createAppearanceControls(): HTMLElement {
  const section = document.createElement('section');
  section.className = 'appearance-controls';
  section.innerHTML = `<p class="field-help">即选即用，自动保存在本机。</p>
    <fieldset class="appearance-modes"><legend>外观</legend>${[['light', '浅色'], ['dark', '深色'], ['system', '跟随系统']].map(([value, label]) => `<label><input type="radio" name="appearance-mode" value="${value}"><span>${label}</span></label>`).join('')}</fieldset>
    <fieldset class="accent-presets"><legend>主题色</legend>${presets.map(([name, color]) => `<button type="button" data-color="${color}" aria-label="${name}" aria-pressed="false" style="--swatch:${color}"><span></span>${name}</button>`).join('')}</fieldset>
    <div class="custom-accent"><label for="accent-picker">自定义颜色</label><input id="accent-picker" type="color" aria-label="选择自定义主题色"><input id="accent-hex" aria-label="主题色十六进制值" maxlength="7" spellcheck="false" placeholder="#087F68"></div>
    <p class="field-help">过亮的颜色会自动调整按钮深浅，保证文字清晰。</p>
    <div class="appearance-footer"><button type="button" class="secondary" data-reset>恢复默认</button><span data-theme-status role="status"></span></div>`;
  const picker = section.querySelector<HTMLInputElement>('#accent-picker')!;
  const hex = section.querySelector<HTMLInputElement>('#accent-hex')!;
  const status = section.querySelector<HTMLElement>('[data-theme-status]')!;
  function sync() {
    section.querySelector<HTMLInputElement>(`input[value="${theme.mode}"]`)!.checked = true;
    picker.value = theme.color;
    hex.value = theme.color.toUpperCase();
    section.querySelectorAll<HTMLButtonElement>('[data-color]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.color === theme.color.toLowerCase())));
  }
  function save(next: Theme) {
    try { localStorage.setItem(key, JSON.stringify(next)); theme = next; apply(); status.textContent = '已保存'; }
    catch { status.textContent = '保存失败，请检查本机存储是否可用。'; }
  }
  section.querySelectorAll<HTMLInputElement>('[name="appearance-mode"]').forEach(input => { input.onchange = () => save({ ...theme, mode: input.value as Mode }); });
  section.querySelectorAll<HTMLButtonElement>('[data-color]').forEach(button => { button.onclick = () => save({ ...theme, color: button.dataset.color! }); });
  picker.oninput = () => save({ ...theme, color: picker.value });
  hex.onchange = () => {
    if (!validColor(hex.value)) { hex.setAttribute('aria-invalid', 'true'); status.textContent = '请输入 # 加六位颜色值，例如 #2563EB。'; return; }
    hex.removeAttribute('aria-invalid'); save({ ...theme, color: hex.value.toLowerCase() });
  };
  section.querySelector<HTMLButtonElement>('[data-reset]')!.onclick = () => { hex.removeAttribute('aria-invalid'); save({ ...defaults }); };
  document.addEventListener('appearance-changed', sync);
  sync();
  return section;
}
