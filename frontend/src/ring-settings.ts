import { imageInputExtensions, imageOutputFormats, type QuickOption } from './quick-options';

type Kind = 'pdf' | 'image';
interface Entry extends QuickOption { id: string }
interface Preference { order: string[]; quality: number; width: number; angle: number }
const key = 'bankflow.general-ring.v1';
const catalog: Record<Kind, Entry[]> = {
  pdf: [
    { id: 'png', label: 'PNG', mode: 'pdf_images', format: 'png' },
    { id: 'jpg', label: 'JPG', mode: 'pdf_images', format: 'jpg' },
    { id: 'split', label: '逐页拆分', mode: 'pdf_split' },
    { id: 'rotate', label: '旋转', mode: 'pdf_rotate' },
    { id: 'merge', label: '合并 PDF', mode: 'pdf_merge' },
  ],
  image: [
    ...imageOutputFormats.map(({format}) => ({ id: format, label: format.toUpperCase(), mode: 'image_convert' as const, format })),
    { id: 'pdf', label: '合并为 PDF', mode: 'images_pdf' },
  ],
};
function defaults(kind: Kind): Preference {
  return { order: catalog[kind].map(item => item.id), quality: 90, width: 0, angle: 90 };
}
function valid(value: unknown, min: number, max: number): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max;
}
function read(kind: Kind): Preference {
  try {
    const value = JSON.parse(localStorage.getItem(key) || '{}')[kind];
    if (!value || !Array.isArray(value.order)) return defaults(kind);
    const order = [...new Set<string>(value.order.filter((id: unknown) => catalog[kind].some(item => item.id === id)))];
    // 至少保留两个常用操作，防止配置成无法操作的空圆盘。
    if (order.length < 2) return defaults(kind);
    return { order, quality: valid(value.quality, 1, 100) ? value.quality : 90,
      width: valid(value.width, 0, 16000) ? value.width : 0, angle: [90, 180, 270].includes(value.angle) ? value.angle : 90 };
  } catch { return defaults(kind); }
}
export function generalOptions(paths: string[]): QuickOption[] {
  if (!paths.length) return [];
  const extensions = paths.map(path => path.split('.').pop()!.toLowerCase());
  const kind: Kind | null = extensions.every(ext => ext === 'pdf') ? 'pdf'
    : extensions.every(ext => imageInputExtensions.includes(ext)) ? 'image' : null;
  if (!kind) return [];
  const prefs = read(kind);
  return prefs.order.map(id => catalog[kind].find(item => item.id === id)!)
    .filter(item => item.mode !== 'pdf_merge' || paths.length > 1)
    .map(item => ({ ...item, quality: prefs.quality, width: prefs.width, angle: prefs.angle,
      label: item.mode === 'pdf_rotate' ? `旋转 ${prefs.angle}°` : item.mode === 'images_pdf' && paths.length === 1 ? 'PDF' : item.label,
      separate: kind === 'pdf' && paths.length > 1 && item.mode !== 'pdf_merge' }));
}

export function createRingSettings(): HTMLElement {
  const section = document.createElement('section');
  section.className = 'ring-settings';
  section.innerHTML = `<p class="field-help">Ctrl 拖动保留银行流水圆盘；Ctrl + Alt 拖动使用以下通用转换配置。</p>
    <div class="settings-tabs" role="group" aria-label="文件类型"><button class="secondary" data-kind="pdf" aria-pressed="true">PDF</button><button class="secondary" data-kind="image" aria-pressed="false">图片</button></div>
    <p class="field-help">勾选显示项目，用上下按钮调整顺序。多份 PDF 默认分别处理，合并需单独选择。</p>
    <div data-items></div><div class="ring-parameters"></div>
    <p role="status" data-ring-status></p><div class="dialog-actions"><button class="secondary" data-reset>恢复默认</button><button class="primary" data-save-ring>保存圆盘</button></div>`;
  let kind: Kind = 'pdf';
  let draft = read(kind);
  let order: string[] = [];
  const status = section.querySelector<HTMLElement>('[data-ring-status]')!;
  const items = section.querySelector<HTMLElement>('[data-items]')!;
  function renderItems() {
    items.replaceChildren();
    order.forEach((id, index) => {
      const item = catalog[kind].find(entry => entry.id === id)!;
      const row = document.createElement('div'); row.className = 'ring-setting-row';
      const label = document.createElement('label'); const input = document.createElement('input');
      input.type = 'checkbox'; input.checked = draft.order.includes(id);
      input.onchange = () => { draft.order = input.checked ? [...draft.order, id] : draft.order.filter(value => value !== id); status.textContent = ''; };
      label.append(input, document.createTextNode(item.label)); row.append(label);
      for (const [delta, text] of [[-1, '上移'], [1, '下移']] as const) {
        const button = document.createElement('button'); button.className = 'secondary'; button.textContent = text;
        button.setAttribute('aria-label', `${item.label}${text}`); button.disabled = index + delta < 0 || index + delta >= order.length;
        button.onclick = () => { [order[index], order[index + delta]] = [order[index + delta], order[index]]; renderItems(); status.textContent = ''; };
        row.append(button);
      }
      items.append(row);
    });
  }
  function render() {
    order = [...draft.order, ...catalog[kind].map(item => item.id).filter(id => !draft.order.includes(id))];
    renderItems(); status.textContent = '';
    section.querySelectorAll<HTMLButtonElement>('[data-kind]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.kind === kind)));
    const params = section.querySelector<HTMLElement>('.ring-parameters')!;
    params.innerHTML = kind === 'image' ? `<label>JPG / WEBP 质量<input data-quality type="number" min="1" max="100" value="${draft.quality}"></label><label>图片宽度（0 为原尺寸）<input data-width type="number" min="0" max="16000" value="${draft.width}"></label>`
      : `<fieldset><legend>旋转角度</legend>${[90,180,270].map(angle => `<label><input type="radio" name="ring-angle" value="${angle}" ${angle === draft.angle ? 'checked' : ''}>${angle}°</label>`).join('')}</fieldset>`;
  }
  section.querySelectorAll<HTMLButtonElement>('[data-kind]').forEach(button => { button.onclick = () => { kind = button.dataset.kind as Kind; draft = read(kind); render(); }; });
  section.querySelector<HTMLButtonElement>('[data-reset]')!.onclick = () => { draft = defaults(kind); render(); status.textContent = '已恢复默认，点击保存后生效。'; };
  section.querySelector<HTMLButtonElement>('[data-save-ring]')!.onclick = () => {
    const quality = Number(section.querySelector<HTMLInputElement>('[data-quality]')?.value ?? draft.quality);
    const width = Number(section.querySelector<HTMLInputElement>('[data-width]')?.value ?? draft.width);
    const angle = Number(section.querySelector<HTMLInputElement>('[name="ring-angle"]:checked')?.value ?? draft.angle);
    const enabled = order.filter(id => draft.order.includes(id));
    if (enabled.length < 2 || (kind === 'pdf' && enabled.filter(id => id !== 'merge').length < 2)) { status.textContent = '请至少保留两个单文件可用的操作。'; return; }
    if (!valid(quality,1,100) || !valid(width,0,16000)) { status.textContent = '质量须为 1–100，宽度须为 0–16000 的整数。'; return; }
    try {
      const saved = { pdf: read('pdf'), image: read('image') };
      draft = { order: enabled, quality, width, angle }; saved[kind] = draft;
      localStorage.setItem(key, JSON.stringify(saved)); status.textContent = '已保存，下次呼出通用圆盘时生效。';
    } catch { status.textContent = '保存失败，请检查本机存储空间后重试。'; }
  };
  render(); return section;
}
