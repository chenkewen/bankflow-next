import { invoke } from '@tauri-apps/api/core';
import { cancelConversion, type TaskSnapshot } from './api';

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
    const success=snapshot.items.filter(item=>item.status==='completed').length;
    const failed=snapshot.items.filter(item=>item.status==='error').length;
    summary.textContent=snapshot.items.length?`${snapshot.running?'处理中':'本次任务结束'} · 共 ${snapshot.items.length} 份，成功 ${success} 份，失败 ${failed} 份`:'还没有转换任务';
    cancel.hidden=!snapshot.running;
    if(!snapshot.running) cancel.disabled=false;
    list.replaceChildren();
    snapshot.items.forEach(item=>{
      const row=document.createElement('li');row.className='task-row';
      const heading=document.createElement('div');heading.className='task-row-heading';
      const name=document.createElement('strong');name.textContent=item.input.split(/[\\/]/).pop()??item.input;name.title=item.input;
      const badge=document.createElement('span');badge.className=`task-badge ${item.status}`;badge.textContent=labels[item.status];heading.append(name,badge);row.append(heading);
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
