import { invoke } from '@tauri-apps/api/core';
import { cancelConversion, type TaskSnapshot } from './api';

export function withElapsed(text: string, elapsedMs: number | null) {
  if (elapsedMs == null) return text;
  const duration = elapsedMs < 1000 ? `${elapsedMs} 毫秒` : `${elapsedMs / 1000} 秒`;
  return `${text} · 耗时 ${duration}`;
}

export function summarizeTasks({ running, items }: TaskSnapshot) {
  const counts = { pending: 0, running: 0, completed: 0, error: 0, cancelled: 0, skipped: 0 };
  for (const item of items) counts[item.status]++;
  let detail = `共 ${items.length} 份，成功 ${counts.completed} 份，失败 ${counts.error} 份`;
  if (counts.cancelled) detail += `，已取消 ${counts.cancelled} 份`;
  if (counts.skipped) detail += `，未执行 ${counts.skipped} 份`;
  if (!items.length) return { title: '还没有转换任务', detail: '', state: 'idle' };
  if (running) return { title: '处理中', detail, state: 'processing' };
  if (counts.error === items.length) {
    return { title: items.length === 1 ? '转换失败' : '全部任务失败', detail, state: 'error' };
  }
  if (counts.error) return { title: '部分任务失败', detail, state: 'error' };
  if (counts.completed === items.length) return { title: '已完成', detail, state: 'completed' };
  return { title: '已取消', detail, state: 'cancelled' };
}

export function createTaskPanel() {
  const dialog=document.createElement('dialog');dialog.id='tasks-dialog';dialog.setAttribute('aria-labelledby','tasks-title');
  dialog.innerHTML='<div class="section-heading"><h2 id="tasks-title">转换任务</h2><button class="secondary" data-close>关闭</button></div><p class="field-help">每份文件独立处理，结果保存在源文件旁；同名自动编号。</p><p data-summary role="status">还没有转换任务</p><ol class="task-list" aria-label="文件转换结果"></ol><p data-error role="alert"></p><div class="dialog-actions"><button class="secondary" data-cancel hidden>取消剩余任务</button></div>';
  document.body.append(dialog);
  const list=dialog.querySelector<HTMLOListElement>('ol')!;
  const summary=dialog.querySelector<HTMLElement>('[data-summary]')!;
  const error=dialog.querySelector<HTMLElement>('[data-error]')!;
  const cancel=dialog.querySelector<HTMLButtonElement>('[data-cancel]')!;
  dialog.querySelector<HTMLButtonElement>('[data-close]')!.onclick=()=>dialog.close();
  cancel.onclick=async()=>{cancel.disabled=true;try{await cancelConversion();summary.textContent='正在取消，已完成的结果会保留。';}catch(e){error.textContent=String(e);cancel.disabled=false;}};
  const labels={pending:'等待中',running:'处理中',completed:'已完成',error:'失败',cancelled:'已取消',skipped:'未执行'};
  function update(snapshot:TaskSnapshot) {
    const { title, detail } = summarizeTasks(snapshot);
    summary.textContent = withElapsed(detail ? `${title} · ${detail}` : title, snapshot.elapsed_ms);
    cancel.hidden=!snapshot.running;
    if(!snapshot.running) cancel.disabled=false;
    list.replaceChildren();
    snapshot.items.forEach(item=>{
      const row=document.createElement('li');row.className='task-row';
      const heading=document.createElement('div');heading.className='task-row-heading';
      const name=document.createElement('strong');name.textContent=item.input.split(/[\\/]/).pop()??item.input;name.title=item.input;
      const badge=document.createElement('span');badge.className=`task-badge ${item.status}`;badge.textContent=withElapsed(labels[item.status],item.elapsed_ms);heading.append(name,badge);row.append(heading);
      if(item.message){const message=document.createElement('p');message.className='task-message';message.textContent=item.message;row.append(message);}
      if(item.output){
        const path=document.createElement('p');path.className='path';path.textContent=item.output;row.append(path);
        const actions=document.createElement('div');actions.className='task-row-actions';
        for(const [label,folder] of [['打开结果',false],['打开目录',true]] as const){
          const button=document.createElement('button');button.className='secondary';button.textContent=label;button.setAttribute('aria-label',`${label}：${name.textContent}`);
          button.onclick=async()=>{error.textContent='';try{await invoke('open_result',{path:item.output,folder});}catch(e){error.textContent=String(e);}};actions.append(button);
        }row.append(actions);
      }
      list.append(row);
    });
  }
  return {update,open:()=>{error.textContent='';if(!dialog.open)dialog.showModal();}};
}
